//! Function-call invoke of a leased XDP image.
//!
//! Enable Cargo feature `exec-call`. This is an additive [`crate::ext`] surface:
//! firmware prepares a buffer and calls [`invoke`]; the call reuses the existing
//! lease flight in [`crate::xdp::ingress`] (`jalr` into IRAM). It is **not** a
//! second network ingress attach — Wi-Fi RX remains behind `esp-radio`.

use xpost_type::XdpAction;

/// Run the currently leased XDP image on `buf`.
///
/// Same contract as the network datapath: returns a Linux-style [`XdpAction`]
/// and a visible window (subslice of `buf`). When no image is leased, returns
/// [`XdpAction::Pass`] and the full buffer. A flight fault returns
/// [`XdpAction::Drop`].
///
/// When perf backends are compiled in, this records the same XDP begin/end
/// hooks as Wi-Fi ingress (`xdp_invocations` / `xdp_action`) so `get perf`
/// shows one execution per `enable exec`.
#[esp_hal::ram]
pub fn invoke(buf: &mut [u8]) -> (XdpAction, &mut [u8]) {
    #[cfg(any(
        feature = "perf-counter",
        feature = "perf-aggregate",
        feature = "perf-trace"
    ))]
    {
        use xpost_type::EspRadioTrace;
        crate::xdp::ingress::XDP_PROG.xdp_invocations();
    }
    let (act, window) = crate::xdp::ingress::run(buf);
    #[cfg(any(
        feature = "perf-counter",
        feature = "perf-aggregate",
        feature = "perf-trace"
    ))]
    {
        use xpost_type::EspRadioTrace;
        crate::xdp::ingress::XDP_PROG.xdp_action(act);
    }
    (act, window)
}

/// Run the leased image `runs` times and return the mean engine time in
/// microseconds, as `(mean_us_x1000, action)`.
///
/// The microsecond clock reads in 0.7 uses and quantizes to whole microseconds,
/// which cannot describe an engine call that takes one to four. Timing `runs`
/// calls together and dividing makes the quantization error `1/runs` of a
/// microsecond: at a thousand runs the mean resolves to a nanosecond, well below
/// the difference between the engines.
///
/// Each run goes through [`crate::xdp::ingress::run`], so the lease check, the
/// `IN_FLIGHT` claim and the packet window validation all happen exactly as they
/// do for a single flight. What the mean excludes is the code before and after
/// the whole batch, which is why it is a mean per flight rather than a total.
///
/// Returns `None` when no image is leased or a run faults.
#[cfg(feature = "perf-aggregate")]
pub fn invoke_batch(buf: &mut [u8], runs: u32) -> Option<(u64, XdpAction)> {
    use crate::ext::perf::aggregate::AggregateSet;
    if runs == 0 {
        return None;
    }
    let mut last = XdpAction::Aborted;
    let t0 = AggregateSet::now_ticks();
    for _ in 0..runs {
        let (act, _) = crate::xdp::ingress::run(buf);
        last = act;
        if act == XdpAction::Aborted {
            return None;
        }
    }
    let total_us = AggregateSet::now_ticks().saturating_sub(t0);
    // Scale before dividing so a sub-microsecond mean survives the integer
    // division: the caller gets thousandths of a microsecond.
    Some((total_us.saturating_mul(1000) / runs as u64, last))
}

/// Run the engine `runs` times with no lease bookkeeping, for comparison.
///
/// Skips [`crate::xdp::ingress::run`] entirely and calls the engine through the
/// same seam the datapath uses. The difference against [`invoke_batch`] is the
/// cost of the lease check, the `IN_FLIGHT` claim and the packet window
/// validation, which is what a per-flight constant would be made of.
///
/// Returns mean nanoseconds per run, or `None` when nothing is leased.
#[cfg(all(feature = "perf-aggregate", feature = "interp"))]
pub fn invoke_batch_bare(buf: &mut [u8], runs: u32) -> Option<u64> {
    use crate::ext::perf::aggregate::AggregateSet;
    if runs == 0 {
        return None;
    }
    let mut ctx = crate::xdp::ingress::ctx::XdpMd::from_buf(buf);
    let ctx_ptr = &mut ctx as *mut crate::xdp::ingress::ctx::XdpMd;
    let t0 = AggregateSet::now_ticks();
    for _ in 0..runs {
        unsafe {
            core::hint::black_box(crate::xdp::test_flight_bare(ctx_ptr));
        }
    }
    let total_us = AggregateSet::now_ticks().saturating_sub(t0);
    Some(total_us.saturating_mul(1000) / runs as u64)
}

/// Run the engine `runs` times with the VM built once, for comparison.
///
/// The gap against [`invoke_batch_bare`] is what building an `EbpfVmMbuff`
/// costs per call. The interpreter builds one on every flight; if that shows up
/// here, the fix is to keep the VM across runs rather than to change the
/// engine.
///
/// Returns mean nanoseconds per run, or `None` when nothing is leased.
#[cfg(all(feature = "perf-aggregate", feature = "interp"))]
pub fn invoke_batch_hoisted(buf: &mut [u8], runs: u32) -> Option<u64> {
    let mut ctx = crate::xdp::ingress::ctx::XdpMd::from_buf(buf);
    let ctx_ptr = &mut ctx as *mut crate::xdp::ingress::ctx::XdpMd;
    unsafe { crate::xdp::test_flight_hoisted(ctx_ptr, runs) }
}
