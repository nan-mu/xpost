//! Compile-time harness config from `config.toml` (`build.rs` → `fw_cfg_data.rs` + `fw_us_*`).

include!(concat!(env!("OUT_DIR"), "/fw_cfg_data.rs"));

pub fn log_cfg() {
    crate::init_log!(
        "[FWTEST] cfg bare_hal probe={}.{}.{}.{}:{} listen=:{} ctrl=:{}",
        probe::IP[0],
        probe::IP[1],
        probe::IP[2],
        probe::IP[3],
        probe::REPLY_PORT,
        dut::LISTEN_PORT,
        dut::CTRL_PORT,
    );
}
