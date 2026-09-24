//! Shared ABI between eBPF and userspace.

#![no_std]

/// Stimulus egress sighting (T0).
pub const KIND_STIM_EGRESS: u8 = 1;
/// Echo ingress sighting (T1) — STA mirrors `FWSTIM`+seq on reply_port.
pub const KIND_ECHO_INGRESS: u8 = 2;
/// VIP bounce ingress — same `FWSTIM`+seq after XDP TX (L2-only or L3/L4 swap).
pub const KIND_BOUNCE_INGRESS: u8 = 3;

/// Stimulus UDP payload magic (`FWSTIM`) — used for T0 / bounce pairing.
pub const MAGIC_STIM: [u8; 6] = *b"FWSTIM";

/// Legit collect datagram: magic + `u16` LE index + 1 data byte (+ optional pad).
/// No realtime reply; MCU stores into a fixed buffer for a later dump.
pub const MAGIC_COLLECT: [u8; 6] = *b"FWCOL1";

/// Dump request / reply magic. Request: magic + `u16` LE `stream_len`.
/// Reply: magic + `u16` LE `n` + `n` collected bytes (0 = slot never written).
pub const MAGIC_DUMP: [u8; 6] = *b"FWDMP1";

/// Clear the MCU collect buffer before a new stream.
pub const MAGIC_COLLECT_RST: [u8; 6] = *b"FWRST1";

/// Fixed echo payload byte from ESP firmware (legacy single-byte reply).
pub const ECHO_BYTE: u8 = 0x42;

/// STA echo replies with [`MAGIC_STIM`] + `u64` LE seq (same header as stimulus).
/// Legacy single-byte [`ECHO_BYTE`] is no longer used for timing pairing.

/// Bytes after magic before seq ends: magic(6) + seq(8).
pub const PAYLOAD_HDR_LEN: usize = 14;

/// `MAGIC_COLLECT` + index(2) + data(1).
pub const COLLECT_HDR_LEN: usize = 9;

/// MCU collect buffer capacity (and max dump payload bytes).
pub const COLLECT_CAP: usize = 256;

/// Filter endpoints written by userspace before attach.
///
/// IPv4 addresses match the on-wire `u32` in the IPv4 header (network byte order
/// as loaded by `ctx.load::<u32>`).
/// Ports are **host** byte order.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct FilterConfig {
    pub esp_ip_be: u32,
    /// VIP (A); 0 disables bounce matching.
    pub vip_ip_be: u32,
    pub echo_port: u16,
    pub reply_port: u16,
}

impl Default for FilterConfig {
    fn default() -> Self {
        Self {
            esp_ip_be: 0,
            vip_ip_be: 0,
            echo_port: 0,
            reply_port: 0,
        }
    }
}

/// One timestamped sighting from TC.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct StampEvent {
    pub kind: u8,
    pub _pad0: u8,
    /// UDP source port (host order); meaningful for echo/bounce ingress.
    pub udp_sport: u16,
    /// skb/L2 length as seen by TC (`ctx.len()`).
    pub l2_len: u16,
    pub _pad1: u16,
    pub ktime_ns: u64,
    pub seq: u64,
}

#[cfg(feature = "user")]
unsafe impl aya::Pod for FilterConfig {}

#[cfg(feature = "user")]
unsafe impl aya::Pod for StampEvent {}

pub mod drop_stats;
pub mod hot_load_op;
pub mod map_crud_op;
pub mod perf_stats;

#[cfg(any(feature = "user", feature = "host"))]
pub mod fw_agent;
