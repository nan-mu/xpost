//! TC attach + ringbuf event pump + seq-keyed waits.

use std::{collections::HashMap, net::Ipv4Addr, sync::Arc, time::Duration};

use anyhow::{Context, Result, bail};
use aya::{
    Ebpf,
    maps::{Array, MapData, RingBuf},
    programs::{SchedClassifier, TcAttachType, tc},
};
use log::{debug, info, warn};
use test_common::{
    FilterConfig, KIND_BOUNCE_INGRESS, KIND_ECHO_INGRESS, KIND_STIM_EGRESS, MAGIC_COLLECT_RST,
    MAGIC_DUMP, MAGIC_STIM, StampEvent,
};
use tokio::{
    sync::{Mutex, Notify},
    task::JoinHandle,
    time::{Instant, timeout},
};

use crate::raw_tx::RawTx;

fn bind_reply_socket(port: u16) -> Result<std::net::UdpSocket> {
    use std::os::fd::FromRawFd;

    let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0) };
    if fd < 0 {
        bail!(
            "socket reply_port {port}: {}",
            std::io::Error::last_os_error()
        );
    }
    let on: libc::c_int = 1;
    let ret = unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_REUSEADDR,
            &on as *const _ as *const libc::c_void,
            std::mem::size_of_val(&on) as libc::socklen_t,
        )
    };
    if ret != 0 {
        unsafe { libc::close(fd) };
        bail!(
            "SO_REUSEADDR reply_port {port}: {}",
            std::io::Error::last_os_error()
        );
    }
    let sockaddr = libc::sockaddr_in {
        sin_family: libc::AF_INET as _,
        sin_port: port.to_be(),
        sin_addr: libc::in_addr {
            s_addr: libc::INADDR_ANY,
        },
        sin_zero: [0; 8],
    };
    let ret = unsafe {
        libc::bind(
            fd,
            &sockaddr as *const _ as *const libc::sockaddr,
            std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
        )
    };
    if ret != 0 {
        unsafe { libc::close(fd) };
        bail!(
            "bind reply_port {port}: {}",
            std::io::Error::last_os_error()
        );
    }
    let sock = unsafe { std::net::UdpSocket::from_raw_fd(fd) };
    sock.set_nonblocking(true)?;
    Ok(sock)
}

#[derive(Debug, Clone, Copy)]
struct Sighting {
    ktime_ns: u64,
    l2_len: u16,
}

