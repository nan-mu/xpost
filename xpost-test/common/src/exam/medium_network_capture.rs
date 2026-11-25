//! Retain both directions of the just_tx capture with their pcap timestamps.

use std::{io, process::Stdio, time::Duration};

use anyhow::{Context, Result, bail, ensure};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, BufReader},
    process::{Child, Command},
    sync::oneshot,
    task::JoinHandle,
    time::timeout,
};
use uuid::Uuid;

use crate::{config::format_mac, pipeline::build_udp_frame};

use super::{Link, Send};

// At 4,000 pps for 30 s, 240,000 records of 82 + 16 bytes total 22.4 MiB.
// Allow duplicate replies and other traffic that matches the filter.
const SIZE_LIMIT: usize = 32 * 1024 * 1024;

pub(super) fn check_rx_path(bridge: &str, rx_iface: Option<&str>) -> Result<()> {
    let Some(iface) = rx_iface else {
        return Ok(());
    };
    ensure!(
        [bridge, iface].iter().all(|name| !name.is_empty()
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
            && !matches!(*name, "." | "..")),
        "invalid capture interface name"
    );
    ensure!(
        bridge == iface
            || std::path::Path::new("/sys/class/net")
                .join(bridge)
                .join("brif")
                .join(iface)
                .exists(),
        "Just TX RX interface {iface} is outside capture bridge {bridge}"
    );
    ensure!(
        !rx_gro_list(iface).with_context(|| format!("check GRO on {iface}"))?,
        "Just TX needs rx-gro-list disabled on {iface} for per-packet timestamps"
    );
    Ok(())
}

#[cfg(target_os = "linux")]
fn rx_gro_list(iface: &str) -> Result<bool> {
    use std::{net::UdpSocket, os::fd::AsRawFd};

    // Linux ethtool UAPI. ETH_SS_FEATURES is string set 4.
    const STRING_SET: u32 = 4;
    const FEATURE_LIMIT: usize = 256;
    const FEATURE_BLOCKS: usize = FEATURE_LIMIT / 32;
    #[repr(C)]
    struct StringSet {
        command: u32,
        reserved: u32,
        mask: u64,
        count: u32,
    }
    #[repr(C)]
    struct FeatureNames {
        command: u32,
        set: u32,
        count: u32,
        names: [[u8; 32]; FEATURE_LIMIT],
    }
    #[repr(C)]
    struct Features {
        command: u32,
        blocks: u32,
        values: [[u32; 4]; FEATURE_BLOCKS],
    }
    let socket = UdpSocket::bind("0.0.0.0:0")?;
    ensure!(
        iface.len() < libc::IFNAMSIZ,
        "RX interface name is too long"
    );
    let request = |data: *mut libc::c_char| -> io::Result<()> {
        // ifreq must include the trailing NUL in the kernel interface name.
        let mut request: libc::ifreq = unsafe { std::mem::zeroed() };
        for (slot, byte) in request.ifr_name.iter_mut().zip(iface.bytes()) {
            *slot = byte as libc::c_char;
        }
        request.ifr_ifru.ifru_data = data;
        // Each UAPI buffer has room for every item specified by its count field.
        let returned =
            unsafe { libc::ioctl(socket.as_raw_fd(), libc::SIOCETHTOOL as _, &mut request) };
        if returned < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    };
    let mut size = StringSet {
        command: 0x37,
        reserved: 0,
        mask: 1 << STRING_SET,
        count: 0,
    };
    request(std::ptr::from_mut(&mut size).cast())?;
    ensure!(
        (1..=FEATURE_LIMIT).contains(&(size.count as usize)),
        "invalid ethtool feature count"
    );
    let mut names = FeatureNames {
        command: 0x1b,
        set: STRING_SET,
        count: size.count,
        names: [[0; 32]; FEATURE_LIMIT],
    };
    request(std::ptr::from_mut(&mut names).cast())?;
    let feature = names.names[..size.count as usize]
        .iter()
        .position(|name| name.starts_with(b"rx-gro-list\0"));
    let Some(index) = feature else {
        return Ok(false);
    };
    let mut features = Features {
        command: 0x3a,
        blocks: size.count.div_ceil(32),
        values: [[0; 4]; FEATURE_BLOCKS],
    };
    request(std::ptr::from_mut(&mut features).cast())?;
    // Feature blocks contain available, requested, active and never_changed masks.
    Ok(features.values[index / 32][2] & (1 << (index % 32)) != 0)
}

