//! TC attach + ringbuf event pump + seq-keyed waits.

use std::{collections::HashMap, net::Ipv4Addr, sync::Arc, time::Duration};

use anyhow::{Context, Result};
use aya::{
    Ebpf,
    maps::{Array, MapData, RingBuf},
    programs::{SchedClassifier, TcAttachType, Xdp, XdpFlags, tc},
};
use common::{
    FilterConfig, KIND_BOUNCE_INGRESS, KIND_ECHO_INGRESS, KIND_STIM_EGRESS, StampEvent,
};
use log::{debug, info, warn};
use tokio::{sync::{Mutex, Notify}, task::JoinHandle};

use crate::pipeline::RawTx;

#[derive(Debug, Clone, Copy)]
struct Sighting {
    ktime_ns: u64,
}

struct Pending {
    t0: Option<Sighting>,
    t_echo: Option<Sighting>,
    t_bounce: Option<Sighting>,
    notify: Arc<Notify>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SeqSight {
    pub t0_ns: Option<u64>,
    pub t_echo_ns: Option<u64>,
    pub t_bounce_ns: Option<u64>,
}

pub struct Harness {
    /// Must outlive TC/XDP links and ringbuf task.
    _ebpf: Ebpf,
    _tc_links: Vec<aya::programs::tc::SchedClassifierLinkId>,
    _xdp_link: Option<aya::programs::xdp::XdpLinkId>,
    raw: Arc<RawTx>,
    pending: Arc<Mutex<HashMap<u64, Pending>>>,
    esp_ip: Ipv4Addr,
    next_seq: u64,
    event_task: JoinHandle<()>,
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
        let mut tc_links = Vec::new();
        {
            let egress: &mut SchedClassifier = ebpf
                .program_mut("test_egress")
                .context("test_egress")?
                .try_into()?;
            egress.load()?;
            tc_links.push(egress.attach(&iface, TcAttachType::Egress)?);
            info!("attached test_egress (TC) on {iface}");
        }

        let xdp_link = {
            let xdp: &mut Xdp = ebpf
                .program_mut("test_xdp")
                .context("test_xdp")?
                .try_into()?;
            xdp.load()?;
            match xdp.attach(&iface, XdpFlags::SKB_MODE).or_else(|e_skb| {
                warn!("XDP SKB attach on {iface} failed: {e_skb}; trying default flags");
                xdp.attach(&iface, XdpFlags::default())
            }) {
                Ok(id) => {
                    info!("attached test_xdp (XDP) on {iface}");
                    Some(id)
                }
                Err(e) => {
                    warn!("XDP attach on {iface} failed: {e}");
                    None
                }
            }
        };
        if xdp_link.is_none() {
            let ingress: &mut SchedClassifier = ebpf
                .program_mut("test_ingress")
                .context("test_ingress")?
                .try_into()?;
            ingress.load()?;
            tc_links.push(ingress.attach(&iface, TcAttachType::Ingress)?);
            info!("attached test_ingress (TC fallback) on {iface}");
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

        Ok(Self {
            _ebpf: ebpf,
            _tc_links: tc_links,
            _xdp_link: xdp_link,
            raw,
            pending,
            esp_ip,
            next_seq: 1,
            event_task,
        })
    }

    pub fn alloc_seq(&mut self) -> u64 {
        let s = self.next_seq;
        self.next_seq = self.next_seq.wrapping_add(1).max(1);
        s
    }

    pub async fn reset_state(&self) {
        self.pending.lock().await.clear();
        // Let the ringbuf pump drop late events for cleared seqs before the next window.
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    /// Remove `seq` from the pending map and return TC sightings.
    pub async fn take_seq(&self, seq: u64) -> SeqSight {
        let mut map = self.pending.lock().await;
        match map.remove(&seq) {
            Some(p) => SeqSight {
                t0_ns: p.t0.map(|s| s.ktime_ns),
                t_echo_ns: p.t_echo.map(|s| s.ktime_ns),
                t_bounce_ns: p.t_bounce.map(|s| s.ktime_ns),
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
        self.register_seq(seq).await;
        let dst = dst_ip.unwrap_or(self.esp_ip);
        self.raw.send_stimulus(src_ip, dst, seq, &[])?;
        Ok(())
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
            if bytes.len() < ::core::mem::size_of::<StampEvent>() {
                continue;
            }
            let ev = unsafe { ::core::ptr::read_unaligned(bytes.as_ptr() as *const StampEvent) };
            handle_event(&pending, ev).await;
        }
        guard.clear_ready();
    }
}

async fn handle_event(pending: &Mutex<HashMap<u64, Pending>>, ev: StampEvent) {
    let mut map = pending.lock().await;
    // Only stamp seqs the userspace side registered. Late events after `reset_state`
    // must not re-insert entries (that bloated the map and starved the ringbuf reader).
    let Some(entry) = map.get_mut(&ev.seq) else {
        return;
    };
    match ev.kind {
        KIND_STIM_EGRESS => {
            debug!("event stim_egress seq={} t0_ns={}", ev.seq, ev.ktime_ns);
            entry.t0 = Some(Sighting {
                ktime_ns: ev.ktime_ns,
            });
            entry.notify.notify_waiters();
        }
        KIND_BOUNCE_INGRESS => {
            debug!(
                "event bounce_ingress seq={} t_bounce_ns={}",
                ev.seq, ev.ktime_ns
            );
            entry.t_bounce = Some(Sighting {
                ktime_ns: ev.ktime_ns,
            });
            entry.notify.notify_waiters();
        }
        KIND_ECHO_INGRESS => {
            debug!(
                "event echo_ingress seq={} sport={} t1_ns={}",
                ev.seq, ev.udp_sport, ev.ktime_ns
            );
            entry.t_echo = Some(Sighting {
                ktime_ns: ev.ktime_ns,
            });
            entry.notify.notify_waiters();
        }
        other => warn!("unknown event kind={other} seq={}", ev.seq),
    }
}
