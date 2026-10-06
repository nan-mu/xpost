-- exec_bench — function-call compute via exec ingress (map input/output; cagt).
-- Loaded after formal.sql (needs exam).

CREATE TABLE IF NOT EXISTS exec_bench_config (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    exam_id UUID NOT NULL REFERENCES exam (id) ON DELETE CASCADE,
    firmware TEXT NOT NULL CHECK (firmware IN ('ebpf', 'hal')),
    workload TEXT NOT NULL,
    input BYTEA NOT NULL,
    started_at TIMESTAMPTZ NOT NULL,
    stopped_at TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS exec_bench_config_exam ON exec_bench_config (exam_id);

-- One row per repetition; perf lives in shared formal/xdp_* tables (eBPF)
-- or exec_bench_hal_* (HAL CAG). eBPF Trace rows go to shared trace_event.
CREATE TABLE IF NOT EXISTS exec_bench_data (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    config_id UUID NOT NULL REFERENCES exec_bench_config (id) ON DELETE CASCADE,
    rep INT NOT NULL CHECK (rep >= 1),
    ok BOOLEAN NOT NULL,
    output BYTEA,
    UNIQUE (config_id, rep)
);

CREATE INDEX IF NOT EXISTS exec_bench_data_config ON exec_bench_data (config_id);

CREATE TABLE IF NOT EXISTS exec_bench_hal_counter (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    config_id UUID NOT NULL REFERENCES exec_bench_config (id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    value BIGINT NOT NULL,
    UNIQUE (config_id, name)
);

CREATE INDEX IF NOT EXISTS exec_bench_hal_counter_config ON exec_bench_hal_counter (config_id);

CREATE TABLE IF NOT EXISTS exec_bench_hal_agg (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    config_id UUID NOT NULL REFERENCES exec_bench_config (id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    count BIGINT NOT NULL,
    total_ticks BIGINT NOT NULL,
    min_ticks BIGINT NOT NULL,
    max_ticks BIGINT NOT NULL,
    lo BIGINT NOT NULL,
    width BIGINT NOT NULL,
    buckets BIGINT[16] NOT NULL,
    CONSTRAINT exec_bench_hal_agg_buckets_len CHECK (cardinality(buckets) = 16),
    UNIQUE (config_id, name)
);

CREATE INDEX IF NOT EXISTS exec_bench_hal_agg_config ON exec_bench_hal_agg (config_id);

CREATE TABLE IF NOT EXISTS exec_bench_hal_gauge (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    config_id UUID NOT NULL REFERENCES exec_bench_config (id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    current BIGINT NOT NULL,
    peak BIGINT NOT NULL,
    UNIQUE (config_id, name)
);

CREATE INDEX IF NOT EXISTS exec_bench_hal_gauge_config ON exec_bench_hal_gauge (config_id);
