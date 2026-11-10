//! The frozen `formal.*` view of a report.
//!
//! The legacy tables (`xdp_counter`, `xdp_agg`, `xdp_gauge`, `trace_event`, and
//! the per-experiment `*_hal_*` tables) name their slots for one arm and have no
//! room for a metric another arm needs. They stay readable but are no longer
//! written by new runs.
//!
//! This module exists for one transition reason: the exam modules still insert
//! into them while the per-mode tables come up, and removing that path in the
//! same change as the format swap would leave nothing to compare against. Every
//! item here is derived from the report's named metrics, so the old tables see
//! the same numbers the new ones do rather than a second measurement.

use std::collections::BTreeMap;

use super::decode::{DecodedReport, Kind};

/// One aggregate in the legacy fixed-slot shape.
#[derive(Clone, Debug)]
pub struct AggSnap {
    pub name: String,
    pub count: u64,
    pub total_ticks: u64,
    pub min_ticks: u64,
    pub max_ticks: u64,
    /// Bucket base. Zero for a fixed-edge histogram, which the legacy table
    /// cannot express; the shape is kept so the insert statement still works.
    pub lo: u64,
    pub width: u64,
    pub buckets: [u64; 16],
}

/// One legacy trace event.
#[derive(Clone, Copy, Debug)]
pub struct TraceEvent {
    pub kind: u8,
    pub arg0: u32,
    pub arg1: u32,
    pub t_us: u32,
}

/// Legacy trace view.
#[derive(Clone, Debug)]
pub struct TraceSnap {
    /// Events the report carried, in time order.
    pub events: Vec<TraceEvent>,
    /// Ring overflow the firmware reported. Non-zero means the timeline is
    /// short, so a flame graph built from it understates the run.
    pub dropped: u32,
}

/// A report seen the way the frozen tables expect.
#[derive(Clone, Debug, Default)]
pub struct LegacyReport {
    pub mask: u8,
    pub counters: BTreeMap<String, i64>,
    pub aggregates: Vec<AggSnap>,
    pub gauges: BTreeMap<String, (i64, i64)>,
    pub trace: Option<TraceSnap>,
}

/// Mask bit meaning "counters present", kept so the old insert path's checks
/// still read the same way.
pub const MASK_C: u8 = 1 << 0;
pub const MASK_A: u8 = 1 << 1;
pub const MASK_G: u8 = 1 << 2;
pub const MASK_T: u8 = 1 << 3;

/// Project a decoded report into the legacy shape.
///
/// The mask records which sections the report actually carried, so a caller that
/// used to test "did this firmware send a trace section" still gets a truthful
/// answer.
pub fn project(report: &DecodedReport) -> LegacyReport {
    let mut out = LegacyReport::default();
    for m in &report.metrics {
        match m.kind {
            Kind::Counter => {
                out.mask |= MASK_C;
                out.counters.insert(m.name.clone(), m.count as i64);
            }
            Kind::Span => {
                out.mask |= MASK_A;
                out.aggregates.push(AggSnap {
                    name: m.name.clone(),
                    count: m.count,
                    total_ticks: m.total_us,
                    min_ticks: m.min_us,
                    max_ticks: m.max_us,
                    lo: 0,
                    width: 0,
                    // The legacy column is a fixed 16 buckets with no edge
                    // table, so a histogram lands here truncated to that shape.
                    buckets: buckets16(m),
                });
            }
            Kind::Gauge => {
                out.mask |= MASK_G;
                out.gauges
                    .insert(m.name.clone(), (m.current as i64, m.peak as i64));
            }
            Kind::Histogram => {
                out.mask |= MASK_A;
                out.aggregates.push(AggSnap {
                    name: m.name.clone(),
                    count: m.count,
                    total_ticks: m.total_us,
                    min_ticks: m.min_us,
                    max_ticks: m.max_us,
                    lo: m.edges.first().copied().unwrap_or(0),
                    // Fixed edges are not uniform, so a single width cannot
                    // describe them. Zero marks "see the edges in the new table".
                    width: 0,
                    buckets: buckets16(m),
                });
            }
            Kind::Event => {}
        }
    }
    if report.has_events() || report.events_dropped != 0 {
        out.mask |= MASK_T;
        let events: Vec<TraceEvent> = report
            .events
            .iter()
            .map(|e| TraceEvent {
                kind: e.kind,
                arg0: e.arg0,
                arg1: e.arg1,
                t_us: e.t_us,
            })
            .collect();
        out.trace = Some(TraceSnap {
            events,
            dropped: report.events_dropped.min(u32::MAX as u64) as u32,
        });
    }
    out
}

fn buckets16(m: &super::decode::Metric) -> [u64; 16] {
    let mut b = [0u64; 16];
    for (slot, count) in b.iter_mut().zip(m.buckets.iter()) {
        *slot = *count;
    }
    b
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::decode::{Event, Metric};

    fn metric(name: &str, kind: Kind) -> Metric {
        Metric {
            name: name.to_string(),
            kind,
            count: 1,
            total_us: 2,
            min_us: 1,
            max_us: 2,
            current: 5,
            peak: 9,
            edges: Vec::new(),
            buckets: Vec::new(),
        }
    }

    #[test]
    fn the_mask_says_which_sections_arrived() {
        let report = DecodedReport {
            metrics: vec![metric("calls", Kind::Counter)],
            ..DecodedReport::default()
        };
        let legacy = project(&report);
        assert_eq!(legacy.mask & MASK_C, MASK_C);
        assert_eq!(legacy.mask & MASK_A, 0);
        assert_eq!(legacy.mask & MASK_T, 0);
        assert_eq!(legacy.counters.get("calls"), Some(&1));
    }

    #[test]
    fn a_report_with_no_events_has_no_trace_section() {
        // The old code refused a cell whose trace was missing. Reporting an
        // empty trace here would turn a missing timeline into an empty one.
        let report = DecodedReport::default();
        let legacy = project(&report);
        assert!(legacy.trace.is_none());
        assert_eq!(legacy.mask & MASK_T, 0);
    }

    #[test]
    fn an_event_stream_becomes_a_trace_section_in_order() {
        let report = DecodedReport {
            events: vec![
                Event { kind: 2, t_us: 10, arg0: 0, arg1: 0 },
                Event { kind: 3, t_us: 20, arg0: 2, arg1: 0 },
            ],
            events_dropped: 3,
            ..DecodedReport::default()
        };
        let legacy = project(&report);
        let tr = legacy.trace.expect("trace present");
        assert_eq!(tr.events.len(), 2);
        assert_eq!(tr.events[0].t_us, 10);
        assert_eq!(tr.dropped, 3);
        assert_eq!(legacy.mask & MASK_T, MASK_T);
    }

    #[test]
    fn a_histogram_keeps_its_counts_in_the_legacy_bucket_column() {
        let mut m = metric("lookup_us", Kind::Histogram);
        m.edges = vec![4, 16];
        m.buckets = vec![1, 2];
        let report = DecodedReport {
            metrics: vec![m],
            ..DecodedReport::default()
        };
        let legacy = project(&report);
        let agg = &legacy.aggregates[0];
        assert_eq!(agg.buckets[0], 1);
        assert_eq!(agg.buckets[1], 2);
        assert_eq!(agg.lo, 4, "the first edge, since a fixed width cannot describe it");
        assert_eq!(agg.width, 0);
    }
}
