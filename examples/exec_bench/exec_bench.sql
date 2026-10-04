-- exec_bench — function-call eBPF via exec ingress (map input/output; cagt).
-- Draft parked under examples/exec_bench/; not loaded by formal::ensure yet.
-- Needs exam (from formal.sql) when wired into xpost-test.

CREATE TABLE IF NOT EXISTS exec_bench_config (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    exam_id UUID NOT NULL REFERENCES exam (id) ON DELETE CASCADE,
    firmware TEXT NOT NULL CHECK (firmware = 'ebpf'),
    workload TEXT NOT NULL,
    input BYTEA NOT NULL,
    started_at TIMESTAMPTZ NOT NULL,
    stopped_at TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS exec_bench_config_exam ON exec_bench_config (exam_id);

-- One row per repetition; perf lives in shared formal/xdp_* tables.
CREATE TABLE IF NOT EXISTS exec_bench_data (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    config_id UUID NOT NULL REFERENCES exec_bench_config (id) ON DELETE CASCADE,
    rep INT NOT NULL CHECK (rep >= 1),
    ok BOOLEAN NOT NULL,
    output BYTEA,
    UNIQUE (config_id, rep)
);

CREATE INDEX IF NOT EXISTS exec_bench_data_config ON exec_bench_data (config_id);
