//! Owned callback registration, fixed-buffer UART service and Embassy task setup.
//!
//! The builder owns every closure and its captures. Registration methods consume
//! it and return a new concrete builder type. `build` freezes the callback set;
//! `run` consumes the handler into its future. No callback reference, type-erased
//! allocation, runtime registration table, or mutable registration API is needed.

use core::convert::Infallible;
use embassy_time::{Duration, Timer};

use super::{
    command::ParseCommandError,
    reply::{ReplyState, ReplyWriter, infallible, write_error, write_ok},
};
use crate::r#type::{
    command::Command,
    reply::{OkReply, ServerError},
};

/// Statically dispatched request callback, owned by the serial handler.
/// Closures implement this directly; arguments cannot outlive the request.
pub trait RequestHandler {
    /// Execute a parsed command and consume its single reply permission.
    fn handle(&mut self, command: Command<'_>, reply: ReplyWriter<'_>);
}

impl<F> RequestHandler for F
where
    F: FnMut(Command<'_>, ReplyWriter<'_>),
{
    fn handle(&mut self, command: Command<'_>, reply: ReplyWriter<'_>) {
        self(command, reply);
    }
}

/// Initial behavior: ping succeeds; unregistered operations are unsupported.
/// Created by [`SerialHandler::builder`], without any allocated state.
pub struct DefaultHandler;

impl RequestHandler for DefaultHandler {
    fn handle(&mut self, command: Command<'_>, reply: ReplyWriter<'_>) {
        match command {
            Command::Ping => reply.ok(OkReply::Ping),
            Command::GetInit | Command::GetFinish => reply.error(&ServerError::Unknown),
            _ => reply.error(&ServerError::Unsupported),
        }
    }
}

/// Registration phase owning the read/write adapters, RX buffer and callbacks.
///
/// Every `on_*` method takes its callback by value, including captured state.
/// Register each operation once; if repeated, the last registration handles it.
/// Earlier closures remain owned until the handler is dropped. [`Self::build`]
/// consumes this interface so the running handler cannot be reconfigured.
pub struct SerialHandlerBuilder<R, W, H, const N: usize> {
    read: R,
    write: W,
    line: [u8; N],
    handler: H,
}