struct Pending {
    t0: Option<Sighting>,
    t_echo: Option<Sighting>,
    t_bounce: Option<Sighting>,
    notify: Arc<Notify>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SeqSight {
    #[allow(dead_code)]
    pub t0_ns: Option<u64>,
    #[allow(dead_code)]
    pub t_echo_ns: Option<u64>,
    #[allow(dead_code)]
    pub t_bounce_ns: Option<u64>,
    #[allow(dead_code)]
    pub bounce_l2: Option<u16>,
}

pub struct Harness {
    /// Must outlive TC links and ringbuf task.
    _ebpf: Ebpf,
    _links: Vec<aya::programs::tc::SchedClassifierLinkId>,
    raw: Arc<RawTx>,
    pending: Arc<Mutex<HashMap<u64, Pending>>>,
    pair_timeout: Duration,
    pub esp_ip: Ipv4Addr,
    pub vip_ip: Option<Ipv4Addr>,
    pub echo_port: u16,
    #[allow(dead_code)]
    pub reply_port: u16,
    pub ctrl_port: u16,
    #[allow(dead_code)]
    pub iface: String,
    next_seq: u64,
    event_task: JoinHandle<()>,
    reply_sock: Arc<tokio::net::UdpSocket>,
    reply_mux: Arc<Mutex<()>>,
}

fn ipv4_wire_u32(ip: Ipv4Addr) -> u32 {
    u32::from_ne_bytes(ip.octets())
}

impl Harness {
    pub async fn start(
        iface: String,
        esp_ip: Ipv4Addr,
        vip_ip: Option<Ipv4Addr>,
        echo_port: u16,
        reply_port: u16,
        ctrl_port: u16,
        pair_timeout_ms: u64,
        raw: RawTx,
    ) -> Result<Self> {
        let raw = Arc::new(raw);
        let rlim = libc::rlimit {
            rlim_cur: libc::RLIM_INFINITY,
            rlim_max: libc::RLIM_INFINITY,
        };
        let ret = unsafe { libc::setrlimit(libc::RLIMIT_MEMLOCK, &rlim) };
        if ret != 0 {
            warn!("setrlimit RLIMIT_MEMLOCK failed ret={ret}");
        }

        // aya_build writes OUT_DIR/<bin-name>; target-dir is OUT_DIR/<package-name>/ (directory).
        let mut ebpf = Ebpf::load(aya::include_bytes_aligned!(concat!(
            env!("OUT_DIR"),
            "/test_bpf"
        )))?;
        match aya_log::EbpfLogger::init(&mut ebpf) {
            Ok(_) => info!("eBPF logger attached"),
            Err(e) => warn!("eBPF logger init failed: {e}"),
        }

        {
            let mut cfg_map: Array<&mut MapData, FilterConfig> =
                Array::try_from(ebpf.map_mut("CONFIG").context("CONFIG map")?)?;
            let cfg = FilterConfig {
                esp_ip_be: ipv4_wire_u32(esp_ip),
                vip_ip_be: vip_ip.map(ipv4_wire_u32).unwrap_or(0),
                echo_port,
                reply_port,
            };
            cfg_map.set(0, cfg, 0)?;
            info!(
                "CONFIG esp_be={:#x} vip_be={:#x} echo={} reply={}",
                cfg.esp_ip_be, cfg.vip_ip_be, cfg.echo_port, cfg.reply_port
            );
        }

        let _ = tc::qdisc_add_clsact(&iface);
        let mut links = Vec::new();
        {
            let egress: &mut SchedClassifier = ebpf
                .program_mut("test_egress")
                .context("test_egress")?
                .try_into()?;
            egress.load()?;
            links.push(egress.attach(&iface, TcAttachType::Egress)?);
            info!("attached test_egress on {iface}");
        }
        {
            let ingress: &mut SchedClassifier = ebpf
                .program_mut("test_ingress")
                .context("test_ingress")?
                .try_into()?;
            ingress.load()?;
            links.push(ingress.attach(&iface, TcAttachType::Ingress)?);
            info!("attached test_ingress on {iface}");
        }

        let events = RingBuf::try_from(ebpf.take_map("EVENTS").context("EVENTS map")?)?;
        let pending: Arc<Mutex<HashMap<u64, Pending>>> = Arc::new(Mutex::new(HashMap::new()));
        let pending_ev = pending.clone();
        let event_task = tokio::spawn(async move {
            if let Err(e) = event_loop(events, pending_ev).await {
                warn!("event_loop ended: {e:#}");
            }
        });

        tokio::task::yield_now().await;

        let reply_sock = {
            let std_sock = bind_reply_socket(reply_port)
                .with_context(|| format!("bind reply UDP :{reply_port}"))?;
            tokio::net::UdpSocket::from_std(std_sock)
                .with_context(|| format!("tokio reply UDP :{reply_port}"))?
        };
        info!("reply UDP bound :{reply_port} (shared, reused for all ctrl acks)");

        Ok(Self {
            _ebpf: ebpf,
            _links: links,
            raw,
            pending,
            pair_timeout: Duration::from_millis(pair_timeout_ms),
            esp_ip,
            vip_ip,
            echo_port,
            reply_port,
            ctrl_port,
            iface,
            next_seq: 1,
            event_task,
            reply_sock: Arc::new(reply_sock),
            reply_mux: Arc::new(Mutex::new(())),
        })
    }

    async fn drain_reply_socket(&self) {
        let mut buf = [0u8; 2048];
        loop {
            match self.reply_sock.try_recv_from(&mut buf) {
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(_) => break,
            }
        }
    }