#[cfg(not(target_os = "linux"))]
fn rx_gro_list(_iface: &str) -> Result<bool> {
    bail!("RX offload verification requires Linux")
}

pub(super) struct Capture {
    child: Child,
    reader: JoinHandle<io::Result<Vec<u8>>>,
    stderr: JoinHandle<io::Result<String>>,
    listening: oneshot::Receiver<()>,
    ready: bool,
    iface: String,
    rx_iface: Option<String>,
}

impl Capture {
    pub(super) fn start(link: &Link) -> Result<Self> {
        check_rx_path(&link.iface, link.capture_rx_iface.as_deref())?;
        let mut child = Command::new("tcpdump")
            .args([
                "-i",
                &link.iface,
                "-nn",
                "-U",
                "-s",
                "0",
                "-j",
                "host",
                "-w",
                "-",
                "ether",
                "host",
                &format_mac(&link.dut_mac),
                "and",
                "udp",
                "port",
                &link.port.to_string(),
            ])
            .kill_on_drop(true)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("start just_tx tcpdump")?;
        let mut stdout = child
            .stdout
            .take()
            .context("tcpdump stdout")?
            .take(SIZE_LIMIT as u64 + 1);
        let mut lines = BufReader::new(child.stderr.take().context("tcpdump stderr")?).lines();
        let (ready, listening) = oneshot::channel();
        let reader = tokio::spawn(async move {
            let mut bytes = Vec::new();
            stdout.read_to_end(&mut bytes).await?;
            Ok(bytes)
        });
        let stderr = tokio::spawn(async move {
            let mut ready = Some(ready);
            let mut text = String::new();
            while let Some(line) = lines.next_line().await? {
                if line.starts_with("tcpdump: listening on ") {
                    if let Some(ready) = ready.take() {
                        let _ = ready.send(());
                    }
                }
                text.push_str(&line);
                text.push('\n');
            }
            Ok(text)
        });
        Ok(Self {
            child,
            reader,
            stderr,
            listening,
            ready: false,
            iface: link.iface.clone(),
            rx_iface: link.capture_rx_iface.clone(),
        })
    }

    pub(super) async fn ready(&mut self) -> Result<()> {
        timeout(Duration::from_secs(3), &mut self.listening)
            .await
            .context("tcpdump readiness timeout")?
            .context("tcpdump exited before listening")?;
        self.ready = true;
        Ok(())
    }

    pub(super) async fn stop(mut self) -> Evidence {
        let stopped = self
            .child
            .id()
            .is_some_and(|pid| unsafe { libc::kill(pid as i32, libc::SIGINT) == 0 });
        let exit = match timeout(Duration::from_secs(2), self.child.wait()).await {
            Ok(Ok(status)) => status.code(),
            _ => {
                let _ = self.child.kill().await;
                None
            }
        };
        let bytes = match timeout(Duration::from_secs(2), &mut self.reader).await {
            Ok(Ok(Ok(bytes))) => bytes,
            _ => Vec::new(),
        };
        let mut stderr = match timeout(Duration::from_secs(2), &mut self.stderr).await {
            Ok(Ok(Ok(text))) => text,
            _ => String::new(),
        };
        let capture_path = check_rx_path(&self.iface, self.rx_iface.as_deref());
        if let Err(error) = &capture_path {
            stderr.push_str(&format!("\nJust TX RX capture path changed: {error:#}\n"));
        }
        Evidence {
            bytes,
            stderr,
            exit,
            stopped: stopped && capture_path.is_ok(),
            listening: self.ready,
        }
    }
}

