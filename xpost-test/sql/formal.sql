-- formal（PGSCHEMA=formal）
-- 开跑路径禁止 DROP/TRUNCATE；旧表由人手工删。
-- 显式清空：formal-clear → TRUNCATE exam CASCADE。
-- Types use DO/duplicate_object so ensure-schema is re-runnable.

CREATE EXTENSION IF NOT EXISTS pgcrypto;

DO $$ BEGIN
CREATE TYPE firmware AS ENUM ('xdp_cag', 'xdp_t', 'hal');
EXCEPTION
    WHEN duplicate_object THEN NULL;
END $$;

DO $$ BEGIN
CREATE TYPE xdp_agg_name AS ENUM (
    'rx_processing',
    'execution',
    'dispatch',
    'post_action',
    'tx_submission',
    'tx_block',
    'helper_execution',
    'map_operation'
);
EXCEPTION
    WHEN duplicate_object THEN NULL;
END $$;

DO $$ BEGIN
CREATE TYPE hal_agg_name AS ENUM (
    'rx_processing',
    'execution',
    'dispatch',
    'post_action',
    'tx_submission',
    'tx_block',
    'helper_execution',
    'map_operation'
);
EXCEPTION
    WHEN duplicate_object THEN NULL;
END $$;

DO $$ BEGIN
CREATE TYPE xdp_gauge_name AS ENUM (
    'rx_queue_depth',
    'tx_queue_depth',
    'tx_inflight',
    'trace_buffer_usage'
);
EXCEPTION
    WHEN duplicate_object THEN NULL;
END $$;

DO $$ BEGIN
CREATE TYPE hal_gauge_name AS ENUM (
    'rx_queue_depth',
    'tx_queue_depth',
    'tx_inflight',
    'trace_buffer_usage'
);
EXCEPTION
    WHEN duplicate_object THEN NULL;
END $$;

DO $$ BEGIN
CREATE TYPE xdp_counter_name AS ENUM (
    'rx_packets',
    'xdp_invocations',
    'xdp_pass',
    'xdp_drop',
    'xdp_tx',
    'xdp_aborted',
    'rx_delivered',
    'rx_discarded',
    'rx_queue_overflow',
    'tx_submitted',
    'tx_failed',
    'tx_completed',
    'runtime_errors',
    'tx_fail_bad_buf',
    'tx_fail_token',
    'tx_grp_src',
    'tx_enq',
    'tx_enq_fail_claim',
    'tx_enq_fail_ring',
    'tx_kick',
    'tx_poll',
    'tx_cap_wait',
    'tx_nomem_wait',
    'tx_no_retain',
    'tx_flush',
    'tx_air_done_fail',
    'tx_ns_ref',
    'tx_ns_free',
    'tx_flow_sample',
    'tx_block_gt10ms',
    'tx_block_gt100ms',
    'inactive_skip',
    'reentry',
    'stack_misalign',
    'bad_md',
    'helper_unresolved',
    'helper_map',
    'pool_drop_misalign',
    'tx_wifi_last_rc',
    'tx_pend',
    'tx_inflight',
    'tx_inflight_cap',
    'tx_pending_peak',
    'tx_inflight_peak',
    'tx_block_max_us',
    'block_since_us'
);
EXCEPTION
    WHEN duplicate_object THEN NULL;
END $$;

DO $$ BEGIN
CREATE TYPE hal_counter_name AS ENUM (
    'rx_packets',
    'rx_delivered',
    'rx_discarded',
    'tx_submitted',
    'tx_failed',
    'tx_completed'
);
EXCEPTION
    WHEN duplicate_object THEN NULL;
END $$;

