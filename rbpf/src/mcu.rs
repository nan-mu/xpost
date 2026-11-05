// SPDX-License-Identifier: (Apache-2.0 OR MIT)
//! MCU ABI hook points (`mcu` feature).
//!
//! Upstream `rbpf` is a user-space VM: helpers live in a `HashMap`, permitted
//! addresses in a `HashSet`, and the eBPF stack is a fresh `Vec` per run. None
//! of that fits a bare-metal MCU that must not allocate on a packet path.
//!
//! This module names the two places where an embedding runtime takes control.
//! It defines no map type on purpose: `rbpf` has no map concept, because a map
//! is only ever "a pointer some helper returned". Both map reads and map writes
//! therefore enter the machine through one of these two hooks.
//!
//! # Hook surface
//!
//! 1. [`McuEnv::call_helper`] — every `call` instruction with `src == 0`.
//!    A map lookup, update or delete is one such call. The hook returns the
//!    value that lands in `r0`, so a null return means "unresolved map" or
//!    "miss" and both reach the program unchanged.
//! 2. [`McuEnv::check_access`] — every load and store that is not inside the
//!    VM stack. A map *value* pointer returned by hook 1 is dereferenced here.
//!
//! Stack storage is not a hook: the caller passes the buffer to
//! [`execute_program_mcu`](crate::interpreter::execute_program_mcu).
//!
//! # Soundness
//!
//! The interpreter trusts [`McuEnv`] completely. `check_access` returning `true`
//! for an address the program may not touch is an out-of-bounds memory access,
//! with the same blast radius as unsafe code. The embedding runtime must derive
//! its answer from the lease, not from the program's own registers.

/// Per-instruction budget result.
///
/// Returned by [`McuEnv::step`]. A refused step ends the run with an error
/// instead of returning a program value, so a caller cannot mistake a truncated
/// run for a legitimate verdict.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Continue executing.
    Go,
    /// Stop: the instruction budget is exhausted.
    Exhausted,
}

/// Host- or firmware-provided services the interpreter calls out to.
///
/// All three fields are plain function pointers, so an `McuEnv` is `Copy`, has
/// no lifetime, and can live in a `static`. That keeps it usable from an
/// interrupt-adjacent context and avoids the `dyn` dispatch upstream uses.
#[derive(Clone, Copy)]
pub struct McuEnv {
    /// Helper dispatch for `call` with `src == 0`.
    ///
    /// `id` is the instruction immediate. `args` is `r1..r5` before the call.
    /// The return value is written to `r0`.
    ///
    /// Safety: the hook must return a value that is either `0` or an address
    /// [`Self::check_access`] will accept. It must not unwind.
    pub call_helper: fn(id: u32, args: [u64; 5]) -> u64,

    /// Permission check for one load or store.
    ///
    /// `addr` is the effective address and `len` the access width in bytes.
    /// The check must cover the whole `addr..addr + len` window, not one byte.
    ///
    /// Safety: the hook must reject every window outside memory the program is
    /// entitled to, and must not accept a wrapped `addr + len`.
    pub check_access: fn(addr: u64, len: usize) -> bool,

    /// Called once per dispatched instruction, before it executes.
    ///
    /// This is the only termination guard against a program with an infinite
    /// loop; upstream has none.
    pub step: fn() -> Step,
}

impl McuEnv {
    /// Build an environment from three function pointers.
    pub const fn new(
        call_helper: fn(id: u32, args: [u64; 5]) -> u64,
        check_access: fn(addr: u64, len: usize) -> bool,
        step: fn() -> Step,
    ) -> Self {
        Self {
            call_helper,
            check_access,
            step,
        }
    }

    /// An environment that rejects every helper, allows nothing, and never
    /// stops early. Useful as a baseline and in tests.
    pub const fn deny_all() -> Self {
        fn no_helper(_id: u32, _args: [u64; 5]) -> u64 {
            0
        }
        fn no_access(_addr: u64, _len: usize) -> bool {
            false
        }
        fn no_step() -> Step {
            Step::Go
        }
        Self::new(no_helper, no_access, no_step)
    }
}

impl core::fmt::Debug for McuEnv {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("McuEnv { .. }")
    }
}
