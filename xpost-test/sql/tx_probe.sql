-- =============================================================================
-- tx_probe — XDP TX-path debug (append-only; never formal-DROP just_tx_*)
-- =============================================================================

CREATE TABLE IF NOT EXISTS tx_probe_config (
    id bigserial PRIMARY KEY,
    fw_type TEXT NOT NULL CHECK (fw_type IN ('xdp', 'hal')),
    target_pps DOUBLE PRECISION NOT NULL,
    started_at TIMESTAMPTZ NOT NULL,
    stopped_at TIMESTAMPTZ NOT NULL,
    sent BIGINT NOT NULL CHECK (sent >= 0),
    counters JSONB NOT NULL DEFAULT '{}'::jsonb
);

CREATE TABLE IF NOT EXISTS tx_probe_data (
    id bigserial PRIMARY KEY,
    config_id BIGINT NOT NULL REFERENCES tx_probe_config (id) ON DELETE CASCADE,
    t_tx TIMESTAMPTZ NOT NULL,
    t_rx TIMESTAMPTZ,
    got_rx BOOLEAN NOT NULL
);

CREATE INDEX IF NOT EXISTS tx_probe_data_config ON tx_probe_data (config_id);
