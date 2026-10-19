"""Checks for mistakes that could change a scientific interpretation."""
import gzip
import hashlib
import json
from pathlib import Path
import tempfile
import unittest

import numpy as np
import pandas as pd

from analysis.data import EXPERIMENTS, load_snapshot
from analysis.figures import map_points
from analysis.metrics import derive, microsecond_mean, pair_intervals, ratio, validate_grid


def fixture():
    settings = dict(rates=[400], blacklist_sizes=[200], traffic=["mixed"], window_s=30,
                    exams={e: e + "-exam" for e in EXPERIMENTS})
    tables = {"exams": pd.DataFrame([dict(id=e + "-exam", started_at="start", stopped_at="stop") for e in EXPERIMENTS])}
    for e in EXPERIMENTS:
        configs, raw = [], []
        for arm in ("hal", "xdp_cag", "xdp_t"):
            cid = e + arm
            trace = arm == "xdp_t"
            configs.append(dict(
                id=cid, exam_id=e+"-exam", firmware=arm, target_pps=1 if trace else 400,
                window_s=0 if trace else 30, sent=1 if trace else 10,
                ok=1 if trace else 3, blacklist_size=8 if trace else 200,
                traffic="hit" if trace else "mixed",
                legal_sent=None if trace else 1024,
                matched=None if trace else 1000,
                loss=None if trace else 1000/1024,
            ))
            if e == "just_tx":
                for duration in ([2] if trace else [1, 3, 8]):
                    start = pd.Timestamp("2026-01-01T00:00:00Z")
                    raw.append(dict(config_id=cid, t_tx=str(start), t_rx=str(start+pd.Timedelta(milliseconds=duration))))
            else:
                n_rows = 256 if e == "just_drop" else (8 if trace else 200)
                for _ in range(n_rows):
                    raw.append(dict(config_id=cid, value=1))
        tables[e+"_config"] = pd.DataFrame(configs)
        tables[e+"_data"] = pd.DataFrame(raw)
        for arm, firmware in (("hal", "hal"), ("xdp", "xdp_cag")):
            cid = e + firmware
            n = 0 if e == "firewall" and arm == "hal" else 2
            tables[f"{e}_{arm}_agg"] = pd.DataFrame([dict(config_id=cid, name="map_operation", count=n, total_ticks=n*5, buckets=[n])])
            tables[f"{e}_{arm}_counter"] = pd.DataFrame([dict(config_id=cid, name="rx_packets", value=10)])
            tables[f"{e}_{arm}_gauge"] = pd.DataFrame([dict(config_id=cid, name="tx_inflight", peak=4)])
    return tables, settings


