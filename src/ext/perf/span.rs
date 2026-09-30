//! Named site cycle/µs **accumulators** (`feature = "perf-span"`).
//!
//! **Dormant / not used in formal profiles.** Timeline nest spans live on Trace
//! rings (`SpanBegin`/`SpanEnd`). This module sums µs per [`ProfSite`] when
//! `prof on` — orthogonal to flame charts; leave disabled unless a future
//! kind-s consumer appears.

use core::sync::atomic::Ordering;

use portable_atomic::{AtomicBool, AtomicU32, AtomicU64};
use xpost_type::ProfSite;

/// Wire size: `ProfSite::COUNT` × (cycles u64 + entries u32 + pad u32).
pub const WIRE_LEN: usize = ProfSite::COUNT * 16;

/// Spans are off by default: per-packet timer reads on the RX path thrash I-cache.
static ENABLED: AtomicBool = AtomicBool::new(false);

/// Turn span timing on or off (serial `prof on|off`).
pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

#[inline]
pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

struct SpanAccum {
    cycles: AtomicU64,
    entries: AtomicU32,
    /// In-flight start tick; 0 = not started.
    start: AtomicU64,
}

/// Fixed site table on the mounted program.
pub struct SpanSet {
    sites: [SpanAccum; ProfSite::COUNT],
}

impl SpanSet {
    pub const fn new() -> Self {
        Self {
            sites: [const {
                SpanAccum {
                    cycles: AtomicU64::new(0),
                    entries: AtomicU32::new(0),
                    start: AtomicU64::new(0),
                }
            }; ProfSite::COUNT],
        }
    }

    #[inline]
    fn now() -> u64 {
        esp_hal::time::Instant::now()
            .duration_since_epoch()
            .as_micros()
    }

    #[inline]
    pub(crate) fn begin(&self, site: ProfSite) {
        if !enabled() {
            return;
        }
        self.sites[site as usize]
            .start
            .store(Self::now().max(1), Ordering::Relaxed);
    }

    #[inline]
    pub(crate) fn end(&self, site: ProfSite) {
        let slot = &self.sites[site as usize];
        let t0 = slot.start.swap(0, Ordering::Relaxed);
        if t0 == 0 {
            return;
        }
        let dt = Self::now().wrapping_sub(t0);
        slot.cycles.fetch_add(dt, Ordering::Relaxed);
        slot.entries.fetch_add(1, Ordering::Relaxed);
    }

    /// Snapshot one site: (cycles, entries).
    pub fn get(&self, site: ProfSite) -> (u64, u32) {
        let s = &self.sites[site as usize];
        (
            s.cycles.load(Ordering::Relaxed),
            s.entries.load(Ordering::Relaxed),
        )
    }

    pub fn reset_all(&self) {
        for s in &self.sites {
            s.cycles.store(0, Ordering::Relaxed);
            s.entries.store(0, Ordering::Relaxed);
            s.start.store(0, Ordering::Relaxed);
        }
    }

    /// Encode LE wire bytes into `out` (must be ≥ [`WIRE_LEN`]).
    pub fn encode_wire(&self, out: &mut [u8]) {
        debug_assert!(out.len() >= WIRE_LEN);
        for (i, site) in ProfSite::ALL.iter().copied().enumerate() {
            let (cycles, entries) = self.get(site);
            let off = i * 16;
            out[off..off + 8].copy_from_slice(&cycles.to_le_bytes());
            out[off + 8..off + 12].copy_from_slice(&entries.to_le_bytes());
            out[off + 12..off + 16].fill(0);
        }
    }
}
