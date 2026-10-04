//! AF_PACKET RAW Ethernet/IPv4/UDP transmitter.

use std::{
    io, mem,
    net::Ipv4Addr,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
};

use anyhow::{Context, Result, bail};
use libc::{
    AF_PACKET, ETH_P_ALL, ETH_P_IP, IFNAMSIZ, SOCK_RAW, bind as libc_bind, sockaddr_ll, socket,
};
use log::debug;
use rand::Rng;
use core::{MAGIC_STIM, PAYLOAD_HDR_LEN};

pub const WIRE_FRAME_BYTES: usize = 64;

pub struct RawTx {
    fd: OwnedFd,
    ifindex: i32,
    src_mac: [u8; 6],
    dst_mac: [u8; 6],
    echo_port: u16,
    /// UDP source port on stimulus frames (`stim_sport`). Must ≠ STA echo `reply_port`
    /// so TC can tell XDP port-swap bounce (`dport=stim_sport`) from STA echo (`dport=reply`).
    sport: u16,
}

impl RawTx {
    pub fn open(
        iface: &str,
        src_mac: [u8; 6],
        dst_mac: [u8; 6],
        _esp_ip: Ipv4Addr,
        echo_port: u16,
        sport: u16,
    ) -> Result<Self> {
        let ifindex = if_nametoindex(iface)?;
        let fd = unsafe { socket(AF_PACKET, SOCK_RAW, (ETH_P_ALL as u16).to_be() as i32) };
        if fd < 0 {
            return Err(io::Error::last_os_error()).context("socket(AF_PACKET)");
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
            return Err(io::Error::last_os_error()).context("bind(AF_PACKET)");
        }

        Ok(Self {
            fd,
            ifindex,
            src_mac,
            dst_mac,
            echo_port,
            sport,
        })
    }

    /// Send stimulus: `FWSTIM` + seq LE (+ optional pad) to `dst_ip:echo_port`.
    pub fn send_stimulus(
        &self,
        src_ip: Ipv4Addr,
        dst_ip: Ipv4Addr,
        seq: u64,
        pad: &[u8],
    ) -> Result<()> {
        let mut payload = Vec::with_capacity(PAYLOAD_HDR_LEN + pad.len());
        payload.extend_from_slice(&MAGIC_STIM);
        payload.extend_from_slice(&seq.to_le_bytes());
        payload.extend_from_slice(pad);

        let frame = build_udp_frame(
            &self.dst_mac,
            &self.src_mac,
            src_ip,
            dst_ip,
            self.sport,
            self.echo_port,
            &payload,
        );
        self.send_udp_frame(&frame)?;
        debug!(
            "RAW stim seq={seq} src={src_ip} dst={dst_ip}:{} len={}",
            self.echo_port,
            frame.len()
        );
        Ok(())
    }

    /// Send arbitrary UDP payload (e.g. MapCrudOp / HotLoadOp protobuf) to `dst_ip:dport`.
    pub fn send_udp(
        &self,
        src_ip: Ipv4Addr,
        dst_ip: Ipv4Addr,
        dport: u16,
        payload: &[u8],
    ) -> Result<()> {
        let frame = build_udp_frame(
            &self.dst_mac,
            &self.src_mac,
            src_ip,
            dst_ip,
            self.sport,
            dport,
            payload,
        );
        self.send_udp_frame(&frame)?;
        debug!(
            "RAW udp src={src_ip} dst={dst_ip}:{dport} len={}",
            frame.len()
        );
        Ok(())
    }

    /// 64-byte L2 UDP frame with random payload padding (drop bench interferer).
    pub fn build_udp_frame_64(
        &self,
        src_ip: Ipv4Addr,
        dst_ip: Ipv4Addr,
        dport: u16,
        rng: &mut impl Rng,
    ) -> [u8; WIRE_FRAME_BYTES] {
        const HDR: usize = 14 + 20 + 8;
        const PAYLOAD_LEN: usize = WIRE_FRAME_BYTES - HDR;
        let mut payload = [0u8; PAYLOAD_LEN];
        rng.fill_bytes(&mut payload);
        let frame = build_udp_frame(
            &self.dst_mac,
            &self.src_mac,
            src_ip,
            dst_ip,
            self.sport,
            dport,
            &payload,
        );
        debug_assert_eq!(frame.len(), WIRE_FRAME_BYTES);
        frame.try_into().expect("64B wire frame")
    }

    pub fn send_udp_frame(&self, frame: &[u8]) -> Result<()> {
        let mut sll: sockaddr_ll = unsafe { mem::zeroed() };
        sll.sll_family = AF_PACKET as u16;
        sll.sll_protocol = (ETH_P_IP as u16).to_be();
        sll.sll_ifindex = self.ifindex;
        sll.sll_halen = 6;
        sll.sll_addr[..6].copy_from_slice(&self.dst_mac);

        let n = unsafe {
            libc::sendto(
                self.fd.as_raw_fd(),
                frame.as_ptr() as *const _,
                frame.len(),
                0,
                &sll as *const _ as *const libc::sockaddr,
                mem::size_of::<sockaddr_ll>() as u32,
            )
        };
        if n < 0 {
            return Err(io::Error::last_os_error()).context("sendto");
        }
        if n as usize != frame.len() {
            bail!("short sendto: {n} of {}", frame.len());
        }
        Ok(())
    }
}

