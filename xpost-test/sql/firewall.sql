-- firewall — Hash Map blacklist experiment (workload-specific).
-- Loaded after formal.sql (needs exam + firmware).

CREATE TABLE IF NOT EXISTS firewall_config (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    exam_id UUID NOT NULL REFERENCES exam (id) ON DELETE CASCADE,
    firmware firmware NOT NULL,
    blacklist_size INT NOT NULL CHECK (blacklist_size >= 0),
    traffic TEXT NOT NULL CHECK (traffic IN ('hit', 'miss')),
    target_pps DOUBLE PRECISION NOT NULL,
    window_s DOUBLE PRECISION NOT NULL,
    started_at TIMESTAMPTZ NOT NULL,
    stopped_at TIMESTAMPTZ NOT NULL,
    sent BIGINT NOT NULL CHECK (sent >= 0)
);

CREATE INDEX IF NOT EXISTS firewall_config_exam ON firewall_config (exam_id);

-- Per-key blacklist counts after a cell (`agent get map(blacklist)`).
CREATE TABLE IF NOT EXISTS firewall_data (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    config_id UUID NOT NULL REFERENCES firewall_config (id) ON DELETE CASCADE,
    key_ip INET NOT NULL,
    value BIGINT NOT NULL,
    UNIQUE (config_id, key_ip)
);

CREATE INDEX IF NOT EXISTS firewall_data_config ON firewall_data (config_id);

CREATE TABLE IF NOT EXISTS firewall_hal_counter (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    config_id UUID NOT NULL REFERENCES firewall_config (id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    value BIGINT NOT NULL,
    UNIQUE (config_id, name)
);

CREATE INDEX IF NOT EXISTS firewall_hal_counter_config ON firewall_hal_counter (config_id);

CREATE TABLE IF NOT EXISTS firewall_hal_agg (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    config_id UUID NOT NULL REFERENCES firewall_config (id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    count BIGINT NOT NULL,
    total_ticks BIGINT NOT NULL,
    min_ticks BIGINT NOT NULL,
    max_ticks BIGINT NOT NULL,
    lo BIGINT NOT NULL,
    width BIGINT NOT NULL,
    buckets BIGINT[16] NOT NULL,
    CONSTRAINT firewall_hal_agg_buckets_len CHECK (cardinality(buckets) = 16),
    UNIQUE (config_id, name)
);

CREATE INDEX IF NOT EXISTS firewall_hal_agg_config ON firewall_hal_agg (config_id);

CREATE TABLE IF NOT EXISTS firewall_hal_gauge (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    config_id UUID NOT NULL REFERENCES firewall_config (id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    current BIGINT NOT NULL,
    peak BIGINT NOT NULL,
    UNIQUE (config_id, name)
);

CREATE INDEX IF NOT EXISTS firewall_hal_gauge_config ON firewall_hal_gauge (config_id);
