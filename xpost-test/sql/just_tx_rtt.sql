-- =============================================================================
-- just_tx_rtt_* — just_tx 实验的扫速 / 工作点 RTT（逐包）
--
-- 配置/轮次：just_tx_rtt_runs
--   phase=sweep     → 各 target_pps 网格点（丢包率、return PPS）
--   phase=operating → 约 5% 丢包工作点上的 RTT 轮次
-- 数据：      just_tx_rtt_samples（每个刺激包一行）
--
-- 逐包语义（接近「发时刻 / 是否回包 / RTT」）：
--   t0      = 主机发送墙钟
--   ok      = 是否观测到回包（TC bounce/echo）
--   rtt_ms  = 有回包时的 RTT；无回包为 NULL
--
-- source_note 例：'xpost-test just-tx-rtt'
-- =============================================================================

-- 某一 target_pps 窗口的汇总（含扫速点与 operating 点）。
CREATE TABLE IF NOT EXISTS just_tx_rtt_runs (
    run_id UUID PRIMARY KEY,
    pair_id INTEGER NOT NULL,
    run_kind TEXT NOT NULL CHECK (run_kind IN ('random', 'control', 'pilot')),
    variant TEXT NOT NULL CHECK (variant IN ('hal', 'xdp')),
    phase TEXT NOT NULL DEFAULT 'operating'
        CHECK (phase IN ('sweep', 'operating')),
    target_pps DOUBLE PRECISION NOT NULL,
    calibrated_loss DOUBLE PRECISION,
    window_secs DOUBLE PRECISION,
    sent BIGINT NOT NULL,
    ok BIGINT NOT NULL,
    loss_rate DOUBLE PRECISION NOT NULL,
    return_pps DOUBLE PRECISION,
    device_rxcnt_delta BIGINT,
    median_rtt_ms DOUBLE PRECISION,
    p95_rtt_ms DOUBLE PRECISION,
    mean_rtt_ms DOUBLE PRECISION,
    started_at_utc TIMESTAMPTZ,
    stopped_at_utc TIMESTAMPTZ,
    status TEXT NOT NULL,
    source_note TEXT
);

-- 旧库兼容：若表已存在但缺 phase，补列（CREATE IF NOT EXISTS 不会改旧表）。
ALTER TABLE just_tx_rtt_runs
    ADD COLUMN IF NOT EXISTS phase TEXT NOT NULL DEFAULT 'operating';

-- 逐包样本；ok=false 时 rtt_ms 为 NULL。
CREATE TABLE IF NOT EXISTS just_tx_rtt_samples (
    run_id UUID NOT NULL REFERENCES just_tx_rtt_runs(run_id) ON DELETE CASCADE,
    seq INTEGER NOT NULL,
    t0 TIMESTAMPTZ NOT NULL,
    ok BOOLEAN NOT NULL,
    rtt_ms DOUBLE PRECISION,
    PRIMARY KEY (run_id, seq)
);

CREATE INDEX IF NOT EXISTS just_tx_rtt_samples_run
    ON just_tx_rtt_samples (run_id);