    async fn recv_reply<T, F>(&self, wait: Duration, label: &str, mut on_pkt: F) -> Result<T>
    where
        F: FnMut(&[u8]) -> Option<T>,
    {
        let _mux = self.reply_mux.lock().await;
        self.drain_reply_socket().await;
        let started = Instant::now();
        let mut buf = vec![0u8; 2048];
        loop {
            let left = wait.saturating_sub(started.elapsed());
            if left.is_zero() {
                bail!("timeout waiting for {label}");
            }
            match timeout(left, self.reply_sock.recv_from(&mut buf)).await {
                Ok(Ok((len, _))) => {
                    if let Some(v) = on_pkt(&buf[..len]) {
                        return Ok(v);
                    }
                }
                Ok(Err(e)) => return Err(e.into()),
                Err(_) => bail!("timeout waiting for {label}"),
            }
        }
    }

    fn skip_stim_dump_echo(buf: &[u8]) -> bool {
        if buf.len() >= 6
            && (buf[..6] == MAGIC_STIM || buf[..6] == MAGIC_DUMP || buf[..6] == MAGIC_COLLECT_RST)
        {
            return true;
        }
        buf.len() == 1 && buf[0] == test_common::ECHO_BYTE
    }

    pub fn alloc_seq(&mut self) -> u64 {
        let s = self.next_seq;
        self.next_seq = self.next_seq.wrapping_add(1).max(1);
        s
    }

    pub async fn reset_state(&self) {
        self.pending.lock().await.clear();
    }

    /// How many pending seqs already have an echo or bounce stamp.
    #[allow(dead_code)]
    pub async fn count_replies(&self) -> u32 {
        let map = self.pending.lock().await;
        map.values()
            .filter(|p| p.t_echo.is_some() || p.t_bounce.is_some())
            .count() as u32
    }

    /// Remove `seq` from the pending map and return TC sightings.
    pub async fn take_seq(&self, seq: u64) -> SeqSight {
        let mut map = self.pending.lock().await;
        match map.remove(&seq) {
            Some(p) => SeqSight {
                t0_ns: p.t0.map(|s| s.ktime_ns),
                t_echo_ns: p.t_echo.map(|s| s.ktime_ns),
                t_bounce_ns: p.t_bounce.map(|s| s.ktime_ns),
                bounce_l2: p.t_bounce.map(|s| s.l2_len),
            },
            None => SeqSight::default(),
        }
    }

    async fn register_seq(&self, seq: u64) {
        let mut map = self.pending.lock().await;
        map.insert(
            seq,
            Pending {
                t0: None,
                t_echo: None,
                t_bounce: None,
                notify: Arc::new(Notify::new()),
            },
        );
    }

    /// Stimulus to `dst_ip` (default: STA / esp_ip). Payload is `FWSTIM`+seq only.
    pub async fn send_stimulus(
        &self,
        src_ip: Ipv4Addr,
        dst_ip: Option<Ipv4Addr>,
        seq: u64,
    ) -> Result<()> {
        self.send_stimulus_pad(src_ip, dst_ip, seq, &[]).await
    }

    /// Stimulus with extra UDP bytes after `FWSTIM`+seq (pairing still uses seq).
    pub async fn send_stimulus_pad(
        &self,
        src_ip: Ipv4Addr,
        dst_ip: Option<Ipv4Addr>,
        seq: u64,
        pad: &[u8],
    ) -> Result<()> {
        self.register_seq(seq).await;
        let dst = dst_ip.unwrap_or(self.esp_ip);
        self.raw.send_stimulus(src_ip, dst, seq, pad)?;
        Ok(())
    }

    /// RAW UDP to STA echo port (no TC seq registration — collect stream only).
    pub fn send_echo_payload(&self, src_ip: Ipv4Addr, payload: &[u8]) -> Result<()> {
        self.raw
            .send_udp(src_ip, self.esp_ip, self.echo_port, payload)
    }

    /// RAW UDP to STA control port (`:8081`).
    pub fn send_ctrl_payload(&self, src_ip: Ipv4Addr, payload: &[u8]) -> Result<()> {
        self.raw
            .send_udp(src_ip, self.esp_ip, self.ctrl_port, payload)
    }

