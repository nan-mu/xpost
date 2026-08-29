//! Static config (mirror config.toml until xpost-build wiring for HAL-only boards).

pub mod wifi {
    pub const SSID: &str = "CMCC-dKji";
    pub const PASSWORD: &str = "22332233";
}

pub mod probe {
    pub const IP: [u8; 4] = [192, 168, 110, 142];
    pub const REPLY_PORT: u16 = 18080;
}

pub mod dut {
    pub const LISTEN_PORT: u16 = 8080;
    pub const CTRL_PORT: u16 = 8081;
}
