//! Compile-time harness config from `config.toml` (`build.rs` → `fw_cfg_data.rs` + `fw_us_*`).

include!(concat!(env!("OUT_DIR"), "/fw_cfg_data.rs"));

/// AOT XDP on the WiFi RX path (derived from enabled network `[[tests]]`).
pub const XDP_ENABLE: bool = XDP_RX;

/// Wire-order octets as native `u32` (matches LE load of `iph->saddr`).
pub const fn ipv4_u32(octets: [u8; 4]) -> u32 {
    u32::from_ne_bytes(octets)
}

pub fn log_cfg() {
    log::info!(
        "[FWTEST] cfg xdp={} probe={}.{}.{}.{}:{} listen=:{} ctrl=:{} seeds={}",
        XDP_ENABLE as u8,
        probe::IP[0],
        probe::IP[1],
        probe::IP[2],
        probe::IP[3],
        probe::REPLY_PORT,
        dut::LISTEN_PORT,
        dut::CTRL_PORT,
        SEEDS.len(),
    );
}