pub(super) struct Evidence {
    pub(super) bytes: Vec<u8>,
    pub(super) stderr: String,
    pub(super) exit: Option<i32>,
    pub(super) stopped: bool,
    pub(super) listening: bool,
}

impl Evidence {
    pub(super) fn dropped(&self) -> Option<i64> {
        self.stderr.lines().find_map(|line| {
            line.strip_suffix(" packets dropped by kernel")?
                .trim()
                .parse()
                .ok()
        })
    }

    fn captured_count(&self) -> Option<i64> {
        self.stderr.lines().find_map(|line| {
            line.strip_suffix(" packets captured")
                .or_else(|| line.strip_suffix(" packet captured"))?
                .trim()
                .parse()
                .ok()
        })
    }

    pub(super) fn complete(&self) -> bool {
        self.listening
            && self.stopped
            && self.exit == Some(0)
            && self.dropped() == Some(0)
            && self.captured_count().is_some()
            && self.bytes.len() <= SIZE_LIMIT
    }

    pub(super) fn packets(
        &self,
        link: &Link,
        run: Uuid,
        hal: bool,
        sent: &[Send],
    ) -> Result<(i32, Vec<Packet>)> {
        let bytes = &self.bytes;
        ensure!(bytes.len() <= SIZE_LIMIT, "pcap size limit exceeded");
        ensure!(bytes.len() >= 24, "short pcap header");
        let (little, hz) = match bytes[..4] {
            [0xd4, 0xc3, 0xb2, 0xa1] => (true, 1_000_000),
            [0xa1, 0xb2, 0xc3, 0xd4] => (false, 1_000_000),
            [0x4d, 0x3c, 0xb2, 0xa1] => (true, 1_000_000_000),
            [0xa1, 0xb2, 0x3c, 0x4d] => (false, 1_000_000_000),
            _ => bail!("unknown pcap format"),
        };
        let read = |at| {
            let field = bytes[at..at + 4].try_into().unwrap();
            if little {
                u32::from_le_bytes(field)
            } else {
                u32::from_be_bytes(field)
            }
        };
        ensure!(read(20) & 0xffff == 1, "pcap link type is not Ethernet");
        let mut at = 24;
        let mut index = 0;
        let mut packets = Vec::new();
        while bytes.len() - at >= 16 {
            let captured = read(at + 8);
            let original = read(at + 12);
            let Some(end) = (at + 16).checked_add(captured as usize) else {
                bail!("pcap record length overflow");
            };
            ensure!(end <= bytes.len(), "truncated pcap record");
            let subsec = read(at + 4);
            ensure!(
                subsec < hz as u32 && captured <= original,
                "invalid pcap record"
            );
            ensure!(captured == original, "truncated pcap frame");
            let frame = &bytes[at + 16..end];
            if let Some((seq, tx)) = classify(frame, link, run)
                && let Ok(send_index) = sent.binary_search_by_key(&seq, |send| send.seq)
            {
                let send = &sent[send_index];
                let (identity, payload) = validate_frame(frame, link, send, tx, hal);
                packets.push(Packet {
                    index,
                    offset: at as i64,
                    sec: i64::from(read(at)),
                    subsec: subsec as i32,
                    captured: i32::try_from(captured)?,
                    original: i32::try_from(original)?,
                    seq,
                    tx,
                    identity,
                    payload,
                });
            }
            at = end;
            index += 1;
        }
        ensure!(at == bytes.len(), "partial pcap record header");
        if let Some(reported) = self.captured_count() {
            ensure!(
                index == reported,
                "pcap record count {index} differs from tcpdump count {reported}"
            );
        }
        Ok((hz, packets))
    }
}

fn classify(frame: &[u8], link: &Link, run: Uuid) -> Option<(i64, bool)> {
    // Keep damaged frames whose UUID and sequence remain readable.
    if frame.get(50..66)? != run.as_bytes() {
        return None;
    }
    let tx = frame.get(6..12)? == link.src_mac;
    let rx = frame.get(6..12)? == link.dut_mac;
    if !tx && !rx {
        return None;
    }
    let seq = i64::try_from(u64::from_be_bytes(frame.get(66..74)?.try_into().ok()?)).ok()?;
    (seq > 0).then_some((seq, tx))
}

