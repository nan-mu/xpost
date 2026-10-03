//! DUT logger init (`esp-println`).

pub fn init_logger() {
    esp_println::logger::init_logger_from_env();
}