pub fn if_nametoindex(name: &str) -> Result<i32> {
    let mut buf = [0u8; IFNAMSIZ];
    let bytes = name.as_bytes();
    if bytes.len() >= IFNAMSIZ {
        bail!("iface name too long: {name}");
    }
    buf[..bytes.len()].copy_from_slice(bytes);
    let idx = unsafe { libc::if_nametoindex(buf.as_ptr() as *const _) };
    if idx == 0 {
        return Err(io::Error::last_os_error()).context(format!("if_nametoindex({name})"));
    }
    Ok(idx as i32)
}

pub fn iface_mac(iface: &str) -> Result<[u8; 6]> {
    let fd = unsafe { socket(AF_PACKET, SOCK_RAW, (ETH_P_ALL as u16).to_be() as i32) };
    if fd < 0 {
        return Err(io::Error::last_os_error()).context("socket for iface_mac");
    }
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };

    let mut ifr: libc::ifreq = unsafe { mem::zeroed() };
    let bytes = iface.as_bytes();
    if bytes.len() >= IFNAMSIZ {
        bail!("iface name too long: {iface}");
    }
    unsafe {
        ::core::ptr::copy_nonoverlapping(
            bytes.as_ptr(),
            ifr.ifr_name.as_mut_ptr() as *mut u8,
            bytes.len(),
        );
    }
    let ret = unsafe { libc::ioctl(fd.as_raw_fd(), libc::SIOCGIFHWADDR as libc::Ioctl, &mut ifr) };
    if ret < 0 {
        return Err(io::Error::last_os_error()).context("SIOCGIFHWADDR");
    }
    let mut mac = [0u8; 6];
    unsafe {
        let addr = ifr.ifr_ifru.ifru_hwaddr.sa_data.as_ptr() as *const u8;
        ::core::ptr::copy_nonoverlapping(addr, mac.as_mut_ptr(), 6);
    }
    Ok(mac)
}

/// IPv4 address configured on `iface` (SIOCGIFADDR).
pub fn iface_ipv4(iface: &str) -> Result<Ipv4Addr> {
    let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error()).context("socket for iface_ipv4");
    }
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };

    let mut ifr: libc::ifreq = unsafe { mem::zeroed() };
    let bytes = iface.as_bytes();
    if bytes.len() >= IFNAMSIZ {
        bail!("iface name too long: {iface}");
    }
    unsafe {
        ::core::ptr::copy_nonoverlapping(
            bytes.as_ptr(),
            ifr.ifr_name.as_mut_ptr() as *mut u8,
            bytes.len(),
        );
    }
    let ret = unsafe { libc::ioctl(fd.as_raw_fd(), libc::SIOCGIFADDR as libc::Ioctl, &mut ifr) };
    if ret < 0 {
        return Err(io::Error::last_os_error()).context("SIOCGIFADDR");
    }
    let addr = unsafe {
        let sa = &ifr.ifr_ifru.ifru_addr as *const libc::sockaddr as *const libc::sockaddr_in;
        u32::from_be((*sa).sin_addr.s_addr)
    };
    Ok(Ipv4Addr::from(addr))
}

pub fn build_udp_frame(
    dst_mac: &[u8; 6],
    src_mac: &[u8; 6],
    src_ip: Ipv4Addr,
    dst_ip: Ipv4Addr,
    sport: u16,
    dport: u16,
    payload: &[u8],
) -> Vec<u8> {
    let udp_len = 8 + payload.len();
    let ip_len = 20 + udp_len;
    let mut frame = Vec::with_capacity(14 + ip_len);

    // Ethernet
    frame.extend_from_slice(dst_mac);
    frame.extend_from_slice(src_mac);
    frame.extend_from_slice(&(ETH_P_IP as u16).to_be_bytes());

    // IPv4 header
    let ip_start = frame.len();
    frame.push(0x45); // v4, IHL=5
    frame.push(0); // DSCP
    frame.extend_from_slice(&(ip_len as u16).to_be_bytes());
    frame.extend_from_slice(&0u16.to_be_bytes()); // id
    frame.extend_from_slice(&0u16.to_be_bytes()); // flags/frag
    frame.push(64); // TTL
    frame.push(17); // UDP
    frame.extend_from_slice(&0u16.to_be_bytes()); // checksum placeholder
    frame.extend_from_slice(&src_ip.octets());
    frame.extend_from_slice(&dst_ip.octets());
    let csum = ipv4_checksum(&frame[ip_start..ip_start + 20]);
    frame[ip_start + 10..ip_start + 12].copy_from_slice(&csum.to_be_bytes());

    // UDP (checksum 0 = unused)
    frame.extend_from_slice(&sport.to_be_bytes());
    frame.extend_from_slice(&dport.to_be_bytes());
    frame.extend_from_slice(&(udp_len as u16).to_be_bytes());
    frame.extend_from_slice(&0u16.to_be_bytes());
    frame.extend_from_slice(payload);

    frame
}

fn ipv4_checksum(header: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    let mut i = 0;
    while i + 1 < header.len() {
        if i == 10 {
            i += 2;
            continue; // skip checksum field
        }
        sum += u16::from_be_bytes([header[i], header[i + 1]]) as u32;
        i += 2;
    }
    while sum > 0xffff {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn udp_frame_64_wire_len() {
        let frame = build_udp_frame(
            &[0u8; 6],
            &[0u8; 6],
            Ipv4Addr::new(10, 0, 0, 1),
            Ipv4Addr::new(10, 0, 0, 2),
            1234,
            5678,
            &[0u8; WIRE_FRAME_BYTES - 42],
        );
        assert_eq!(frame.len(), WIRE_FRAME_BYTES);
    }
}
