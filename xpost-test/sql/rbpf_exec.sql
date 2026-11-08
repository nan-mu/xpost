-- rbpf interpreter arm for the exec_bench comparison.
--
-- Kept separate from `formal.*` on purpose: the interpreter is a third engine
-- and its rows must not mix with the AOT and HAL baselines that already exist.
-- `formal.exec_bench_config.firmware` has a two-value CHECK constraint, and
-- widening it would rewrite what the published baselines mean.
--
-- Same shape as the HAL tables, so the same decoding and plotting code applies.

CREATE TABLE IF NOT EXISTS rbpf.exec_bench_config (
    id         uuid    NOT NULL DEFAULT gen_random_uuid(),
    exam_id    uuid    NOT NULL,
    workload   text    NOT NULL,
    input      bytea   NOT NULL,
    started_at timestamptz NOT NULL,
    stopped_at timestamptz,
    PRIMARY KEY (id)
);

CREATE TABLE IF NOT EXISTS rbpf.exec_bench_data (
    id        uuid    NOT NULL DEFAULT gen_random_uuid(),
    config_id uuid    NOT NULL REFERENCES rbpf.exec_bench_config(id) ON DELETE CASCADE,
    rep       integer NOT NULL CHECK (rep >= 1),
    ok        boolean NOT NULL,
    output    bytea,
    PRIMARY KEY (id),
    UNIQUE (config_id, rep)
);

-- One row per perf aggregate for one run.
--
-- `name` is kept as plain text rather than an enum: the interpreter reports a
-- different metric set from the AOT arm (only `execution` is populated today),
-- and an enum would force a migration every time a metric appears.
CREATE TABLE IF NOT EXISTS rbpf.exec_bench_agg (
    id          uuid   NOT NULL DEFAULT gen_random_uuid(),
    config_id   uuid   NOT NULL REFERENCES rbpf.exec_bench_config(id) ON DELETE CASCADE,
    name        text   NOT NULL,
    count       bigint NOT NULL,
    total_ticks bigint NOT NULL,
    min_ticks   bigint NOT NULL,
    max_ticks   bigint NOT NULL,
    PRIMARY KEY (id),
    UNIQUE (config_id, name)
);

-- Segment timings for the interpreter arm, in microseconds.
--
-- The AOT baseline cannot separate fixed overhead from computation: all eight
-- workloads measured 142 us. These columns carry the breakdown that shows where
-- a flight's time actually goes, so the two engines can be compared on the same
-- footing.
CREATE TABLE IF NOT EXISTS rbpf.exec_bench_segment (
    id         uuid   NOT NULL DEFAULT gen_random_uuid(),
    config_id  uuid   NOT NULL REFERENCES rbpf.exec_bench_config(id) ON DELETE CASCADE,
    rep        integer NOT NULL,
    -- from the enable command's reply to the flight entry
    lease_us   bigint,
    -- program execution only: the engine call
    engine_us  bigint,
    -- from flight return to the result-map write completing
    publish_us bigint,
    PRIMARY KEY (id),
    UNIQUE (config_id, rep)
);
