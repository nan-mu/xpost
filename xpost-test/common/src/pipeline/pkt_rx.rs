//! AF_PACKET reader for reply identity.
//!
//! This socket is not the TC/XDP timestamp ring. A missing stamp can still
//! leave a sequence here.

use std::{
    collections::HashMap,
    io,
    mem,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    sync::Arc,
};

use anyhow::{Context, Result};
use common::{MAGIC_STIM, PAYLOAD_HDR_LEN};
use libc::{AF_PACKET, ETH_P_ALL, ETH_P_IP, SOCK_RAW, bind as libc_bind, sockaddr_ll, socket};
use tokio::{io::unix::AsyncFd, sync::Mutex, task::JoinHandle};

use super::raw_tx::if_nametoindex;

const PACKET_OUTGOING: u8 = 4;
const PACKET_STATISTICS: i32 = 6;
const IPPROTO_UDP: u8 = 17;

#[repr(C)]
struct TpacketStats {
    tp_packets: u32,
    tp_drops: u32,
}

#[derive(Clone, Copy, Default)]
struct PktNote {
    n: u32,
}

pub struct PktBook {
    fd: Arc<OwnedFd>,
    notes: Arc<Mutex<HashMap<u64, PktNote>>>,
    task: JoinHandle<()>,
}

impl PktBook {
    pub fn open(iface: &str) -> Result<Self> {
        let ifindex = if_nametoindex(iface)?;
        let fd = unsafe { socket(AF_PACKET, SOCK_RAW, (ETH_P_ALL as u16).to_be() as i32) };
        if fd < 0 {
            return Err(io::Error::last_os_error()).context("packet socket");
        }
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        let mut sll: sockaddr_ll = unsafe { mem::zeroed() };
        sll.sll_family = AF_PACKET as u16;
        sll.sll_protocol = (ETH_P_ALL as u16).to_be();
        sll.sll_ifindex = ifindex;
        let ret = unsafe {
            libc_bind(
                fd.as_raw_fd(),
                &sll as *const _ as *const libc::sockaddr,
                mem::size_of::<sockaddr_ll>() as u32,
            )
        };
        if ret < 0 {
            return Err(io::Error::last_os_error()).context("bind packet socket");
        }
        let rcv = 8 * 1024 * 1024i32;
        let _ = unsafe {
            libc::setsockopt(
                fd.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_RCVBUF,
                &rcv as *const _ as *const libc::c_void,
                mem::size_of_val(&rcv) as u32,
            )
        };
        let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
        if flags < 0 || unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
        {
            return Err(io::Error::last_os_error()).context("packet socket nonblock");
        }
        let fd = Arc::new(fd);
        let notes = Arc::new(Mutex::new(HashMap::new()));
        let task_fd = fd.clone();
        let task_notes = notes.clone();
        let task = tokio::spawn(async move {
            if let Err(e) = pump(task_fd, task_notes).await {
                log::warn!("packet observer ended: {e:#}");
            }
        });
        Ok(Self { fd, notes, task })
    }

    pub async fn reset(&self) {
        self.notes.lock().await.clear();
    }

    pub async fn take(&self, seq: u64) -> u32 {
        self.notes.lock().await.remove(&seq).map(|n| n.n).unwrap_or(0)
    }

    /// Kernel drops since the previous read. The read clears the counter.
    pub fn take_drops(&self) -> Result<u64> {
        let mut stats = TpacketStats {
            tp_packets: 0,
            tp_drops: 0,
        };
        let mut len = mem::size_of::<TpacketStats>() as u32;
        let ret = unsafe {
            libc::getsockopt(
                self.fd.as_raw_fd(),
                libc::SOL_PACKET,
                PACKET_STATISTICS,
                &mut stats as *mut _ as *mut libc::c_void,
                &mut len,
            )
        };
        if ret < 0 {
            return Err(io::Error::last_os_error()).context("PACKET_STATISTICS");
        }
        Ok(u64::from(stats.tp_drops))
    }
}

impl Drop for PktBook {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn pump(fd: Arc<OwnedFd>, notes: Arc<Mutex<HashMap<u64, PktNote>>>) -> Result<()> {
    let async_fd = AsyncFd::new(std::os::fd::OwnedFd::try_clone(&fd)?)?;
    let mut buf = vec![0u8; 2048];
    loop {
        let mut guard = async_fd.readable().await?;
        loop {
            let mut addr: sockaddr_ll = unsafe { mem::zeroed() };
            let mut addrlen = mem::size_of::<sockaddr_ll>() as u32;
            let n = unsafe {
                libc::recvfrom(
                    fd.as_raw_fd(),
                    buf.as_mut_ptr() as *mut libc::c_void,
                    buf.len(),
                    0,
                    &mut addr as *mut _ as *mut libc::sockaddr,
                    &mut addrlen,
                )
            };
            if n < 0 {
                let err = io::Error::last_os_error();
                if err.kind() == io::ErrorKind::WouldBlock {
                    break;
                }
                return Err(err).context("recvfrom");
            }
            if addr.sll_pkttype == PACKET_OUTGOING {
                continue;
            }
            let frame = &buf[..n as usize];
            let Some(seq) = stim_seq_from_frame(frame) else {
                continue;
            };
            let mut map = notes.lock().await;
            let note = map.entry(seq).or_default();
            note.n = note.n.saturating_add(1);
        }
        guard.clear_ready();
    }
}

/// Sequence from an incoming IPv4 UDP frame whose payload starts with `FWSTIM`.
pub fn stim_seq_from_frame(frame: &[u8]) -> Option<u64> {
    if frame.len() < 14 + 20 + 8 + PAYLOAD_HDR_LEN {
        return None;
    }
    let eth_type = u16::from_be_bytes([frame[12], frame[13]]);
    if eth_type != ETH_P_IP as u16 {
        return None;
    }
    let ihl = usize::from(frame[14] & 0x0f) * 4;
    if ihl < 20 || frame.len() < 14 + ihl + 8 + PAYLOAD_HDR_LEN {
        return None;
    }
    if frame[14 + 9] != IPPROTO_UDP {
        return None;
    }
    let payload = 14 + ihl + 8;
    let magic_at = if frame[payload..payload + 4] == *b"QUIC" {
        payload + 8
    } else {
        payload
    };
    if frame.len() < magic_at + PAYLOAD_HDR_LEN {
        return None;
    }
    if frame[magic_at..magic_at + 6] != MAGIC_STIM {
        return None;
    }
    let mut seq = [0u8; 8];
    seq.copy_from_slice(&frame[magic_at + 6..magic_at + 14]);
    Some(u64::from_le_bytes(seq))
}

#[cfg(test)]
mod tests {
    use super::stim_seq_from_frame;

    fn frame(payload: &[u8]) -> Vec<u8> {
        let mut out = vec![0u8; 14 + 20 + 8];
        out[12] = 0x08;
        out[14] = 0x45;
        out[14 + 9] = 17;
        out.extend_from_slice(payload);
        out
    }

    #[test]
    fn reads_seq_without_a_timestamp() {
        let mut payload = b"FWSTIM".to_vec();
        payload.extend_from_slice(&7u64.to_le_bytes());
        assert_eq!(stim_seq_from_frame(&frame(&payload)), Some(7));
    }

    #[test]
    fn ignores_a_frame_without_the_magic() {
        let payload = vec![0u8; 14];
        assert_eq!(stim_seq_from_frame(&frame(&payload)), None);
    }
}