class MetricTests(unittest.TestCase):
    def test_zero_denominator_is_missing_but_ratio_above_one_survives(self):
        actual = ratio(pd.Series([0., 5., 11.]), pd.Series([0., 0., 10.]))
        self.assertTrue(actual.iloc[:2].isna().all())
        self.assertEqual(actual.iloc[2], 1.1)

    def test_lookup_cycle_rows_are_not_microseconds(self):
        frame = pd.DataFrame({
            "name": ["rx_processing", "lookup_non_null", "lookup_null"],
            "total_ticks": [50, 320, 0],
            "count": [10, 4, 0],
        })
        mean = microsecond_mean(frame)
        self.assertEqual(mean.iloc[0], 5)
        self.assertTrue(mean.iloc[1:].isna().all())

    def test_formulas_preserve_measurement_boundaries(self):
        tables, settings = fixture()
        data, audit = derive(tables, settings)
        tx = data["just_tx"].query("firmware == 'hal'").iloc[0]
        self.assertAlmostEqual(tx.sent_pps, 10/30)
        self.assertEqual(tx.success_pct, 30)
        self.assertEqual(tx.loss_pct, 70)
        self.assertAlmostEqual(tx.reply_pps, .1)
        self.assertEqual(tx.rtt_n, 3)
        self.assertEqual(tx.p50_ms, 3)
        self.assertAlmostEqual(tx.p99_ms, 7.9)
        self.assertTrue(np.isnan(data["firewall_agg"].query("firmware == 'hal'").mean_us.iloc[0]))
        self.assertEqual(data["just_drop_agg"].mean_us.iloc[0], 5)
        self.assertTrue(any("zero_sample_cells=1" in w for w in audit["warnings"]))
        self.assertTrue(data["just_tx"].query("firmware == 'xdp_t'").sent_pps.isna().all())

    def test_missing_metric_is_a_gap_not_a_bridged_line(self):
        tables, settings = fixture()
        tables["just_drop_hal_agg"] = pd.DataFrame()
        tables["just_drop_xdp_agg"] = pd.DataFrame()
        data, audit = derive(tables, settings)
        points = map_points(data, "just_drop")
        self.assertEqual(len(points), 2)
        self.assertTrue(points.mean_us.isna().all())
        self.assertTrue(any("missing_sample_cells=2" in w for w in audit["warnings"]))

    def test_duplicate_grid_cannot_replace_missing_condition(self):
        tables, settings = fixture()
        config = tables["just_drop_config"].copy()
        config.loc[1, "firmware"] = "hal"
        with self.assertRaisesRegex(ValueError, "grid mismatch"):
            validate_grid(config, "just_drop", settings)

    def test_each_round_must_contain_the_full_grid(self):
        tables, settings = fixture()
        config = tables["just_drop_config"].copy()
        extra = config[config.window_s > 0].copy()
        config.loc[config.window_s > 0, "round"] = 1
        extra["round"] = 2
        extra["id"] = extra["id"] + "-r2"
        repeated = pd.concat([config, extra], ignore_index=True)
        grid = validate_grid(repeated, "just_drop", settings)
        self.assertEqual(grid["rounds"], 2)
        self.assertEqual(grid["cag_cells"], 4)
        extra.loc[extra.firmware == "xdp_cag", "firmware"] = "hal"
        broken = pd.concat([config, extra], ignore_index=True)
        with self.assertRaisesRegex(ValueError, "grid mismatch"):
            validate_grid(broken, "just_drop", settings)

    def test_rtt_count_must_agree_with_success_count(self):
        tables, settings = fixture()
        tables["just_tx_config"].loc[0, "ok"] = 4
        with self.assertRaisesRegex(ValueError, "RTT row count"):
            derive(tables, settings)

    def test_firewall_legal_delivery_uses_matched_tokens(self):
        tables, settings = fixture()
        tables["firewall_xdp_counter"] = pd.DataFrame([
            dict(config_id="firewallxdp_cag", name="xdp_pass", value=10),
            dict(config_id="firewallxdp_cag", name="rx_queue_overflow", value=3),
        ])
        data, _ = derive(tables, settings)
        fw = data["firewall"].set_index("firmware")
        self.assertAlmostEqual(fw.loc["xdp_cag", "legal_delivery_pct"], 1000 / 1024 * 100)
        self.assertAlmostEqual(fw.loc["hal", "legal_delivery_pct"], 1000 / 1024 * 100)
        self.assertTrue(fw.pass_queue_loss_pct.isna().all())
        self.assertEqual(fw.loc["xdp_cag", "hit_record_pct"], 2000)  # map sum 200 / sent 10 * 100


class TraceTests(unittest.TestCase):
    def test_nested_pairs_and_missing_endpoints(self):
        kinds = ["rx_begin", "helper_enter", "helper_enter", "helper_leave", "helper_leave", "rx_end", "span_end", "xdp_begin"]
        frame = pd.DataFrame([dict(seq=i, kind=k, t_us=i*10, arg0=1) for i,k in enumerate(kinds)])
        intervals, unmatched = pair_intervals(frame)
        self.assertEqual(unmatched, 2)
        self.assertEqual(list(zip(intervals.start_us, intervals.end_us)), [(20,30),(10,40),(0,50)])

    def test_different_configurations_are_not_paired(self):
        frame = pd.DataFrame([dict(config_id="a", seq=1, kind="rx_begin", t_us=0, arg0=0),
                              dict(config_id="b", seq=2, kind="rx_end", t_us=10, arg0=0)])
        with self.assertRaisesRegex(ValueError, "one configuration"):
            pair_intervals(frame)

    def test_empty_ring_stays_empty(self):
        intervals, unmatched = pair_intervals(pd.DataFrame())
        self.assertTrue(intervals.empty)
        self.assertEqual(unmatched, 0)


class SnapshotTests(unittest.TestCase):
    def test_tampering_or_row_count_mismatch_is_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp)
            payload = b'{"rows":[{"x":1}]}'
            manifest = dict(source_sha256=hashlib.sha256(payload).hexdigest(), row_counts={"rows":1})
            def save(raw, metadata):
                (path/"source.json.gz").write_bytes(gzip.compress(raw))
                (path/"manifest.json").write_text(json.dumps(metadata))
            save(payload, manifest)
            self.assertEqual(load_snapshot(path)[1]["rows"].x.iloc[0], 1)
            save(payload.replace(b'1', b'2'), manifest)
            with self.assertRaisesRegex(ValueError, "checksum"):
                load_snapshot(path)
            manifest["row_counts"]["rows"] = 2
            save(payload, manifest)
            with self.assertRaisesRegex(ValueError, "row counts"):
                load_snapshot(path)


if __name__ == "__main__":
    unittest.main()
