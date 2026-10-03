//! Wire requests: App-mode line command shapes. `no_std`, no `alloc`.
//!
//! Parsing lives on the server side; host formatting lives on the client.

/// App-mode line command (server UART).
pub enum Command<'a> {
    Ping,
    EnableXdp,
    DisableXdp,
    ClearMap {
        name: &'a str,
    },
    GetMap {
        name: &'a str,
    },
    UpdateMap {
        name: &'a str,
        key: &'a [u8],
        value: &'a [u8],
    },
    ClearPerf,
    GetPerf,
}
