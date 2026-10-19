-- just_drop — Array Map state experiment (workload-specific).
-- Loaded after formal.sql (needs exam + firmware).

CREATE TABLE IF NOT EXISTS just_drop_config (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    exam_id UUID NOT NULL REFERENCES exam (id) ON DELETE CASCADE,
    firmware firmware NOT NULL,
    target_pps DOUBLE PRECISION NOT NULL,
    window_s DOUBLE PRECISION NOT NULL,
    source_seed BIGINT NOT NULL,
    started_at TIMESTAMPTZ NOT NULL,
    stopped_at TIMESTAMPTZ NOT NULL,
    sent BIGINT NOT NULL CHECK (sent >= 0),
    round INT,
    plan_seed BIGINT,
    hal_first BOOLEAN
);

CREATE INDEX IF NOT EXISTS just_drop_config_exam ON just_drop_config (exam_id);

ALTER TABLE just_drop_config ADD COLUMN IF NOT EXISTS round INT;
ALTER TABLE just_drop_config ADD COLUMN IF NOT EXISTS plan_seed BIGINT;
ALTER TABLE just_drop_config ADD COLUMN IF NOT EXISTS hal_first BOOLEAN;

CREATE TABLE IF NOT EXISTS just_drop_data (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    config_id UUID NOT NULL REFERENCES just_drop_config (id) ON DELETE CASCADE,
    slot INT NOT NULL CHECK (slot >= 0 AND slot < 256),
    value BIGINT NOT NULL,
    UNIQUE (config_id, slot)
);

CREATE INDEX IF NOT EXISTS just_drop_data_config ON just_drop_data (config_id);

CREATE TABLE IF NOT EXISTS just_drop_hal_counter (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    config_id UUID NOT NULL REFERENCES just_drop_config (id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    value BIGINT NOT NULL,
    UNIQUE (config_id, name)
);

CREATE INDEX IF NOT EXISTS just_drop_hal_counter_config ON just_drop_hal_counter (config_id);

CREATE TABLE IF NOT EXISTS just_drop_hal_agg (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    config_id UUID NOT NULL REFERENCES just_drop_config (id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    count BIGINT NOT NULL,
    total_ticks BIGINT NOT NULL,
    min_ticks BIGINT NOT NULL,
    max_ticks BIGINT NOT NULL,
    lo BIGINT NOT NULL,
    width BIGINT NOT NULL,
    buckets BIGINT[16] NOT NULL,
    CONSTRAINT just_drop_hal_agg_buckets_len CHECK (cardinality(buckets) = 16),
    UNIQUE (config_id, name)
);

CREATE INDEX IF NOT EXISTS just_drop_hal_agg_config ON just_drop_hal_agg (config_id);

CREATE TABLE IF NOT EXISTS just_drop_hal_gauge (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    config_id UUID NOT NULL REFERENCES just_drop_config (id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    current BIGINT NOT NULL,
    peak BIGINT NOT NULL,
    UNIQUE (config_id, name)
);

CREATE INDEX IF NOT EXISTS just_drop_hal_gauge_config ON just_drop_hal_gauge (config_id);