fn validate_frame(frame: &[u8], link: &Link, send: &Send, tx: bool, hal: bool) -> (bool, bool) {
    let body_ok = frame.get(42..) == Some(send.payload.as_slice());
    if tx || !hal {
        let (dst, src) = if tx {
            (&link.dut_mac, &link.src_mac)
        } else {
            (&link.src_mac, &link.dut_mac)
        };
        let expected = build_udp_frame(
            dst,
            src,
            send.source,
            link.dut,
            link.sport,
            link.port,
            &send.payload,
        );
        return (frame.get(..42) == expected.get(..42), body_ok);
    }
    let identity = frame.len() == 42 + send.payload.len()
        && frame[..6] == link.src_mac
        && frame[6..12] == link.dut_mac
        && frame[12..15] == [0x08, 0x00, 0x45]
        && u16::from_be_bytes([frame[16], frame[17]]) as usize == frame.len() - 14
        && u16::from_be_bytes([frame[20], frame[21]]) & 0x3fff == 0
        && frame[23] == 17
        && frame[26..30] == link.dut.octets()
        && frame[30..34] == send.source.octets()
        && frame[34..36] == link.port.to_be_bytes()
        && frame[36..38] == link.sport.to_be_bytes()
        && u16::from_be_bytes([frame[38], frame[39]]) as usize == frame.len() - 34;
    (identity, body_ok)
}

pub(super) struct Packet {
    pub(super) index: i64,
    pub(super) offset: i64,
    pub(super) sec: i64,
    pub(super) subsec: i32,
    pub(super) captured: i32,
    pub(super) original: i32,
    pub(super) seq: i64,
    pub(super) tx: bool,
    pub(super) identity: bool,
    pub(super) payload: bool,
}

impl Packet {
    pub(super) fn ns(&self, hz: i32) -> u64 {
        self.sec as u64 * 1_000_000_000 + self.subsec as u64 * (1_000_000_000 / hz as u64)
    }

