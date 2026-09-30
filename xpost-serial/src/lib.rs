//! Host serial / flash backend for `xpost-agent`.
//!
//! **Library only** (no binary). Exclusive USB-UART with a seq-ordered event
//! transcript ([`Sink`]) so runs can be browsed like a file.
//!
//! End-to-end experiment **workflow** (what to flash, when to stim, which
//! Postgres tables) lives in **`xpost-test`** rustdoc (`overview` / `config`).
//! This crate documents the **serial/flash API** that the agent wraps as HTTP
//! `/v1/*`.
//!
//! # Roles
//!
//! | Type | Role |
//! |------|------|
//! | [`Session`] | One exclusive port: Download (espflash) ↔ App (line I/O) |
//! | [`Sink`] | Ordered [`Event`] transcript (`StdoutSink`, `FileSink`, …) |
//! | [`Image`] / [`XdpImage`] | Firmware + partitions + optional cold XDP for [`Session::program`] |
//! | [`ColdTable`] / [`JUST_DROP_TABLE`] | Cold-region span for XDP ELF containers |
//!
//! # Cold XDP install (by name)
//!
//! Normal path is **by XDP program name** (from the ELF via `xpost-elf`), not raw
//! cold addresses:
//!
//! - [`Session::install_xdp`] — overwrite same name, else append within the table
//! - [`Session::list_xdp`] / [`Session::clear_xdp`]
//! - [`xdp_name_from_file`] / [`plan_install`] — planning helpers
//!
//! # Session surface (agent maps these to `/v1/*`)
//!
//! | Method | Typical agent route | Notes |
//! |--------|---------------------|-------|
//! | [`Session::open`] | listen startup | `port=None` → unique USB serial; 0/many → error |
//! | [`Session::program`] | `/v1/flash_profile` | Full image: firmware + partitions + optional xdp |
//! | [`Session::install_xdp`] | `/v1/program` | Cold XDP only; often followed by reset-to-app |
//! | [`Session::list_xdp`] / [`clear_xdp`] | `/v1/list`, `/v1/clear` | Cold table |
//! | [`Session::request`] | `/v1/request`, `/v1/counters` | App-mode line; waits for `OK`/`ERR` |
//! | [`Session::serial_window`] | `/v1/serial_window` | Sink slice by wall time (e.g. `tsflow`) |
//! | [`Session::reset_to_app`] | after program | Leave download, run app |
//!
//! # Modes
//!
//! [`Mode::Download`] uses espflash stub/ROM. [`Mode::App`] is line-oriented UART
//! for firmware commands (`counters`, `drop on`, …). Do not interleave without
//! going through session transitions.

mod error;
mod event;
mod partitions;
mod session;
mod sink;
mod table;

pub use error::{Error, Result};
pub use event::{
    Chip, Dir, Event, FlashPhase, FlashTarget, Image, IoData, IoRole, Kind, Mode, SessionWhy,
    XdpImage,
};
pub use session::Session;
pub use sink::{FileSink, Sink, StdoutSink, VecSink};
pub use table::{
    ColdTable, InstallOutcome, JUST_DROP_TABLE, XdpEntry, list_entries, plan_install,
    xdp_name_from_bytes, xdp_name_from_file,
};