    /// `FWRST1` on ctrl port; wait for rst ack on `reply_port`.
    pub async fn collect_reset(&self, src_ip: Ipv4Addr) -> Result<()> {
        self.send_ctrl_payload(src_ip, &MAGIC_COLLECT_RST)?;
        self.recv_reply(self.pair_timeout, "collect rst ack", |buf| {
            if buf.len() >= 6 && buf[..6] == MAGIC_COLLECT_RST {
                Some(())
            } else if Self::skip_stim_dump_echo(buf) {
                None
            } else {
                None
            }
        })
        .await
    }

    /// `FWDMP1` on ctrl port; wait for dump reply on `reply_port`.
    pub async fn collect_dump(&self, src_ip: Ipv4Addr, stream_len: u16) -> Result<Vec<u8>> {
        let mut req = [0u8; 8];
        req[..6].copy_from_slice(&MAGIC_DUMP);
        req[6..8].copy_from_slice(&stream_len.to_le_bytes());
        self.send_ctrl_payload(src_ip, &req)?;

        self.recv_reply(self.pair_timeout, "collect dump", |buf| {
            if buf.len() < 8 || buf[..6] != MAGIC_DUMP {
                return None;
            }
            let n = u16::from_le_bytes([buf[6], buf[7]]) as usize;
            if buf.len() < 8 + n {
                return None;
            }
            Some(buf[8..8 + n].to_vec())
        })
        .await
    }

    pub async fn wait_t0(&self, seq: u64, wait: Option<Duration>) -> Result<Option<u64>> {
        self.wait_field(seq, wait, |p| p.t0.as_ref().map(|s| s.ktime_ns), "wait_t0")
            .await
    }

    pub async fn wait_echo(&self, seq: u64, wait: Option<Duration>) -> Result<Option<u64>> {
        self.wait_field(
            seq,
            wait,
            |p| p.t_echo.as_ref().map(|s| s.ktime_ns),
            "wait_echo",
        )
        .await
    }

    pub async fn wait_bounce(&self, seq: u64, wait: Option<Duration>) -> Result<Option<u64>> {
        Ok(self
            .wait_bounce_meta(seq, wait)
            .await?
            .map(|(ktime_ns, _)| ktime_ns))
    }

    /// Bounce sighting: ktime + TC L2 length.
    pub async fn wait_bounce_meta(
        &self,
        seq: u64,
        wait: Option<Duration>,
    ) -> Result<Option<(u64, u16)>> {
        self.wait_field(
            seq,
            wait,
            |p| p.t_bounce.as_ref().map(|s| (s.ktime_ns, s.l2_len)),
            "wait_bounce",
        )
        .await
    }

    /// True if a bounce was sighted for `seq` (no wait).
    pub async fn saw_bounce(&self, seq: u64) -> bool {
        let map = self.pending.lock().await;
        map.get(&seq).is_some_and(|p| p.t_bounce.is_some())
    }

    async fn wait_field<T: Copy>(
        &self,
        seq: u64,
        wait: Option<Duration>,
        get: impl Fn(&Pending) -> Option<T>,
        label: &str,
    ) -> Result<Option<T>> {
        let deadline = wait.unwrap_or(self.pair_timeout);
        let started = Instant::now();
        loop {
            {
                let map = self.pending.lock().await;
                if let Some(p) = map.get(&seq) {
                    if let Some(v) = get(p) {
                        return Ok(Some(v));
                    }
                } else {
                    bail!("{label}: unknown seq {seq}");
                }
            }
            let left = deadline.saturating_sub(started.elapsed());
            if left.is_zero() {
                return Ok(None);
            }
            let n = {
                let map = self.pending.lock().await;
                map.get(&seq).map(|p| p.notify.clone())
            };
            let Some(n) = n else {
                bail!("{label}: seq {seq} vanished");
            };
            let _ = timeout(left, n.notified()).await;
        }
    }

    pub fn pair_timeout(&self) -> Duration {
        self.pair_timeout
    }

    pub fn raw(&self) -> Arc<RawTx> {
        self.raw.clone()
    }

