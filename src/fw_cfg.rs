//! Compile-time firewall test harness config from `.env` / `dotenv-build`.

/// `"1"` / `"true"` enable AOT XDP on the WiFi RX path.
pub const XDP_ENABLE: bool = const {
    match env!("XDP_ENABLE").as_bytes() {
        b"1" | b"true" | b"TRUE" | b"yes" | b"YES" => true,
        _ => false,
    }
};

const fn parse_u16(s: &str) -> u16 {
    let bytes = s.as_bytes();
    let mut n: u32 = 0;
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        assert!(c >= b'0' && c <= b'9', "FW_ECHO_PORT must be decimal");
        n = n * 10 + (c - b'0') as u32;
        assert!(n <= u16::MAX as u32, "FW_ECHO_PORT overflow");
        i += 1;
    }
    n as u16
}

/// Parse `a.b.c.d` into wire-order octets (for `from_ne_bytes` / `Ipv4Address`).
pub const fn parse_dotted_ipv4(s: &str) -> [u8; 4] {
    let bytes = s.as_bytes();
    let mut out = [0u8; 4];
    let mut octet = 0u8;
    let mut part = 0usize;
    let mut i = 0usize;
    let mut empty = true;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'.' {
            assert!(!empty && part < 3, "bad IPv4");
            out[part] = octet;
            part += 1;
            octet = 0;
            empty = true;
        } else {
            assert!(c >= b'0' && c <= b'9', "bad IPv4 digit");
            let d = c - b'0';
            let next = octet as u16 * 10 + d as u16;
            assert!(next <= 255, "IPv4 octet overflow");
            octet = next as u8;
            empty = false;
        }
        i += 1;
    }
    assert!(!empty && part == 3, "bad IPv4");
    out[3] = octet;
    out
}

/// Source IP seeded as XDP DROP on [`FW_LISTEN_PORT`] (firewall payload builds).
pub const FW_BLOCKED_SRC_IP: [u8; 4] = parse_dotted_ipv4(env!("FW_BLOCKED_SRC_IP"));
pub const FW_TR3000_IP: [u8; 4] = parse_dotted_ipv4(env!("FW_TR3000_IP"));
/// VIP (A): XDP L2 rewrite + TX; stack never owns this address.
pub const FW_VIP_A_IP: [u8; 4] = parse_dotted_ipv4(env!("FW_VIP_A_IP"));
/// UDP destination port on TR3000 for the fixed echo.
pub const FW_ECHO_PORT: u16 = parse_u16(env!("FW_ECHO_PORT"));
pub const FW_LISTEN_PORT: u16 = 8080;
/// UDP control port for `VipMapOp` (XDP always PASSes this dport).
pub const FW_CTRL_PORT: u16 = 8081;

/// Wire-order octets as native `u32` (matches LE load of `iph->saddr`).
pub const fn ipv4_u32(octets: [u8; 4]) -> u32 {
    u32::from_ne_bytes(octets)
}

pub fn log_cfg() {
    log::info!(
        "[FWTEST] cfg xdp={} blocked={}.{}.{}.{} vip_a={}.{}.{}.{} tr3000={}.{}.{}.{}:{} listen=:{}",
        XDP_ENABLE as u8,
        FW_BLOCKED_SRC_IP[0],
        FW_BLOCKED_SRC_IP[1],
        FW_BLOCKED_SRC_IP[2],
        FW_BLOCKED_SRC_IP[3],
        FW_VIP_A_IP[0],
        FW_VIP_A_IP[1],
        FW_VIP_A_IP[2],
        FW_VIP_A_IP[3],
        FW_TR3000_IP[0],
        FW_TR3000_IP[1],
        FW_TR3000_IP[2],
        FW_TR3000_IP[3],
        FW_ECHO_PORT,
        FW_LISTEN_PORT,
    );
}
