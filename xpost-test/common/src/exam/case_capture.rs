//! Capture tagged case replies, retaining the original tcpdump pcap bytes.

use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use uuid::Uuid;

use crate::config::{FileConfig, format_mac};
use crate::pipeline::{self, RawTx, build_udp_frame};

pub(super) const PORT: u16 = 8080;
pub(super) const PPS: i32 = 500;
pub(super) const PRE: Duration = Duration::from_millis(250);
pub(super) const POST: Duration = Duration::from_millis(200);
const CAPTURE_MAX: usize = 4 * 1024 * 1024;

pub(super) struct Probes {
    raw: RawTx,
    src_mac: [u8; 6],
    dut_mac: [u8; 6],
    src: std::net::Ipv4Addr,
    dut: std::net::Ipv4Addr,
    sport: u16,
    run: Uuid,
}

impl Probes {
    pub fn open(cfg: &FileConfig, run: Uuid) -> Result<Self> {
        let iface = &cfg.probe.iface;
        let src_mac = match cfg.src_mac_override()? {
            Some(mac) => mac,
            None => pipeline::iface_mac(iface)?,
        };
        let dut_mac = cfg.esp_mac_bytes()?;
        let src = pipeline::iface_ipv4(iface)?;
        let dut = cfg.target.esp_ip;
        let sport = cfg.target.stim_sport;
        let raw = RawTx::open(iface, src_mac, dut_mac, dut, PORT, sport)?;
        Ok(Self {
            raw,
            src_mac,
            dut_mac,
            src,
            dut,
            sport,
            run,
        })
    }

    fn body(&self, seq: u64) -> [u8; 40] {
        let mut payload = [0; 40];
        payload[..8].copy_from_slice(b"JTX1\x01\0\0\x20");
        payload[8..24].copy_from_slice(self.run.as_bytes());
        payload[24..32].copy_from_slice(&seq.to_be_bytes());
        payload
    }

    fn send(&self, seq: u64) -> Result<()> {
        self.raw.send_udp(self.src, self.dut, PORT, &self.body(seq))
    }

    fn identify(&self, frame: &[u8]) -> Option<(u64, u8)> {
        let version = *frame.get(77)?;
        if frame.len() != 82 || !matches!(version, 1 | 2) || frame[50..66] != *self.run.as_bytes() {
            return None;
        }
        let seq = u64::from_be_bytes(frame[66..74].try_into().ok()?);
        if seq == 0 {
            return None;
        }
        let mut payload = self.body(seq);
        payload[77 - 42] = version;
        payload[36..40].copy_from_slice(b"CAS2");
        let expected = build_udp_frame(
            &self.src_mac,
            &self.dut_mac,
            self.src,
            self.dut,
            self.sport,
            PORT,
            &payload,
        );
        (frame == expected).then_some((seq, version))
    }
}

#[derive(Clone, Copy)]
pub(super) struct Packet {
    pub index: i64,
    pub offset: i64,
    pub sec: i64,
    pub subsec: i32,
    pub seq: i64,
    pub version: u8,
}

pub(super) struct Evidence {
    pub pcap: Vec<u8>,
    pub stderr: String,
    pub hz: i32,
    pub packets: Vec<Packet>,
}

impl Evidence {
    pub fn kernel_dropped(&self) -> Option<i64> {
        self.stderr.lines().find_map(|line| {
            line.strip_suffix(" packets dropped by kernel")?
                .trim()
                .parse()
                .ok()
        })
    }

    pub fn loss_free(&self) -> bool {
        self.kernel_dropped() == Some(0)
    }
}

pub(super) struct Capture {
    child: Child,
    bytes: Arc<Mutex<Vec<u8>>>,
    reader: JoinHandle<Result<()>>,
    stderr: JoinHandle<Result<String>>,
    sender: JoinHandle<Result<()>>,
    probes: Arc<Probes>,
}

