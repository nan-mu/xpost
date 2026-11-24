//! HAL boot sweep used by the workload comparison script.

use exec_bench_hal::run_hal;
use log::info;
use xpost_perf::sweep;

/// Sweep a workload's work size through the HAL path.
///
/// The XPOST diagnostics use the same work sizes and run count.
/// The timing interval covers the calls to `run_hal`.
pub(super) fn scale_sweep() {
    hal_sweep_one(sweep::OP_INCR, "incr");
    hal_sweep_one(sweep::OP_SQUARE, "square");
    hal_sweep_one(sweep::OP_BITSWAP, "bitswap");
    hal_sweep_one(sweep::OP_FIB, "fib");
    hal_sweep_one(sweep::OP_BUF, "buf");
    hal_sweep_one(sweep::OP_MEMCPY, "memcpy");
    hal_sweep_one(sweep::OP_FLETCHER32, "fletcher32");
    hal_sweep_one(sweep::OP_BSORT, "bsort");
}

/// One workload's sweep.
///
/// Sizes come from the shared list, so this measures the same work the two XDP
/// engines measure. `fib` clamps at 47 and `bsort` at 6, which their lists
/// already respect.
fn hal_sweep_one(opcode: u32, name: &str) {
    let Some(params) = sweep::params_for(opcode) else {
        return;
    };
    let now = || {
        esp_hal::time::Instant::now()
            .duration_since_epoch()
            .as_micros()
    };
    let mut frame = [0u8; 8 + 1024 + 4];
    for &param in params {
        let n = sweep::frame_len_for(opcode, param);
        sweep::fill_op(&mut frame[..n], opcode, param);
        let t0 = now();
        for _ in 0..sweep::RUNS {
            let _ = run_hal(&mut frame[..n]);
        }
        let total_us = now().saturating_sub(t0);
        let mean_ns = total_us.saturating_mul(1000) / sweep::RUNS as u64;
        info!(
            "sweep wl={name} param={param} runs={} mean_ns={mean_ns}",
            sweep::RUNS
        );
    }
}
