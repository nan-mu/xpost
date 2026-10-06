//! Firmware / ENUM name mapping (wire → SQL).

/// DB `firmware` enum — pipeline tag, not a Cargo feature name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Firmware {
    XdpCag,
    XdpT,
    Hal,
}

impl Firmware {
    pub fn as_db(self) -> &'static str {
        match self {
            Self::XdpCag => "xdp_cag",
            Self::XdpT => "xdp_t",
            Self::Hal => "hal",
        }
    }

    pub fn is_hal(self) -> bool {
        matches!(self, Self::Hal)
    }

    #[allow(dead_code)]
    pub fn is_trace(self) -> bool {
        matches!(self, Self::XdpT)
    }
}

/// Wire aggregate names → SQL `xdp_agg_name` / `hal_agg_name`.
pub fn map_agg_name(wire: &str) -> Option<&'static str> {
    Some(match wire {
        "rx_processing" => "rx_processing",
        "xdp_execution" | "execution" => "execution",
        "xdp_dispatch" | "dispatch" => "dispatch",
        "post_action" => "post_action",
        "tx_submission" => "tx_submission",
        "tx_block" => "tx_block",
        "helper_execution" => "helper_execution",
        "map_operation" => "map_operation",
        _ => return None,
    })
}

/// Wire kind byte → SQL `perf_trace_kind`.
pub fn map_trace_kind(kind: u8) -> Option<&'static str> {
    Some(match kind {
        1 => "rx_begin",
        2 => "xdp_begin",
        3 => "xdp_end",
        4 => "rx_end",
        5 => "slow_execution",
        6 => "rx_overflow",
        7 => "runtime_error",
        8 => "user_marker",
        9 => "span_begin",
        10 => "span_end",
        11 => "helper_enter",
        12 => "helper_leave",
        13 => "rx_delivered",
        14 => "rx_discarded",
        15 => "tx_sub_begin",
        16 => "tx_submitted",
        17 => "tx_failed",
        18 => "tx_completed",
        19 => "tx_fail_bad_buf",
        20 => "tx_fail_token",
        21 => "tx_enq",
        22 => "tx_kick",
        23 => "tx_poll",
        24 => "tx_cap_wait",
        25 => "tx_nomem_wait",
        26 => "tx_block",
        27 => "tx_enq_fail_claim",
        28 => "tx_enq_fail_ring",
        29 => "tx_flush",
        30 => "tx_grp_src",
        31 => "tx_no_retain",
        32 => "app_udp_recv",
        33 => "app_echo_begin",
        34 => "app_echo_end",
        35 => "app_udp_send_begin",
        36 => "app_udp_send_end",
        37 => "app_wifi_tx_begin",
        38 => "app_wifi_tx_end",
        _ => return None,
    })
}

/// HAL counters written into experiment `*_hal_counter` tables.
/// Matches `hal_perf` / xpost wire names; unused firmware slots stay 0.
pub const HAL_COUNTER_NAMES: &[&str] = &[
    "rx_packets",
    "xdp_invocations",
    "xdp_pass",
    "xdp_drop",
    "xdp_tx",
    "xdp_aborted",
    "rx_delivered",
    "rx_discarded",
    "rx_queue_overflow",
    "tx_submitted",
    "tx_failed",
    "tx_completed",
    "runtime_errors",
    "helper_map",
    "bad_md",
    "tx_block_gt10ms",
    "tx_block_gt100ms",
    "tx_block_max_us",
    "tx_pend",
    "tx_inflight",
    "tx_inflight_cap",
    "tx_pending_peak",
    "tx_inflight_peak",
];

