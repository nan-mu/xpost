-- firewall — Hash Map blacklist experiment (workload-specific).
-- Loaded after formal.sql (needs exam + firmware).

CREATE TABLE IF NOT EXISTS firewall_config (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    exam_id UUID NOT NULL REFERENCES exam (id) ON DELETE CASCADE,
    firmware firmware NOT NULL,
    blacklist_size INT NOT NULL CHECK (blacklist_size >= 0),
    traffic TEXT NOT NULL CHECK (traffic IN ('hit', 'miss', 'mixed')),
    target_pps DOUBLE PRECISION NOT NULL,
    window_s DOUBLE PRECISION NOT NULL,
    started_at TIMESTAMPTZ NOT NULL,
    stopped_at TIMESTAMPTZ NOT NULL,
    sent BIGINT NOT NULL CHECK (sent >= 0),
    legal_sent BIGINT CHECK (legal_sent IS NULL OR legal_sent >= 0),
    matched BIGINT CHECK (matched IS NULL OR matched >= 0),
    drop_sample BIGINT CHECK (drop_sample IS NULL OR drop_sample >= 0),
    loss DOUBLE PRECISION,
    round INT,
    plan_seed BIGINT,
    hal_first BOOLEAN
);

CREATE INDEX IF NOT EXISTS firewall_config_exam ON firewall_config (exam_id);

-- Existing DBs created before legal-flow columns / mixed traffic.
ALTER TABLE firewall_config ADD COLUMN IF NOT EXISTS legal_sent BIGINT;
ALTER TABLE firewall_config ADD COLUMN IF NOT EXISTS matched BIGINT;
ALTER TABLE firewall_config ADD COLUMN IF NOT EXISTS drop_sample BIGINT;
ALTER TABLE firewall_config ADD COLUMN IF NOT EXISTS loss DOUBLE PRECISION;
ALTER TABLE firewall_config ADD COLUMN IF NOT EXISTS round INT;
ALTER TABLE firewall_config ADD COLUMN IF NOT EXISTS plan_seed BIGINT;
ALTER TABLE firewall_config ADD COLUMN IF NOT EXISTS hal_first BOOLEAN;
ALTER TABLE firewall_config DROP CONSTRAINT IF EXISTS firewall_config_traffic_check;
ALTER TABLE firewall_config
    ADD CONSTRAINT firewall_config_traffic_check
    CHECK (traffic IN ('hit', 'miss', 'mixed'));

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