impl Capture {
    pub fn start(cfg: &FileConfig, run: Uuid) -> Result<Self> {
        let probes = Arc::new(Probes::open(cfg, run)?);
        let mac = format_mac(&probes.dut_mac);
        let mut child = Command::new("tcpdump")
            .args([
                "-i",
                &cfg.probe.iface,
                "-n",
                "--immediate-mode",
                "-U",
                "-s",
                "256",
                "-w",
                "-",
                "ether",
                "host",
                &mac,
                "and",
                "udp",
                "dst",
                "port",
                "8080",
            ])
            .kill_on_drop(true)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("start case tcpdump")?;
        let mut stdout = child.stdout.take().context("tcpdump stdout")?;
        let mut stderr = child.stderr.take().context("tcpdump stderr")?;
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let store = bytes.clone();
        let reader = tokio::spawn(async move {
            let mut buf = [0u8; 8192];
            loop {
                let n = stdout.read(&mut buf).await?;
                if n == 0 {
                    return Ok(());
                }
                let mut captured = store.lock().unwrap();
                ensure!(
                    captured.len() + n <= CAPTURE_MAX,
                    "case capture exceeded its byte limit"
                );
                captured.extend_from_slice(&buf[..n]);
            }
        });
        let stderr = tokio::spawn(async move {
            let mut text = String::new();
            stderr.read_to_string(&mut text).await?;
            Ok(text)
        });
        let source = probes.clone();
        let sender = tokio::spawn(async move {
            let mut clock = tokio::time::interval(Duration::from_micros(2_000));
            clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            for seq in 1.. {
                clock.tick().await;
                source.send(seq)?;
            }
            Ok(())
        });
        Ok(Self {
            child,
            bytes,
            reader,
            stderr,
            sender,
            probes,
        })
    }

    pub async fn observe(&self, version: u8) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            {
                let bytes = self.bytes.lock().unwrap();
                if bytes.len() >= 24
                    && parse(&bytes, &self.probes, false)?
                        .1
                        .iter()
                        .any(|packet| packet.version == version)
                {
                    return Ok(());
                }
            }
            ensure!(
                !self.sender.is_finished() && !self.reader.is_finished(),
                "case probe or capture stopped early"
            );
            ensure!(
                Instant::now() < deadline,
                "no valid CAS2 reply for version {version}"
            );
            tokio::time::sleep(Duration::from_millis(15)).await;
        }
    }

    pub async fn stop(mut self) -> Result<Evidence> {
        self.sender.abort();
        if let Some(id) = self.child.id() {
            // TCPdump writes final loss statistics when it handles SIGINT.
            unsafe { libc::kill(id as i32, libc::SIGINT) };
        }
        let status = tokio::time::timeout(Duration::from_secs(2), self.child.wait()).await??;
        (&mut self.reader).await??;
        let stderr = (&mut self.stderr).await??;
        let pcap = std::mem::take(&mut *self.bytes.lock().unwrap());
        let (hz, packets) = parse(&pcap, &self.probes, true)?;
        ensure!(status.success(), "tcpdump failed: {status} {stderr}");
        Ok(Evidence {
            pcap,
            stderr,
            hz,
            packets,
        })
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.sender.abort();
        let _ = self.child.start_kill();
    }
}

fn parse(bytes: &[u8], probes: &Probes, complete: bool) -> Result<(i32, Vec<Packet>)> {
    ensure!(bytes.len() >= 24, "truncated pcap header");
    let (little, hz) = match bytes[..4] {
        [0xd4, 0xc3, 0xb2, 0xa1] => (true, 1_000_000),
        [0xa1, 0xb2, 0xc3, 0xd4] => (false, 1_000_000),
        [0x4d, 0x3c, 0xb2, 0xa1] => (true, 1_000_000_000),
        [0xa1, 0xb2, 0x3c, 0x4d] => (false, 1_000_000_000),
        _ => bail!("unknown pcap format"),
    };
    let read = |offset| {
        let field = bytes[offset..offset + 4].try_into().unwrap();
        if little {
            u32::from_le_bytes(field)
        } else {
            u32::from_be_bytes(field)
        }
    };
    ensure!(
        read(20) & 0xffff == 1,
        "case tcpdump requires Ethernet capture"
    );
    let mut offset = 24;
    let mut index = 0;
    let mut packets = Vec::new();
    while offset + 16 <= bytes.len() {
        let sec = read(offset);
        let subsec = read(offset + 4);
        let len = read(offset + 8);
        let original = read(offset + 12);
        let end = offset + 16 + len as usize;
        if end > bytes.len() {
            break;
        }
        ensure!(subsec < hz as u32 && len <= original, "invalid pcap record");
        if len == original
            && let Some((seq, version)) = probes.identify(&bytes[offset + 16..end])
        {
            packets.push(Packet {
                index,
                offset: offset as i64,
                sec: i64::from(sec),
                subsec: subsec as i32,
                seq: i64::try_from(seq)?,
                version,
            });
        }
        offset = end;
        index += 1;
    }
    ensure!(!complete || offset == bytes.len(), "truncated pcap record");
    Ok((hz, packets))
}