DO $$ BEGIN
CREATE TYPE perf_trace_kind AS ENUM (
    'rx_begin',
    'xdp_begin',
    'xdp_end',
    'rx_end',
    'slow_execution',
    'rx_overflow',
    'runtime_error',
    'user_marker',
    'span_begin',
    'span_end',
    'helper_enter',
    'helper_leave',
    'rx_delivered',
    'rx_discarded',
    'tx_sub_begin',
    'tx_submitted',
    'tx_failed',
    'tx_completed',
    'tx_fail_bad_buf',
    'tx_fail_token',
    'tx_enq',
    'tx_kick',
    'tx_poll',
    'tx_cap_wait',
    'tx_nomem_wait',
    'tx_block',
    'tx_enq_fail_claim',
    'tx_enq_fail_ring',
    'tx_flush',
    'tx_grp_src',
    'tx_no_retain',
    'app_udp_recv',
    'app_echo_begin',
    'app_echo_end',
    'app_udp_send_begin',
    'app_udp_send_end',
    'app_wifi_tx_begin',
    'app_wifi_tx_end'
);
EXCEPTION
    WHEN duplicate_object THEN NULL;
END $$;

CREATE TABLE IF NOT EXISTS exam (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    started_at TIMESTAMPTZ NOT NULL,
    stopped_at TIMESTAMPTZ,
    note TEXT
);

COMMENT ON TABLE exam IS '整场正式跑批；开跑只 INSERT，不自动清空';

CREATE TABLE IF NOT EXISTS just_tx_config (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    exam_id UUID NOT NULL REFERENCES exam (id) ON DELETE CASCADE,
    firmware firmware NOT NULL,
    target_pps DOUBLE PRECISION NOT NULL,
    window_s DOUBLE PRECISION NOT NULL,
    started_at TIMESTAMPTZ NOT NULL,
    stopped_at TIMESTAMPTZ NOT NULL,
    sent BIGINT NOT NULL CHECK (sent >= 0)
);

CREATE INDEX IF NOT EXISTS just_tx_config_exam ON just_tx_config (exam_id);

CREATE TABLE IF NOT EXISTS just_drop_config (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    exam_id UUID NOT NULL REFERENCES exam (id) ON DELETE CASCADE,
    firmware firmware NOT NULL,
    target_pps DOUBLE PRECISION NOT NULL,
    window_s DOUBLE PRECISION NOT NULL,
    sample_interval_ms INT CHECK (sample_interval_ms IS NULL OR sample_interval_ms > 0),
    started_at TIMESTAMPTZ NOT NULL,
    stopped_at TIMESTAMPTZ NOT NULL,
    sent BIGINT NOT NULL CHECK (sent >= 0)
);

CREATE INDEX IF NOT EXISTS just_drop_config_exam ON just_drop_config (exam_id);

CREATE TABLE IF NOT EXISTS just_tx_data (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    config_id UUID NOT NULL REFERENCES just_tx_config (id) ON DELETE CASCADE,
    t_tx TIMESTAMPTZ NOT NULL,
    t_rx TIMESTAMPTZ,
    got_rx BOOLEAN NOT NULL
);

CREATE INDEX IF NOT EXISTS just_tx_data_config ON just_tx_data (config_id);

CREATE TABLE IF NOT EXISTS just_drop_data (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    config_id UUID NOT NULL REFERENCES just_drop_config (id) ON DELETE CASCADE,
    t TIMESTAMPTZ NOT NULL,
    counter_value BIGINT NOT NULL
);

CREATE INDEX IF NOT EXISTS just_drop_data_config ON just_drop_data (config_id);

CREATE TABLE IF NOT EXISTS xdp_counter (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    exam_id UUID NOT NULL REFERENCES exam (id) ON DELETE CASCADE,
    config_id UUID NOT NULL,
    name xdp_counter_name NOT NULL,
    value BIGINT NOT NULL,
    UNIQUE (config_id, name)
);

CREATE INDEX IF NOT EXISTS xdp_counter_exam ON xdp_counter (exam_id);
CREATE INDEX IF NOT EXISTS xdp_counter_config ON xdp_counter (config_id);
COMMENT ON COLUMN xdp_counter.config_id IS
    '假外键：just_tx_config.id 或 just_drop_config.id';