    pub(super) fn valid(&self) -> bool {
        self.identity && self.payload
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    const SECONDS: u32 = 1_700_000_000;

    fn link() -> Link {
        Link {
            capture_rx_iface: None,
            iface: "eth0".into(),
            src: Ipv4Addr::new(10, 0, 0, 2),
            dut: Ipv4Addr::new(10, 0, 0, 3),
            hit: Ipv4Addr::new(10, 0, 0, 4),
            src_mac: [1; 6],
            dut_mac: [2; 6],
            port: 8080,
            sport: 17777,
        }
    }

    fn evidence(records: usize) -> Evidence {
        // Classic little-endian pcap, microseconds, full snaplen, Ethernet.
        Evidence {
            bytes: vec![
                0xd4, 0xc3, 0xb2, 0xa1, 2, 0, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 4, 0, 1, 0, 0, 0,
            ],
            stderr: format!(
                "{records} packet{} captured\n0 packets dropped by kernel\n",
                if records == 1 { "" } else { "s" }
            ),
            exit: Some(0),
            stopped: true,
            listening: true,
        }
    }

    fn append_record(bytes: &mut Vec<u8>, offset_us: u32, frame: &[u8]) {
        let length = (frame.len() as u32).to_le_bytes();
        bytes.extend_from_slice(&(SECONDS + offset_us / 1_000_000).to_le_bytes());
        bytes.extend_from_slice(&(offset_us % 1_000_000).to_le_bytes());
        bytes.extend_from_slice(&length);
        bytes.extend_from_slice(&length);
        bytes.extend_from_slice(frame);
    }

    #[test]
    fn full_rate_window_retains_both_directions_and_timestamps() {
        const PROBES: usize = 4_000 * 30;
        let link = link();
        let run = Uuid::nil();
        let mut capture = evidence(2 * PROBES);
        let mut sends = Vec::with_capacity(PROBES);
        for i in 0..PROBES {
            let seq = i as i64 + 1;
            let offset_us = i as u32 * 250;
            let wall = u64::from(SECONDS) * 1_000_000_000 + u64::from(offset_us) * 1_000;
            let payload = super::super::payload(run, seq, None);
            for (dst, src, time) in [
                (&link.dut_mac, &link.src_mac, offset_us),
                (&link.src_mac, &link.dut_mac, offset_us + 125),
            ] {
                let frame = build_udp_frame(
                    dst, src, link.src, link.dut, link.sport, link.port, &payload,
                );
                assert_eq!(frame.len(), 82);
                append_record(&mut capture.bytes, time, &frame);
            }
            sends.push(Send {
                seq,
                wall,
                mono: u64::from(offset_us) * 1_000,
                submitted: true,
                error: None,
                class: "probe",
                source: link.src,
                payload,
            });
        }

        assert_eq!(capture.bytes.len(), 24 + 2 * PROBES * (16 + 82));
        assert!(capture.bytes.len() > 16 * 1024 * 1024);
        assert!(capture.complete());
        let (hz, packets) = capture.packets(&link, run, false, &sends).unwrap();
        assert_eq!(hz, 1_000_000);
        assert_eq!(packets.len(), 2 * PROBES);
        for (pair, send) in packets.chunks_exact(2).zip(&sends) {
            let tx = &pair[0];
            let rx = &pair[1];
            assert!(tx.tx && !rx.tx);
            assert!(tx.valid() && rx.valid());
            assert_eq!((tx.seq, rx.seq), (send.seq, send.seq));
            assert_eq!(tx.ns(hz), send.wall);
            assert_eq!(rx.ns(hz), send.wall + 125_000);
        }
    }

    #[test]
    fn rejects_truncated_capture() {
        let link = link();
        let run = Uuid::nil();
        let mut capture = evidence(1);
        append_record(&mut capture.bytes, 0, &[0; 82]);
        assert!(capture.complete());
        assert!(capture.packets(&link, run, false, &[]).is_ok());

        // A snaplen-truncated frame still has a complete pcap record.
        capture.bytes[36..40].copy_from_slice(&83_u32.to_le_bytes());
        let error = capture.packets(&link, run, false, &[]).err().unwrap();
        assert!(
            error.to_string().contains("truncated pcap frame"),
            "{error:#}"
        );
        capture.bytes[36..40].copy_from_slice(&82_u32.to_le_bytes());

        for (len, message) in [
            (24 + 16 + 81, "truncated pcap record"),
            (24 + 15, "partial pcap record header"),
            (24, "pcap record count"),
        ] {
            capture.bytes.truncate(len);
            let error = capture.packets(&link, run, false, &[]).err().unwrap();
            assert!(error.to_string().contains(message), "{error:#}");
        }
    }

    #[test]
    fn rejects_capture_over_size_limit() {
        let mut capture = evidence(0);
        capture.bytes.resize(SIZE_LIMIT + 1, 0);
        assert!(!capture.complete());
        let error = capture
            .packets(&link(), Uuid::nil(), false, &[])
            .err()
            .unwrap();
        assert!(error.to_string().contains("pcap size limit"), "{error:#}");
    }

    #[test]
    fn only_the_published_mac_swap_is_a_valid_xdp_reply() {
        let link = link();
        let run = Uuid::new_v4();
        let send = Send {
            seq: 1,
            wall: 0,
            mono: 0,
            submitted: true,
            error: None,
            class: "probe",
            source: link.src,
            payload: super::super::payload(run, 1, None),
        };
        let mut frame = build_udp_frame(
            &link.src_mac,
            &link.dut_mac,
            link.src,
            link.dut,
            link.sport,
            link.port,
            &send.payload,
        );
        assert_eq!(classify(&frame, &link, run), Some((1, false)));
        assert_eq!(
            validate_frame(&frame, &link, &send, false, false),
            (true, true)
        );
        frame[40] = 1;
        assert_eq!(
            validate_frame(&frame, &link, &send, false, false),
            (false, true)
        );
        frame[74] = 1;
        assert_eq!(
            validate_frame(&frame, &link, &send, false, false),
            (false, false)
        );
    }
}
