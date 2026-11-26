//! Embed a readiness marker in the Firmware ELF without exposing Wi-Fi inputs.

#[unsafe(export_name = "xpost_medium_wifi_inputs")]
static WIFI_INPUTS_PRESENT: u8 = match (option_env!("WIFI_SSID"), option_env!("WIFI_PASS")) {
    (Some(ssid), Some(_)) if !ssid.is_empty() => 1,
    _ => 0,
};

pub(super) fn retain_marker() {
    core::hint::black_box(&WIFI_INPUTS_PRESENT);
}
