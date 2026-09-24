//! Drop-stats UDP oneshot on port 8081 (1-byte request, 24-byte query reply).

pub const OP_QUERY: u8 = 0x00;
pub const OP_RESET: u8 = 0x01;
/// End blast window: disable drop path, seal `t_last` if below `CAP_PKTS` (no reply).
pub const OP_STOP: u8 = 0x02;
/// DUT → host: bench cap reached (`BENCH_DONE_LEN` = 1 + reply).
pub const OP_BENCH_DONE: u8 = 0x03;

pub const REQ_LEN: usize = 1;
pub const REPLY_LEN: usize = 24;
pub const BENCH_DONE_LEN: usize = 25;

/// DUT idle watchdog (µs); must match firmware `drop_stats::IDLE_US`.
pub const IDLE_WATCHDOG_US: u64 = 5_000_000;
/// Firmware polls watchdog every 100ms (TIMG1 ISR on drop-xdp).
pub const IDLE_WATCHDOG_POLL_MS: u64 = 100;
/// After RESET: async map clear + arm WDT on DUT before bench TX.
pub const RESET_SETTLE_MS: u64 = 4_000;
/// After host stops TX: WiFi / driver queue may still deliver to DUT.
pub const POST_BLAST_DRAIN_SECS: u64 = 10;
/// After blast: wait this many watchdog periods, QUERY once per period.
pub const WATCHDOG_QUERY_ROUNDS: usize = 8;
/// drop-xdp: settle after flash before ctrl probe.
pub const POST_FLASH_SETTLE_DROP_XDP_SECS: u64 = 30;
/// drop-xdp: settle after ctrl ready, before first RESET.
pub const PRE_BENCH_SETTLE_DROP_XDP_SECS: u64 = 15;

/// One watchdog wait slice: idle timeout + ISR poll + slack.
pub const fn watchdog_query_period_us() -> u64 {
    IDLE_WATCHDOG_US + IDLE_WATCHDOG_POLL_MS * 1000 + 500_000
}

/// Warm-up packets (not in `t_first`..`t_last` window); must match firmware.
pub const WARMUP_PKTS: u64 = 10_000;
/// Stop counting / milestone `t_last` at this cap; must match firmware.
pub const CAP_PKTS: u64 = 100_000;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DropStats {
    pub drop_count: u64,
    /// Timestamp when `drop_count` crosses `WARMUP_PKTS`.
    pub t_first_us: u64,
    /// Timestamp when `drop_count` crosses `CAP_PKTS`, or host `STOP` if below cap.
    pub t_last_us: u64,
}

impl DropStats {
    /// Packets inside the measurement window `[WARMUP_PKTS, min(count, CAP_PKTS)]`.
    pub fn measured_pkts(&self) -> u64 {
        let hi = self.drop_count.min(CAP_PKTS);
        hi.saturating_sub(WARMUP_PKTS)
    }

    /// DUT pps over the milestone window; `None` until both timestamps are set.
    pub fn dut_pps(&self) -> Option<f64> {
        let span = self.t_last_us.saturating_sub(self.t_first_us);
        let pkts = self.measured_pkts();
        if pkts == 0 || span == 0 || self.t_first_us == 0 || self.t_last_us == 0 {
            return None;
        }
        Some(pkts as f64 / span as f64 * 1e6)
    }
}

pub fn encode_req(op: u8, out: &mut [u8]) -> Option<usize> {
    if out.is_empty() {
        return None;
    }
    out[0] = op;
    Some(REQ_LEN)
}

pub fn encode_reply(stats: &DropStats, out: &mut [u8]) -> Option<usize> {
    if out.len() < REPLY_LEN {
        return None;
    }
    write_u64(&mut out[0..8], stats.drop_count);
    write_u64(&mut out[8..16], stats.t_first_us);
    write_u64(&mut out[16..24], stats.t_last_us);
    Some(REPLY_LEN)
}

pub fn decode_reply(buf: &[u8]) -> Option<DropStats> {
    if buf.len() < REPLY_LEN {
        return None;
    }
    Some(DropStats {
        drop_count: read_u64(&buf[0..8]),
        t_first_us: read_u64(&buf[8..16]),
        t_last_us: read_u64(&buf[16..24]),
    })
}

/// DUT `OP_BENCH_DONE` notify on probe `reply_port`.
pub fn decode_bench_done(buf: &[u8]) -> Option<DropStats> {
    if buf.len() < BENCH_DONE_LEN || buf[0] != OP_BENCH_DONE {
        return None;
    }
    decode_reply(&buf[1..BENCH_DONE_LEN])
}

fn read_u64(buf: &[u8]) -> u64 {
    u64::from_le_bytes(buf[..8].try_into().expect("u64"))
}

fn write_u64(buf: &mut [u8], v: u64) {
    buf[..8].copy_from_slice(&v.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_reply() {
        let stats = DropStats {
            drop_count: 42,
            t_first_us: 100,
            t_last_us: 200,
        };
        let mut buf = [0u8; REPLY_LEN];
        encode_reply(&stats, &mut buf).unwrap();
        let got = decode_reply(&buf).unwrap();
        assert_eq!(got, stats);
        assert!(got.dut_pps().is_none());
    }

    #[test]
    fn dut_pps_milestone_window() {
        let stats = DropStats {
            drop_count: CAP_PKTS,
            t_first_us: 1_000_000,
            t_last_us: 11_000_000,
        };
        assert_eq!(stats.measured_pkts(), CAP_PKTS - WARMUP_PKTS);
        assert!((stats.dut_pps().unwrap() - 9000.0).abs() < 1e-6);
    }

    #[test]
    fn watchdog_query_period() {
        assert_eq!(watchdog_query_period_us(), 5_500_000);
    }

    #[test]
    fn measured_pkts_respects_cap() {
        let stats = DropStats {
            drop_count: 150_000,
            t_first_us: 0,
            t_last_us: 0,
        };
        assert_eq!(stats.measured_pkts(), CAP_PKTS - WARMUP_PKTS);
    }
}
