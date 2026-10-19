"""Fixed lookup edges, read from the firmware source.

These checks are collection correctness. They are not paper results.
"""
import re
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
EDGE_SRC = ROOT / "src" / "ext" / "perf" / "aggregate.rs"


def load_edges():
    text = EDGE_SRC.read_text()
    match = re.search(
        r"pub const LOOKUP_EDGE: \[u64; HIST_BUCKETS\] = \[([^\]]+)\]",
        text,
    )
    edges = [int(item) for item in re.findall(r"\d+", match.group(1))]
    if len(edges) != 16 or edges[0] != 0 or edges != sorted(set(edges)):
        raise AssertionError(edges)
    return edges


def bucket(edges, ticks):
    idx = 0
    for i, edge in enumerate(edges):
        if i == 0:
            continue
        if ticks < edge:
            return i - 1
        idx = i
    return idx


def covers(edges, ticks, idx):
    lo = edges[idx]
    hi = edges[idx + 1] if idx + 1 < len(edges) else None
    return ticks >= lo and (hi is None or ticks < hi)


class Hist:
    def __init__(self, edges):
        self.edges = edges
        self.count = 0
        self.total = 0
        self.min = None
        self.max = None
        self.buckets = [0] * len(edges)
        self.raw = []

    def record(self, ticks):
        idx = bucket(self.edges, ticks)
        if not covers(self.edges, ticks, idx):
            raise AssertionError((ticks, idx))
        self.buckets[idx] += 1
        self.count += 1
        self.total += ticks
        self.min = ticks if self.min is None else min(self.min, ticks)
        self.max = ticks if self.max is None else max(self.max, ticks)
        self.raw.append(ticks)

    def reset(self):
        self.__init__(self.edges)

    def audit(self):
        if sum(self.buckets) != self.count or self.count != len(self.raw):
            raise AssertionError((sum(self.buckets), self.count, len(self.raw)))
        if self.total != sum(self.raw):
            raise AssertionError((self.total, sum(self.raw)))
        if self.count == 0:
            return
        if self.min != min(self.raw) or self.max != max(self.raw):
            raise AssertionError((self.min, self.max))
        for ticks in self.raw:
            idx = bucket(self.edges, ticks)
            if self.buckets[idx] == 0 or not covers(self.edges, ticks, idx):
                raise AssertionError((ticks, idx))


def record_lookup(scenes, dt, returned_null, empty_dt):
    scenes[returned_null].record(dt)
    scenes["empty"].record(empty_dt)


class LookupHist(unittest.TestCase):
    def test_fault_inputs_stay_covered(self):
        edges = load_edges()
        for seq in ([5, 0], [8, 16, 0], [0, 5], [16, 8, 0]):
            hist = Hist(edges)
            for ticks in seq:
                hist.record(ticks)
            hist.audit()
            self.assertLess(max(seq), edges[1])

    def test_both_sides_of_each_edge(self):
        edges = load_edges()
        hist = Hist(edges)
        for i, edge in enumerate(edges):
            hist.record(edge)
            self.assertEqual(bucket(edges, edge), i)
            if edge > 0:
                hist.record(edge - 1)
                self.assertEqual(bucket(edges, edge - 1), i - 1)
        hist.record(edges[-1] + 10_000)
        self.assertEqual(bucket(edges, edges[-1] + 10_000), len(edges) - 1)
        hist.audit()

    def test_return_classes_do_not_share_samples(self):
        edges = load_edges()
        scenes = {False: Hist(edges), True: Hist(edges), "empty": Hist(edges)}
        record_lookup(scenes, 40, False, 3)
        record_lookup(scenes, 0, True, 4)
        for hist in scenes.values():
            hist.audit()
        self.assertEqual(scenes[False].raw, [40])
        self.assertEqual(scenes[True].raw, [0])
        self.assertEqual(scenes["empty"].raw, [3, 4])
        self.assertEqual(scenes[False].count, 1)
        self.assertEqual(scenes[True].buckets[bucket(edges, 40)], 0)

    def test_reset_then_read_is_empty(self):
        edges = load_edges()
        hist = Hist(edges)
        hist.record(8)
        hist.reset()
        hist.audit()
        self.assertEqual(hist.count, 0)
        self.assertEqual(sum(hist.buckets), 0)

    def test_wire_slot_count_stays_unique(self):
        aggregate = 11 * 176
        for path in (
            ROOT / "src" / "ext" / "perf" / "report.rs",
            ROOT / "xpost-serial" / "src" / "client" / "serial" / "perf.rs",
            ROOT / "xpost-test" / "common" / "src" / "db" / "decode.rs",
            ROOT / "examples" / "hal_perf" / "src" / "lib.rs",
        ):
            self.assertIn("11 * 176", path.read_text(), path)
        counter, gauge, trace = 184, 32, 16 + 256 * 16 + 64 * 16
        lengths = []
        for mask in range(16):
            n = 0
            if mask & 1:
                n += counter
            if mask & 2:
                n += aggregate
            if mask & 4:
                n += gauge
            if mask & 8:
                n += trace
            lengths.append(n)
        self.assertEqual(len(set(lengths)), 16)