CREATE TABLE IF NOT EXISTS xdp_agg (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    exam_id UUID NOT NULL REFERENCES exam (id) ON DELETE CASCADE,
    config_id UUID NOT NULL,
    name xdp_agg_name NOT NULL,
    count BIGINT NOT NULL,
    total_ticks BIGINT NOT NULL,
    min_ticks BIGINT NOT NULL,
    max_ticks BIGINT NOT NULL,
    lo BIGINT NOT NULL,
    width BIGINT NOT NULL,
    buckets BIGINT[16] NOT NULL,
    CONSTRAINT xdp_agg_buckets_len CHECK (cardinality(buckets) = 16),
    UNIQUE (config_id, name)
);

CREATE INDEX IF NOT EXISTS xdp_agg_exam ON xdp_agg (exam_id);
CREATE INDEX IF NOT EXISTS xdp_agg_config ON xdp_agg (config_id);

CREATE TABLE IF NOT EXISTS xdp_gauge (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    exam_id UUID NOT NULL REFERENCES exam (id) ON DELETE CASCADE,
    config_id UUID NOT NULL,
    name xdp_gauge_name NOT NULL,
    current BIGINT NOT NULL,
    peak BIGINT NOT NULL,
    UNIQUE (config_id, name)
);

CREATE INDEX IF NOT EXISTS xdp_gauge_exam ON xdp_gauge (exam_id);
CREATE INDEX IF NOT EXISTS xdp_gauge_config ON xdp_gauge (config_id);

CREATE TABLE IF NOT EXISTS hal_counter (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    exam_id UUID NOT NULL REFERENCES exam (id) ON DELETE CASCADE,
    config_id UUID NOT NULL,
    name hal_counter_name NOT NULL,
    value BIGINT NOT NULL,
    UNIQUE (config_id, name)
);

CREATE INDEX IF NOT EXISTS hal_counter_exam ON hal_counter (exam_id);
CREATE INDEX IF NOT EXISTS hal_counter_config ON hal_counter (config_id);

CREATE TABLE IF NOT EXISTS hal_agg (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    exam_id UUID NOT NULL REFERENCES exam (id) ON DELETE CASCADE,
    config_id UUID NOT NULL,
    name hal_agg_name NOT NULL,
    count BIGINT NOT NULL,
    total_ticks BIGINT NOT NULL,
    min_ticks BIGINT NOT NULL,
    max_ticks BIGINT NOT NULL,
    lo BIGINT NOT NULL,
    width BIGINT NOT NULL,
    buckets BIGINT[16] NOT NULL,
    CONSTRAINT hal_agg_buckets_len CHECK (cardinality(buckets) = 16),
    UNIQUE (config_id, name)
);

CREATE INDEX IF NOT EXISTS hal_agg_exam ON hal_agg (exam_id);
CREATE INDEX IF NOT EXISTS hal_agg_config ON hal_agg (config_id);

CREATE TABLE IF NOT EXISTS hal_gauge (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    exam_id UUID NOT NULL REFERENCES exam (id) ON DELETE CASCADE,
    config_id UUID NOT NULL,
    name hal_gauge_name NOT NULL,
    current BIGINT NOT NULL,
    peak BIGINT NOT NULL,
    UNIQUE (config_id, name)
);

CREATE INDEX IF NOT EXISTS hal_gauge_exam ON hal_gauge (exam_id);
CREATE INDEX IF NOT EXISTS hal_gauge_config ON hal_gauge (config_id);

CREATE TABLE IF NOT EXISTS trace_event (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    exam_id UUID NOT NULL REFERENCES exam (id) ON DELETE CASCADE,
    config_id UUID NOT NULL,
    seq INT NOT NULL CHECK (seq >= 0),
    kind perf_trace_kind NOT NULL,
    t_us BIGINT NOT NULL CHECK (t_us >= 0),
    arg0 BIGINT NOT NULL,
    arg1 BIGINT NOT NULL,
    UNIQUE (config_id, seq)
);

CREATE INDEX IF NOT EXISTS trace_event_exam ON trace_event (exam_id);
CREATE INDEX IF NOT EXISTS trace_event_config ON trace_event (config_id, seq);
COMMENT ON TABLE trace_event IS '火焰事件；config.firmware 应为 xdp_t';
