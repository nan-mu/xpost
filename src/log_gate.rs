//! DUT logger init (`esp-println`). Log macros live in `xdp_mcu`.

pub fn init_logger() {
    esp_println::logger::init_logger_from_env();
}