// Each registration wraps the previous concrete handler in an owned closure.
// The compiler stores captures inline; the chain requires neither dyn nor alloc.
macro_rules! register {
    ($(#[$doc:meta])* $method:ident($($arg:ident: $ty:ty),*) => $pattern:pat) => {
        $(#[$doc])*
        pub fn $method<F>(self, mut callback: F) -> SerialHandlerBuilder<R, W, impl RequestHandler, N>
        where F: FnMut($($ty,)* ReplyWriter<'_>),
        {
            let Self { read, write, line, mut handler } = self;
            SerialHandlerBuilder {
                read, write, line,
                handler: move |command: Command<'_>, reply: ReplyWriter<'_>| {
                    match command {
                        $pattern => callback($($arg,)* reply),
                        other => handler.handle(other, reply),
                    }
                },
            }
        }
    };
}

impl<R, W, H: RequestHandler, const N: usize> SerialHandlerBuilder<R, W, H, N> {
    register!(/// Override ping execution and reply generation.
        on_ping() => Command::Ping);
    register!(/// Register XDP enable execution and reply generation.
        on_enable_xdp() => Command::EnableXdp);
    register!(/// Register XDP disable execution and reply generation.
        on_disable_xdp() => Command::DisableXdp);
    register!(/// Register function-call exec enable and reply generation.
        on_enable_exec() => Command::EnableExec);
    register!(/// Register function-call exec disable and reply generation.
        on_disable_exec() => Command::DisableExec);
    register!(/// Register map clearing by name.
        on_clear_map(name: &str) => Command::ClearMap { name });
    register!(/// Register map reporting; `ReplyWriter::get_map` streams its payload.
        on_get_map(name: &str) => Command::GetMap { name });
    register!(/// Register map update and map-specific key/value validation.
        on_update_map(name: &str, key: &[u8], value: &[u8]) => Command::UpdateMap { name, key, value });
    register!(/// Register performance reset execution and reply generation.
        on_clear_perf() => Command::ClearPerf);
    register!(/// Register performance reporting, e.g. a closure calling `report_perf`.
        on_get_perf() => Command::GetPerf);
    register!(/// Copy a cold image into an idle slot. Does not publish it.
        on_xdp_stage(name: &str, slot: u8) => Command::XdpStage { name, slot });
    register!(/// Quiesce and publish a staged slot.
        on_xdp_activate(slot: u8, generation: u32, pass: bool) => Command::XdpActivate { slot, generation, pass });
    register!(/// Quiesce and publish the retained slot.
        on_xdp_rollback(pass: bool) => Command::XdpRollback { pass });
    register!(/// Report boot id, slot roles and quiesce counts.
        on_get_xdp_status() => Command::GetXdpStatus);

    /// Freeze registration, moving adapters, buffer and all captures into the handler.
    pub fn build(self) -> SerialHandler<R, W, H, N> {
        SerialHandler {
            read: self.read,
            write: self.write,
            line: self.line,
            handler: self.handler,
            used: 0,
            overflow: false,
            started: false,
        }
    }
}

/// Firmware serial owner. Adapters, callbacks and captures are stored by value.
///
/// Build with [`Self::builder`], then consume with [`Self::run`]. Use the
/// [`serial_task!`](super::serial_task) macro to instantiate an Embassy task for
/// anonymous captured closure types. The task future owns the whole handler.
/// No registration setters, clone, or callback-extraction methods are exposed.
///
/// Callbacks are synchronous and must finish promptly; do not block waiting for
/// another Embassy task. Firmware remains responsible for runtime locking and
/// for keeping other UART printers quiet during replies. This module cannot
/// serialize global `esp_println` calls from unrelated tasks/interrupts.
pub struct SerialHandler<R, W, H = DefaultHandler, const N: usize = 128> {
    read: R,
    write: W,
    line: [u8; N],
    handler: H,
    used: usize,
    overflow: bool,
    started: bool,
}

impl<R, W, const N: usize> SerialHandler<R, W, DefaultHandler, N> {
    /// Begin registration, taking ownership of adapters and a fixed RX buffer.
    ///
    /// `read` is nonblocking: one byte or `None` when idle; handle hardware errors
    /// in that adapter. `write` writes all bytes, matching the infallible sink
    /// used by `xpost::ext::perf::report_perf` and `esp_println::Printer`.
    /// Empty buffers reject every nonempty line as `ERR line-too-long`.
    pub fn builder(
        read: R,
        write: W,
        line: [u8; N],
    ) -> SerialHandlerBuilder<R, W, DefaultHandler, N>
    where
        R: FnMut() -> Option<u8>,
        W: FnMut(&[u8]) -> Result<(), Infallible>,
    {
        SerialHandlerBuilder {
            read,
            write,
            line,
            handler: DefaultHandler,
        }
    }
}

impl<R, W, H, const N: usize> SerialHandler<R, W, H, N>
where
    R: FnMut() -> Option<u8>,
    W: FnMut(&[u8]) -> Result<(), Infallible>,
    H: RequestHandler,
{
    /// Emit unsolicited `OK init` once (same contract as [`Self::run`] entry).
    pub fn start(&mut self) {
        if self.started {
            return;
        }
        infallible(write_ok(&OkReply::Init, &mut self.write));
        self.started = true;
    }

    /// Drain all currently available RX bytes synchronously (host / test driver).
    ///
    /// Calls [`Self::start`] on first use. Returns `false` if a callback broke
    /// binary framing (same terminal condition as [`Self::run`]).
    pub fn pump(&mut self) -> bool {
        self.start();
        loop {
            let Some(byte) = (self.read)() else {
                return true;
            };
            if !self.receive(byte) {
                return false;
            }
        }
    }

    /// Consume the configured handler and service UART requests in Embassy.
    ///
    /// Start only after firmware initialization: sends `OK init` on entry.
    /// Accepts CR, LF and CRLF; blank lines are ignored. Overlong requests are
    /// discarded through the next terminator, never executed as a valid prefix.
    /// Drains at most 64 bytes before yielding, sleeping 10 ms when idle and
    /// 1 ms after a full batch so sustained RX cannot monopolize the executor.
    ///
    /// Returns only if a callback emitted a malformed-length binary frame. No
    /// more responses are sent then: firmware must restore transport framing
    /// before starting another session. Dropping the future drops owned captures.
    pub async fn run(mut self) {
        self.start();
        loop {
            let mut idle = false;
            for _ in 0..64 {
                let Some(byte) = (self.read)() else {
                    idle = true;
                    break;
                };
                if !self.receive(byte) {
                    return;
                }
            }
            Timer::after(Duration::from_millis(if idle { 10 } else { 1 })).await;
        }
    }

    fn receive(&mut self, byte: u8) -> bool {
        if byte == b'\n' || byte == b'\r' {
            if self.overflow {
                infallible(write_error(&ParseCommandError::TooLong, &mut self.write));
                self.used = 0;
                self.overflow = false;
            } else if self.used != 0 {
                let complete = self.handle_line();
                self.used = 0;
                return complete;
            }
        } else if !self.overflow {
            if let Some(slot) = self.line.get_mut(self.used) {
                *slot = byte;
                self.used += 1;
            } else {
                self.overflow = true;
            }
        }
        true
    }

    fn handle_line(&mut self) -> bool {
        let command = match Command::try_from(&self.line[..self.used]) {
            Ok(command) => command,
            Err(ParseCommandError::Empty) => return true,
            Err(error) => {
                infallible(write_error(&error, &mut self.write));
                return true;
            }
        };
        let mut state = ReplyState::Pending;
        self.handler
            .handle(command, ReplyWriter::new(&mut self.write, &mut state));
        match state {
            ReplyState::Complete => true,
            ReplyState::Pending => {
                // Callback forgot to respond; no header has been sent yet.
                infallible(write_error(&ServerError::Logic, &mut self.write));
                true
            }
            ReplyState::Broken => false,
        }
    }
}

/// Declare a concrete Embassy task whose setup block returns an owned handler.
///
/// Arguments must be concrete `'static` types, as for an ordinary Embassy task.
/// The setup block runs synchronously when the task's spawn token is created;
/// register callbacks there and end it with `.build()`. Closures may capture
/// task arguments by `move`. The generated future then owns the complete handler;
/// no registration runs after its UART loop starts.
///
/// Uses Embassy's supported non-async task factory returning `impl Future`, so
/// captured closure types need not be named and no heap/static callback arena is
/// required. A task pool of one is statically allocated by the Embassy macro.
/// Firmware supplies executor/critical-section/time drivers.
#[macro_export]
macro_rules! xpost_serial_task {
    ($(#[$meta:meta])* $vis:vis fn $name:ident($($arg:ident: $ty:ty),* $(,)?) $setup:block) => {
        #[$crate::server::__executor::task(embassy_executor = $crate::server::__executor)]
        $(#[$meta])*
        $vis fn $name($($arg: $ty),*) -> impl ::core::future::Future<Output = ()> {
            let handler = $setup;
            handler.run()
        }
    };
}
