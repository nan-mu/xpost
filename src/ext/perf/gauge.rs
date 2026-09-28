//! Gauge current + peak (`feature = "perf-gauge"`).

use core::sync::atomic::Ordering;

use portable_atomic::AtomicUsize;

/// One gauge: latest value and historical peak.
pub struct Gauge {
    current: AtomicUsize,
    peak: AtomicUsize,
}

impl Gauge {
    pub const fn new() -> Self {
        Self {
            current: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
        }
    }

    #[inline]
    pub fn set(&self, v: usize) {
        self.current.store(v, Ordering::Relaxed);
        let _ = self
            .peak
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |m| {
                (v > m).then_some(v)
            });
    }

    #[inline]
    pub fn current(&self) -> usize {
        self.current.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn peak(&self) -> usize {
        self.peak.load(Ordering::Relaxed)
    }

    pub fn reset(&self) {
        self.current.store(0, Ordering::Relaxed);
        self.peak.store(0, Ordering::Relaxed);
    }
}

macro_rules! define_gauge_set {
    ($($name:ident),+ $(,)?) => {
        /// Named gauges for the mounted program.
        pub struct GaugeSet {
            $( pub $name: Gauge, )+
        }

        impl GaugeSet {
            pub const fn new() -> Self {
                Self {
                    $( $name: Gauge::new(), )+
                }
            }

            pub fn reset_all(&self) {
                $( self.$name.reset(); )+
            }
        }
    };
}

define_gauge_set! {
    rx_queue_depth,
    tx_queue_depth,
    tx_inflight,
    trace_buffer_usage,
}
