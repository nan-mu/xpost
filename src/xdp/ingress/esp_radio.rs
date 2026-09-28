//! Wi-Fi bring-up with native-XDP [`xpost_type::XdpProg`].
//!
//! Passes the crate-mounted program into [`::esp_radio::wifi::new`]. Performance
//! hooks are empty unless a `perf-*` feature fills them via [`crate::ext::perf`].

/// Create the Wi-Fi controller with the crate default XDP program.
pub fn new<'d>(
    device: esp_hal::peripherals::WIFI<'d>,
    config: ::esp_radio::wifi::ControllerConfig,
) -> Result<
    (
        ::esp_radio::wifi::WifiController<'d>,
        ::esp_radio::wifi::Interfaces<'d>,
    ),
    ::esp_radio::wifi::WifiError,
> {
    ::esp_radio::wifi::new(device, config, &super::XDP_PROG)
}
