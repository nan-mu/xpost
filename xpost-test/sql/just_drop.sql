-- =============================================================================
-- just_drop — 满速 blast 下 drop 计数时间序列（实验名严格为 just_drop）
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
