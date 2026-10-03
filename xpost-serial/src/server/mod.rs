//! Firmware App-mode server: winnow parsing, wire encoding and an Embassy task.
//!
//! Enable `default-features = false, features = ["server"]`. Parsing enables
//! only winnow's `ascii`/`parser` features, not `std` or `alloc`. Firmware supplies
//! the Embassy executor/time drivers, UART adapters and fixed callback/RX storage.
//!
//! `command` holds parsers; `reply` holds byte-output helpers; private `wire_fmt`
//! implements [`Command::try_from`] and [`crate::reply::OkReply::write_to`], plus
//! each flat error type's [`WireError`] conversion. No Debug/Display is required.
//! [`dispatch`] is a decode-only convenience; [`SerialHandler`] executes requests.
//!
//! Register callbacks through [`SerialHandler::builder`], consume the builder
//! with [`SerialHandlerBuilder::build`], then move the handler into its run future.
//! The handler owns the closures and their captures by value; it exposes no
//! registration setters. Spawn after firmware initialization: the task announces
//! readiness with `OK init`. `get init` is never a wire command.
//!
//! [`serial_task!`] declares the concrete Embassy task while allowing anonymous
//! captured closures inside its synchronous setup block. The block runs when
//! creating the spawn token, before the task loop starts. Its final value is the
//! configured handler; `.run()` moves that value into the statically stored future.
//!
//! ```no_run
//! use xpost_serial::{reply::OkReply, server::{serial_task, ReplyWriter, SerialHandler}};
//!
//! serial_task! {
//!     fn control_task(initial_count: u32) {
//!         let mut count = initial_count;
//!         SerialHandler::builder(
//!             || None, // Replace with a move closure owning the UART RX device.
//!             |_: &[u8]| Ok::<(), core::convert::Infallible>(()),
//!             [0; 128],
//!         )
//!         .on_ping(move |reply: ReplyWriter<'_>| {
//!             count += 1; // Captured state is owned by the handler/task.
//!             reply.ok(OkReply::Ping);
//!         })
//!         .build()
//!     }
//! }
//!
//! fn start(spawner: embassy_executor::Spawner) {
//!     spawner.spawn(control_task(0).unwrap());
//! }
//! ```
//!
//! `ReplyWriter` is passed by value and consumed by its output method, so each
//! request has one reply permission. Errors remain flat `ErrReply` implementors;
//! a callback can call `reply.error(&MapError::NameNotFound)` directly.
//!
//! # Direct perf reporting
//!
//! Keep calling `xpost::ext::perf::report_perf` in the firmware's callback. It
//! already accepts a chunk writer and needs no complete payload allocation.
//! `ReplyWriter::get_perf` sends the new `OK get perf <n>` header, then forwards
//! those chunks to the configured UART sink and checks the declared length.
//! This replaces the old `OK report <n>` framing, not the perf encoder.
//! The firmware supplies `xpost` and `esp_println`; neither is a server dependency:
//!
//! ```ignore
//! fn get_perf(reply: xpost_serial::server::ReplyWriter<'_>) {
//!     // Compile/register this callback only when perf is enabled, or send
//!     // PerfError::Unavailable when that facility is absent.
//!     reply.get_perf(xpost::ext::perf::report_payload_len(), |write| {
//!         xpost::ext::perf::report_perf(write);
//!     });
//! }
//!
//! fn uart_write(bytes: &[u8]) -> Result<(), core::convert::Infallible> {
//!     esp_println::Printer::write_bytes(bytes);
//!     Ok(())
//! }
//! ```
//!
//! Callbacks must use the supplied writer for protocol output. Global logging or
//! other direct UART printers must stay quiet while a header/blob is being sent.
//! Streaming avoids a server-owned report buffer; the existing perf encoder may
//! still use its own per-section stack buffers.

pub mod command;
pub mod reply;
pub mod serial;
mod wire_fmt;

pub use crate::xpost_serial_task as serial_task;
pub use command::{ParseCommandError, parse_command};
pub use reply::{ReplyWriter, WireError, WriteBytes, write_error, write_ok};
pub use serial::{SerialHandler, SerialHandlerBuilder};

#[doc(hidden)]
pub use embassy_executor as __executor;

use crate::r#type::command::Command;

/// Parse one complete request line, borrowing arguments without executing it.
/// See [`parse_command`] for whitespace, arity and input-lifetime rules.
pub fn dispatch(line: &[u8]) -> Result<Command<'_>, ParseCommandError> {
    Command::try_from(line)
}
