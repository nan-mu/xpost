-- =============================================================================
-- just_tx — 逐包刺激 / 回包观测（实验名严格为 just_tx，无 rtt 后缀）
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
