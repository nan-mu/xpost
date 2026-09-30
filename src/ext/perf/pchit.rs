//! PC-hit histogram (`feature = "perf-pchit"`).

use core::sync::atomic::Ordering;

use portable_atomic::{AtomicU32, AtomicBool};

/// Slot count (open-addressed).
pub const PC_SLOTS: usize = 256;

/// Wire: samples u32 + overflow u32 + reserved u32 + 256 × (pc u32, hits u32) = 12 + 2048 = 2060.
/// Extra reserved word avoids colliding with Trace-only payload length (2056).
pub const WIRE_LEN: usize = 12 + PC_SLOTS * 8;

static ENABLED: AtomicBool = AtomicBool::new(false);

/// Share enable with span profiling (`prof on|off`).
pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

#[inline]
pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

struct PcSlot {
    pc: AtomicU32,
    hits: AtomicU32,
}

/// Heapless PC histogram on the mounted program.
pub struct PcHitSet {
    slots: [PcSlot; PC_SLOTS],
    samples: AtomicU32,
    overflow: AtomicU32,
}

impl PcHitSet {
    pub const fn new() -> Self {
        Self {
            slots: [const {
                PcSlot {
                    pc: AtomicU32::new(0),
                    hits: AtomicU32::new(0),
                }
            }; PC_SLOTS],
            samples: AtomicU32::new(0),
            overflow: AtomicU32::new(0),
        }
    }

    #[inline]
    pub(crate) fn hit(&self, pc: u32) {
        if !enabled() || pc == 0 {
            return;
        }
        self.samples.fetch_add(1, Ordering::Relaxed);
        let mut i = (pc as usize).wrapping_mul(2654435761) % PC_SLOTS;
        for _ in 0..PC_SLOTS {
            let slot = &self.slots[i];
            let cur = slot.pc.load(Ordering::Acquire);
            if cur == pc {
                slot.hits.fetch_add(1, Ordering::Relaxed);
                return;
            }
            if cur == 0
                && slot
                    .pc
                    .compare_exchange(0, pc, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
            {
                slot.hits.fetch_add(1, Ordering::Relaxed);
                return;
            }
            if cur == 0 {
                continue;
            }
            i = (i + 1) % PC_SLOTS;
        }
        self.overflow.fetch_add(1, Ordering::Relaxed);
    }

    pub fn samples(&self) -> u32 {
        self.samples.load(Ordering::Relaxed)
    }

    pub fn overflow(&self) -> u32 {
        self.overflow.load(Ordering::Relaxed)
    }

    pub fn reset_all(&self) {
        for slot in &self.slots {
            slot.pc.store(0, Ordering::Relaxed);
            slot.hits.store(0, Ordering::Relaxed);
        }
        self.samples.store(0, Ordering::Relaxed);
        self.overflow.store(0, Ordering::Relaxed);
    }

    pub fn encode_wire(&self, out: &mut [u8]) {
        debug_assert!(out.len() >= WIRE_LEN);
        out[0..4].copy_from_slice(&self.samples().to_le_bytes());
        out[4..8].copy_from_slice(&self.overflow().to_le_bytes());
        out[8..12].fill(0); // reserved
        for (i, slot) in self.slots.iter().enumerate() {
            let off = 12 + i * 8;
            let pc = slot.pc.load(Ordering::Relaxed);
            let hits = slot.hits.load(Ordering::Relaxed);
            out[off..off + 4].copy_from_slice(&pc.to_le_bytes());
            out[off + 4..off + 8].copy_from_slice(&hits.to_le_bytes());
        }
    }
}