    /// Drop-stats RESET on ctrl port (`0x01`, no reply).
    pub async fn drop_stats_reset(&self, src_ip: Ipv4Addr) -> Result<()> {
        use test_common::drop_stats::{self, OP_RESET};

        let mut req = [0u8; drop_stats::REQ_LEN];
        drop_stats::encode_req(OP_RESET, &mut req).context("encode drop stats RESET")?;
        self.send_ctrl_payload(src_ip, &req)?;
        Ok(())
    }

    /// Drop-stats STOP on ctrl port (`0x02`, no reply): disable path, seal `t_last`.
    pub async fn drop_stats_stop(&self, src_ip: Ipv4Addr) -> Result<()> {
        use test_common::drop_stats::{self, OP_STOP};

        let mut req = [0u8; drop_stats::REQ_LEN];
        drop_stats::encode_req(OP_STOP, &mut req).context("encode drop stats STOP")?;
        self.send_ctrl_payload(src_ip, &req)?;
        Ok(())
    }

    /// Drop-stats QUERY on ctrl port (`0x00` → 24-byte reply).
    pub async fn drop_stats_query(
        &self,
        src_ip: Ipv4Addr,
    ) -> Result<test_common::drop_stats::DropStats> {
        use test_common::drop_stats::{self, OP_QUERY};

        let mut req = [0u8; drop_stats::REQ_LEN];
        drop_stats::encode_req(OP_QUERY, &mut req).context("encode drop stats QUERY")?;
        self.send_ctrl_payload(src_ip, &req)?;

        self.recv_reply(self.pair_timeout, "drop stats reply", |buf| {
            drop_stats::decode_reply(buf)
        })
        .await
    }

    /// Wait for DUT `OP_BENCH_DONE` on `reply_port` (ringbuf cap path).
    pub async fn wait_bench_done(
        &self,
        timeout: Duration,
    ) -> Result<test_common::drop_stats::DropStats> {
        use test_common::drop_stats;

        self.recv_reply(timeout, "bench done notify", |buf| {
            drop_stats::decode_bench_done(buf)
        })
        .await
    }

    /// Send `MapCrudOp` to STA:`ctrl_port`, wait for `MapCrudAck` on `reply_port`.
    pub async fn map_crud_op(
        &self,
        src_ip: Ipv4Addr,
        op: test_common::map_crud_op::MapCrudOp<'_>,
    ) -> Result<test_common::map_crud_op::MapCrudAck<'static>> {
        use test_common::map_crud_op;

        let mut payload = [0u8; 512];
        let n = map_crud_op::encode_op(&op, &mut payload).context("encode MapCrudOp")?;
        self.raw
            .send_udp(src_ip, self.esp_ip, self.ctrl_port, &payload[..n])?;

