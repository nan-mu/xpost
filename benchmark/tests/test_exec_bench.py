"""Prevent unsupported execution-performance claims and incomplete comparisons."""
import unittest

import pandas as pd

from analysis.data import encode_value
from analysis.exec_bench import WORKLOADS, attach_trace_runtime, derive_exec


def fixture():
    settings = {"exams": {"exec_bench": "exec-exam"}}
    configs, results = [], []
    tables = {"exams": pd.DataFrame([{
        "id": "exec-exam", "started_at": "2026-10-06T00:00:00Z",
        "stopped_at": "2026-10-06T00:01:00Z",
    }]), "exec_bench_trace": pd.DataFrame()}
    for arm in ("ebpf", "hal"):
        agg = []
        for workload in WORKLOADS:
            cid = f"{arm}-{workload}"
            configs.append(dict(id=cid, exam_id="exec-exam", firmware=arm,
                                workload=workload, input="00" * 32,
                                started_at="2026-10-06T00:00:00Z",
                                stopped_at="2026-10-06T00:00:00.631Z"))
            results.append(dict(config_id=cid, rep=1, ok=True, output="01" * 32))
            for metric in ("execution", "helper_execution"):
                n = int(arm == "ebpf" and metric == "helper_execution")
                agg.append(dict(config_id=cid, name=metric, count=n,
                                total_ticks=300 * n, buckets=[n]))
        prefix = "exec_bench_" + ("xdp" if arm == "ebpf" else "hal")
        tables[prefix + "_agg"] = pd.DataFrame(agg)
        tables[prefix + "_counter"] = pd.DataFrame()
        tables[prefix + "_gauge"] = pd.DataFrame()
    tables["exec_bench_config"] = pd.DataFrame(configs)
    tables["exec_bench_data"] = pd.DataFrame(results)
    return tables, settings


