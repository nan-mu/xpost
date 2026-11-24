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

const SIZE_LIMIT: u64 = 16 * 1024 * 1024;

pub(super) struct Capture {
    child: Child,
    reader: JoinHandle<io::Result<Vec<u8>>>,
    stderr: JoinHandle<io::Result<String>>,
    listening: oneshot::Receiver<()>,
    ready: bool,
}

impl Capture {
    pub(super) fn start(link: &Link) -> Result<Self> {
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
            .take(SIZE_LIMIT + 1);
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
        let stderr = match timeout(Duration::from_secs(2), &mut self.stderr).await {
            Ok(Ok(Ok(text))) => text,
            _ => String::new(),
        };
        Evidence {
            bytes,
            stderr,
            exit,
            stopped,
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

    pub(super) fn complete(&self) -> bool {
        self.listening
            && self.stopped
            && self.exit == Some(0)
            && self.dropped() == Some(0)
            && self.bytes.len() <= SIZE_LIMIT as usize
    }

    pub(super) fn packets(
        &self,
        link: &Link,
        run: Uuid,
        hal: bool,
        sent: &[Send],
    ) -> Result<(i32, Vec<Packet>)> {
        let bytes = &self.bytes;
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
                    identity: identity && captured == original,
                    payload,
                });
            }
            at = end;
            index += 1;
        }
        ensure!(at == bytes.len(), "partial pcap record header");
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

    #[test]
    fn only_the_published_mac_swap_is_a_valid_xdp_reply() {
        let link = Link {
            iface: "eth0".into(),
            src: Ipv4Addr::new(10, 0, 0, 2),
            dut: Ipv4Addr::new(10, 0, 0, 3),
            hit: Ipv4Addr::new(10, 0, 0, 4),
            src_mac: [1; 6],
            dut_mac: [2; 6],
            port: 8080,
            sport: 17777,
        };
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
