//! Which execution engine a leased image runs on, and how one flight enters it.
//!
//! A slot holds bytes. `Engine` records what those bytes are, so the flight
//! code has one place to branch instead of scattering `if interp` checks
//! through the datapath. The AOT arm jumps into the bytes; the interpreter arm
//! hands them to `rbpf`.
//!
//! Both arms run under the same [`super::ingress::xdp`] flight: the `IN_FLIGHT`
//! claim, the quiesce policy, and the `xdp_md` window validation all happen
//! before [`flight`] is reached, so neither engine can bypass them.

use crate::xdp::ingress::ctx::XdpMd;

/// How the bytes in a slot are executed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Engine {
    /// RV32IMC machine code produced by the AOT compilation path.
    /// The slot base is the function entry.
    Aot,
    /// eBPF bytecode in the `mcu` fork's reduced ISA. The slot base is the
    /// first instruction; `len` is the byte count.
    #[cfg(feature = "interp")]
    Interp { len: usize },
}

impl Engine {
    /// Code stored in the image metadata wire format.
    pub(crate) const AOT_CODE: u8 = 0;
    /// Code for the interpreter.
    pub(crate) const INTERP_CODE: u8 = 1;

    /// Encode for the published-state atomics.
    pub(crate) fn code(self) -> u8 {
        match self {
            Engine::Aot => Self::AOT_CODE,
            #[cfg(feature = "interp")]
            Engine::Interp { .. } => Self::INTERP_CODE,
        }
    }
}

/// Bytes the interpreter needs to reach the end of the program.
///
/// The AOT arm reads this as 0: a machine-code entry is a function address, not
/// a range.
pub(crate) fn image_len(engine: Engine) -> usize {
    match engine {
        Engine::Aot => 0,
        #[cfg(feature = "interp")]
        Engine::Interp { len } => len,
    }
}

/// Enter the leased image for one flight.
///
/// Returns the program's `r0` as an `xdp_action`. The caller in
/// [`super::ingress`] validates the resulting packet window; this function only
/// runs the program.
///
/// # Safety
///
/// - `base` must be the published entry of the current lease, and
///   `base..base + image_len(engine)` must stay readable for the whole call.
/// - `ctx` must point to a live [`XdpMd`] whose `data..data_end` lies inside the
///   caller's frame.
/// - The caller must own `IN_FLIGHT` for the whole call.
#[inline(always)]
pub(crate) unsafe fn flight(engine: Engine, base: usize, ctx: *mut XdpMd) -> i32 {
    match engine {
        Engine::Aot => unsafe { super::ingress::inner_xdp(base, ctx) },
        #[cfg(feature = "interp")]
        Engine::Interp { len } => unsafe { super::interp::run(base, len, ctx) },
    }
}
