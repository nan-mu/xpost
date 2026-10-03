-- =============================================================================
-- experiment_* — PPS 时间序列（just_drop 与 just_tx 的 PPS 阶段共用）
--
-- 配置/轮次：experiment_runs（一臂一轮窗口一行）
-- 数据：      counter_samples（窗口内按 sample_interval_ms 采样的计数器）
-- 可选：      sample_reads（旧 triad 读法，formal PPS 主路径可不写）
--
-- source_note 区分实验写入方，例如：
--   'xpost-test just-drop-pps'
--   'xpost-test just-tx-pps'
-- =============================================================================

-- 一轮满速（或定点）UDP 窗口的元数据：臂、窗口、采样间隔、主机发包速率、状态。
CREATE TABLE IF NOT EXISTS experiment_runs (
    run_id UUID PRIMARY KEY,
    pair_id INTEGER NOT NULL,
    run_kind TEXT NOT NULL CHECK (run_kind IN ('random', 'control', 'pilot')),
    -- 固件臂：hal = 纯 app 路径；xdp = XDP 冷装路径
    variant TEXT NOT NULL CHECK (variant IN ('hal', 'xdp')),
    firmware_sha256 TEXT,
    elf_sha256 TEXT,
    -- 主机侧实测提交速率（udp_blast host_pps），不是 DUT 处理 PPS
    sender_target_pps BIGINT,
    udp_payload_bytes INTEGER NOT NULL DEFAULT 8,
    sample_seed BIGINT,
    window_secs DOUBLE PRECISION,
    sample_interval_ms INTEGER,
    started_at_utc TIMESTAMPTZ,
    stopped_at_utc TIMESTAMPTZ,
    status TEXT NOT NULL,
    status_reason TEXT,
    source_note TEXT
);

-- 该 run 内计数器快照：time_ns 为相对窗口起点；counter_value 为 drop 或 rxcnt。
CREATE TABLE IF NOT EXISTS counter_samples (
    run_id UUID NOT NULL REFERENCES experiment_runs(run_id) ON DELETE CASCADE,
    time_ns BIGINT NOT NULL,
    counter_value BIGINT NOT NULL,
    PRIMARY KEY (run_id, time_ns)
);

-- 遗留 triad 采样表（ensure 兼容；当前 formal PPS 主路径可不填充）。
CREATE TABLE IF NOT EXISTS sample_reads (
    run_id UUID NOT NULL REFERENCES experiment_runs(run_id) ON DELETE CASCADE,
    sample_id INTEGER NOT NULL,
    triad_id INTEGER,
    tau_s DOUBLE PRECISION,
    t_req_ns BIGINT,
    t_resp_ns BIGINT,
    counter_value BIGINT,
    valid BOOLEAN NOT NULL DEFAULT TRUE,
    PRIMARY KEY (run_id, sample_id)
);

CREATE INDEX IF NOT EXISTS counter_samples_run_time
    ON counter_samples (run_id, time_ns);
