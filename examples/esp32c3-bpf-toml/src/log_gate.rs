//! DUT logger init (`esp-println`). Log macros live in `xpost`.

pub fn init_logger() {
    esp_println::logger::init_logger_from_env();
}
