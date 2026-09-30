-- =============================================================================
-- just_tx — 逐包刺激 / 回包观测（kind-c）+ A/G/T 固定 pps 仪器窗
--
-- formal：同名旧表 DROP CASCADE 后整表重建（见 pg_dsn），再写入空表。
-- =============================================================================

-- 一轮固定目标速率窗口的配置（一臂、一 target_pps、一窗口）。
CREATE TABLE IF NOT EXISTS just_tx_config (
    id bigserial PRIMARY KEY,
    -- DUT 固件臂：hal = app 路径；xdp = XDP 冷装路径
    fw_type TEXT NOT NULL CHECK (fw_type IN ('xdp', 'hal')),
    -- 主机目标发包速率（pps）
    target_pps DOUBLE PRECISION NOT NULL,
    started_at TIMESTAMPTZ NOT NULL,
    stopped_at TIMESTAMPTZ NOT NULL,
    -- 本窗口实际发出的刺激包数
    sent BIGINT NOT NULL CHECK (sent >= 0),
    -- 本窗口内 counters 变化：仅包含有变化的键（数值或字符串），无变化则为 {}
    -- 例：{"failed_tx": 3, "net_rx": 120}
    counters JSONB NOT NULL DEFAULT '{}'::jsonb
);

-- 逐包数据：发包时刻、回包时刻（无回包为 NULL）、是否收到回包。
CREATE TABLE IF NOT EXISTS just_tx_data (
    id bigserial PRIMARY KEY,
    config_id BIGINT NOT NULL REFERENCES just_tx_config (id) ON DELETE CASCADE,
    t_tx TIMESTAMPTZ NOT NULL,
    t_rx TIMESTAMPTZ,
    got_rx BOOLEAN NOT NULL
);

CREATE INDEX IF NOT EXISTS just_tx_data_config ON just_tx_data (config_id);

-- ---------------------------------------------------------------------------
-- A / G / T 共用一轮元数据（固定 pps）；C 仍只用上面两张表。
-- ---------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS just_tx_run (
    id bigserial PRIMARY KEY,
    fw_type TEXT NOT NULL CHECK (fw_type IN ('xdp', 'hal')),
    kind TEXT NOT NULL CHECK (kind IN ('a', 'g', 't')),
    target_pps DOUBLE PRECISION NOT NULL,
    window_s DOUBLE PRECISION NOT NULL,
    started_at TIMESTAMPTZ NOT NULL,
    stopped_at TIMESTAMPTZ NOT NULL,
    sent BIGINT NOT NULL CHECK (sent >= 0),
    counters JSONB NOT NULL DEFAULT '{}'::jsonb,
    -- Agent/serial reply verbatim: `OK report <n> b64=…` (no host-side decode).
    report_raw TEXT
);

CREATE INDEX IF NOT EXISTS just_tx_run_kind ON just_tx_run (kind, fw_type);

-- Optional offline decode tables (formal path does NOT write these).
CREATE TABLE IF NOT EXISTS just_tx_agg (
    id bigserial PRIMARY KEY,
    run_id BIGINT NOT NULL REFERENCES just_tx_run (id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    count BIGINT NOT NULL,
    total_ticks BIGINT NOT NULL,
    min_ticks BIGINT NOT NULL,
    max_ticks BIGINT NOT NULL,
    lo BIGINT NOT NULL,
    width BIGINT NOT NULL,
    buckets JSONB NOT NULL DEFAULT '[]'::jsonb
);

CREATE INDEX IF NOT EXISTS just_tx_agg_run ON just_tx_agg (run_id);

CREATE TABLE IF NOT EXISTS just_tx_gauge (
    id bigserial PRIMARY KEY,
    run_id BIGINT NOT NULL REFERENCES just_tx_run (id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    current BIGINT NOT NULL,
    peak BIGINT NOT NULL
);

CREATE INDEX IF NOT EXISTS just_tx_gauge_run ON just_tx_gauge (run_id);

CREATE TABLE IF NOT EXISTS just_tx_trace_summary (
    id bigserial PRIMARY KEY,
    run_id BIGINT NOT NULL REFERENCES just_tx_run (id) ON DELETE CASCADE,
    len BIGINT NOT NULL,
    dropped BIGINT NOT NULL,
    kind_hist JSONB NOT NULL DEFAULT '{}'::jsonb
);

CREATE INDEX IF NOT EXISTS just_tx_trace_summary_run ON just_tx_trace_summary (run_id);

CREATE TABLE IF NOT EXISTS just_tx_trace_event (
    id bigserial PRIMARY KEY,
    run_id BIGINT NOT NULL REFERENCES just_tx_run (id) ON DELETE CASCADE,
    seq INT NOT NULL CHECK (seq >= 0),
    kind TEXT NOT NULL,
    t_us BIGINT NOT NULL CHECK (t_us >= 0),
    arg0 BIGINT NOT NULL,
    arg1 BIGINT NOT NULL
);

CREATE INDEX IF NOT EXISTS just_tx_trace_event_run ON just_tx_trace_event (run_id, seq);

-- Legacy stage-collapsed one-calls (pre-report_raw); not written by current harness.
CREATE TABLE IF NOT EXISTS just_tx_trace_call (
    id bigserial PRIMARY KEY,
    run_id BIGINT NOT NULL REFERENCES just_tx_run (id) ON DELETE CASCADE,
    call_idx INT NOT NULL CHECK (call_idx >= 0),
    pkt_len INT NOT NULL CHECK (pkt_len >= 0),
    action INT,
    dispatch_us INT NOT NULL CHECK (dispatch_us >= 0),
    exec_us INT NOT NULL CHECK (exec_us >= 0),
    post_us INT NOT NULL CHECK (post_us >= 0),
    total_us INT NOT NULL CHECK (total_us >= 0)
);

CREATE INDEX IF NOT EXISTS just_tx_trace_call_run ON just_tx_trace_call (run_id);
