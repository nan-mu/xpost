//! Network Trace dictionary 3 and the corresponding Metrics counter names.
//!
//! Points 15–21 are HAL socket boundaries. The other points belong to the
//! XDP or driver path. All records are instants. Context IDs are 1 for the XDP path
//! (including radio callbacks) and 2 for the HAL socket. Span fields are zero.
//! Arg0 carries the frame or payload length for receive, queue depth for
//! enqueue and overflow, the signed Wi-Fi return code for submit failure,
//! or an XDP fault reason. Other Arg0 values and all Arg1 values are zero.
//! Fault reasons for the aggregate runtime event are 1 (inactive lease),
//! 2 (reentry), 3 (stack alignment), 4 (invalid metadata), and 5 (unresolved helper).
//! Network Trace records tagged packets only. TX submission and air completion
//! have aggregate counters. Packet tags end at the synchronous RX dispatch.
//! Points 29 through 35 identify Metrics counters.

pub const DICTIONARY_VERSION: u16 = 3;

macro_rules! points {
    ($($point:ident = $id:literal => $name:literal),+ $(,)?) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        #[repr(u16)]
        pub enum Point {
            $($point = $id,)+
        }

        impl Point {
            pub const COUNT: usize = [$(Self::$point),+].len();

            pub const fn name(self) -> &'static str {
                match self {
                    $(Self::$point => $name,)+
                }
            }
        }
    };
}

points! {
    DriverRx = 1 => "driver_rx",
    XdpRunEnter = 2 => "xdp_run_enter",
    XdpPass = 3 => "xdp_action_pass",
    XdpDrop = 4 => "xdp_action_drop",
    XdpTx = 5 => "xdp_action_tx",
    XdpAborted = 6 => "xdp_action_aborted",
    DriverRxEnqueued = 7 => "driver_rx_enqueued",
    DriverRxDiscarded = 8 => "driver_rx_discarded",
    DriverRxOverflow = 9 => "driver_rx_overflow",
    DriverTxSubmitted = 10 => "driver_tx_submitted",
    DriverTxSubmitFailed = 11 => "driver_tx_submit_failed",
    DriverTxCompleted = 12 => "driver_tx_completed",
    DriverTxCompletionFailed = 13 => "driver_tx_completion_failed",
    XdpFault = 14 => "xdp_runtime_fault",
    SocketRx = 15 => "socket_rx",
    SocketDiscarded = 16 => "socket_discarded",
    SocketDelivered = 17 => "socket_delivered",
    SocketSendQueued = 18 => "socket_send_queued",
    SocketSendFailed = 19 => "socket_send_failed",
    SocketAddressRejected = 20 => "socket_address_rejected",
    SocketSendBegin = 21 => "socket_send_begin",
    DriverTxEnqueueFailed = 22 => "driver_tx_enqueue_failed",
    DriverRxEnd = 23 => "driver_rx_end",
    DriverTxSubmitBegin = 24 => "driver_tx_submit_begin",
    DriverNormalDrop = 25 => "driver_normal_drop",
    DriverPacketFault = 26 => "driver_packet_fault",
    DriverTxEnqueued = 27 => "driver_tx_enqueued",
    XdpPolicyReturned = 28 => "xdp_policy_returned",
    TaggedDriverRx = 29 => "tagged_driver_rx",
    TaggedDriverNormalDrop = 30 => "tagged_driver_normal_drop",
    TaggedDriverFault = 31 => "tagged_driver_fault",
    TaggedSocketRx = 32 => "tagged_socket_rx",
    TaggedSocketDiscarded = 33 => "tagged_socket_discarded",
    TaggedSocketFault = 34 => "tagged_socket_fault",
    TaggedPolicyReturned = 35 => "tagged_xdp_policy_returned",
}

/// XDP counters available in both network carriers.
pub const XDP_POINTS: &[Point] = &[
    Point::DriverRx,
    Point::XdpRunEnter,
    Point::XdpPass,
    Point::XdpDrop,
    Point::XdpTx,
    Point::XdpAborted,
    Point::DriverRxEnqueued,
    Point::DriverRxDiscarded,
    Point::DriverRxOverflow,
    Point::DriverTxSubmitted,
    Point::DriverTxSubmitFailed,
    Point::DriverTxEnqueueFailed,
    Point::DriverTxCompleted,
    Point::DriverTxCompletionFailed,
    Point::XdpFault,
    Point::TaggedDriverRx,
    Point::TaggedDriverNormalDrop,
    Point::TaggedDriverFault,
    Point::TaggedPolicyReturned,
];

/// Return the sequence of a JTX1 v1 UDP payload for this run.
pub fn probe_sequence(payload: &[u8], run_uuid: &[u8; 16]) -> Option<u64> {
    if payload.get(..8)? != b"JTX1\x01\0\0\x20" || payload.get(8..24)? != run_uuid {
        return None;
    }
    let seq = u64::from_be_bytes(payload.get(24..32)?.try_into().ok()?);
    (seq != 0 && seq != crate::trace::PROBE_UNATTRIBUTED).then_some(seq)
}

/// Find the stimulus UDP payload in an unfragmented Ethernet IPv4 frame.
pub fn stimulus_payload(frame: &[u8]) -> Option<&[u8]> {
    if frame.get(12..14)? != [0x08, 0x00] {
        return None;
    }
    let ip = frame.get(14..)?;
    let base = ip.get(..20)?;
    let header_len = usize::from(base[0] & 0x0f) * 4;
    let total_len = usize::from(u16::from_be_bytes([base[2], base[3]]));
    let fragments = u16::from_be_bytes([base[6], base[7]]) & 0xbfff;
    if base[0] >> 4 != 4 || header_len < 20 || base[9] != 17 || fragments != 0 {
        return None;
    }
    let udp = ip.get(header_len..total_len)?;
    let header = udp.get(..8)?;
    let dst_port = u16::from_be_bytes([header[2], header[3]]);
    let udp_len = usize::from(u16::from_be_bytes([header[4], header[5]]));
    if dst_port != 8080 || udp.len() != udp_len {
        return None;
    }
    udp.get(8..)
}