class ExecBenchTests(unittest.TestCase):
    def test_zero_samples_are_missing_and_helper_is_separate(self):
        tables, settings = fixture()
        data, audit = derive_exec(tables, settings)
        points = data["exec_bench"]
        self.assertTrue(points.execution_mean_us.isna().all())
        self.assertTrue(points.query("firmware == 'hal'").helper_execution_mean_us.isna().all())
        self.assertTrue(points.query("firmware == 'ebpf'").helper_execution_mean_us.eq(300).all())
        self.assertEqual(audit["execution_zero_sample_conditions"], 16)
        self.assertTrue(points.host_command_ms.eq(631).all())

    def test_absent_perf_remains_missing(self):
        tables, settings = fixture()
        tables["exec_bench_hal_agg"] = pd.DataFrame()
        _, audit = derive_exec(tables, settings)
        self.assertEqual(audit["execution_missing_conditions"], 8)
        self.assertEqual(audit["execution_zero_sample_conditions"], 8)

    def test_duplicate_condition_cannot_replace_missing_workload(self):
        tables, settings = fixture()
        tables["exec_bench_config"].loc[0, "workload"] = "square"
        with self.assertRaisesRegex(ValueError, "one condition"):
            derive_exec(tables, settings)

    def test_incomplete_and_failed_results_are_rejected(self):
        for change in ("unfinished", "missing", "failed", "duplicate"):
            with self.subTest(change=change):
                tables, settings = fixture()
                if change == "unfinished":
                    tables["exams"].loc[0, "stopped_at"] = None
                elif change == "missing":
                    tables["exec_bench_data"] = tables["exec_bench_data"].iloc[1:]
                elif change == "failed":
                    tables["exec_bench_data"].loc[0, "ok"] = False
                else:
                    tables["exec_bench_data"].loc[0, "config_id"] = "ebpf-square"
                with self.assertRaises(ValueError):
                    derive_exec(tables, settings)

    def test_different_paired_bytes_are_rejected(self):
        for field, table in (("input", "exec_bench_config"), ("output", "exec_bench_data")):
            tables, settings = fixture()
            tables[table].loc[0, field] = "02" * 32
            with self.assertRaisesRegex(ValueError, f"paired {field}"):
                derive_exec(tables, settings)

    def test_bytea_encoding_preserves_zero_and_non_utf8_bytes(self):
        raw = bytes(range(32)) + b"\x00\xff"
        for value in (raw, bytearray(raw), memoryview(raw)):
            self.assertEqual(bytes.fromhex(encode_value(value)), raw)

    def test_runtime_us_is_invoke_minus_printk(self):
        tables, settings = fixture()
        rows = []
        for i, workload in enumerate(WORKLOADS):
            cid = f"ebpf-{workload}"
            t0 = 1000 * i
            rows += [
                dict(config_id=cid, seq=0, kind="xdp_begin", t_us=t0, arg0=0),
                dict(config_id=cid, seq=1, kind="helper_enter", t_us=t0 + 50, arg0=6),
                dict(config_id=cid, seq=2, kind="helper_leave", t_us=t0 + 350, arg0=6),
                dict(config_id=cid, seq=3, kind="xdp_end", t_us=t0 + 360, arg0=2),
            ]
        tables["exec_bench_trace"] = pd.DataFrame(rows)
        data, audit = derive_exec(tables, settings)
        points = data["exec_bench"]
        ebpf = points.query("firmware == 'ebpf'")
        self.assertTrue(ebpf.invoke_us.eq(360).all())
        self.assertTrue(ebpf.printk_us.eq(300).all())
        self.assertTrue(ebpf.runtime_us.eq(60).all())
        self.assertTrue(points.query("firmware == 'hal'").runtime_us.isna().all())
        self.assertEqual(audit["runtime_us_missing_conditions"], 8)

    def test_current_report_rejects_missing_trace(self):
        tables, settings = fixture()
        settings["exec_bench"] = {"require_trace": True}
        with self.assertRaisesRegex(ValueError, "requires complete"):
            derive_exec(tables, settings)

    def test_trace_rejects_unmatched_reversed_duplicate_and_external_print(self):
        events = [
            dict(config_id="c", seq=0, kind="xdp_begin", t_us=100, arg0=0),
            dict(config_id="c", seq=1, kind="helper_enter", t_us=110, arg0=6),
            dict(config_id="c", seq=2, kind="helper_leave", t_us=150, arg0=6),
            dict(config_id="c", seq=3, kind="xdp_end", t_us=170, arg0=2),
        ]
        config = pd.DataFrame([dict(config_id="c")])
        for change in ("unmatched", "reversed", "duplicate", "external", "aborted", "second_invoke"):
            with self.subTest(change=change):
                trace = pd.DataFrame(events)
                if change == "unmatched":
                    trace = trace.drop(2)
                elif change == "reversed":
                    trace.loc[2, "t_us"] = 105
                elif change == "duplicate":
                    trace.loc[2, "seq"] = 1
                elif change == "external":
                    trace.loc[3, ["seq", "t_us"]] = [-1, 90]
                    trace.loc[0, ["seq", "t_us"]] = [-2, 80]
                elif change == "aborted":
                    trace.loc[3, "arg0"] = 0
                else:
                    other = trace.copy()
                    other.seq += 4
                    other.t_us += 100
                    trace = pd.concat([trace, other])
                with self.assertRaises(ValueError):
                    attach_trace_runtime(config.copy(), trace)

    def test_two_prints_are_summed_but_other_helpers_are_not_subtracted(self):
        events = pd.DataFrame([
            dict(config_id="c", seq=i, kind=k, t_us=t, arg0=a)
            for i, (k, t, a) in enumerate([
                ("xdp_begin", 0, 0), ("helper_enter", 10, 6), ("helper_leave", 20, 6),
                ("helper_enter", 25, 1), ("helper_leave", 30, 1),
                ("helper_enter", 40, 6), ("helper_leave", 60, 6), ("xdp_end", 80, 2),
            ])
        ])
        result = attach_trace_runtime(pd.DataFrame([dict(config_id="c")]), events)
        self.assertEqual(result.printk_us.iloc[0], 30)
        self.assertEqual(result.runtime_us.iloc[0], 50)

    def test_nested_prints_cannot_be_double_subtracted(self):
        trace = pd.DataFrame([
            dict(config_id="c", seq=i, kind=k, t_us=i * 10, arg0=a)
            for i, (k, a) in enumerate([
                ("xdp_begin", 0), ("helper_enter", 6), ("helper_enter", 6),
                ("helper_leave", 6), ("helper_leave", 6), ("xdp_end", 2),
            ])
        ])
        with self.assertRaisesRegex(ValueError, "overlapping"):
            attach_trace_runtime(pd.DataFrame([dict(config_id="c")]), trace)


if __name__ == "__main__":
    unittest.main()
