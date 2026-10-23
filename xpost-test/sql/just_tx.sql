-- just_tx — TX bounce experiment (workload-specific).
-- Loaded after formal.sql (needs exam + firmware).
--
-- just_tx_data holds one row per send.
-- seq identifies the probe. t_rx is null when the RTT pair is incomplete.
-- saw_tx / saw_rx record each kernel stamp independently of a valid RTT.
-- pkt_rx is an AF_PACKET hit for that seq. It is not a timestamp.
-- kernel_tx_ns / kernel_rx_ns are raw bpf ktime values. They are not wall clocks.
-- ok counts rows with both stamps and t_rx >= t_tx.
-- matched counts rows with saw_rx. pkt_matched counts rows with pkt_rx.
-- stamp_drops and pkt_drops are channel loss during the window.

CREATE TABLE IF NOT EXISTS just_tx_config (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    exam_id UUID NOT NULL REFERENCES exam (id) ON DELETE CASCADE,
    firmware firmware NOT NULL,
    target_pps DOUBLE PRECISION NOT NULL,
    window_s DOUBLE PRECISION NOT NULL,
    started_at TIMESTAMPTZ NOT NULL,
    stopped_at TIMESTAMPTZ NOT NULL,
    sent BIGINT NOT NULL CHECK (sent >= 0),
    ok BIGINT NOT NULL CHECK (ok >= 0),
    matched BIGINT,
    lookup BOOLEAN NOT NULL DEFAULT false,
    round INT,
    plan_seed BIGINT,
    hal_first BOOLEAN
);

CREATE INDEX IF NOT EXISTS just_tx_config_exam ON just_tx_config (exam_id);

ALTER TABLE just_tx_config ADD COLUMN IF NOT EXISTS round INT;
ALTER TABLE just_tx_config ADD COLUMN IF NOT EXISTS plan_seed BIGINT;
ALTER TABLE just_tx_config ADD COLUMN IF NOT EXISTS hal_first BOOLEAN;
ALTER TABLE just_tx_config ADD COLUMN IF NOT EXISTS matched BIGINT;
ALTER TABLE just_tx_config ADD COLUMN IF NOT EXISTS lookup BOOLEAN NOT NULL DEFAULT false;
ALTER TABLE just_tx_config ADD COLUMN IF NOT EXISTS stamp_drops BIGINT;
ALTER TABLE just_tx_config ADD COLUMN IF NOT EXISTS pkt_drops BIGINT;
ALTER TABLE just_tx_config ADD COLUMN IF NOT EXISTS pkt_matched BIGINT;
-- Send interval. NULL means the collector did not record it.
-- Do not treat NULL as 30 seconds.
-- send_elapsed_ns is a monotonic clock and excludes drain and export.
ALTER TABLE just_tx_config ADD COLUMN IF NOT EXISTS send_begin TIMESTAMPTZ;
ALTER TABLE just_tx_config ADD COLUMN IF NOT EXISTS send_end TIMESTAMPTZ;
ALTER TABLE just_tx_config ADD COLUMN IF NOT EXISTS drain_end TIMESTAMPTZ;
ALTER TABLE just_tx_config ADD COLUMN IF NOT EXISTS send_elapsed_ns BIGINT;
ALTER TABLE just_tx_config ADD COLUMN IF NOT EXISTS export_elapsed_ns BIGINT;

CREATE TABLE IF NOT EXISTS just_tx_data (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    config_id UUID NOT NULL REFERENCES just_tx_config (id) ON DELETE CASCADE,
    seq BIGINT,
    t_tx TIMESTAMPTZ NOT NULL,
    t_rx TIMESTAMPTZ,
    saw_tx BOOLEAN,
    saw_rx BOOLEAN
);

ALTER TABLE just_tx_data ADD COLUMN IF NOT EXISTS seq BIGINT;
ALTER TABLE just_tx_data ADD COLUMN IF NOT EXISTS saw_tx BOOLEAN;
ALTER TABLE just_tx_data ADD COLUMN IF NOT EXISTS saw_rx BOOLEAN;
ALTER TABLE just_tx_data ALTER COLUMN t_rx DROP NOT NULL;
ALTER TABLE just_tx_data ADD COLUMN IF NOT EXISTS pkt_rx BOOLEAN;
ALTER TABLE just_tx_data ADD COLUMN IF NOT EXISTS pkt_n INT;
ALTER TABLE just_tx_data ADD COLUMN IF NOT EXISTS kernel_tx_ns BIGINT;
ALTER TABLE just_tx_data ADD COLUMN IF NOT EXISTS kernel_rx_ns BIGINT;
ALTER TABLE just_tx_data ADD COLUMN IF NOT EXISTS rx_stamp_n INT;

CREATE INDEX IF NOT EXISTS just_tx_data_config ON just_tx_data (config_id);

CREATE TABLE IF NOT EXISTS just_tx_hal_counter (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    config_id UUID NOT NULL REFERENCES just_tx_config (id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    value BIGINT NOT NULL,
    UNIQUE (config_id, name)
);

CREATE INDEX IF NOT EXISTS just_tx_hal_counter_config ON just_tx_hal_counter (config_id);

CREATE TABLE IF NOT EXISTS just_tx_hal_agg (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    config_id UUID NOT NULL REFERENCES just_tx_config (id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    count BIGINT NOT NULL,
    total_ticks BIGINT NOT NULL,
    min_ticks BIGINT NOT NULL,
    max_ticks BIGINT NOT NULL,
    lo BIGINT NOT NULL,
    width BIGINT NOT NULL,
    buckets BIGINT[16] NOT NULL,
    CONSTRAINT just_tx_hal_agg_buckets_len CHECK (cardinality(buckets) = 16),
    UNIQUE (config_id, name)
);

CREATE INDEX IF NOT EXISTS just_tx_hal_agg_config ON just_tx_hal_agg (config_id);

CREATE TABLE IF NOT EXISTS just_tx_hal_gauge (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    config_id UUID NOT NULL REFERENCES just_tx_config (id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    current BIGINT NOT NULL,
    peak BIGINT NOT NULL,
    UNIQUE (config_id, name)
);

CREATE INDEX IF NOT EXISTS just_tx_hal_gauge_config ON just_tx_hal_gauge (config_id);
