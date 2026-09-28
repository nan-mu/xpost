-- =============================================================================
-- just_drop — drop 实验表（C 满速序列 + A/G/T 固定 pps 多轮）
--
-- formal：同名旧表 DROP CASCADE 后整表重建（见 pg_dsn），再写入空表。
-- =============================================================================

CREATE TABLE IF NOT EXISTS just_drop_config (
    id bigserial PRIMARY KEY,
    fw_type TEXT NOT NULL CHECK (fw_type IN ('xdp', 'hal')),
    -- 主机侧实测提交速率（blast host_pps）
    target_pps DOUBLE PRECISION NOT NULL,
    started_at TIMESTAMPTZ NOT NULL,
    stopped_at TIMESTAMPTZ NOT NULL,
    sent BIGINT NOT NULL CHECK (sent >= 0),
    -- 窗口内 counters 变化（仅变化键）；无变化为 {}
    counters JSONB NOT NULL DEFAULT '{}'::jsonb
);

-- 采样点：墙钟时刻 + 当时的 drop 计数。
CREATE TABLE IF NOT EXISTS just_drop_data (
    id bigserial PRIMARY KEY,
    config_id BIGINT NOT NULL REFERENCES just_drop_config (id) ON DELETE CASCADE,
    t TIMESTAMPTZ NOT NULL,
    counter_value BIGINT NOT NULL
);

CREATE INDEX IF NOT EXISTS just_drop_data_config ON just_drop_data (config_id);

-- ---------------------------------------------------------------------------
-- A / G / T 共用一轮元数据（固定 pps）；C 仍只用上面两张表。
-- drop_site: xdp = XDP_DROP 镜像；app = Pass 后 socket 丢。
-- ---------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS just_drop_run (
    id bigserial PRIMARY KEY,
    fw_type TEXT NOT NULL CHECK (fw_type IN ('xdp', 'hal')),
    drop_site TEXT NOT NULL CHECK (drop_site IN ('xdp', 'app')),
    kind TEXT NOT NULL CHECK (kind IN ('a', 'g', 't')),
    target_pps DOUBLE PRECISION NOT NULL,
    window_s DOUBLE PRECISION NOT NULL,
    started_at TIMESTAMPTZ NOT NULL,
    stopped_at TIMESTAMPTZ NOT NULL,
    sent BIGINT NOT NULL CHECK (sent >= 0),
    counters JSONB NOT NULL DEFAULT '{}'::jsonb
);

CREATE INDEX IF NOT EXISTS just_drop_run_kind ON just_drop_run (kind, fw_type);

CREATE TABLE IF NOT EXISTS just_drop_agg (
    id bigserial PRIMARY KEY,
    run_id BIGINT NOT NULL REFERENCES just_drop_run (id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    count BIGINT NOT NULL,
    total_ticks BIGINT NOT NULL,
    min_ticks BIGINT NOT NULL,
    max_ticks BIGINT NOT NULL,
    lo BIGINT NOT NULL,
    width BIGINT NOT NULL,
    buckets JSONB NOT NULL DEFAULT '[]'::jsonb
);

CREATE INDEX IF NOT EXISTS just_drop_agg_run ON just_drop_agg (run_id);

CREATE TABLE IF NOT EXISTS just_drop_gauge (
    id bigserial PRIMARY KEY,
    run_id BIGINT NOT NULL REFERENCES just_drop_run (id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    current BIGINT NOT NULL,
    peak BIGINT NOT NULL
);

CREATE INDEX IF NOT EXISTS just_drop_gauge_run ON just_drop_gauge (run_id);

CREATE TABLE IF NOT EXISTS just_drop_trace_summary (
    id bigserial PRIMARY KEY,
    run_id BIGINT NOT NULL REFERENCES just_drop_run (id) ON DELETE CASCADE,
    len BIGINT NOT NULL,
    dropped BIGINT NOT NULL,
    kind_hist JSONB NOT NULL DEFAULT '{}'::jsonb
);

CREATE INDEX IF NOT EXISTS just_drop_trace_summary_run ON just_drop_trace_summary (run_id);
