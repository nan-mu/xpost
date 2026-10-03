//! ESP32-C3 MPC performance counters (`mpcer`/`mpcmr`/`mpccr`).
//!
//! Standard RISC-V `mcycle`/`minstret` trap on ESP32-C3; use Espressif custom
//! CSRs documented in TRM §1.4. Only one event is counted per measurement
//! (IPC needs two passes).

use crate::datapath;
use crate::mount;
use crate::part;
use crate::prog_table;

/// MPC CSR addresses (ESP32-C3 TRM).
const CSR_MPCER: u32 = 0x7E0;
const CSR_MPCMR: u32 = 0x7E1;
const CSR_MPCCR: u32 = 0x7E2;

const MPCER_CYCLE: u32 = 1 << 0;
const MPCER_INST: u32 = 1 << 2;
const MPCMR_ENABLE: u32 = 1 << 0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PerfError {
    Unsupported,
    DatapathActive,
    NoProgram,
    IterationsZero,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub iterations: u32,
    pub cycles: u32,
    pub instructions: u32,
}

impl Snapshot {
    /// `instructions * 1000 / cycles`, or 0 when `cycles == 0`.
    #[inline]
    pub fn ipc_milli(self) -> u32 {
        if self.cycles == 0 {
            0
        } else {
            ((self.instructions as u64) * 1000 / (self.cycles as u64)) as u32
        }
    }
}

/// Minimal IPv4 frame: eth(14) + ip(20), protocol ICMP.
static BENCH_PKT: [u8; 34] = [
    0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
    0x02, 0x00, 0x00, 0x00, 0x00, 0x01,
    0x08, 0x00,
    0x45, 0x00, 0x00, 0x14,
    0x00, 0x00, 0x40, 0x00,
    0x40, 0x01, 0x00, 0x00,
    0xc0, 0xa8, 0x01, 0x02,
    0xc0, 0xa8, 0x01, 0x01,
];

#[cfg(target_arch = "riscv32")]
mod imp {
    use super::*;

    #[inline(always)]
    fn csr_write<const CSR: u32>(val: u32) {
        unsafe {
            core::arch::asm!("csrw {}, {}", const CSR, in(reg) val, options(nostack));
        }
    }

    #[inline(always)]
    fn csr_read<const CSR: u32>() -> u32 {
        let v: u32;
        unsafe {
            core::arch::asm!("csrr {}, {}", out(reg) v, const CSR, options(nostack));
        }
        v
    }

    #[inline(always)]
    fn counter_begin(event: u32) {
        csr_write::<CSR_MPCCR>(0);
        csr_write::<CSR_MPCER>(event);
        csr_write::<CSR_MPCMR>(MPCMR_ENABLE);
    }

    #[inline(always)]
    fn counter_end() -> u32 {
        let v = csr_read::<CSR_MPCCR>();
        csr_write::<CSR_MPCMR>(0);
        v
    }

    fn measure(event: u32, iterations: u32) -> u32 {
        let mut pkt = BENCH_PKT;
        let mut total = 0u32;
        critical_section::with(|_| {
            counter_begin(event);
            for _ in 0..iterations {
                let _ = part::xdp(&mut pkt);
            }
            total = counter_end();
        });
        total
    }

    /// Bench RX hook (`part::xdp`) with datapath stopped and a loaded RX slot.
    pub fn bench_rx_hook(iterations: u32) -> Result<Snapshot, PerfError> {
        if iterations == 0 {
            return Err(PerfError::IterationsZero);
        }
        if datapath::is_active() {
            return Err(PerfError::DatapathActive);
        }
        let Some(slot) = mount::try_rx_slot() else {
            return Err(PerfError::NoProgram);
        };
        if !prog_table::is_loaded(slot) {
            return Err(PerfError::NoProgram);
        }

        let cycles = measure(MPCER_CYCLE, iterations);
        let instructions = measure(MPCER_INST, iterations);
        Ok(Snapshot {
            iterations,
            cycles,
            instructions,
        })
    }
}

#[cfg(not(target_arch = "riscv32"))]
mod imp {
    use super::*;

    pub fn bench_rx_hook(_iterations: u32) -> Result<Snapshot, PerfError> {
        Err(PerfError::Unsupported)
    }
}

/// Bench RX hook (`part::xdp`) with datapath stopped and a loaded RX slot.
pub fn bench_rx_hook(iterations: u32) -> Result<Snapshot, PerfError> {
    imp::bench_rx_hook(iterations)
}