        self.recv_reply(self.pair_timeout, "MapCrudAck", |buf| {
            if Self::skip_stim_dump_echo(buf) {
                return None;
            }
            match map_crud_op::decode_ack(buf) {
                Ok(ack) => {
                    let value: &'static [u8] = if ack.value.is_empty() {
                        &[]
                    } else {
                        &*Box::leak(ack.value.to_vec().into_boxed_slice())
                    };
                    let reason: &'static str = if ack.reason.is_empty() {
                        ""
                    } else {
                        Box::leak(ack.reason.to_string().into_boxed_str())
                    };
                    Some(test_common::map_crud_op::MapCrudAck {
                        kind: ack.kind,
                        status: ack.status,
                        value,
                        reason,
                    })
                }
                Err(_) => None,
            }
        })
        .await
    }

    /// Send `HotLoadOp` to STA:`ctrl_port`, wait for `HotLoadOpAck` on `reply_port`.
    pub async fn hot_load_op(
        &self,
        src_ip: Ipv4Addr,
        op: test_common::hot_load_op::HotLoadOp<'_>,
    ) -> Result<test_common::hot_load_op::HotLoadOpAck<'static>> {
        self.hot_load_op_wait(src_ip, op, self.pair_timeout).await
    }

    pub async fn hot_load_op_wait(
        &self,
        src_ip: Ipv4Addr,
        op: test_common::hot_load_op::HotLoadOp<'_>,
        wait: Duration,
    ) -> Result<test_common::hot_load_op::HotLoadOpAck<'static>> {
        use test_common::hot_load_op;

        let mut payload = vec![0u8; op.payload.len() + 64];
        let n = hot_load_op::encode_op(&op, &mut payload).context("encode HotLoadOp")?;
        self.raw
            .send_udp(src_ip, self.esp_ip, self.ctrl_port, &payload[..n])?;

        self.recv_reply(wait, "HotLoadOpAck", |buf| {
            if Self::skip_stim_dump_echo(buf) {
                return None;
            }
            match hot_load_op::decode_ack(buf) {
                Ok(ack) => {
                    let reason: &'static str = if ack.reason.is_empty() {
                        ""
                    } else {
                        Box::leak(ack.reason.to_string().into_boxed_str())
                    };
                    Some(test_common::hot_load_op::HotLoadOpAck {
                        kind: ack.kind,
                        region: ack.region,
                        dram_slot: ack.dram_slot,
                        status: ack.status,
                        stashed_len: ack.stashed_len,
                        loaded_len: ack.loaded_len,
                        readback_len: ack.readback_len,
                        copied_len: ack.copied_len,
                        sample_len: ack.sample_len,
                        sample: ack.sample,
                        reason,
                    })
                }
                Err(_) => None,
            }
        })
        .await
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.event_task.abort();
    }
}

async fn event_loop(
    events: RingBuf<MapData>,
    pending: Arc<Mutex<HashMap<u64, Pending>>>,
) -> Result<()> {
    let mut fd = tokio::io::unix::AsyncFd::with_interest(events, tokio::io::Interest::READABLE)?;
    loop {
        let mut guard = fd.readable_mut().await?;
        let ring = guard.get_inner_mut();
        while let Some(item) = ring.next() {
            let bytes: &[u8] = &item;
            if bytes.len() < core::mem::size_of::<StampEvent>() {
                continue;
            }
            let ev = unsafe { core::ptr::read_unaligned(bytes.as_ptr() as *const StampEvent) };
            handle_event(&pending, ev).await;
        }
        guard.clear_ready();
    }
}

async fn handle_event(pending: &Mutex<HashMap<u64, Pending>>, ev: StampEvent) {
    let mut map = pending.lock().await;
    match ev.kind {
        KIND_STIM_EGRESS => {
            let entry = map.entry(ev.seq).or_insert_with(|| Pending {
                t0: None,
                t_echo: None,
                t_bounce: None,
                notify: Arc::new(Notify::new()),
            });
            debug!("event stim_egress seq={} t0_ns={}", ev.seq, ev.ktime_ns);
            entry.t0 = Some(Sighting {
                ktime_ns: ev.ktime_ns,
                l2_len: ev.l2_len,
            });
            entry.notify.notify_waiters();
        }
        KIND_BOUNCE_INGRESS => {
            let entry = map.entry(ev.seq).or_insert_with(|| Pending {
                t0: None,
                t_echo: None,
                t_bounce: None,
                notify: Arc::new(Notify::new()),
            });
            debug!(
                "event bounce_ingress seq={} t_bounce_ns={}",
                ev.seq, ev.ktime_ns
            );
            entry.t_bounce = Some(Sighting {
                ktime_ns: ev.ktime_ns,
                l2_len: ev.l2_len,
            });
            entry.notify.notify_waiters();
        }
        KIND_ECHO_INGRESS => {
            let entry = map.entry(ev.seq).or_insert_with(|| Pending {
                t0: None,
                t_echo: None,
                t_bounce: None,
                notify: Arc::new(Notify::new()),
            });
            debug!(
                "event echo_ingress seq={} sport={} t1_ns={}",
                ev.seq, ev.udp_sport, ev.ktime_ns
            );
            entry.t_echo = Some(Sighting {
                ktime_ns: ev.ktime_ns,
                l2_len: ev.l2_len,
            });
            entry.notify.notify_waiters();
        }
        other => warn!("unknown event kind={other} seq={}", ev.seq),
    }
}
