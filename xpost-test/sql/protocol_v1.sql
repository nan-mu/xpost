-- XPOST experiment design v1. PostgreSQL 14+.
-- Apply to a NEW database. Existing table-name collisions abort the transaction.
-- No destructive migration, DROP, or TRUNCATE is included.
BEGIN;
CREATE SCHEMA IF NOT EXISTS bench;
CREATE DOMAIN bench.u64 AS numeric(20,0)
  CHECK (VALUE >= 0 AND VALUE <= 18446744073709551615);
CREATE TABLE bench.condition (
  condition_id uuid PRIMARY KEY,
  experiment text NOT NULL CHECK (experiment IN ('firewall','just_drop','just_tx','exec_bench','case')),
  protocol_version text NOT NULL,
  parameters jsonb NOT NULL CHECK (jsonb_typeof(parameters)='object'),
  planned_modes text[] NOT NULL DEFAULT ARRAY['hal','aot','jit','rbpf'],
  UNIQUE(condition_id, experiment)
);
CREATE TABLE bench.artifact (
  artifact_id uuid PRIMARY KEY,
  role text NOT NULL,
  sha256 text NOT NULL CHECK (sha256 ~ '^[0-9a-f]{64}$'),
  size_bytes bigint NOT NULL CHECK (size_bytes>=0),
  original_bytes bytea NOT NULL,
  media_type text NOT NULL,
  metadata jsonb NOT NULL DEFAULT '{}',
  CHECK (octet_length(original_bytes)=size_bytes)
);
CREATE TABLE bench.run (
  run_id uuid PRIMARY KEY,
  condition_id uuid NOT NULL,
  config_id uuid NOT NULL,
  mode text NOT NULL CHECK (mode IN ('hal','aot','jit','rbpf')),
  experiment text NOT NULL CHECK (experiment IN ('firewall','just_drop','just_tx','exec_bench','case')),
  variant text NOT NULL CHECK (variant IN ('metrics','trace')),
  phase text NOT NULL CHECK (phase IN ('formal','calibration','qualification','pilot')),
  repeat_no integer NOT NULL CHECK(repeat_no>=1),
  seed bench.u64 NOT NULL,
  plan_order integer NOT NULL CHECK(plan_order>=0),
  status text NOT NULL CHECK(status IN ('planned','running','complete','failed','unsupported','inconclusive','blocked')),
  run_uuid uuid NOT NULL UNIQUE,
  boot_id bench.u64,
  xpost_commit text,
  libbpf_commit text,
  toolchain jsonb NOT NULL DEFAULT '{}',
  host_send_begin_ns bench.u64,
  host_send_end_ns bench.u64,
  host_drain_end_ns bench.u64,
  send_elapsed_ns bench.u64,
  export_elapsed_ns bench.u64,
  issues jsonb NOT NULL DEFAULT '[]' CHECK(jsonb_typeof(issues)='array'),
  FOREIGN KEY(condition_id,experiment) REFERENCES bench.condition(condition_id,experiment),
  UNIQUE(run_id, mode),
  UNIQUE(run_id, mode, experiment),
  UNIQUE(run_id, mode, experiment, variant, config_id),
  CHECK(host_send_end_ns IS NULL OR host_send_begin_ns IS NULL OR host_send_end_ns>=host_send_begin_ns),
  CHECK(host_drain_end_ns IS NULL OR host_send_end_ns IS NULL OR host_drain_end_ns>=host_send_end_ns)
);
CREATE TABLE bench.run_artifact (
  run_id uuid NOT NULL REFERENCES bench.run,
  role text NOT NULL,
  artifact_id uuid NOT NULL REFERENCES bench.artifact,
  PRIMARY KEY(run_id,role,artifact_id)
);
CREATE TABLE bench.clock (
  run_id uuid NOT NULL REFERENCES bench.run,
  clock_id text NOT NULL,
  domain text NOT NULL CHECK(domain IN ('dut','host_monotonic','host_pcap','agent')),
  tick_hz bigint NOT NULL CHECK(tick_hz>0),
  tick_bits integer NOT NULL CHECK(tick_bits BETWEEN 1 AND 64),
  boot_id bench.u64,
  precision_description text NOT NULL,
  PRIMARY KEY(run_id,clock_id)
);
CREATE FUNCTION bench.valid_bounds(b bigint[]) RETURNS boolean
LANGUAGE plpgsql IMMUTABLE AS $$
DECLARE i integer;
BEGIN
  IF b IS NULL OR cardinality(b) NOT BETWEEN 1 AND 64 OR array_ndims(b)<>1 OR array_lower(b,1)<>1 THEN RETURN false; END IF;
  FOR i IN 1..cardinality(b) LOOP
    IF b[i] IS NULL OR b[i]<0 THEN RETURN false; END IF;
    IF i>1 AND b[i]<=b[i-1] THEN RETURN false; END IF;
  END LOOP;
  RETURN true;
END $$;
CREATE TABLE bench.calibration (
  calibration_id uuid PRIMARY KEY,
  condition_id uuid NOT NULL REFERENCES bench.condition,
  metric_name text NOT NULL,
  boundary text NOT NULL,
  tick_hz bigint NOT NULL CHECK(tick_hz>0),
  finite_bounds bigint[] NOT NULL CHECK(bench.valid_bounds(finite_bounds)),
  profile_hash text NOT NULL CHECK(profile_hash ~ '^[0-9a-f]{64}$'),
  algorithm_version text NOT NULL,
  seed bench.u64 NOT NULL,
  status text NOT NULL CHECK(status IN ('candidate','frozen','incomplete','failed','resolution_limited')),
  planned_modes text[] NOT NULL,
  available_modes text[] NOT NULL,
  diagnostics jsonb NOT NULL,
  source_artifact_id uuid NOT NULL REFERENCES bench.artifact
);
CREATE TABLE bench.calibration_sample (
  calibration_id uuid NOT NULL REFERENCES bench.calibration,
  mode text NOT NULL CHECK(mode IN ('hal','aot','jit','rbpf')),
  split text NOT NULL CHECK(split IN ('train','validation','overhead')),
  ordinal integer NOT NULL CHECK(ordinal>=0),
  duration_ticks bench.u64 NOT NULL,
  sample_context jsonb NOT NULL DEFAULT '{}',
  PRIMARY KEY(calibration_id,mode,split,ordinal)
);
CREATE TABLE bench.event_definition (
  experiment text NOT NULL,
  dictionary_version integer NOT NULL CHECK(dictionary_version>=1),
  event_id integer NOT NULL CHECK(event_id BETWEEN 0 AND 65535),
  name text NOT NULL,
  semantics text NOT NULL,
  boundary text NOT NULL,
  PRIMARY KEY(experiment,dictionary_version,event_id)
);
CREATE TABLE bench.send_attempt (
  run_id uuid NOT NULL REFERENCES bench.run,
  seq bigint NOT NULL CHECK(seq>=1),
  mono_ns bench.u64 NOT NULL,
  wall_ns bench.u64 NOT NULL,
  submitted boolean NOT NULL,
  error_code integer,
  traffic_class text CHECK(traffic_class IN ('legal','illegal','probe')),
  payload bytea NOT NULL,
  source_ip inet,
  PRIMARY KEY(run_id,seq),
  CHECK(NOT submitted OR error_code IS NULL)
);
CREATE TABLE bench.capture (
  capture_id uuid PRIMARY KEY,
  run_id uuid NOT NULL REFERENCES bench.run,
  direction text NOT NULL CHECK(direction IN ('tx','rx')),
  interface text NOT NULL,
  clock_id text NOT NULL,
  precision integer NOT NULL CHECK(precision IN (1000000,1000000000)),
  listening boolean NOT NULL,
  exited_by_controller boolean NOT NULL,
  exit_code integer,
  kernel_dropped bigint CHECK(kernel_dropped>=0),
  complete boolean NOT NULL,
  stderr_text text NOT NULL,
  artifact_id uuid NOT NULL REFERENCES bench.artifact,
  FOREIGN KEY(run_id,clock_id) REFERENCES bench.clock,
  UNIQUE(capture_id,run_id)
);
CREATE TABLE bench.packet_fact (
  capture_id uuid NOT NULL REFERENCES bench.capture,
  record_index bigint NOT NULL CHECK(record_index>=0),
  file_offset bigint NOT NULL CHECK(file_offset>=0),
  captured_len integer NOT NULL CHECK(captured_len>=0),
  original_len integer NOT NULL CHECK(original_len>=captured_len),
  ts_sec bigint NOT NULL,
  ts_subsec integer NOT NULL CHECK(ts_subsec>=0),
  timestamp_hz integer NOT NULL CHECK(timestamp_hz IN (1000000,1000000000)),
  observed_uuid uuid,
  seq bigint CHECK(seq>=1),
  parse_status text NOT NULL,
  identity_ok boolean,
  payload_ok boolean,
  version_marker integer,
  parsed jsonb NOT NULL DEFAULT '{}',
  PRIMARY KEY(capture_id,record_index),
  CHECK(ts_subsec<timestamp_hz)
);
CREATE TABLE bench.analysis (
  analysis_id uuid PRIMARY KEY,
  run_id uuid NOT NULL REFERENCES bench.run,
  algorithm_version text NOT NULL,
  source_digest text NOT NULL CHECK(source_digest ~ '^[0-9a-f]{64}$'),
  qualified boolean NOT NULL,
  issues jsonb NOT NULL CHECK(jsonb_typeof(issues)='array'),
  created_at timestamptz NOT NULL DEFAULT now(),
  UNIQUE(analysis_id,run_id)
);

-- Mode schema: hal. All mode checks are literal, not caller conventions.
CREATE SCHEMA IF NOT EXISTS hal;
CREATE TABLE hal.metric_definition (
  metric_id uuid PRIMARY KEY,
  name text NOT NULL,
  kind text NOT NULL CHECK(kind IN ('counter','gauge','histogram')),
  unit text NOT NULL,
  boundary text NOT NULL,
  scope text NOT NULL,
  definition_version text NOT NULL,
  UNIQUE(metric_id,kind), UNIQUE(name,definition_version)
);
CREATE TABLE hal.metric_series (
  series_id uuid PRIMARY KEY,
  metric_id uuid NOT NULL,
  kind text NOT NULL,
  labels jsonb NOT NULL CHECK(jsonb_typeof(labels)='object'),
  FOREIGN KEY(metric_id,kind) REFERENCES hal.metric_definition(metric_id,kind),
  UNIQUE(series_id,kind), UNIQUE(metric_id,labels)
);
CREATE TABLE hal.metric_snapshot (
  snapshot_id uuid PRIMARY KEY,
  run_id uuid NOT NULL,
  mode text NOT NULL DEFAULT 'hal' CHECK(mode='hal'),
  phase text NOT NULL CHECK(phase IN ('baseline','final')),
  epoch bigint NOT NULL CHECK(epoch>=0),
  boot_id bench.u64 NOT NULL,
  clock_id text NOT NULL,
  at_tick bench.u64 NOT NULL,
  profile_hash text NOT NULL CHECK(profile_hash ~ '^[0-9a-f]{64}$'),
  raw_artifact_id uuid NOT NULL REFERENCES bench.artifact,
  FOREIGN KEY(run_id,mode) REFERENCES bench.run(run_id,mode),
  FOREIGN KEY(run_id,clock_id) REFERENCES bench.clock,
  UNIQUE(snapshot_id,run_id), UNIQUE(run_id,epoch,phase)
);
CREATE TABLE hal.scalar_sample (
  snapshot_id uuid NOT NULL REFERENCES hal.metric_snapshot,
  series_id uuid NOT NULL,
  kind text NOT NULL CHECK(kind IN ('counter','gauge')),
  value numeric NOT NULL,
  FOREIGN KEY(series_id,kind) REFERENCES hal.metric_series(series_id,kind),
  PRIMARY KEY(snapshot_id,series_id)
);
CREATE TABLE hal.histogram_sample (
  snapshot_id uuid NOT NULL REFERENCES hal.metric_snapshot,
  series_id uuid NOT NULL,
  kind text NOT NULL DEFAULT 'histogram' CHECK(kind='histogram'),
  calibration_id uuid NOT NULL REFERENCES bench.calibration,
  sample_count bench.u64 NOT NULL,
  sample_sum bench.u64 NOT NULL,
  FOREIGN KEY(series_id,kind) REFERENCES hal.metric_series(series_id,kind),
  PRIMARY KEY(snapshot_id,series_id),
  UNIQUE(snapshot_id,series_id,sample_count)
);
CREATE TABLE hal.histogram_bucket (
  snapshot_id uuid NOT NULL,
  series_id uuid NOT NULL,
  ordinal integer NOT NULL CHECK(ordinal>=1),
  upper_bound bigint CHECK(upper_bound>=0), -- NULL represents +Inf.
  cumulative_count bench.u64 NOT NULL,
  sample_count bench.u64 NOT NULL,
  FOREIGN KEY(snapshot_id,series_id,sample_count)
    REFERENCES hal.histogram_sample(snapshot_id,series_id,sample_count),
  PRIMARY KEY(snapshot_id,series_id,ordinal),
  CHECK(cumulative_count<=sample_count),
  CHECK(upper_bound IS NOT NULL OR cumulative_count=sample_count)
);
CREATE UNIQUE INDEX hal_hist_one_inf ON hal.histogram_bucket(snapshot_id,series_id)
  WHERE upper_bound IS NULL;

CREATE TABLE hal.firewall_config (
  config_id uuid PRIMARY KEY,
  condition_id uuid NOT NULL,
  experiment text NOT NULL DEFAULT 'firewall' CHECK(experiment='firewall'),
  instrumentation text NOT NULL CHECK(instrumentation IN ('counter_only','helper_hist','stage_hist','batch_hist','switch_hist','trace')),
  profile_hash text NOT NULL CHECK(profile_hash ~ '^[0-9a-f]{64}$'),
  calibration_ids uuid[] NOT NULL DEFAULT ARRAY[]::uuid[],
  parameters jsonb NOT NULL DEFAULT '{}',
  target_pps integer CHECK(target_pps>0),
  send_window_ns bench.u64,
  drain_ns bench.u64 NOT NULL,
  payload_bytes integer NOT NULL CHECK(payload_bytes>=32),
  blacklist_capacity integer NOT NULL CHECK(blacklist_capacity=256),
  blacklist_entries integer NOT NULL CHECK(blacklist_entries BETWEEN 1 AND 256),
  legal_numerator integer NOT NULL CHECK(legal_numerator BETWEEN 0 AND 10),
  legal_denominator integer NOT NULL CHECK(legal_denominator=10),
  lookup_case text NOT NULL CHECK(lookup_case IN ('mixed','hit','miss')),
  map_fixture_artifact_id uuid NOT NULL REFERENCES bench.artifact,
  FOREIGN KEY(condition_id,experiment) REFERENCES bench.condition(condition_id,experiment),
  UNIQUE(config_id,condition_id)
);
CREATE TABLE hal.firewall_metrics_data (
  run_id uuid NOT NULL,
  config_id uuid NOT NULL REFERENCES hal.firewall_config,
  mode text NOT NULL DEFAULT 'hal' CHECK(mode='hal'),
  experiment text NOT NULL DEFAULT 'firewall' CHECK(experiment='firewall'),
  variant text NOT NULL DEFAULT 'metrics' CHECK(variant='metrics'),
  baseline_snapshot_id uuid,
  final_snapshot_id uuid,
  analysis_id uuid NOT NULL,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL CHECK(jsonb_typeof(issues)='array'),
  sent_legal bench.u64, sent_illegal bench.u64,
  matched_legal bench.u64, blocked_hit_delta bench.u64,
  token_refused bench.u64,
  CHECK(matched_legal IS NULL OR sent_legal IS NULL OR matched_legal<=sent_legal),
  PRIMARY KEY(run_id,analysis_id),
  FOREIGN KEY(run_id,mode,experiment,variant,config_id)
    REFERENCES bench.run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(baseline_snapshot_id,run_id) REFERENCES hal.metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(final_snapshot_id,run_id) REFERENCES hal.metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(analysis_id,run_id) REFERENCES bench.analysis(analysis_id,run_id),
  CHECK(NOT qualified OR (baseline_snapshot_id IS NOT NULL AND final_snapshot_id IS NOT NULL))
);
CREATE TABLE hal.firewall_trace_data (
  run_id uuid PRIMARY KEY,
  config_id uuid NOT NULL REFERENCES hal.firewall_config,
  mode text NOT NULL DEFAULT 'hal' CHECK(mode='hal'),
  experiment text NOT NULL DEFAULT 'firewall' CHECK(experiment='firewall'),
  variant text NOT NULL DEFAULT 'trace' CHECK(variant='trace'),
  epoch bigint NOT NULL CHECK(epoch>=0),
  dictionary_version integer NOT NULL CHECK(dictionary_version>=1),
  boot_id bench.u64 NOT NULL,
  clock_id text NOT NULL,
  capacity integer NOT NULL CHECK(capacity>0),
  written integer NOT NULL CHECK(written>=0 AND written<=capacity),
  dropped bigint NOT NULL CHECK(dropped>=0),
  flags bigint NOT NULL CHECK(flags>=0),
  complete boolean NOT NULL,
  result_checked boolean NOT NULL,
  result_ok boolean,
  raw_artifact_id uuid NOT NULL REFERENCES bench.artifact,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL CHECK(jsonb_typeof(issues)='array'),
  FOREIGN KEY(run_id,mode,experiment,variant,config_id)
    REFERENCES bench.run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(run_id,clock_id) REFERENCES bench.clock,
  CHECK(NOT complete OR (dropped=0 AND flags=0)),
  CHECK(NOT result_checked OR result_ok IS NOT NULL),
  CHECK(NOT qualified OR (complete AND (NOT result_checked OR result_ok)))
);
CREATE TABLE hal.firewall_event (
  run_id uuid NOT NULL,
  mode text NOT NULL DEFAULT 'hal' CHECK(mode='hal'),
  experiment text NOT NULL DEFAULT 'firewall' CHECK(experiment='firewall'),
  source text NOT NULL CHECK(source IN ('dut','host','agent')),
  event_sequence bigint NOT NULL CHECK(event_sequence>=0),
  dictionary_version integer NOT NULL,
  event_id integer NOT NULL,
  clock_id text NOT NULL,
  timestamp_raw bench.u64 NOT NULL,
  kind text NOT NULL CHECK(kind IN ('begin','end','instant')),
  context_id integer NOT NULL CHECK(context_id BETWEEN 0 AND 255),
  probe_seq bigint CHECK(probe_seq>=1),
  span_id bigint CHECK(span_id>=0),
  parent_span_id bigint CHECK(parent_span_id>=0),
  arg0 bigint CHECK(arg0 BETWEEN 0 AND 4294967295),
  arg1 bigint CHECK(arg1 BETWEEN 0 AND 4294967295),
  FOREIGN KEY(run_id,mode,experiment) REFERENCES bench.run(run_id,mode,experiment),
  FOREIGN KEY(run_id,clock_id) REFERENCES bench.clock,
  FOREIGN KEY(experiment,dictionary_version,event_id) REFERENCES bench.event_definition,
  PRIMARY KEY(run_id,source,event_sequence)
);

CREATE TABLE hal.just_drop_config (
  config_id uuid PRIMARY KEY,
  condition_id uuid NOT NULL,
  experiment text NOT NULL DEFAULT 'just_drop' CHECK(experiment='just_drop'),
  instrumentation text NOT NULL CHECK(instrumentation IN ('counter_only','helper_hist','stage_hist','batch_hist','switch_hist','trace')),
  profile_hash text NOT NULL CHECK(profile_hash ~ '^[0-9a-f]{64}$'),
  calibration_ids uuid[] NOT NULL DEFAULT ARRAY[]::uuid[],
  parameters jsonb NOT NULL DEFAULT '{}',
  target_pps integer CHECK(target_pps>0),
  send_window_ns bench.u64,
  drain_ns bench.u64 NOT NULL,
  payload_bytes integer NOT NULL CHECK(payload_bytes>=32),
  workload_version text NOT NULL CHECK(workload_version='drop-minimal-v1'),
  neighbor_policy text NOT NULL,
  saturation_grid boolean NOT NULL,
  FOREIGN KEY(condition_id,experiment) REFERENCES bench.condition(condition_id,experiment),
  UNIQUE(config_id,condition_id)
);
CREATE TABLE hal.just_drop_metrics_data (
  run_id uuid NOT NULL,
  config_id uuid NOT NULL REFERENCES hal.just_drop_config,
  mode text NOT NULL DEFAULT 'hal' CHECK(mode='hal'),
  experiment text NOT NULL DEFAULT 'just_drop' CHECK(experiment='just_drop'),
  variant text NOT NULL DEFAULT 'metrics' CHECK(variant='metrics'),
  baseline_snapshot_id uuid,
  final_snapshot_id uuid,
  analysis_id uuid NOT NULL,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL CHECK(jsonb_typeof(issues)='array'),
  sent_success bench.u64, ingress_seen bench.u64,
  policy_enter bench.u64, normal_drop bench.u64,
  runtime_fault bench.u64, device_window_ticks bench.u64,
  sender_limited boolean NOT NULL,
  plateau_observed boolean NOT NULL,
  PRIMARY KEY(run_id,analysis_id),
  FOREIGN KEY(run_id,mode,experiment,variant,config_id)
    REFERENCES bench.run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(baseline_snapshot_id,run_id) REFERENCES hal.metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(final_snapshot_id,run_id) REFERENCES hal.metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(analysis_id,run_id) REFERENCES bench.analysis(analysis_id,run_id),
  CHECK(NOT qualified OR (baseline_snapshot_id IS NOT NULL AND final_snapshot_id IS NOT NULL))
);
CREATE TABLE hal.just_drop_trace_data (
  run_id uuid PRIMARY KEY,
  config_id uuid NOT NULL REFERENCES hal.just_drop_config,
  mode text NOT NULL DEFAULT 'hal' CHECK(mode='hal'),
  experiment text NOT NULL DEFAULT 'just_drop' CHECK(experiment='just_drop'),
  variant text NOT NULL DEFAULT 'trace' CHECK(variant='trace'),
  epoch bigint NOT NULL CHECK(epoch>=0),
  dictionary_version integer NOT NULL CHECK(dictionary_version>=1),
  boot_id bench.u64 NOT NULL,
  clock_id text NOT NULL,
  capacity integer NOT NULL CHECK(capacity>0),
  written integer NOT NULL CHECK(written>=0 AND written<=capacity),
  dropped bigint NOT NULL CHECK(dropped>=0),
  flags bigint NOT NULL CHECK(flags>=0),
  complete boolean NOT NULL,
  result_checked boolean NOT NULL,
  result_ok boolean,
  raw_artifact_id uuid NOT NULL REFERENCES bench.artifact,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL CHECK(jsonb_typeof(issues)='array'),
  FOREIGN KEY(run_id,mode,experiment,variant,config_id)
    REFERENCES bench.run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(run_id,clock_id) REFERENCES bench.clock,
  CHECK(NOT complete OR (dropped=0 AND flags=0)),
  CHECK(NOT result_checked OR result_ok IS NOT NULL),
  CHECK(NOT qualified OR (complete AND (NOT result_checked OR result_ok)))
);
CREATE TABLE hal.just_drop_event (
  run_id uuid NOT NULL,
  mode text NOT NULL DEFAULT 'hal' CHECK(mode='hal'),
  experiment text NOT NULL DEFAULT 'just_drop' CHECK(experiment='just_drop'),
  source text NOT NULL CHECK(source IN ('dut','host','agent')),
  event_sequence bigint NOT NULL CHECK(event_sequence>=0),
  dictionary_version integer NOT NULL,
  event_id integer NOT NULL,
  clock_id text NOT NULL,
  timestamp_raw bench.u64 NOT NULL,
  kind text NOT NULL CHECK(kind IN ('begin','end','instant')),
  context_id integer NOT NULL CHECK(context_id BETWEEN 0 AND 255),
  probe_seq bigint CHECK(probe_seq>=1),
  span_id bigint CHECK(span_id>=0),
  parent_span_id bigint CHECK(parent_span_id>=0),
  arg0 bigint CHECK(arg0 BETWEEN 0 AND 4294967295),
  arg1 bigint CHECK(arg1 BETWEEN 0 AND 4294967295),
  FOREIGN KEY(run_id,mode,experiment) REFERENCES bench.run(run_id,mode,experiment),
  FOREIGN KEY(run_id,clock_id) REFERENCES bench.clock,
  FOREIGN KEY(experiment,dictionary_version,event_id) REFERENCES bench.event_definition,
  PRIMARY KEY(run_id,source,event_sequence)
);

CREATE TABLE hal.just_tx_config (
  config_id uuid PRIMARY KEY,
  condition_id uuid NOT NULL,
  experiment text NOT NULL DEFAULT 'just_tx' CHECK(experiment='just_tx'),
  instrumentation text NOT NULL CHECK(instrumentation IN ('counter_only','helper_hist','stage_hist','batch_hist','switch_hist','trace')),
  profile_hash text NOT NULL CHECK(profile_hash ~ '^[0-9a-f]{64}$'),
  calibration_ids uuid[] NOT NULL DEFAULT ARRAY[]::uuid[],
  parameters jsonb NOT NULL DEFAULT '{}',
  target_pps integer CHECK(target_pps>0),
  send_window_ns bench.u64,
  drain_ns bench.u64 NOT NULL,
  payload_bytes integer NOT NULL CHECK(payload_bytes BETWEEN 32 AND 1472),
  probe_version text NOT NULL CHECK(probe_version='JTX1'),
  capture_interface text NOT NULL,
  FOREIGN KEY(condition_id,experiment) REFERENCES bench.condition(condition_id,experiment),
  UNIQUE(config_id,condition_id)
);
CREATE TABLE hal.just_tx_metrics_data (
  run_id uuid NOT NULL,
  config_id uuid NOT NULL REFERENCES hal.just_tx_config,
  mode text NOT NULL DEFAULT 'hal' CHECK(mode='hal'),
  experiment text NOT NULL DEFAULT 'just_tx' CHECK(experiment='just_tx'),
  variant text NOT NULL DEFAULT 'metrics' CHECK(variant='metrics'),
  baseline_snapshot_id uuid,
  final_snapshot_id uuid,
  analysis_id uuid NOT NULL,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL CHECK(jsonb_typeof(issues)='array'),
  sent_success bench.u64, returned bench.u64,
  duplicate_rx bench.u64, late_rx bench.u64,
  valid_rtt_count bench.u64,
  CHECK(returned IS NULL OR sent_success IS NULL OR returned<=sent_success),
  CHECK(valid_rtt_count IS NULL OR returned IS NULL OR valid_rtt_count<=returned),
  PRIMARY KEY(run_id,analysis_id),
  FOREIGN KEY(run_id,mode,experiment,variant,config_id)
    REFERENCES bench.run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(baseline_snapshot_id,run_id) REFERENCES hal.metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(final_snapshot_id,run_id) REFERENCES hal.metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(analysis_id,run_id) REFERENCES bench.analysis(analysis_id,run_id),
  CHECK(NOT qualified OR (baseline_snapshot_id IS NOT NULL AND final_snapshot_id IS NOT NULL))
);
CREATE TABLE hal.just_tx_trace_data (
  run_id uuid PRIMARY KEY,
  config_id uuid NOT NULL REFERENCES hal.just_tx_config,
  mode text NOT NULL DEFAULT 'hal' CHECK(mode='hal'),
  experiment text NOT NULL DEFAULT 'just_tx' CHECK(experiment='just_tx'),
  variant text NOT NULL DEFAULT 'trace' CHECK(variant='trace'),
  epoch bigint NOT NULL CHECK(epoch>=0),
  dictionary_version integer NOT NULL CHECK(dictionary_version>=1),
  boot_id bench.u64 NOT NULL,
  clock_id text NOT NULL,
  capacity integer NOT NULL CHECK(capacity>0),
  written integer NOT NULL CHECK(written>=0 AND written<=capacity),
  dropped bigint NOT NULL CHECK(dropped>=0),
  flags bigint NOT NULL CHECK(flags>=0),
  complete boolean NOT NULL,
  result_checked boolean NOT NULL,
  result_ok boolean,
  raw_artifact_id uuid NOT NULL REFERENCES bench.artifact,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL CHECK(jsonb_typeof(issues)='array'),
  FOREIGN KEY(run_id,mode,experiment,variant,config_id)
    REFERENCES bench.run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(run_id,clock_id) REFERENCES bench.clock,
  CHECK(NOT complete OR (dropped=0 AND flags=0)),
  CHECK(NOT result_checked OR result_ok IS NOT NULL),
  CHECK(NOT qualified OR (complete AND (NOT result_checked OR result_ok)))
);
CREATE TABLE hal.just_tx_event (
  run_id uuid NOT NULL,
  mode text NOT NULL DEFAULT 'hal' CHECK(mode='hal'),
  experiment text NOT NULL DEFAULT 'just_tx' CHECK(experiment='just_tx'),
  source text NOT NULL CHECK(source IN ('dut','host','agent')),
  event_sequence bigint NOT NULL CHECK(event_sequence>=0),
  dictionary_version integer NOT NULL,
  event_id integer NOT NULL,
  clock_id text NOT NULL,
  timestamp_raw bench.u64 NOT NULL,
  kind text NOT NULL CHECK(kind IN ('begin','end','instant')),
  context_id integer NOT NULL CHECK(context_id BETWEEN 0 AND 255),
  probe_seq bigint CHECK(probe_seq>=1),
  span_id bigint CHECK(span_id>=0),
  parent_span_id bigint CHECK(parent_span_id>=0),
  arg0 bigint CHECK(arg0 BETWEEN 0 AND 4294967295),
  arg1 bigint CHECK(arg1 BETWEEN 0 AND 4294967295),
  FOREIGN KEY(run_id,mode,experiment) REFERENCES bench.run(run_id,mode,experiment),
  FOREIGN KEY(run_id,clock_id) REFERENCES bench.clock,
  FOREIGN KEY(experiment,dictionary_version,event_id) REFERENCES bench.event_definition,
  PRIMARY KEY(run_id,source,event_sequence)
);

CREATE TABLE hal.exec_bench_config (
  config_id uuid PRIMARY KEY,
  condition_id uuid NOT NULL,
  experiment text NOT NULL DEFAULT 'exec_bench' CHECK(experiment='exec_bench'),
  instrumentation text NOT NULL CHECK(instrumentation IN ('counter_only','helper_hist','stage_hist','batch_hist','switch_hist','trace')),
  profile_hash text NOT NULL CHECK(profile_hash ~ '^[0-9a-f]{64}$'),
  calibration_ids uuid[] NOT NULL DEFAULT ARRAY[]::uuid[],
  parameters jsonb NOT NULL DEFAULT '{}',
  workload text NOT NULL CHECK(workload IN ('incr','square','bitswap','fib','buf','memcpy','fletcher32','bsort')),
  work_param integer NOT NULL CHECK(work_param>=0),
  batch_count bigint NOT NULL CHECK(batch_count BETWEEN 1 AND 4294967295),
  samples_per_run integer NOT NULL CHECK(samples_per_run>0),
  input_bytes integer NOT NULL CHECK(input_bytes=88),
  input_protocol text NOT NULL CHECK(input_protocol='EXB2'),
  wifi_enabled boolean NOT NULL CHECK(NOT wifi_enabled),
  step_budget bench.u64,
  fixture_artifact_id uuid NOT NULL REFERENCES bench.artifact,
  FOREIGN KEY(condition_id,experiment) REFERENCES bench.condition(condition_id,experiment),
  UNIQUE(config_id,condition_id)
);
CREATE TABLE hal.exec_bench_metrics_data (
  run_id uuid NOT NULL,
  config_id uuid NOT NULL REFERENCES hal.exec_bench_config,
  mode text NOT NULL DEFAULT 'hal' CHECK(mode='hal'),
  experiment text NOT NULL DEFAULT 'exec_bench' CHECK(experiment='exec_bench'),
  variant text NOT NULL DEFAULT 'metrics' CHECK(variant='metrics'),
  baseline_snapshot_id uuid,
  final_snapshot_id uuid,
  analysis_id uuid NOT NULL,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL CHECK(jsonb_typeof(issues)='array'),
  batches_expected integer NOT NULL CHECK(batches_expected>0),
  batches_completed integer NOT NULL CHECK(batches_completed>=0),
  valid_batches integer NOT NULL CHECK(valid_batches>=0),
  CHECK(valid_batches<=batches_completed AND batches_completed<=batches_expected),
  PRIMARY KEY(run_id,analysis_id),
  FOREIGN KEY(run_id,mode,experiment,variant,config_id)
    REFERENCES bench.run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(baseline_snapshot_id,run_id) REFERENCES hal.metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(final_snapshot_id,run_id) REFERENCES hal.metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(analysis_id,run_id) REFERENCES bench.analysis(analysis_id,run_id),
  CHECK(NOT qualified OR (baseline_snapshot_id IS NOT NULL AND final_snapshot_id IS NOT NULL))
);
CREATE TABLE hal.exec_bench_trace_data (
  run_id uuid PRIMARY KEY,
  config_id uuid NOT NULL REFERENCES hal.exec_bench_config,
  mode text NOT NULL DEFAULT 'hal' CHECK(mode='hal'),
  experiment text NOT NULL DEFAULT 'exec_bench' CHECK(experiment='exec_bench'),
  variant text NOT NULL DEFAULT 'trace' CHECK(variant='trace'),
  epoch bigint NOT NULL CHECK(epoch>=0),
  dictionary_version integer NOT NULL CHECK(dictionary_version>=1),
  boot_id bench.u64 NOT NULL,
  clock_id text NOT NULL,
  capacity integer NOT NULL CHECK(capacity>0),
  written integer NOT NULL CHECK(written>=0 AND written<=capacity),
  dropped bigint NOT NULL CHECK(dropped>=0),
  flags bigint NOT NULL CHECK(flags>=0),
  complete boolean NOT NULL,
  result_checked boolean NOT NULL,
  result_ok boolean,
  raw_artifact_id uuid NOT NULL REFERENCES bench.artifact,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL CHECK(jsonb_typeof(issues)='array'),
  FOREIGN KEY(run_id,mode,experiment,variant,config_id)
    REFERENCES bench.run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(run_id,clock_id) REFERENCES bench.clock,
  CHECK(NOT complete OR (dropped=0 AND flags=0)),
  CHECK(NOT result_checked OR result_ok IS NOT NULL),
  CHECK(NOT qualified OR (complete AND (NOT result_checked OR result_ok)))
);
CREATE TABLE hal.exec_bench_event (
  run_id uuid NOT NULL,
  mode text NOT NULL DEFAULT 'hal' CHECK(mode='hal'),
  experiment text NOT NULL DEFAULT 'exec_bench' CHECK(experiment='exec_bench'),
  source text NOT NULL CHECK(source IN ('dut','host','agent')),
  event_sequence bigint NOT NULL CHECK(event_sequence>=0),
  dictionary_version integer NOT NULL,
  event_id integer NOT NULL,
  clock_id text NOT NULL,
  timestamp_raw bench.u64 NOT NULL,
  kind text NOT NULL CHECK(kind IN ('begin','end','instant')),
  context_id integer NOT NULL CHECK(context_id BETWEEN 0 AND 255),
  probe_seq bigint CHECK(probe_seq>=1),
  span_id bigint CHECK(span_id>=0),
  parent_span_id bigint CHECK(parent_span_id>=0),
  arg0 bigint CHECK(arg0 BETWEEN 0 AND 4294967295),
  arg1 bigint CHECK(arg1 BETWEEN 0 AND 4294967295),
  FOREIGN KEY(run_id,mode,experiment) REFERENCES bench.run(run_id,mode,experiment),
  FOREIGN KEY(run_id,clock_id) REFERENCES bench.clock,
  FOREIGN KEY(experiment,dictionary_version,event_id) REFERENCES bench.event_definition,
  PRIMARY KEY(run_id,source,event_sequence)
);

CREATE TABLE hal.case_config (
  config_id uuid PRIMARY KEY,
  condition_id uuid NOT NULL,
  experiment text NOT NULL DEFAULT 'case' CHECK(experiment='case'),
  instrumentation text NOT NULL CHECK(instrumentation IN ('counter_only','helper_hist','stage_hist','batch_hist','switch_hist','trace')),
  profile_hash text NOT NULL CHECK(profile_hash ~ '^[0-9a-f]{64}$'),
  calibration_ids uuid[] NOT NULL DEFAULT ARRAY[]::uuid[],
  parameters jsonb NOT NULL DEFAULT '{}',
  target_pps integer NOT NULL CHECK(target_pps>0),
  switch_count integer NOT NULL CHECK(switch_count>0),
  switch_kind text NOT NULL CHECK(switch_kind IN ('hot_publish','native_policy_select','stop_copy_start','cold_install')),
  quiesce_policy text NOT NULL CHECK(quiesce_policy IN ('drop','pass','native')),
  pre_window_ns bench.u64 NOT NULL,
  post_window_ns bench.u64 NOT NULL,
  payload_bytes integer NOT NULL CHECK(payload_bytes=40),
  source_mode text NOT NULL,
  target_mode text NOT NULL,
  FOREIGN KEY(condition_id,experiment) REFERENCES bench.condition(condition_id,experiment),
  UNIQUE(config_id,condition_id)
);
CREATE TABLE hal.case_metrics_data (
  run_id uuid NOT NULL,
  config_id uuid NOT NULL REFERENCES hal.case_config,
  mode text NOT NULL DEFAULT 'hal' CHECK(mode='hal'),
  experiment text NOT NULL DEFAULT 'case' CHECK(experiment='case'),
  variant text NOT NULL DEFAULT 'metrics' CHECK(variant='metrics'),
  baseline_snapshot_id uuid,
  final_snapshot_id uuid,
  analysis_id uuid NOT NULL,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL CHECK(jsonb_typeof(issues)='array'),
  attempted integer NOT NULL CHECK(attempted>=0),
  successful integer NOT NULL CHECK(successful>=0 AND successful<=attempted),
  boot_unchanged boolean NOT NULL,
  old_after_new bench.u64,
  PRIMARY KEY(run_id,analysis_id),
  FOREIGN KEY(run_id,mode,experiment,variant,config_id)
    REFERENCES bench.run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(baseline_snapshot_id,run_id) REFERENCES hal.metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(final_snapshot_id,run_id) REFERENCES hal.metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(analysis_id,run_id) REFERENCES bench.analysis(analysis_id,run_id),
  CHECK(NOT qualified OR (baseline_snapshot_id IS NOT NULL AND final_snapshot_id IS NOT NULL))
);
CREATE TABLE hal.case_trace_data (
  run_id uuid PRIMARY KEY,
  config_id uuid NOT NULL REFERENCES hal.case_config,
  mode text NOT NULL DEFAULT 'hal' CHECK(mode='hal'),
  experiment text NOT NULL DEFAULT 'case' CHECK(experiment='case'),
  variant text NOT NULL DEFAULT 'trace' CHECK(variant='trace'),
  epoch bigint NOT NULL CHECK(epoch>=0),
  dictionary_version integer NOT NULL CHECK(dictionary_version>=1),
  boot_id bench.u64 NOT NULL,
  clock_id text NOT NULL,
  capacity integer NOT NULL CHECK(capacity>0),
  written integer NOT NULL CHECK(written>=0 AND written<=capacity),
  dropped bigint NOT NULL CHECK(dropped>=0),
  flags bigint NOT NULL CHECK(flags>=0),
  complete boolean NOT NULL,
  result_checked boolean NOT NULL,
  result_ok boolean,
  raw_artifact_id uuid NOT NULL REFERENCES bench.artifact,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL CHECK(jsonb_typeof(issues)='array'),
  FOREIGN KEY(run_id,mode,experiment,variant,config_id)
    REFERENCES bench.run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(run_id,clock_id) REFERENCES bench.clock,
  CHECK(NOT complete OR (dropped=0 AND flags=0)),
  CHECK(NOT result_checked OR result_ok IS NOT NULL),
  CHECK(NOT qualified OR (complete AND (NOT result_checked OR result_ok)))
);
CREATE TABLE hal.case_event (
  run_id uuid NOT NULL,
  mode text NOT NULL DEFAULT 'hal' CHECK(mode='hal'),
  experiment text NOT NULL DEFAULT 'case' CHECK(experiment='case'),
  source text NOT NULL CHECK(source IN ('dut','host','agent')),
  event_sequence bigint NOT NULL CHECK(event_sequence>=0),
  dictionary_version integer NOT NULL,
  event_id integer NOT NULL,
  clock_id text NOT NULL,
  timestamp_raw bench.u64 NOT NULL,
  kind text NOT NULL CHECK(kind IN ('begin','end','instant')),
  context_id integer NOT NULL CHECK(context_id BETWEEN 0 AND 255),
  probe_seq bigint CHECK(probe_seq>=1),
  span_id bigint CHECK(span_id>=0),
  parent_span_id bigint CHECK(parent_span_id>=0),
  arg0 bigint CHECK(arg0 BETWEEN 0 AND 4294967295),
  arg1 bigint CHECK(arg1 BETWEEN 0 AND 4294967295),
  FOREIGN KEY(run_id,mode,experiment) REFERENCES bench.run(run_id,mode,experiment),
  FOREIGN KEY(run_id,clock_id) REFERENCES bench.clock,
  FOREIGN KEY(experiment,dictionary_version,event_id) REFERENCES bench.event_definition,
  PRIMARY KEY(run_id,source,event_sequence)
);

CREATE TABLE hal.probe_result (
  analysis_id uuid NOT NULL,
  run_id uuid NOT NULL,
  mode text NOT NULL DEFAULT 'hal' CHECK(mode='hal'),
  seq bigint NOT NULL,
  returned boolean NOT NULL,
  duplicate_rx bigint NOT NULL CHECK(duplicate_rx>=0),
  late_rx bigint NOT NULL CHECK(late_rx>=0),
  tx_capture_id uuid,
  tx_record_index bigint,
  rx_capture_id uuid,
  rx_record_index bigint,
  rtt_ns bench.u64,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL,
  FOREIGN KEY(run_id,mode) REFERENCES bench.run(run_id,mode),
  FOREIGN KEY(analysis_id,run_id) REFERENCES bench.analysis(analysis_id,run_id),
  FOREIGN KEY(run_id,seq) REFERENCES bench.send_attempt,
  FOREIGN KEY(tx_capture_id,run_id) REFERENCES bench.capture(capture_id,run_id),
  FOREIGN KEY(rx_capture_id,run_id) REFERENCES bench.capture(capture_id,run_id),
  FOREIGN KEY(tx_capture_id,tx_record_index) REFERENCES bench.packet_fact,
  FOREIGN KEY(rx_capture_id,rx_record_index) REFERENCES bench.packet_fact,
  PRIMARY KEY(analysis_id,seq),
  CHECK((tx_capture_id IS NULL)=(tx_record_index IS NULL)),
  CHECK((rx_capture_id IS NULL)=(rx_record_index IS NULL)),
  CHECK(rtt_ns IS NULL OR (returned AND qualified AND tx_capture_id IS NOT NULL AND rx_capture_id IS NOT NULL))
);
CREATE TABLE hal.firewall_token (
  run_id uuid NOT NULL,
  mode text NOT NULL DEFAULT 'hal' CHECK(mode='hal'),
  experiment text NOT NULL DEFAULT 'firewall' CHECK(experiment='firewall'),
  seq bigint NOT NULL,
  sent boolean NOT NULL,
  observed boolean NOT NULL,
  received_bytes bytea,
  FOREIGN KEY(run_id,mode,experiment) REFERENCES bench.run(run_id,mode,experiment),
  FOREIGN KEY(run_id,seq) REFERENCES bench.send_attempt,
  PRIMARY KEY(run_id,seq)
);
CREATE TABLE hal.exec_batch (
  run_id uuid NOT NULL,
  mode text NOT NULL DEFAULT 'hal' CHECK(mode='hal'),
  experiment text NOT NULL DEFAULT 'exec_bench' CHECK(experiment='exec_bench'),
  sample_index integer NOT NULL CHECK(sample_index>=0),
  clock_id text NOT NULL,
  begin_tick bench.u64 NOT NULL,
  end_tick bench.u64 NOT NULL,
  batch_count bigint NOT NULL CHECK(batch_count>0),
  completed_iterations bigint NOT NULL CHECK(completed_iterations>=0),
  seed bench.u64 NOT NULL,
  input_bytes bytea NOT NULL,
  expected_bytes bytea NOT NULL,
  actual_bytes bytea NOT NULL,
  result_ok boolean NOT NULL,
  qualified boolean NOT NULL,
  FOREIGN KEY(run_id,mode,experiment) REFERENCES bench.run(run_id,mode,experiment),
  FOREIGN KEY(run_id,clock_id) REFERENCES bench.clock,
  PRIMARY KEY(run_id,sample_index),
  CHECK(end_tick>=begin_tick),
  CHECK(NOT qualified OR (result_ok AND completed_iterations=batch_count))
);
CREATE TABLE hal.case_switch (
  run_id uuid NOT NULL,
  mode text NOT NULL DEFAULT 'hal' CHECK(mode='hal'),
  experiment text NOT NULL DEFAULT 'case' CHECK(experiment='case'),
  switch_index integer NOT NULL CHECK(switch_index>=1),
  switch_kind text NOT NULL CHECK(switch_kind IN ('hot_publish','native_policy_select','stop_copy_start','cold_install')),
  source_version integer NOT NULL CHECK(source_version IN (1,2)),
  target_version integer NOT NULL CHECK(target_version IN (1,2)),
  source_slot integer,
  target_slot integer,
  source_generation bigint,
  target_generation bigint,
  host_clock_id text NOT NULL,
  host_command_begin_ns bench.u64 NOT NULL,
  host_reply_ns bench.u64 NOT NULL,
  dut_clock_id text,
  dut_begin_tick bench.u64,
  dut_end_tick bench.u64,
  quiesce_ticks bench.u64,
  rebind_ticks bench.u64,
  publish_ticks bench.u64,
  last_old_capture_id uuid,
  last_old_record_index bigint,
  first_new_capture_id uuid,
  first_new_record_index bigint,
  reply_transition_gap_ns bench.u64,
  old_after_new bigint CHECK(old_after_new>=0),
  command_ok boolean NOT NULL,
  error_code text,
  boot_unchanged boolean NOT NULL,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL,
  FOREIGN KEY(run_id,mode,experiment) REFERENCES bench.run(run_id,mode,experiment),
  FOREIGN KEY(run_id,host_clock_id) REFERENCES bench.clock,
  FOREIGN KEY(run_id,dut_clock_id) REFERENCES bench.clock,
  FOREIGN KEY(last_old_capture_id,run_id) REFERENCES bench.capture(capture_id,run_id),
  FOREIGN KEY(first_new_capture_id,run_id) REFERENCES bench.capture(capture_id,run_id),
  FOREIGN KEY(last_old_capture_id,last_old_record_index) REFERENCES bench.packet_fact,
  FOREIGN KEY(first_new_capture_id,first_new_record_index) REFERENCES bench.packet_fact,
  PRIMARY KEY(run_id,switch_index),
  CHECK(source_version<>target_version),
  CHECK(host_reply_ns>=host_command_begin_ns),
  CHECK(dut_end_tick IS NULL OR dut_begin_tick IS NULL OR dut_end_tick>=dut_begin_tick),
  CHECK((last_old_capture_id IS NULL)=(last_old_record_index IS NULL)),
  CHECK((first_new_capture_id IS NULL)=(first_new_record_index IS NULL)),
  CHECK(reply_transition_gap_ns IS NULL OR (last_old_capture_id IS NOT NULL AND first_new_capture_id IS NOT NULL)),
  CHECK(NOT qualified OR (command_ok AND boot_unchanged))
);
ALTER TABLE hal.case_config ADD CHECK(switch_kind='native_policy_select' AND quiesce_policy='native');
ALTER TABLE hal.case_switch ADD CHECK(switch_kind='native_policy_select' AND source_slot IS NULL AND target_slot IS NULL AND rebind_ticks IS NULL);

-- Mode schema: aot. All mode checks are literal, not caller conventions.
CREATE SCHEMA IF NOT EXISTS aot;
CREATE TABLE aot.metric_definition (
  metric_id uuid PRIMARY KEY,
  name text NOT NULL,
  kind text NOT NULL CHECK(kind IN ('counter','gauge','histogram')),
  unit text NOT NULL,
  boundary text NOT NULL,
  scope text NOT NULL,
  definition_version text NOT NULL,
  UNIQUE(metric_id,kind), UNIQUE(name,definition_version)
);
CREATE TABLE aot.metric_series (
  series_id uuid PRIMARY KEY,
  metric_id uuid NOT NULL,
  kind text NOT NULL,
  labels jsonb NOT NULL CHECK(jsonb_typeof(labels)='object'),
  FOREIGN KEY(metric_id,kind) REFERENCES aot.metric_definition(metric_id,kind),
  UNIQUE(series_id,kind), UNIQUE(metric_id,labels)
);
CREATE TABLE aot.metric_snapshot (
  snapshot_id uuid PRIMARY KEY,
  run_id uuid NOT NULL,
  mode text NOT NULL DEFAULT 'aot' CHECK(mode='aot'),
  phase text NOT NULL CHECK(phase IN ('baseline','final')),
  epoch bigint NOT NULL CHECK(epoch>=0),
  boot_id bench.u64 NOT NULL,
  clock_id text NOT NULL,
  at_tick bench.u64 NOT NULL,
  profile_hash text NOT NULL CHECK(profile_hash ~ '^[0-9a-f]{64}$'),
  raw_artifact_id uuid NOT NULL REFERENCES bench.artifact,
  FOREIGN KEY(run_id,mode) REFERENCES bench.run(run_id,mode),
  FOREIGN KEY(run_id,clock_id) REFERENCES bench.clock,
  UNIQUE(snapshot_id,run_id), UNIQUE(run_id,epoch,phase)
);
CREATE TABLE aot.scalar_sample (
  snapshot_id uuid NOT NULL REFERENCES aot.metric_snapshot,
  series_id uuid NOT NULL,
  kind text NOT NULL CHECK(kind IN ('counter','gauge')),
  value numeric NOT NULL,
  FOREIGN KEY(series_id,kind) REFERENCES aot.metric_series(series_id,kind),
  PRIMARY KEY(snapshot_id,series_id)
);
CREATE TABLE aot.histogram_sample (
  snapshot_id uuid NOT NULL REFERENCES aot.metric_snapshot,
  series_id uuid NOT NULL,
  kind text NOT NULL DEFAULT 'histogram' CHECK(kind='histogram'),
  calibration_id uuid NOT NULL REFERENCES bench.calibration,
  sample_count bench.u64 NOT NULL,
  sample_sum bench.u64 NOT NULL,
  FOREIGN KEY(series_id,kind) REFERENCES aot.metric_series(series_id,kind),
  PRIMARY KEY(snapshot_id,series_id),
  UNIQUE(snapshot_id,series_id,sample_count)
);
CREATE TABLE aot.histogram_bucket (
  snapshot_id uuid NOT NULL,
  series_id uuid NOT NULL,
  ordinal integer NOT NULL CHECK(ordinal>=1),
  upper_bound bigint CHECK(upper_bound>=0), -- NULL represents +Inf.
  cumulative_count bench.u64 NOT NULL,
  sample_count bench.u64 NOT NULL,
  FOREIGN KEY(snapshot_id,series_id,sample_count)
    REFERENCES aot.histogram_sample(snapshot_id,series_id,sample_count),
  PRIMARY KEY(snapshot_id,series_id,ordinal),
  CHECK(cumulative_count<=sample_count),
  CHECK(upper_bound IS NOT NULL OR cumulative_count=sample_count)
);
CREATE UNIQUE INDEX aot_hist_one_inf ON aot.histogram_bucket(snapshot_id,series_id)
  WHERE upper_bound IS NULL;

CREATE TABLE aot.firewall_config (
  config_id uuid PRIMARY KEY,
  condition_id uuid NOT NULL,
  experiment text NOT NULL DEFAULT 'firewall' CHECK(experiment='firewall'),
  instrumentation text NOT NULL CHECK(instrumentation IN ('counter_only','helper_hist','stage_hist','batch_hist','switch_hist','trace')),
  profile_hash text NOT NULL CHECK(profile_hash ~ '^[0-9a-f]{64}$'),
  calibration_ids uuid[] NOT NULL DEFAULT ARRAY[]::uuid[],
  parameters jsonb NOT NULL DEFAULT '{}',
  target_pps integer CHECK(target_pps>0),
  send_window_ns bench.u64,
  drain_ns bench.u64 NOT NULL,
  payload_bytes integer NOT NULL CHECK(payload_bytes>=32),
  blacklist_capacity integer NOT NULL CHECK(blacklist_capacity=256),
  blacklist_entries integer NOT NULL CHECK(blacklist_entries BETWEEN 1 AND 256),
  legal_numerator integer NOT NULL CHECK(legal_numerator BETWEEN 0 AND 10),
  legal_denominator integer NOT NULL CHECK(legal_denominator=10),
  lookup_case text NOT NULL CHECK(lookup_case IN ('mixed','hit','miss')),
  map_fixture_artifact_id uuid NOT NULL REFERENCES bench.artifact,
  FOREIGN KEY(condition_id,experiment) REFERENCES bench.condition(condition_id,experiment),
  UNIQUE(config_id,condition_id)
);
CREATE TABLE aot.firewall_metrics_data (
  run_id uuid NOT NULL,
  config_id uuid NOT NULL REFERENCES aot.firewall_config,
  mode text NOT NULL DEFAULT 'aot' CHECK(mode='aot'),
  experiment text NOT NULL DEFAULT 'firewall' CHECK(experiment='firewall'),
  variant text NOT NULL DEFAULT 'metrics' CHECK(variant='metrics'),
  baseline_snapshot_id uuid,
  final_snapshot_id uuid,
  analysis_id uuid NOT NULL,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL CHECK(jsonb_typeof(issues)='array'),
  sent_legal bench.u64, sent_illegal bench.u64,
  matched_legal bench.u64, blocked_hit_delta bench.u64,
  token_refused bench.u64,
  CHECK(matched_legal IS NULL OR sent_legal IS NULL OR matched_legal<=sent_legal),
  PRIMARY KEY(run_id,analysis_id),
  FOREIGN KEY(run_id,mode,experiment,variant,config_id)
    REFERENCES bench.run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(baseline_snapshot_id,run_id) REFERENCES aot.metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(final_snapshot_id,run_id) REFERENCES aot.metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(analysis_id,run_id) REFERENCES bench.analysis(analysis_id,run_id),
  CHECK(NOT qualified OR (baseline_snapshot_id IS NOT NULL AND final_snapshot_id IS NOT NULL))
);
CREATE TABLE aot.firewall_trace_data (
  run_id uuid PRIMARY KEY,
  config_id uuid NOT NULL REFERENCES aot.firewall_config,
  mode text NOT NULL DEFAULT 'aot' CHECK(mode='aot'),
  experiment text NOT NULL DEFAULT 'firewall' CHECK(experiment='firewall'),
  variant text NOT NULL DEFAULT 'trace' CHECK(variant='trace'),
  epoch bigint NOT NULL CHECK(epoch>=0),
  dictionary_version integer NOT NULL CHECK(dictionary_version>=1),
  boot_id bench.u64 NOT NULL,
  clock_id text NOT NULL,
  capacity integer NOT NULL CHECK(capacity>0),
  written integer NOT NULL CHECK(written>=0 AND written<=capacity),
  dropped bigint NOT NULL CHECK(dropped>=0),
  flags bigint NOT NULL CHECK(flags>=0),
  complete boolean NOT NULL,
  result_checked boolean NOT NULL,
  result_ok boolean,
  raw_artifact_id uuid NOT NULL REFERENCES bench.artifact,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL CHECK(jsonb_typeof(issues)='array'),
  FOREIGN KEY(run_id,mode,experiment,variant,config_id)
    REFERENCES bench.run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(run_id,clock_id) REFERENCES bench.clock,
  CHECK(NOT complete OR (dropped=0 AND flags=0)),
  CHECK(NOT result_checked OR result_ok IS NOT NULL),
  CHECK(NOT qualified OR (complete AND (NOT result_checked OR result_ok)))
);
CREATE TABLE aot.firewall_event (
  run_id uuid NOT NULL,
  mode text NOT NULL DEFAULT 'aot' CHECK(mode='aot'),
  experiment text NOT NULL DEFAULT 'firewall' CHECK(experiment='firewall'),
  source text NOT NULL CHECK(source IN ('dut','host','agent')),
  event_sequence bigint NOT NULL CHECK(event_sequence>=0),
  dictionary_version integer NOT NULL,
  event_id integer NOT NULL,
  clock_id text NOT NULL,
  timestamp_raw bench.u64 NOT NULL,
  kind text NOT NULL CHECK(kind IN ('begin','end','instant')),
  context_id integer NOT NULL CHECK(context_id BETWEEN 0 AND 255),
  probe_seq bigint CHECK(probe_seq>=1),
  span_id bigint CHECK(span_id>=0),
  parent_span_id bigint CHECK(parent_span_id>=0),
  arg0 bigint CHECK(arg0 BETWEEN 0 AND 4294967295),
  arg1 bigint CHECK(arg1 BETWEEN 0 AND 4294967295),
  FOREIGN KEY(run_id,mode,experiment) REFERENCES bench.run(run_id,mode,experiment),
  FOREIGN KEY(run_id,clock_id) REFERENCES bench.clock,
  FOREIGN KEY(experiment,dictionary_version,event_id) REFERENCES bench.event_definition,
  PRIMARY KEY(run_id,source,event_sequence)
);

CREATE TABLE aot.just_drop_config (
  config_id uuid PRIMARY KEY,
  condition_id uuid NOT NULL,
  experiment text NOT NULL DEFAULT 'just_drop' CHECK(experiment='just_drop'),
  instrumentation text NOT NULL CHECK(instrumentation IN ('counter_only','helper_hist','stage_hist','batch_hist','switch_hist','trace')),
  profile_hash text NOT NULL CHECK(profile_hash ~ '^[0-9a-f]{64}$'),
  calibration_ids uuid[] NOT NULL DEFAULT ARRAY[]::uuid[],
  parameters jsonb NOT NULL DEFAULT '{}',
  target_pps integer CHECK(target_pps>0),
  send_window_ns bench.u64,
  drain_ns bench.u64 NOT NULL,
  payload_bytes integer NOT NULL CHECK(payload_bytes>=32),
  workload_version text NOT NULL CHECK(workload_version='drop-minimal-v1'),
  neighbor_policy text NOT NULL,
  saturation_grid boolean NOT NULL,
  FOREIGN KEY(condition_id,experiment) REFERENCES bench.condition(condition_id,experiment),
  UNIQUE(config_id,condition_id)
);
CREATE TABLE aot.just_drop_metrics_data (
  run_id uuid NOT NULL,
  config_id uuid NOT NULL REFERENCES aot.just_drop_config,
  mode text NOT NULL DEFAULT 'aot' CHECK(mode='aot'),
  experiment text NOT NULL DEFAULT 'just_drop' CHECK(experiment='just_drop'),
  variant text NOT NULL DEFAULT 'metrics' CHECK(variant='metrics'),
  baseline_snapshot_id uuid,
  final_snapshot_id uuid,
  analysis_id uuid NOT NULL,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL CHECK(jsonb_typeof(issues)='array'),
  sent_success bench.u64, ingress_seen bench.u64,
  policy_enter bench.u64, normal_drop bench.u64,
  runtime_fault bench.u64, device_window_ticks bench.u64,
  sender_limited boolean NOT NULL,
  plateau_observed boolean NOT NULL,
  PRIMARY KEY(run_id,analysis_id),
  FOREIGN KEY(run_id,mode,experiment,variant,config_id)
    REFERENCES bench.run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(baseline_snapshot_id,run_id) REFERENCES aot.metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(final_snapshot_id,run_id) REFERENCES aot.metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(analysis_id,run_id) REFERENCES bench.analysis(analysis_id,run_id),
  CHECK(NOT qualified OR (baseline_snapshot_id IS NOT NULL AND final_snapshot_id IS NOT NULL))
);
CREATE TABLE aot.just_drop_trace_data (
  run_id uuid PRIMARY KEY,
  config_id uuid NOT NULL REFERENCES aot.just_drop_config,
  mode text NOT NULL DEFAULT 'aot' CHECK(mode='aot'),
  experiment text NOT NULL DEFAULT 'just_drop' CHECK(experiment='just_drop'),
  variant text NOT NULL DEFAULT 'trace' CHECK(variant='trace'),
  epoch bigint NOT NULL CHECK(epoch>=0),
  dictionary_version integer NOT NULL CHECK(dictionary_version>=1),
  boot_id bench.u64 NOT NULL,
  clock_id text NOT NULL,
  capacity integer NOT NULL CHECK(capacity>0),
  written integer NOT NULL CHECK(written>=0 AND written<=capacity),
  dropped bigint NOT NULL CHECK(dropped>=0),
  flags bigint NOT NULL CHECK(flags>=0),
  complete boolean NOT NULL,
  result_checked boolean NOT NULL,
  result_ok boolean,
  raw_artifact_id uuid NOT NULL REFERENCES bench.artifact,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL CHECK(jsonb_typeof(issues)='array'),
  FOREIGN KEY(run_id,mode,experiment,variant,config_id)
    REFERENCES bench.run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(run_id,clock_id) REFERENCES bench.clock,
  CHECK(NOT complete OR (dropped=0 AND flags=0)),
  CHECK(NOT result_checked OR result_ok IS NOT NULL),
  CHECK(NOT qualified OR (complete AND (NOT result_checked OR result_ok)))
);
CREATE TABLE aot.just_drop_event (
  run_id uuid NOT NULL,
  mode text NOT NULL DEFAULT 'aot' CHECK(mode='aot'),
  experiment text NOT NULL DEFAULT 'just_drop' CHECK(experiment='just_drop'),
  source text NOT NULL CHECK(source IN ('dut','host','agent')),
  event_sequence bigint NOT NULL CHECK(event_sequence>=0),
  dictionary_version integer NOT NULL,
  event_id integer NOT NULL,
  clock_id text NOT NULL,
  timestamp_raw bench.u64 NOT NULL,
  kind text NOT NULL CHECK(kind IN ('begin','end','instant')),
  context_id integer NOT NULL CHECK(context_id BETWEEN 0 AND 255),
  probe_seq bigint CHECK(probe_seq>=1),
  span_id bigint CHECK(span_id>=0),
  parent_span_id bigint CHECK(parent_span_id>=0),
  arg0 bigint CHECK(arg0 BETWEEN 0 AND 4294967295),
  arg1 bigint CHECK(arg1 BETWEEN 0 AND 4294967295),
  FOREIGN KEY(run_id,mode,experiment) REFERENCES bench.run(run_id,mode,experiment),
  FOREIGN KEY(run_id,clock_id) REFERENCES bench.clock,
  FOREIGN KEY(experiment,dictionary_version,event_id) REFERENCES bench.event_definition,
  PRIMARY KEY(run_id,source,event_sequence)
);

CREATE TABLE aot.just_tx_config (
  config_id uuid PRIMARY KEY,
  condition_id uuid NOT NULL,
  experiment text NOT NULL DEFAULT 'just_tx' CHECK(experiment='just_tx'),
  instrumentation text NOT NULL CHECK(instrumentation IN ('counter_only','helper_hist','stage_hist','batch_hist','switch_hist','trace')),
  profile_hash text NOT NULL CHECK(profile_hash ~ '^[0-9a-f]{64}$'),
  calibration_ids uuid[] NOT NULL DEFAULT ARRAY[]::uuid[],
  parameters jsonb NOT NULL DEFAULT '{}',
  target_pps integer CHECK(target_pps>0),
  send_window_ns bench.u64,
  drain_ns bench.u64 NOT NULL,
  payload_bytes integer NOT NULL CHECK(payload_bytes BETWEEN 32 AND 1472),
  probe_version text NOT NULL CHECK(probe_version='JTX1'),
  capture_interface text NOT NULL,
  FOREIGN KEY(condition_id,experiment) REFERENCES bench.condition(condition_id,experiment),
  UNIQUE(config_id,condition_id)
);
CREATE TABLE aot.just_tx_metrics_data (
  run_id uuid NOT NULL,
  config_id uuid NOT NULL REFERENCES aot.just_tx_config,
  mode text NOT NULL DEFAULT 'aot' CHECK(mode='aot'),
  experiment text NOT NULL DEFAULT 'just_tx' CHECK(experiment='just_tx'),
  variant text NOT NULL DEFAULT 'metrics' CHECK(variant='metrics'),
  baseline_snapshot_id uuid,
  final_snapshot_id uuid,
  analysis_id uuid NOT NULL,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL CHECK(jsonb_typeof(issues)='array'),
  sent_success bench.u64, returned bench.u64,
  duplicate_rx bench.u64, late_rx bench.u64,
  valid_rtt_count bench.u64,
  CHECK(returned IS NULL OR sent_success IS NULL OR returned<=sent_success),
  CHECK(valid_rtt_count IS NULL OR returned IS NULL OR valid_rtt_count<=returned),
  PRIMARY KEY(run_id,analysis_id),
  FOREIGN KEY(run_id,mode,experiment,variant,config_id)
    REFERENCES bench.run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(baseline_snapshot_id,run_id) REFERENCES aot.metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(final_snapshot_id,run_id) REFERENCES aot.metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(analysis_id,run_id) REFERENCES bench.analysis(analysis_id,run_id),
  CHECK(NOT qualified OR (baseline_snapshot_id IS NOT NULL AND final_snapshot_id IS NOT NULL))
);
CREATE TABLE aot.just_tx_trace_data (
  run_id uuid PRIMARY KEY,
  config_id uuid NOT NULL REFERENCES aot.just_tx_config,
  mode text NOT NULL DEFAULT 'aot' CHECK(mode='aot'),
  experiment text NOT NULL DEFAULT 'just_tx' CHECK(experiment='just_tx'),
  variant text NOT NULL DEFAULT 'trace' CHECK(variant='trace'),
  epoch bigint NOT NULL CHECK(epoch>=0),
  dictionary_version integer NOT NULL CHECK(dictionary_version>=1),
  boot_id bench.u64 NOT NULL,
  clock_id text NOT NULL,
  capacity integer NOT NULL CHECK(capacity>0),
  written integer NOT NULL CHECK(written>=0 AND written<=capacity),
  dropped bigint NOT NULL CHECK(dropped>=0),
  flags bigint NOT NULL CHECK(flags>=0),
  complete boolean NOT NULL,
  result_checked boolean NOT NULL,
  result_ok boolean,
  raw_artifact_id uuid NOT NULL REFERENCES bench.artifact,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL CHECK(jsonb_typeof(issues)='array'),
  FOREIGN KEY(run_id,mode,experiment,variant,config_id)
    REFERENCES bench.run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(run_id,clock_id) REFERENCES bench.clock,
  CHECK(NOT complete OR (dropped=0 AND flags=0)),
  CHECK(NOT result_checked OR result_ok IS NOT NULL),
  CHECK(NOT qualified OR (complete AND (NOT result_checked OR result_ok)))
);
CREATE TABLE aot.just_tx_event (
  run_id uuid NOT NULL,
  mode text NOT NULL DEFAULT 'aot' CHECK(mode='aot'),
  experiment text NOT NULL DEFAULT 'just_tx' CHECK(experiment='just_tx'),
  source text NOT NULL CHECK(source IN ('dut','host','agent')),
  event_sequence bigint NOT NULL CHECK(event_sequence>=0),
  dictionary_version integer NOT NULL,
  event_id integer NOT NULL,
  clock_id text NOT NULL,
  timestamp_raw bench.u64 NOT NULL,
  kind text NOT NULL CHECK(kind IN ('begin','end','instant')),
  context_id integer NOT NULL CHECK(context_id BETWEEN 0 AND 255),
  probe_seq bigint CHECK(probe_seq>=1),
  span_id bigint CHECK(span_id>=0),
  parent_span_id bigint CHECK(parent_span_id>=0),
  arg0 bigint CHECK(arg0 BETWEEN 0 AND 4294967295),
  arg1 bigint CHECK(arg1 BETWEEN 0 AND 4294967295),
  FOREIGN KEY(run_id,mode,experiment) REFERENCES bench.run(run_id,mode,experiment),
  FOREIGN KEY(run_id,clock_id) REFERENCES bench.clock,
  FOREIGN KEY(experiment,dictionary_version,event_id) REFERENCES bench.event_definition,
  PRIMARY KEY(run_id,source,event_sequence)
);

CREATE TABLE aot.exec_bench_config (
  config_id uuid PRIMARY KEY,
  condition_id uuid NOT NULL,
  experiment text NOT NULL DEFAULT 'exec_bench' CHECK(experiment='exec_bench'),
  instrumentation text NOT NULL CHECK(instrumentation IN ('counter_only','helper_hist','stage_hist','batch_hist','switch_hist','trace')),
  profile_hash text NOT NULL CHECK(profile_hash ~ '^[0-9a-f]{64}$'),
  calibration_ids uuid[] NOT NULL DEFAULT ARRAY[]::uuid[],
  parameters jsonb NOT NULL DEFAULT '{}',
  workload text NOT NULL CHECK(workload IN ('incr','square','bitswap','fib','buf','memcpy','fletcher32','bsort')),
  work_param integer NOT NULL CHECK(work_param>=0),
  batch_count bigint NOT NULL CHECK(batch_count BETWEEN 1 AND 4294967295),
  samples_per_run integer NOT NULL CHECK(samples_per_run>0),
  input_bytes integer NOT NULL CHECK(input_bytes=88),
  input_protocol text NOT NULL CHECK(input_protocol='EXB2'),
  wifi_enabled boolean NOT NULL CHECK(NOT wifi_enabled),
  step_budget bench.u64,
  fixture_artifact_id uuid NOT NULL REFERENCES bench.artifact,
  FOREIGN KEY(condition_id,experiment) REFERENCES bench.condition(condition_id,experiment),
  UNIQUE(config_id,condition_id)
);
CREATE TABLE aot.exec_bench_metrics_data (
  run_id uuid NOT NULL,
  config_id uuid NOT NULL REFERENCES aot.exec_bench_config,
  mode text NOT NULL DEFAULT 'aot' CHECK(mode='aot'),
  experiment text NOT NULL DEFAULT 'exec_bench' CHECK(experiment='exec_bench'),
  variant text NOT NULL DEFAULT 'metrics' CHECK(variant='metrics'),
  baseline_snapshot_id uuid,
  final_snapshot_id uuid,
  analysis_id uuid NOT NULL,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL CHECK(jsonb_typeof(issues)='array'),
  batches_expected integer NOT NULL CHECK(batches_expected>0),
  batches_completed integer NOT NULL CHECK(batches_completed>=0),
  valid_batches integer NOT NULL CHECK(valid_batches>=0),
  CHECK(valid_batches<=batches_completed AND batches_completed<=batches_expected),
  PRIMARY KEY(run_id,analysis_id),
  FOREIGN KEY(run_id,mode,experiment,variant,config_id)
    REFERENCES bench.run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(baseline_snapshot_id,run_id) REFERENCES aot.metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(final_snapshot_id,run_id) REFERENCES aot.metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(analysis_id,run_id) REFERENCES bench.analysis(analysis_id,run_id),
  CHECK(NOT qualified OR (baseline_snapshot_id IS NOT NULL AND final_snapshot_id IS NOT NULL))
);
CREATE TABLE aot.exec_bench_trace_data (
  run_id uuid PRIMARY KEY,
  config_id uuid NOT NULL REFERENCES aot.exec_bench_config,
  mode text NOT NULL DEFAULT 'aot' CHECK(mode='aot'),
  experiment text NOT NULL DEFAULT 'exec_bench' CHECK(experiment='exec_bench'),
  variant text NOT NULL DEFAULT 'trace' CHECK(variant='trace'),
  epoch bigint NOT NULL CHECK(epoch>=0),
  dictionary_version integer NOT NULL CHECK(dictionary_version>=1),
  boot_id bench.u64 NOT NULL,
  clock_id text NOT NULL,
  capacity integer NOT NULL CHECK(capacity>0),
  written integer NOT NULL CHECK(written>=0 AND written<=capacity),
  dropped bigint NOT NULL CHECK(dropped>=0),
  flags bigint NOT NULL CHECK(flags>=0),
  complete boolean NOT NULL,
  result_checked boolean NOT NULL,
  result_ok boolean,
  raw_artifact_id uuid NOT NULL REFERENCES bench.artifact,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL CHECK(jsonb_typeof(issues)='array'),
  FOREIGN KEY(run_id,mode,experiment,variant,config_id)
    REFERENCES bench.run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(run_id,clock_id) REFERENCES bench.clock,
  CHECK(NOT complete OR (dropped=0 AND flags=0)),
  CHECK(NOT result_checked OR result_ok IS NOT NULL),
  CHECK(NOT qualified OR (complete AND (NOT result_checked OR result_ok)))
);
CREATE TABLE aot.exec_bench_event (
  run_id uuid NOT NULL,
  mode text NOT NULL DEFAULT 'aot' CHECK(mode='aot'),
  experiment text NOT NULL DEFAULT 'exec_bench' CHECK(experiment='exec_bench'),
  source text NOT NULL CHECK(source IN ('dut','host','agent')),
  event_sequence bigint NOT NULL CHECK(event_sequence>=0),
  dictionary_version integer NOT NULL,
  event_id integer NOT NULL,
  clock_id text NOT NULL,
  timestamp_raw bench.u64 NOT NULL,
  kind text NOT NULL CHECK(kind IN ('begin','end','instant')),
  context_id integer NOT NULL CHECK(context_id BETWEEN 0 AND 255),
  probe_seq bigint CHECK(probe_seq>=1),
  span_id bigint CHECK(span_id>=0),
  parent_span_id bigint CHECK(parent_span_id>=0),
  arg0 bigint CHECK(arg0 BETWEEN 0 AND 4294967295),
  arg1 bigint CHECK(arg1 BETWEEN 0 AND 4294967295),
  FOREIGN KEY(run_id,mode,experiment) REFERENCES bench.run(run_id,mode,experiment),
  FOREIGN KEY(run_id,clock_id) REFERENCES bench.clock,
  FOREIGN KEY(experiment,dictionary_version,event_id) REFERENCES bench.event_definition,
  PRIMARY KEY(run_id,source,event_sequence)
);

CREATE TABLE aot.case_config (
  config_id uuid PRIMARY KEY,
  condition_id uuid NOT NULL,
  experiment text NOT NULL DEFAULT 'case' CHECK(experiment='case'),
  instrumentation text NOT NULL CHECK(instrumentation IN ('counter_only','helper_hist','stage_hist','batch_hist','switch_hist','trace')),
  profile_hash text NOT NULL CHECK(profile_hash ~ '^[0-9a-f]{64}$'),
  calibration_ids uuid[] NOT NULL DEFAULT ARRAY[]::uuid[],
  parameters jsonb NOT NULL DEFAULT '{}',
  target_pps integer NOT NULL CHECK(target_pps>0),
  switch_count integer NOT NULL CHECK(switch_count>0),
  switch_kind text NOT NULL CHECK(switch_kind IN ('hot_publish','native_policy_select','stop_copy_start','cold_install')),
  quiesce_policy text NOT NULL CHECK(quiesce_policy IN ('drop','pass','native')),
  pre_window_ns bench.u64 NOT NULL,
  post_window_ns bench.u64 NOT NULL,
  payload_bytes integer NOT NULL CHECK(payload_bytes=40),
  source_mode text NOT NULL,
  target_mode text NOT NULL,
  FOREIGN KEY(condition_id,experiment) REFERENCES bench.condition(condition_id,experiment),
  UNIQUE(config_id,condition_id)
);
CREATE TABLE aot.case_metrics_data (
  run_id uuid NOT NULL,
  config_id uuid NOT NULL REFERENCES aot.case_config,
  mode text NOT NULL DEFAULT 'aot' CHECK(mode='aot'),
  experiment text NOT NULL DEFAULT 'case' CHECK(experiment='case'),
  variant text NOT NULL DEFAULT 'metrics' CHECK(variant='metrics'),
  baseline_snapshot_id uuid,
  final_snapshot_id uuid,
  analysis_id uuid NOT NULL,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL CHECK(jsonb_typeof(issues)='array'),
  attempted integer NOT NULL CHECK(attempted>=0),
  successful integer NOT NULL CHECK(successful>=0 AND successful<=attempted),
  boot_unchanged boolean NOT NULL,
  old_after_new bench.u64,
  PRIMARY KEY(run_id,analysis_id),
  FOREIGN KEY(run_id,mode,experiment,variant,config_id)
    REFERENCES bench.run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(baseline_snapshot_id,run_id) REFERENCES aot.metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(final_snapshot_id,run_id) REFERENCES aot.metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(analysis_id,run_id) REFERENCES bench.analysis(analysis_id,run_id),
  CHECK(NOT qualified OR (baseline_snapshot_id IS NOT NULL AND final_snapshot_id IS NOT NULL))
);
CREATE TABLE aot.case_trace_data (
  run_id uuid PRIMARY KEY,
  config_id uuid NOT NULL REFERENCES aot.case_config,
  mode text NOT NULL DEFAULT 'aot' CHECK(mode='aot'),
  experiment text NOT NULL DEFAULT 'case' CHECK(experiment='case'),
  variant text NOT NULL DEFAULT 'trace' CHECK(variant='trace'),
  epoch bigint NOT NULL CHECK(epoch>=0),
  dictionary_version integer NOT NULL CHECK(dictionary_version>=1),
  boot_id bench.u64 NOT NULL,
  clock_id text NOT NULL,
  capacity integer NOT NULL CHECK(capacity>0),
  written integer NOT NULL CHECK(written>=0 AND written<=capacity),
  dropped bigint NOT NULL CHECK(dropped>=0),
  flags bigint NOT NULL CHECK(flags>=0),
  complete boolean NOT NULL,
  result_checked boolean NOT NULL,
  result_ok boolean,
  raw_artifact_id uuid NOT NULL REFERENCES bench.artifact,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL CHECK(jsonb_typeof(issues)='array'),
  FOREIGN KEY(run_id,mode,experiment,variant,config_id)
    REFERENCES bench.run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(run_id,clock_id) REFERENCES bench.clock,
  CHECK(NOT complete OR (dropped=0 AND flags=0)),
  CHECK(NOT result_checked OR result_ok IS NOT NULL),
  CHECK(NOT qualified OR (complete AND (NOT result_checked OR result_ok)))
);
CREATE TABLE aot.case_event (
  run_id uuid NOT NULL,
  mode text NOT NULL DEFAULT 'aot' CHECK(mode='aot'),
  experiment text NOT NULL DEFAULT 'case' CHECK(experiment='case'),
  source text NOT NULL CHECK(source IN ('dut','host','agent')),
  event_sequence bigint NOT NULL CHECK(event_sequence>=0),
  dictionary_version integer NOT NULL,
  event_id integer NOT NULL,
  clock_id text NOT NULL,
  timestamp_raw bench.u64 NOT NULL,
  kind text NOT NULL CHECK(kind IN ('begin','end','instant')),
  context_id integer NOT NULL CHECK(context_id BETWEEN 0 AND 255),
  probe_seq bigint CHECK(probe_seq>=1),
  span_id bigint CHECK(span_id>=0),
  parent_span_id bigint CHECK(parent_span_id>=0),
  arg0 bigint CHECK(arg0 BETWEEN 0 AND 4294967295),
  arg1 bigint CHECK(arg1 BETWEEN 0 AND 4294967295),
  FOREIGN KEY(run_id,mode,experiment) REFERENCES bench.run(run_id,mode,experiment),
  FOREIGN KEY(run_id,clock_id) REFERENCES bench.clock,
  FOREIGN KEY(experiment,dictionary_version,event_id) REFERENCES bench.event_definition,
  PRIMARY KEY(run_id,source,event_sequence)
);

CREATE TABLE aot.probe_result (
  analysis_id uuid NOT NULL,
  run_id uuid NOT NULL,
  mode text NOT NULL DEFAULT 'aot' CHECK(mode='aot'),
  seq bigint NOT NULL,
  returned boolean NOT NULL,
  duplicate_rx bigint NOT NULL CHECK(duplicate_rx>=0),
  late_rx bigint NOT NULL CHECK(late_rx>=0),
  tx_capture_id uuid,
  tx_record_index bigint,
  rx_capture_id uuid,
  rx_record_index bigint,
  rtt_ns bench.u64,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL,
  FOREIGN KEY(run_id,mode) REFERENCES bench.run(run_id,mode),
  FOREIGN KEY(analysis_id,run_id) REFERENCES bench.analysis(analysis_id,run_id),
  FOREIGN KEY(run_id,seq) REFERENCES bench.send_attempt,
  FOREIGN KEY(tx_capture_id,run_id) REFERENCES bench.capture(capture_id,run_id),
  FOREIGN KEY(rx_capture_id,run_id) REFERENCES bench.capture(capture_id,run_id),
  FOREIGN KEY(tx_capture_id,tx_record_index) REFERENCES bench.packet_fact,
  FOREIGN KEY(rx_capture_id,rx_record_index) REFERENCES bench.packet_fact,
  PRIMARY KEY(analysis_id,seq),
  CHECK((tx_capture_id IS NULL)=(tx_record_index IS NULL)),
  CHECK((rx_capture_id IS NULL)=(rx_record_index IS NULL)),
  CHECK(rtt_ns IS NULL OR (returned AND qualified AND tx_capture_id IS NOT NULL AND rx_capture_id IS NOT NULL))
);
CREATE TABLE aot.firewall_token (
  run_id uuid NOT NULL,
  mode text NOT NULL DEFAULT 'aot' CHECK(mode='aot'),
  experiment text NOT NULL DEFAULT 'firewall' CHECK(experiment='firewall'),
  seq bigint NOT NULL,
  sent boolean NOT NULL,
  observed boolean NOT NULL,
  received_bytes bytea,
  FOREIGN KEY(run_id,mode,experiment) REFERENCES bench.run(run_id,mode,experiment),
  FOREIGN KEY(run_id,seq) REFERENCES bench.send_attempt,
  PRIMARY KEY(run_id,seq)
);
CREATE TABLE aot.exec_batch (
  run_id uuid NOT NULL,
  mode text NOT NULL DEFAULT 'aot' CHECK(mode='aot'),
  experiment text NOT NULL DEFAULT 'exec_bench' CHECK(experiment='exec_bench'),
  sample_index integer NOT NULL CHECK(sample_index>=0),
  clock_id text NOT NULL,
  begin_tick bench.u64 NOT NULL,
  end_tick bench.u64 NOT NULL,
  batch_count bigint NOT NULL CHECK(batch_count>0),
  completed_iterations bigint NOT NULL CHECK(completed_iterations>=0),
  seed bench.u64 NOT NULL,
  input_bytes bytea NOT NULL,
  expected_bytes bytea NOT NULL,
  actual_bytes bytea NOT NULL,
  result_ok boolean NOT NULL,
  qualified boolean NOT NULL,
  FOREIGN KEY(run_id,mode,experiment) REFERENCES bench.run(run_id,mode,experiment),
  FOREIGN KEY(run_id,clock_id) REFERENCES bench.clock,
  PRIMARY KEY(run_id,sample_index),
  CHECK(end_tick>=begin_tick),
  CHECK(NOT qualified OR (result_ok AND completed_iterations=batch_count))
);
CREATE TABLE aot.case_switch (
  run_id uuid NOT NULL,
  mode text NOT NULL DEFAULT 'aot' CHECK(mode='aot'),
  experiment text NOT NULL DEFAULT 'case' CHECK(experiment='case'),
  switch_index integer NOT NULL CHECK(switch_index>=1),
  switch_kind text NOT NULL CHECK(switch_kind IN ('hot_publish','native_policy_select','stop_copy_start','cold_install')),
  source_version integer NOT NULL CHECK(source_version IN (1,2)),
  target_version integer NOT NULL CHECK(target_version IN (1,2)),
  source_slot integer,
  target_slot integer,
  source_generation bigint,
  target_generation bigint,
  host_clock_id text NOT NULL,
  host_command_begin_ns bench.u64 NOT NULL,
  host_reply_ns bench.u64 NOT NULL,
  dut_clock_id text,
  dut_begin_tick bench.u64,
  dut_end_tick bench.u64,
  quiesce_ticks bench.u64,
  rebind_ticks bench.u64,
  publish_ticks bench.u64,
  last_old_capture_id uuid,
  last_old_record_index bigint,
  first_new_capture_id uuid,
  first_new_record_index bigint,
  reply_transition_gap_ns bench.u64,
  old_after_new bigint CHECK(old_after_new>=0),
  command_ok boolean NOT NULL,
  error_code text,
  boot_unchanged boolean NOT NULL,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL,
  FOREIGN KEY(run_id,mode,experiment) REFERENCES bench.run(run_id,mode,experiment),
  FOREIGN KEY(run_id,host_clock_id) REFERENCES bench.clock,
  FOREIGN KEY(run_id,dut_clock_id) REFERENCES bench.clock,
  FOREIGN KEY(last_old_capture_id,run_id) REFERENCES bench.capture(capture_id,run_id),
  FOREIGN KEY(first_new_capture_id,run_id) REFERENCES bench.capture(capture_id,run_id),
  FOREIGN KEY(last_old_capture_id,last_old_record_index) REFERENCES bench.packet_fact,
  FOREIGN KEY(first_new_capture_id,first_new_record_index) REFERENCES bench.packet_fact,
  PRIMARY KEY(run_id,switch_index),
  CHECK(source_version<>target_version),
  CHECK(host_reply_ns>=host_command_begin_ns),
  CHECK(dut_end_tick IS NULL OR dut_begin_tick IS NULL OR dut_end_tick>=dut_begin_tick),
  CHECK((last_old_capture_id IS NULL)=(last_old_record_index IS NULL)),
  CHECK((first_new_capture_id IS NULL)=(first_new_record_index IS NULL)),
  CHECK(reply_transition_gap_ns IS NULL OR (last_old_capture_id IS NOT NULL AND first_new_capture_id IS NOT NULL)),
  CHECK(NOT qualified OR (command_ok AND boot_unchanged))
);
ALTER TABLE aot.case_config ADD CHECK(switch_kind<>'native_policy_select' AND source_mode='aot' AND target_mode='aot');
ALTER TABLE aot.case_switch ADD CHECK(switch_kind<>'native_policy_select');

-- Mode schema: jit. All mode checks are literal, not caller conventions.
CREATE SCHEMA IF NOT EXISTS jit;
CREATE TABLE jit.metric_definition (
  metric_id uuid PRIMARY KEY,
  name text NOT NULL,
  kind text NOT NULL CHECK(kind IN ('counter','gauge','histogram')),
  unit text NOT NULL,
  boundary text NOT NULL,
  scope text NOT NULL,
  definition_version text NOT NULL,
  UNIQUE(metric_id,kind), UNIQUE(name,definition_version)
);
CREATE TABLE jit.metric_series (
  series_id uuid PRIMARY KEY,
  metric_id uuid NOT NULL,
  kind text NOT NULL,
  labels jsonb NOT NULL CHECK(jsonb_typeof(labels)='object'),
  FOREIGN KEY(metric_id,kind) REFERENCES jit.metric_definition(metric_id,kind),
  UNIQUE(series_id,kind), UNIQUE(metric_id,labels)
);
CREATE TABLE jit.metric_snapshot (
  snapshot_id uuid PRIMARY KEY,
  run_id uuid NOT NULL,
  mode text NOT NULL DEFAULT 'jit' CHECK(mode='jit'),
  phase text NOT NULL CHECK(phase IN ('baseline','final')),
  epoch bigint NOT NULL CHECK(epoch>=0),
  boot_id bench.u64 NOT NULL,
  clock_id text NOT NULL,
  at_tick bench.u64 NOT NULL,
  profile_hash text NOT NULL CHECK(profile_hash ~ '^[0-9a-f]{64}$'),
  raw_artifact_id uuid NOT NULL REFERENCES bench.artifact,
  FOREIGN KEY(run_id,mode) REFERENCES bench.run(run_id,mode),
  FOREIGN KEY(run_id,clock_id) REFERENCES bench.clock,
  UNIQUE(snapshot_id,run_id), UNIQUE(run_id,epoch,phase)
);
CREATE TABLE jit.scalar_sample (
  snapshot_id uuid NOT NULL REFERENCES jit.metric_snapshot,
  series_id uuid NOT NULL,
  kind text NOT NULL CHECK(kind IN ('counter','gauge')),
  value numeric NOT NULL,
  FOREIGN KEY(series_id,kind) REFERENCES jit.metric_series(series_id,kind),
  PRIMARY KEY(snapshot_id,series_id)
);
CREATE TABLE jit.histogram_sample (
  snapshot_id uuid NOT NULL REFERENCES jit.metric_snapshot,
  series_id uuid NOT NULL,
  kind text NOT NULL DEFAULT 'histogram' CHECK(kind='histogram'),
  calibration_id uuid NOT NULL REFERENCES bench.calibration,
  sample_count bench.u64 NOT NULL,
  sample_sum bench.u64 NOT NULL,
  FOREIGN KEY(series_id,kind) REFERENCES jit.metric_series(series_id,kind),
  PRIMARY KEY(snapshot_id,series_id),
  UNIQUE(snapshot_id,series_id,sample_count)
);
CREATE TABLE jit.histogram_bucket (
  snapshot_id uuid NOT NULL,
  series_id uuid NOT NULL,
  ordinal integer NOT NULL CHECK(ordinal>=1),
  upper_bound bigint CHECK(upper_bound>=0), -- NULL represents +Inf.
  cumulative_count bench.u64 NOT NULL,
  sample_count bench.u64 NOT NULL,
  FOREIGN KEY(snapshot_id,series_id,sample_count)
    REFERENCES jit.histogram_sample(snapshot_id,series_id,sample_count),
  PRIMARY KEY(snapshot_id,series_id,ordinal),
  CHECK(cumulative_count<=sample_count),
  CHECK(upper_bound IS NOT NULL OR cumulative_count=sample_count)
);
CREATE UNIQUE INDEX jit_hist_one_inf ON jit.histogram_bucket(snapshot_id,series_id)
  WHERE upper_bound IS NULL;

CREATE TABLE jit.firewall_config (
  config_id uuid PRIMARY KEY,
  condition_id uuid NOT NULL,
  experiment text NOT NULL DEFAULT 'firewall' CHECK(experiment='firewall'),
  instrumentation text NOT NULL CHECK(instrumentation IN ('counter_only','helper_hist','stage_hist','batch_hist','switch_hist','trace')),
  profile_hash text NOT NULL CHECK(profile_hash ~ '^[0-9a-f]{64}$'),
  calibration_ids uuid[] NOT NULL DEFAULT ARRAY[]::uuid[],
  parameters jsonb NOT NULL DEFAULT '{}',
  target_pps integer CHECK(target_pps>0),
  send_window_ns bench.u64,
  drain_ns bench.u64 NOT NULL,
  payload_bytes integer NOT NULL CHECK(payload_bytes>=32),
  blacklist_capacity integer NOT NULL CHECK(blacklist_capacity=256),
  blacklist_entries integer NOT NULL CHECK(blacklist_entries BETWEEN 1 AND 256),
  legal_numerator integer NOT NULL CHECK(legal_numerator BETWEEN 0 AND 10),
  legal_denominator integer NOT NULL CHECK(legal_denominator=10),
  lookup_case text NOT NULL CHECK(lookup_case IN ('mixed','hit','miss')),
  map_fixture_artifact_id uuid NOT NULL REFERENCES bench.artifact,
  FOREIGN KEY(condition_id,experiment) REFERENCES bench.condition(condition_id,experiment),
  UNIQUE(config_id,condition_id)
);
CREATE TABLE jit.firewall_metrics_data (
  run_id uuid NOT NULL,
  config_id uuid NOT NULL REFERENCES jit.firewall_config,
  mode text NOT NULL DEFAULT 'jit' CHECK(mode='jit'),
  experiment text NOT NULL DEFAULT 'firewall' CHECK(experiment='firewall'),
  variant text NOT NULL DEFAULT 'metrics' CHECK(variant='metrics'),
  baseline_snapshot_id uuid,
  final_snapshot_id uuid,
  analysis_id uuid NOT NULL,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL CHECK(jsonb_typeof(issues)='array'),
  sent_legal bench.u64, sent_illegal bench.u64,
  matched_legal bench.u64, blocked_hit_delta bench.u64,
  token_refused bench.u64,
  CHECK(matched_legal IS NULL OR sent_legal IS NULL OR matched_legal<=sent_legal),
  PRIMARY KEY(run_id,analysis_id),
  FOREIGN KEY(run_id,mode,experiment,variant,config_id)
    REFERENCES bench.run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(baseline_snapshot_id,run_id) REFERENCES jit.metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(final_snapshot_id,run_id) REFERENCES jit.metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(analysis_id,run_id) REFERENCES bench.analysis(analysis_id,run_id),
  CHECK(NOT qualified OR (baseline_snapshot_id IS NOT NULL AND final_snapshot_id IS NOT NULL))
);
CREATE TABLE jit.firewall_trace_data (
  run_id uuid PRIMARY KEY,
  config_id uuid NOT NULL REFERENCES jit.firewall_config,
  mode text NOT NULL DEFAULT 'jit' CHECK(mode='jit'),
  experiment text NOT NULL DEFAULT 'firewall' CHECK(experiment='firewall'),
  variant text NOT NULL DEFAULT 'trace' CHECK(variant='trace'),
  epoch bigint NOT NULL CHECK(epoch>=0),
  dictionary_version integer NOT NULL CHECK(dictionary_version>=1),
  boot_id bench.u64 NOT NULL,
  clock_id text NOT NULL,
  capacity integer NOT NULL CHECK(capacity>0),
  written integer NOT NULL CHECK(written>=0 AND written<=capacity),
  dropped bigint NOT NULL CHECK(dropped>=0),
  flags bigint NOT NULL CHECK(flags>=0),
  complete boolean NOT NULL,
  result_checked boolean NOT NULL,
  result_ok boolean,
  raw_artifact_id uuid NOT NULL REFERENCES bench.artifact,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL CHECK(jsonb_typeof(issues)='array'),
  FOREIGN KEY(run_id,mode,experiment,variant,config_id)
    REFERENCES bench.run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(run_id,clock_id) REFERENCES bench.clock,
  CHECK(NOT complete OR (dropped=0 AND flags=0)),
  CHECK(NOT result_checked OR result_ok IS NOT NULL),
  CHECK(NOT qualified OR (complete AND (NOT result_checked OR result_ok)))
);
CREATE TABLE jit.firewall_event (
  run_id uuid NOT NULL,
  mode text NOT NULL DEFAULT 'jit' CHECK(mode='jit'),
  experiment text NOT NULL DEFAULT 'firewall' CHECK(experiment='firewall'),
  source text NOT NULL CHECK(source IN ('dut','host','agent')),
  event_sequence bigint NOT NULL CHECK(event_sequence>=0),
  dictionary_version integer NOT NULL,
  event_id integer NOT NULL,
  clock_id text NOT NULL,
  timestamp_raw bench.u64 NOT NULL,
  kind text NOT NULL CHECK(kind IN ('begin','end','instant')),
  context_id integer NOT NULL CHECK(context_id BETWEEN 0 AND 255),
  probe_seq bigint CHECK(probe_seq>=1),
  span_id bigint CHECK(span_id>=0),
  parent_span_id bigint CHECK(parent_span_id>=0),
  arg0 bigint CHECK(arg0 BETWEEN 0 AND 4294967295),
  arg1 bigint CHECK(arg1 BETWEEN 0 AND 4294967295),
  FOREIGN KEY(run_id,mode,experiment) REFERENCES bench.run(run_id,mode,experiment),
  FOREIGN KEY(run_id,clock_id) REFERENCES bench.clock,
  FOREIGN KEY(experiment,dictionary_version,event_id) REFERENCES bench.event_definition,
  PRIMARY KEY(run_id,source,event_sequence)
);

CREATE TABLE jit.just_drop_config (
  config_id uuid PRIMARY KEY,
  condition_id uuid NOT NULL,
  experiment text NOT NULL DEFAULT 'just_drop' CHECK(experiment='just_drop'),
  instrumentation text NOT NULL CHECK(instrumentation IN ('counter_only','helper_hist','stage_hist','batch_hist','switch_hist','trace')),
  profile_hash text NOT NULL CHECK(profile_hash ~ '^[0-9a-f]{64}$'),
  calibration_ids uuid[] NOT NULL DEFAULT ARRAY[]::uuid[],
  parameters jsonb NOT NULL DEFAULT '{}',
  target_pps integer CHECK(target_pps>0),
  send_window_ns bench.u64,
  drain_ns bench.u64 NOT NULL,
  payload_bytes integer NOT NULL CHECK(payload_bytes>=32),
  workload_version text NOT NULL CHECK(workload_version='drop-minimal-v1'),
  neighbor_policy text NOT NULL,
  saturation_grid boolean NOT NULL,
  FOREIGN KEY(condition_id,experiment) REFERENCES bench.condition(condition_id,experiment),
  UNIQUE(config_id,condition_id)
);
CREATE TABLE jit.just_drop_metrics_data (
  run_id uuid NOT NULL,
  config_id uuid NOT NULL REFERENCES jit.just_drop_config,
  mode text NOT NULL DEFAULT 'jit' CHECK(mode='jit'),
  experiment text NOT NULL DEFAULT 'just_drop' CHECK(experiment='just_drop'),
  variant text NOT NULL DEFAULT 'metrics' CHECK(variant='metrics'),
  baseline_snapshot_id uuid,
  final_snapshot_id uuid,
  analysis_id uuid NOT NULL,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL CHECK(jsonb_typeof(issues)='array'),
  sent_success bench.u64, ingress_seen bench.u64,
  policy_enter bench.u64, normal_drop bench.u64,
  runtime_fault bench.u64, device_window_ticks bench.u64,
  sender_limited boolean NOT NULL,
  plateau_observed boolean NOT NULL,
  PRIMARY KEY(run_id,analysis_id),
  FOREIGN KEY(run_id,mode,experiment,variant,config_id)
    REFERENCES bench.run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(baseline_snapshot_id,run_id) REFERENCES jit.metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(final_snapshot_id,run_id) REFERENCES jit.metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(analysis_id,run_id) REFERENCES bench.analysis(analysis_id,run_id),
  CHECK(NOT qualified OR (baseline_snapshot_id IS NOT NULL AND final_snapshot_id IS NOT NULL))
);
CREATE TABLE jit.just_drop_trace_data (
  run_id uuid PRIMARY KEY,
  config_id uuid NOT NULL REFERENCES jit.just_drop_config,
  mode text NOT NULL DEFAULT 'jit' CHECK(mode='jit'),
  experiment text NOT NULL DEFAULT 'just_drop' CHECK(experiment='just_drop'),
  variant text NOT NULL DEFAULT 'trace' CHECK(variant='trace'),
  epoch bigint NOT NULL CHECK(epoch>=0),
  dictionary_version integer NOT NULL CHECK(dictionary_version>=1),
  boot_id bench.u64 NOT NULL,
  clock_id text NOT NULL,
  capacity integer NOT NULL CHECK(capacity>0),
  written integer NOT NULL CHECK(written>=0 AND written<=capacity),
  dropped bigint NOT NULL CHECK(dropped>=0),
  flags bigint NOT NULL CHECK(flags>=0),
  complete boolean NOT NULL,
  result_checked boolean NOT NULL,
  result_ok boolean,
  raw_artifact_id uuid NOT NULL REFERENCES bench.artifact,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL CHECK(jsonb_typeof(issues)='array'),
  FOREIGN KEY(run_id,mode,experiment,variant,config_id)
    REFERENCES bench.run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(run_id,clock_id) REFERENCES bench.clock,
  CHECK(NOT complete OR (dropped=0 AND flags=0)),
  CHECK(NOT result_checked OR result_ok IS NOT NULL),
  CHECK(NOT qualified OR (complete AND (NOT result_checked OR result_ok)))
);
CREATE TABLE jit.just_drop_event (
  run_id uuid NOT NULL,
  mode text NOT NULL DEFAULT 'jit' CHECK(mode='jit'),
  experiment text NOT NULL DEFAULT 'just_drop' CHECK(experiment='just_drop'),
  source text NOT NULL CHECK(source IN ('dut','host','agent')),
  event_sequence bigint NOT NULL CHECK(event_sequence>=0),
  dictionary_version integer NOT NULL,
  event_id integer NOT NULL,
  clock_id text NOT NULL,
  timestamp_raw bench.u64 NOT NULL,
  kind text NOT NULL CHECK(kind IN ('begin','end','instant')),
  context_id integer NOT NULL CHECK(context_id BETWEEN 0 AND 255),
  probe_seq bigint CHECK(probe_seq>=1),
  span_id bigint CHECK(span_id>=0),
  parent_span_id bigint CHECK(parent_span_id>=0),
  arg0 bigint CHECK(arg0 BETWEEN 0 AND 4294967295),
  arg1 bigint CHECK(arg1 BETWEEN 0 AND 4294967295),
  FOREIGN KEY(run_id,mode,experiment) REFERENCES bench.run(run_id,mode,experiment),
  FOREIGN KEY(run_id,clock_id) REFERENCES bench.clock,
  FOREIGN KEY(experiment,dictionary_version,event_id) REFERENCES bench.event_definition,
  PRIMARY KEY(run_id,source,event_sequence)
);

CREATE TABLE jit.just_tx_config (
  config_id uuid PRIMARY KEY,
  condition_id uuid NOT NULL,
  experiment text NOT NULL DEFAULT 'just_tx' CHECK(experiment='just_tx'),
  instrumentation text NOT NULL CHECK(instrumentation IN ('counter_only','helper_hist','stage_hist','batch_hist','switch_hist','trace')),
  profile_hash text NOT NULL CHECK(profile_hash ~ '^[0-9a-f]{64}$'),
  calibration_ids uuid[] NOT NULL DEFAULT ARRAY[]::uuid[],
  parameters jsonb NOT NULL DEFAULT '{}',
  target_pps integer CHECK(target_pps>0),
  send_window_ns bench.u64,
  drain_ns bench.u64 NOT NULL,
  payload_bytes integer NOT NULL CHECK(payload_bytes BETWEEN 32 AND 1472),
  probe_version text NOT NULL CHECK(probe_version='JTX1'),
  capture_interface text NOT NULL,
  FOREIGN KEY(condition_id,experiment) REFERENCES bench.condition(condition_id,experiment),
  UNIQUE(config_id,condition_id)
);
CREATE TABLE jit.just_tx_metrics_data (
  run_id uuid NOT NULL,
  config_id uuid NOT NULL REFERENCES jit.just_tx_config,
  mode text NOT NULL DEFAULT 'jit' CHECK(mode='jit'),
  experiment text NOT NULL DEFAULT 'just_tx' CHECK(experiment='just_tx'),
  variant text NOT NULL DEFAULT 'metrics' CHECK(variant='metrics'),
  baseline_snapshot_id uuid,
  final_snapshot_id uuid,
  analysis_id uuid NOT NULL,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL CHECK(jsonb_typeof(issues)='array'),
  sent_success bench.u64, returned bench.u64,
  duplicate_rx bench.u64, late_rx bench.u64,
  valid_rtt_count bench.u64,
  CHECK(returned IS NULL OR sent_success IS NULL OR returned<=sent_success),
  CHECK(valid_rtt_count IS NULL OR returned IS NULL OR valid_rtt_count<=returned),
  PRIMARY KEY(run_id,analysis_id),
  FOREIGN KEY(run_id,mode,experiment,variant,config_id)
    REFERENCES bench.run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(baseline_snapshot_id,run_id) REFERENCES jit.metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(final_snapshot_id,run_id) REFERENCES jit.metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(analysis_id,run_id) REFERENCES bench.analysis(analysis_id,run_id),
  CHECK(NOT qualified OR (baseline_snapshot_id IS NOT NULL AND final_snapshot_id IS NOT NULL))
);
CREATE TABLE jit.just_tx_trace_data (
  run_id uuid PRIMARY KEY,
  config_id uuid NOT NULL REFERENCES jit.just_tx_config,
  mode text NOT NULL DEFAULT 'jit' CHECK(mode='jit'),
  experiment text NOT NULL DEFAULT 'just_tx' CHECK(experiment='just_tx'),
  variant text NOT NULL DEFAULT 'trace' CHECK(variant='trace'),
  epoch bigint NOT NULL CHECK(epoch>=0),
  dictionary_version integer NOT NULL CHECK(dictionary_version>=1),
  boot_id bench.u64 NOT NULL,
  clock_id text NOT NULL,
  capacity integer NOT NULL CHECK(capacity>0),
  written integer NOT NULL CHECK(written>=0 AND written<=capacity),
  dropped bigint NOT NULL CHECK(dropped>=0),
  flags bigint NOT NULL CHECK(flags>=0),
  complete boolean NOT NULL,
  result_checked boolean NOT NULL,
  result_ok boolean,
  raw_artifact_id uuid NOT NULL REFERENCES bench.artifact,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL CHECK(jsonb_typeof(issues)='array'),
  FOREIGN KEY(run_id,mode,experiment,variant,config_id)
    REFERENCES bench.run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(run_id,clock_id) REFERENCES bench.clock,
  CHECK(NOT complete OR (dropped=0 AND flags=0)),
  CHECK(NOT result_checked OR result_ok IS NOT NULL),
  CHECK(NOT qualified OR (complete AND (NOT result_checked OR result_ok)))
);
CREATE TABLE jit.just_tx_event (
  run_id uuid NOT NULL,
  mode text NOT NULL DEFAULT 'jit' CHECK(mode='jit'),
  experiment text NOT NULL DEFAULT 'just_tx' CHECK(experiment='just_tx'),
  source text NOT NULL CHECK(source IN ('dut','host','agent')),
  event_sequence bigint NOT NULL CHECK(event_sequence>=0),
  dictionary_version integer NOT NULL,
  event_id integer NOT NULL,
  clock_id text NOT NULL,
  timestamp_raw bench.u64 NOT NULL,
  kind text NOT NULL CHECK(kind IN ('begin','end','instant')),
  context_id integer NOT NULL CHECK(context_id BETWEEN 0 AND 255),
  probe_seq bigint CHECK(probe_seq>=1),
  span_id bigint CHECK(span_id>=0),
  parent_span_id bigint CHECK(parent_span_id>=0),
  arg0 bigint CHECK(arg0 BETWEEN 0 AND 4294967295),
  arg1 bigint CHECK(arg1 BETWEEN 0 AND 4294967295),
  FOREIGN KEY(run_id,mode,experiment) REFERENCES bench.run(run_id,mode,experiment),
  FOREIGN KEY(run_id,clock_id) REFERENCES bench.clock,
  FOREIGN KEY(experiment,dictionary_version,event_id) REFERENCES bench.event_definition,
  PRIMARY KEY(run_id,source,event_sequence)
);

CREATE TABLE jit.exec_bench_config (
  config_id uuid PRIMARY KEY,
  condition_id uuid NOT NULL,
  experiment text NOT NULL DEFAULT 'exec_bench' CHECK(experiment='exec_bench'),
  instrumentation text NOT NULL CHECK(instrumentation IN ('counter_only','helper_hist','stage_hist','batch_hist','switch_hist','trace')),
  profile_hash text NOT NULL CHECK(profile_hash ~ '^[0-9a-f]{64}$'),
  calibration_ids uuid[] NOT NULL DEFAULT ARRAY[]::uuid[],
  parameters jsonb NOT NULL DEFAULT '{}',
  workload text NOT NULL CHECK(workload IN ('incr','square','bitswap','fib','buf','memcpy','fletcher32','bsort')),
  work_param integer NOT NULL CHECK(work_param>=0),
  batch_count bigint NOT NULL CHECK(batch_count BETWEEN 1 AND 4294967295),
  samples_per_run integer NOT NULL CHECK(samples_per_run>0),
  input_bytes integer NOT NULL CHECK(input_bytes=88),
  input_protocol text NOT NULL CHECK(input_protocol='EXB2'),
  wifi_enabled boolean NOT NULL CHECK(NOT wifi_enabled),
  step_budget bench.u64,
  fixture_artifact_id uuid NOT NULL REFERENCES bench.artifact,
  FOREIGN KEY(condition_id,experiment) REFERENCES bench.condition(condition_id,experiment),
  UNIQUE(config_id,condition_id)
);
CREATE TABLE jit.exec_bench_metrics_data (
  run_id uuid NOT NULL,
  config_id uuid NOT NULL REFERENCES jit.exec_bench_config,
  mode text NOT NULL DEFAULT 'jit' CHECK(mode='jit'),
  experiment text NOT NULL DEFAULT 'exec_bench' CHECK(experiment='exec_bench'),
  variant text NOT NULL DEFAULT 'metrics' CHECK(variant='metrics'),
  baseline_snapshot_id uuid,
  final_snapshot_id uuid,
  analysis_id uuid NOT NULL,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL CHECK(jsonb_typeof(issues)='array'),
  batches_expected integer NOT NULL CHECK(batches_expected>0),
  batches_completed integer NOT NULL CHECK(batches_completed>=0),
  valid_batches integer NOT NULL CHECK(valid_batches>=0),
  CHECK(valid_batches<=batches_completed AND batches_completed<=batches_expected),
  PRIMARY KEY(run_id,analysis_id),
  FOREIGN KEY(run_id,mode,experiment,variant,config_id)
    REFERENCES bench.run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(baseline_snapshot_id,run_id) REFERENCES jit.metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(final_snapshot_id,run_id) REFERENCES jit.metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(analysis_id,run_id) REFERENCES bench.analysis(analysis_id,run_id),
  CHECK(NOT qualified OR (baseline_snapshot_id IS NOT NULL AND final_snapshot_id IS NOT NULL))
);
CREATE TABLE jit.exec_bench_trace_data (
  run_id uuid PRIMARY KEY,
  config_id uuid NOT NULL REFERENCES jit.exec_bench_config,
  mode text NOT NULL DEFAULT 'jit' CHECK(mode='jit'),
  experiment text NOT NULL DEFAULT 'exec_bench' CHECK(experiment='exec_bench'),
  variant text NOT NULL DEFAULT 'trace' CHECK(variant='trace'),
  epoch bigint NOT NULL CHECK(epoch>=0),
  dictionary_version integer NOT NULL CHECK(dictionary_version>=1),
  boot_id bench.u64 NOT NULL,
  clock_id text NOT NULL,
  capacity integer NOT NULL CHECK(capacity>0),
  written integer NOT NULL CHECK(written>=0 AND written<=capacity),
  dropped bigint NOT NULL CHECK(dropped>=0),
  flags bigint NOT NULL CHECK(flags>=0),
  complete boolean NOT NULL,
  result_checked boolean NOT NULL,
  result_ok boolean,
  raw_artifact_id uuid NOT NULL REFERENCES bench.artifact,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL CHECK(jsonb_typeof(issues)='array'),
  FOREIGN KEY(run_id,mode,experiment,variant,config_id)
    REFERENCES bench.run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(run_id,clock_id) REFERENCES bench.clock,
  CHECK(NOT complete OR (dropped=0 AND flags=0)),
  CHECK(NOT result_checked OR result_ok IS NOT NULL),
  CHECK(NOT qualified OR (complete AND (NOT result_checked OR result_ok)))
);
CREATE TABLE jit.exec_bench_event (
  run_id uuid NOT NULL,
  mode text NOT NULL DEFAULT 'jit' CHECK(mode='jit'),
  experiment text NOT NULL DEFAULT 'exec_bench' CHECK(experiment='exec_bench'),
  source text NOT NULL CHECK(source IN ('dut','host','agent')),
  event_sequence bigint NOT NULL CHECK(event_sequence>=0),
  dictionary_version integer NOT NULL,
  event_id integer NOT NULL,
  clock_id text NOT NULL,
  timestamp_raw bench.u64 NOT NULL,
  kind text NOT NULL CHECK(kind IN ('begin','end','instant')),
  context_id integer NOT NULL CHECK(context_id BETWEEN 0 AND 255),
  probe_seq bigint CHECK(probe_seq>=1),
  span_id bigint CHECK(span_id>=0),
  parent_span_id bigint CHECK(parent_span_id>=0),
  arg0 bigint CHECK(arg0 BETWEEN 0 AND 4294967295),
  arg1 bigint CHECK(arg1 BETWEEN 0 AND 4294967295),
  FOREIGN KEY(run_id,mode,experiment) REFERENCES bench.run(run_id,mode,experiment),
  FOREIGN KEY(run_id,clock_id) REFERENCES bench.clock,
  FOREIGN KEY(experiment,dictionary_version,event_id) REFERENCES bench.event_definition,
  PRIMARY KEY(run_id,source,event_sequence)
);

CREATE TABLE jit.case_config (
  config_id uuid PRIMARY KEY,
  condition_id uuid NOT NULL,
  experiment text NOT NULL DEFAULT 'case' CHECK(experiment='case'),
  instrumentation text NOT NULL CHECK(instrumentation IN ('counter_only','helper_hist','stage_hist','batch_hist','switch_hist','trace')),
  profile_hash text NOT NULL CHECK(profile_hash ~ '^[0-9a-f]{64}$'),
  calibration_ids uuid[] NOT NULL DEFAULT ARRAY[]::uuid[],
  parameters jsonb NOT NULL DEFAULT '{}',
  target_pps integer NOT NULL CHECK(target_pps>0),
  switch_count integer NOT NULL CHECK(switch_count>0),
  switch_kind text NOT NULL CHECK(switch_kind IN ('hot_publish','native_policy_select','stop_copy_start','cold_install')),
  quiesce_policy text NOT NULL CHECK(quiesce_policy IN ('drop','pass','native')),
  pre_window_ns bench.u64 NOT NULL,
  post_window_ns bench.u64 NOT NULL,
  payload_bytes integer NOT NULL CHECK(payload_bytes=40),
  source_mode text NOT NULL,
  target_mode text NOT NULL,
  FOREIGN KEY(condition_id,experiment) REFERENCES bench.condition(condition_id,experiment),
  UNIQUE(config_id,condition_id)
);
CREATE TABLE jit.case_metrics_data (
  run_id uuid NOT NULL,
  config_id uuid NOT NULL REFERENCES jit.case_config,
  mode text NOT NULL DEFAULT 'jit' CHECK(mode='jit'),
  experiment text NOT NULL DEFAULT 'case' CHECK(experiment='case'),
  variant text NOT NULL DEFAULT 'metrics' CHECK(variant='metrics'),
  baseline_snapshot_id uuid,
  final_snapshot_id uuid,
  analysis_id uuid NOT NULL,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL CHECK(jsonb_typeof(issues)='array'),
  attempted integer NOT NULL CHECK(attempted>=0),
  successful integer NOT NULL CHECK(successful>=0 AND successful<=attempted),
  boot_unchanged boolean NOT NULL,
  old_after_new bench.u64,
  PRIMARY KEY(run_id,analysis_id),
  FOREIGN KEY(run_id,mode,experiment,variant,config_id)
    REFERENCES bench.run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(baseline_snapshot_id,run_id) REFERENCES jit.metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(final_snapshot_id,run_id) REFERENCES jit.metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(analysis_id,run_id) REFERENCES bench.analysis(analysis_id,run_id),
  CHECK(NOT qualified OR (baseline_snapshot_id IS NOT NULL AND final_snapshot_id IS NOT NULL))
);
CREATE TABLE jit.case_trace_data (
  run_id uuid PRIMARY KEY,
  config_id uuid NOT NULL REFERENCES jit.case_config,
  mode text NOT NULL DEFAULT 'jit' CHECK(mode='jit'),
  experiment text NOT NULL DEFAULT 'case' CHECK(experiment='case'),
  variant text NOT NULL DEFAULT 'trace' CHECK(variant='trace'),
  epoch bigint NOT NULL CHECK(epoch>=0),
  dictionary_version integer NOT NULL CHECK(dictionary_version>=1),
  boot_id bench.u64 NOT NULL,
  clock_id text NOT NULL,
  capacity integer NOT NULL CHECK(capacity>0),
  written integer NOT NULL CHECK(written>=0 AND written<=capacity),
  dropped bigint NOT NULL CHECK(dropped>=0),
  flags bigint NOT NULL CHECK(flags>=0),
  complete boolean NOT NULL,
  result_checked boolean NOT NULL,
  result_ok boolean,
  raw_artifact_id uuid NOT NULL REFERENCES bench.artifact,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL CHECK(jsonb_typeof(issues)='array'),
  FOREIGN KEY(run_id,mode,experiment,variant,config_id)
    REFERENCES bench.run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(run_id,clock_id) REFERENCES bench.clock,
  CHECK(NOT complete OR (dropped=0 AND flags=0)),
  CHECK(NOT result_checked OR result_ok IS NOT NULL),
  CHECK(NOT qualified OR (complete AND (NOT result_checked OR result_ok)))
);
CREATE TABLE jit.case_event (
  run_id uuid NOT NULL,
  mode text NOT NULL DEFAULT 'jit' CHECK(mode='jit'),
  experiment text NOT NULL DEFAULT 'case' CHECK(experiment='case'),
  source text NOT NULL CHECK(source IN ('dut','host','agent')),
  event_sequence bigint NOT NULL CHECK(event_sequence>=0),
  dictionary_version integer NOT NULL,
  event_id integer NOT NULL,
  clock_id text NOT NULL,
  timestamp_raw bench.u64 NOT NULL,
  kind text NOT NULL CHECK(kind IN ('begin','end','instant')),
  context_id integer NOT NULL CHECK(context_id BETWEEN 0 AND 255),
  probe_seq bigint CHECK(probe_seq>=1),
  span_id bigint CHECK(span_id>=0),
  parent_span_id bigint CHECK(parent_span_id>=0),
  arg0 bigint CHECK(arg0 BETWEEN 0 AND 4294967295),
  arg1 bigint CHECK(arg1 BETWEEN 0 AND 4294967295),
  FOREIGN KEY(run_id,mode,experiment) REFERENCES bench.run(run_id,mode,experiment),
  FOREIGN KEY(run_id,clock_id) REFERENCES bench.clock,
  FOREIGN KEY(experiment,dictionary_version,event_id) REFERENCES bench.event_definition,
  PRIMARY KEY(run_id,source,event_sequence)
);

CREATE TABLE jit.probe_result (
  analysis_id uuid NOT NULL,
  run_id uuid NOT NULL,
  mode text NOT NULL DEFAULT 'jit' CHECK(mode='jit'),
  seq bigint NOT NULL,
  returned boolean NOT NULL,
  duplicate_rx bigint NOT NULL CHECK(duplicate_rx>=0),
  late_rx bigint NOT NULL CHECK(late_rx>=0),
  tx_capture_id uuid,
  tx_record_index bigint,
  rx_capture_id uuid,
  rx_record_index bigint,
  rtt_ns bench.u64,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL,
  FOREIGN KEY(run_id,mode) REFERENCES bench.run(run_id,mode),
  FOREIGN KEY(analysis_id,run_id) REFERENCES bench.analysis(analysis_id,run_id),
  FOREIGN KEY(run_id,seq) REFERENCES bench.send_attempt,
  FOREIGN KEY(tx_capture_id,run_id) REFERENCES bench.capture(capture_id,run_id),
  FOREIGN KEY(rx_capture_id,run_id) REFERENCES bench.capture(capture_id,run_id),
  FOREIGN KEY(tx_capture_id,tx_record_index) REFERENCES bench.packet_fact,
  FOREIGN KEY(rx_capture_id,rx_record_index) REFERENCES bench.packet_fact,
  PRIMARY KEY(analysis_id,seq),
  CHECK((tx_capture_id IS NULL)=(tx_record_index IS NULL)),
  CHECK((rx_capture_id IS NULL)=(rx_record_index IS NULL)),
  CHECK(rtt_ns IS NULL OR (returned AND qualified AND tx_capture_id IS NOT NULL AND rx_capture_id IS NOT NULL))
);
CREATE TABLE jit.firewall_token (
  run_id uuid NOT NULL,
  mode text NOT NULL DEFAULT 'jit' CHECK(mode='jit'),
  experiment text NOT NULL DEFAULT 'firewall' CHECK(experiment='firewall'),
  seq bigint NOT NULL,
  sent boolean NOT NULL,
  observed boolean NOT NULL,
  received_bytes bytea,
  FOREIGN KEY(run_id,mode,experiment) REFERENCES bench.run(run_id,mode,experiment),
  FOREIGN KEY(run_id,seq) REFERENCES bench.send_attempt,
  PRIMARY KEY(run_id,seq)
);
CREATE TABLE jit.exec_batch (
  run_id uuid NOT NULL,
  mode text NOT NULL DEFAULT 'jit' CHECK(mode='jit'),
  experiment text NOT NULL DEFAULT 'exec_bench' CHECK(experiment='exec_bench'),
  sample_index integer NOT NULL CHECK(sample_index>=0),
  clock_id text NOT NULL,
  begin_tick bench.u64 NOT NULL,
  end_tick bench.u64 NOT NULL,
  batch_count bigint NOT NULL CHECK(batch_count>0),
  completed_iterations bigint NOT NULL CHECK(completed_iterations>=0),
  seed bench.u64 NOT NULL,
  input_bytes bytea NOT NULL,
  expected_bytes bytea NOT NULL,
  actual_bytes bytea NOT NULL,
  result_ok boolean NOT NULL,
  qualified boolean NOT NULL,
  FOREIGN KEY(run_id,mode,experiment) REFERENCES bench.run(run_id,mode,experiment),
  FOREIGN KEY(run_id,clock_id) REFERENCES bench.clock,
  PRIMARY KEY(run_id,sample_index),
  CHECK(end_tick>=begin_tick),
  CHECK(NOT qualified OR (result_ok AND completed_iterations=batch_count))
);
CREATE TABLE jit.case_switch (
  run_id uuid NOT NULL,
  mode text NOT NULL DEFAULT 'jit' CHECK(mode='jit'),
  experiment text NOT NULL DEFAULT 'case' CHECK(experiment='case'),
  switch_index integer NOT NULL CHECK(switch_index>=1),
  switch_kind text NOT NULL CHECK(switch_kind IN ('hot_publish','native_policy_select','stop_copy_start','cold_install')),
  source_version integer NOT NULL CHECK(source_version IN (1,2)),
  target_version integer NOT NULL CHECK(target_version IN (1,2)),
  source_slot integer,
  target_slot integer,
  source_generation bigint,
  target_generation bigint,
  host_clock_id text NOT NULL,
  host_command_begin_ns bench.u64 NOT NULL,
  host_reply_ns bench.u64 NOT NULL,
  dut_clock_id text,
  dut_begin_tick bench.u64,
  dut_end_tick bench.u64,
  quiesce_ticks bench.u64,
  rebind_ticks bench.u64,
  publish_ticks bench.u64,
  last_old_capture_id uuid,
  last_old_record_index bigint,
  first_new_capture_id uuid,
  first_new_record_index bigint,
  reply_transition_gap_ns bench.u64,
  old_after_new bigint CHECK(old_after_new>=0),
  command_ok boolean NOT NULL,
  error_code text,
  boot_unchanged boolean NOT NULL,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL,
  FOREIGN KEY(run_id,mode,experiment) REFERENCES bench.run(run_id,mode,experiment),
  FOREIGN KEY(run_id,host_clock_id) REFERENCES bench.clock,
  FOREIGN KEY(run_id,dut_clock_id) REFERENCES bench.clock,
  FOREIGN KEY(last_old_capture_id,run_id) REFERENCES bench.capture(capture_id,run_id),
  FOREIGN KEY(first_new_capture_id,run_id) REFERENCES bench.capture(capture_id,run_id),
  FOREIGN KEY(last_old_capture_id,last_old_record_index) REFERENCES bench.packet_fact,
  FOREIGN KEY(first_new_capture_id,first_new_record_index) REFERENCES bench.packet_fact,
  PRIMARY KEY(run_id,switch_index),
  CHECK(source_version<>target_version),
  CHECK(host_reply_ns>=host_command_begin_ns),
  CHECK(dut_end_tick IS NULL OR dut_begin_tick IS NULL OR dut_end_tick>=dut_begin_tick),
  CHECK((last_old_capture_id IS NULL)=(last_old_record_index IS NULL)),
  CHECK((first_new_capture_id IS NULL)=(first_new_record_index IS NULL)),
  CHECK(reply_transition_gap_ns IS NULL OR (last_old_capture_id IS NOT NULL AND first_new_capture_id IS NOT NULL)),
  CHECK(NOT qualified OR (command_ok AND boot_unchanged))
);
ALTER TABLE jit.case_config ADD CHECK(switch_kind<>'native_policy_select' AND source_mode='jit' AND target_mode='jit');
ALTER TABLE jit.case_switch ADD CHECK(switch_kind<>'native_policy_select');

-- Mode schema: rbpf. All mode checks are literal, not caller conventions.
CREATE SCHEMA IF NOT EXISTS rbpf;
CREATE TABLE rbpf.metric_definition (
  metric_id uuid PRIMARY KEY,
  name text NOT NULL,
  kind text NOT NULL CHECK(kind IN ('counter','gauge','histogram')),
  unit text NOT NULL,
  boundary text NOT NULL,
  scope text NOT NULL,
  definition_version text NOT NULL,
  UNIQUE(metric_id,kind), UNIQUE(name,definition_version)
);
CREATE TABLE rbpf.metric_series (
  series_id uuid PRIMARY KEY,
  metric_id uuid NOT NULL,
  kind text NOT NULL,
  labels jsonb NOT NULL CHECK(jsonb_typeof(labels)='object'),
  FOREIGN KEY(metric_id,kind) REFERENCES rbpf.metric_definition(metric_id,kind),
  UNIQUE(series_id,kind), UNIQUE(metric_id,labels)
);
CREATE TABLE rbpf.metric_snapshot (
  snapshot_id uuid PRIMARY KEY,
  run_id uuid NOT NULL,
  mode text NOT NULL DEFAULT 'rbpf' CHECK(mode='rbpf'),
  phase text NOT NULL CHECK(phase IN ('baseline','final')),
  epoch bigint NOT NULL CHECK(epoch>=0),
  boot_id bench.u64 NOT NULL,
  clock_id text NOT NULL,
  at_tick bench.u64 NOT NULL,
  profile_hash text NOT NULL CHECK(profile_hash ~ '^[0-9a-f]{64}$'),
  raw_artifact_id uuid NOT NULL REFERENCES bench.artifact,
  FOREIGN KEY(run_id,mode) REFERENCES bench.run(run_id,mode),
  FOREIGN KEY(run_id,clock_id) REFERENCES bench.clock,
  UNIQUE(snapshot_id,run_id), UNIQUE(run_id,epoch,phase)
);
CREATE TABLE rbpf.scalar_sample (
  snapshot_id uuid NOT NULL REFERENCES rbpf.metric_snapshot,
  series_id uuid NOT NULL,
  kind text NOT NULL CHECK(kind IN ('counter','gauge')),
  value numeric NOT NULL,
  FOREIGN KEY(series_id,kind) REFERENCES rbpf.metric_series(series_id,kind),
  PRIMARY KEY(snapshot_id,series_id)
);
CREATE TABLE rbpf.histogram_sample (
  snapshot_id uuid NOT NULL REFERENCES rbpf.metric_snapshot,
  series_id uuid NOT NULL,
  kind text NOT NULL DEFAULT 'histogram' CHECK(kind='histogram'),
  calibration_id uuid NOT NULL REFERENCES bench.calibration,
  sample_count bench.u64 NOT NULL,
  sample_sum bench.u64 NOT NULL,
  FOREIGN KEY(series_id,kind) REFERENCES rbpf.metric_series(series_id,kind),
  PRIMARY KEY(snapshot_id,series_id),
  UNIQUE(snapshot_id,series_id,sample_count)
);
CREATE TABLE rbpf.histogram_bucket (
  snapshot_id uuid NOT NULL,
  series_id uuid NOT NULL,
  ordinal integer NOT NULL CHECK(ordinal>=1),
  upper_bound bigint CHECK(upper_bound>=0), -- NULL represents +Inf.
  cumulative_count bench.u64 NOT NULL,
  sample_count bench.u64 NOT NULL,
  FOREIGN KEY(snapshot_id,series_id,sample_count)
    REFERENCES rbpf.histogram_sample(snapshot_id,series_id,sample_count),
  PRIMARY KEY(snapshot_id,series_id,ordinal),
  CHECK(cumulative_count<=sample_count),
  CHECK(upper_bound IS NOT NULL OR cumulative_count=sample_count)
);
CREATE UNIQUE INDEX rbpf_hist_one_inf ON rbpf.histogram_bucket(snapshot_id,series_id)
  WHERE upper_bound IS NULL;

CREATE TABLE rbpf.firewall_config (
  config_id uuid PRIMARY KEY,
  condition_id uuid NOT NULL,
  experiment text NOT NULL DEFAULT 'firewall' CHECK(experiment='firewall'),
  instrumentation text NOT NULL CHECK(instrumentation IN ('counter_only','helper_hist','stage_hist','batch_hist','switch_hist','trace')),
  profile_hash text NOT NULL CHECK(profile_hash ~ '^[0-9a-f]{64}$'),
  calibration_ids uuid[] NOT NULL DEFAULT ARRAY[]::uuid[],
  parameters jsonb NOT NULL DEFAULT '{}',
  target_pps integer CHECK(target_pps>0),
  send_window_ns bench.u64,
  drain_ns bench.u64 NOT NULL,
  payload_bytes integer NOT NULL CHECK(payload_bytes>=32),
  blacklist_capacity integer NOT NULL CHECK(blacklist_capacity=256),
  blacklist_entries integer NOT NULL CHECK(blacklist_entries BETWEEN 1 AND 256),
  legal_numerator integer NOT NULL CHECK(legal_numerator BETWEEN 0 AND 10),
  legal_denominator integer NOT NULL CHECK(legal_denominator=10),
  lookup_case text NOT NULL CHECK(lookup_case IN ('mixed','hit','miss')),
  map_fixture_artifact_id uuid NOT NULL REFERENCES bench.artifact,
  FOREIGN KEY(condition_id,experiment) REFERENCES bench.condition(condition_id,experiment),
  UNIQUE(config_id,condition_id)
);
CREATE TABLE rbpf.firewall_metrics_data (
  run_id uuid NOT NULL,
  config_id uuid NOT NULL REFERENCES rbpf.firewall_config,
  mode text NOT NULL DEFAULT 'rbpf' CHECK(mode='rbpf'),
  experiment text NOT NULL DEFAULT 'firewall' CHECK(experiment='firewall'),
  variant text NOT NULL DEFAULT 'metrics' CHECK(variant='metrics'),
  baseline_snapshot_id uuid,
  final_snapshot_id uuid,
  analysis_id uuid NOT NULL,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL CHECK(jsonb_typeof(issues)='array'),
  sent_legal bench.u64, sent_illegal bench.u64,
  matched_legal bench.u64, blocked_hit_delta bench.u64,
  token_refused bench.u64,
  CHECK(matched_legal IS NULL OR sent_legal IS NULL OR matched_legal<=sent_legal),
  PRIMARY KEY(run_id,analysis_id),
  FOREIGN KEY(run_id,mode,experiment,variant,config_id)
    REFERENCES bench.run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(baseline_snapshot_id,run_id) REFERENCES rbpf.metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(final_snapshot_id,run_id) REFERENCES rbpf.metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(analysis_id,run_id) REFERENCES bench.analysis(analysis_id,run_id),
  CHECK(NOT qualified OR (baseline_snapshot_id IS NOT NULL AND final_snapshot_id IS NOT NULL))
);
CREATE TABLE rbpf.firewall_trace_data (
  run_id uuid PRIMARY KEY,
  config_id uuid NOT NULL REFERENCES rbpf.firewall_config,
  mode text NOT NULL DEFAULT 'rbpf' CHECK(mode='rbpf'),
  experiment text NOT NULL DEFAULT 'firewall' CHECK(experiment='firewall'),
  variant text NOT NULL DEFAULT 'trace' CHECK(variant='trace'),
  epoch bigint NOT NULL CHECK(epoch>=0),
  dictionary_version integer NOT NULL CHECK(dictionary_version>=1),
  boot_id bench.u64 NOT NULL,
  clock_id text NOT NULL,
  capacity integer NOT NULL CHECK(capacity>0),
  written integer NOT NULL CHECK(written>=0 AND written<=capacity),
  dropped bigint NOT NULL CHECK(dropped>=0),
  flags bigint NOT NULL CHECK(flags>=0),
  complete boolean NOT NULL,
  result_checked boolean NOT NULL,
  result_ok boolean,
  raw_artifact_id uuid NOT NULL REFERENCES bench.artifact,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL CHECK(jsonb_typeof(issues)='array'),
  FOREIGN KEY(run_id,mode,experiment,variant,config_id)
    REFERENCES bench.run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(run_id,clock_id) REFERENCES bench.clock,
  CHECK(NOT complete OR (dropped=0 AND flags=0)),
  CHECK(NOT result_checked OR result_ok IS NOT NULL),
  CHECK(NOT qualified OR (complete AND (NOT result_checked OR result_ok)))
);
CREATE TABLE rbpf.firewall_event (
  run_id uuid NOT NULL,
  mode text NOT NULL DEFAULT 'rbpf' CHECK(mode='rbpf'),
  experiment text NOT NULL DEFAULT 'firewall' CHECK(experiment='firewall'),
  source text NOT NULL CHECK(source IN ('dut','host','agent')),
  event_sequence bigint NOT NULL CHECK(event_sequence>=0),
  dictionary_version integer NOT NULL,
  event_id integer NOT NULL,
  clock_id text NOT NULL,
  timestamp_raw bench.u64 NOT NULL,
  kind text NOT NULL CHECK(kind IN ('begin','end','instant')),
  context_id integer NOT NULL CHECK(context_id BETWEEN 0 AND 255),
  probe_seq bigint CHECK(probe_seq>=1),
  span_id bigint CHECK(span_id>=0),
  parent_span_id bigint CHECK(parent_span_id>=0),
  arg0 bigint CHECK(arg0 BETWEEN 0 AND 4294967295),
  arg1 bigint CHECK(arg1 BETWEEN 0 AND 4294967295),
  FOREIGN KEY(run_id,mode,experiment) REFERENCES bench.run(run_id,mode,experiment),
  FOREIGN KEY(run_id,clock_id) REFERENCES bench.clock,
  FOREIGN KEY(experiment,dictionary_version,event_id) REFERENCES bench.event_definition,
  PRIMARY KEY(run_id,source,event_sequence)
);

CREATE TABLE rbpf.just_drop_config (
  config_id uuid PRIMARY KEY,
  condition_id uuid NOT NULL,
  experiment text NOT NULL DEFAULT 'just_drop' CHECK(experiment='just_drop'),
  instrumentation text NOT NULL CHECK(instrumentation IN ('counter_only','helper_hist','stage_hist','batch_hist','switch_hist','trace')),
  profile_hash text NOT NULL CHECK(profile_hash ~ '^[0-9a-f]{64}$'),
  calibration_ids uuid[] NOT NULL DEFAULT ARRAY[]::uuid[],
  parameters jsonb NOT NULL DEFAULT '{}',
  target_pps integer CHECK(target_pps>0),
  send_window_ns bench.u64,
  drain_ns bench.u64 NOT NULL,
  payload_bytes integer NOT NULL CHECK(payload_bytes>=32),
  workload_version text NOT NULL CHECK(workload_version='drop-minimal-v1'),
  neighbor_policy text NOT NULL,
  saturation_grid boolean NOT NULL,
  FOREIGN KEY(condition_id,experiment) REFERENCES bench.condition(condition_id,experiment),
  UNIQUE(config_id,condition_id)
);
CREATE TABLE rbpf.just_drop_metrics_data (
  run_id uuid NOT NULL,
  config_id uuid NOT NULL REFERENCES rbpf.just_drop_config,
  mode text NOT NULL DEFAULT 'rbpf' CHECK(mode='rbpf'),
  experiment text NOT NULL DEFAULT 'just_drop' CHECK(experiment='just_drop'),
  variant text NOT NULL DEFAULT 'metrics' CHECK(variant='metrics'),
  baseline_snapshot_id uuid,
  final_snapshot_id uuid,
  analysis_id uuid NOT NULL,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL CHECK(jsonb_typeof(issues)='array'),
  sent_success bench.u64, ingress_seen bench.u64,
  policy_enter bench.u64, normal_drop bench.u64,
  runtime_fault bench.u64, device_window_ticks bench.u64,
  sender_limited boolean NOT NULL,
  plateau_observed boolean NOT NULL,
  PRIMARY KEY(run_id,analysis_id),
  FOREIGN KEY(run_id,mode,experiment,variant,config_id)
    REFERENCES bench.run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(baseline_snapshot_id,run_id) REFERENCES rbpf.metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(final_snapshot_id,run_id) REFERENCES rbpf.metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(analysis_id,run_id) REFERENCES bench.analysis(analysis_id,run_id),
  CHECK(NOT qualified OR (baseline_snapshot_id IS NOT NULL AND final_snapshot_id IS NOT NULL))
);
CREATE TABLE rbpf.just_drop_trace_data (
  run_id uuid PRIMARY KEY,
  config_id uuid NOT NULL REFERENCES rbpf.just_drop_config,
  mode text NOT NULL DEFAULT 'rbpf' CHECK(mode='rbpf'),
  experiment text NOT NULL DEFAULT 'just_drop' CHECK(experiment='just_drop'),
  variant text NOT NULL DEFAULT 'trace' CHECK(variant='trace'),
  epoch bigint NOT NULL CHECK(epoch>=0),
  dictionary_version integer NOT NULL CHECK(dictionary_version>=1),
  boot_id bench.u64 NOT NULL,
  clock_id text NOT NULL,
  capacity integer NOT NULL CHECK(capacity>0),
  written integer NOT NULL CHECK(written>=0 AND written<=capacity),
  dropped bigint NOT NULL CHECK(dropped>=0),
  flags bigint NOT NULL CHECK(flags>=0),
  complete boolean NOT NULL,
  result_checked boolean NOT NULL,
  result_ok boolean,
  raw_artifact_id uuid NOT NULL REFERENCES bench.artifact,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL CHECK(jsonb_typeof(issues)='array'),
  FOREIGN KEY(run_id,mode,experiment,variant,config_id)
    REFERENCES bench.run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(run_id,clock_id) REFERENCES bench.clock,
  CHECK(NOT complete OR (dropped=0 AND flags=0)),
  CHECK(NOT result_checked OR result_ok IS NOT NULL),
  CHECK(NOT qualified OR (complete AND (NOT result_checked OR result_ok)))
);
CREATE TABLE rbpf.just_drop_event (
  run_id uuid NOT NULL,
  mode text NOT NULL DEFAULT 'rbpf' CHECK(mode='rbpf'),
  experiment text NOT NULL DEFAULT 'just_drop' CHECK(experiment='just_drop'),
  source text NOT NULL CHECK(source IN ('dut','host','agent')),
  event_sequence bigint NOT NULL CHECK(event_sequence>=0),
  dictionary_version integer NOT NULL,
  event_id integer NOT NULL,
  clock_id text NOT NULL,
  timestamp_raw bench.u64 NOT NULL,
  kind text NOT NULL CHECK(kind IN ('begin','end','instant')),
  context_id integer NOT NULL CHECK(context_id BETWEEN 0 AND 255),
  probe_seq bigint CHECK(probe_seq>=1),
  span_id bigint CHECK(span_id>=0),
  parent_span_id bigint CHECK(parent_span_id>=0),
  arg0 bigint CHECK(arg0 BETWEEN 0 AND 4294967295),
  arg1 bigint CHECK(arg1 BETWEEN 0 AND 4294967295),
  FOREIGN KEY(run_id,mode,experiment) REFERENCES bench.run(run_id,mode,experiment),
  FOREIGN KEY(run_id,clock_id) REFERENCES bench.clock,
  FOREIGN KEY(experiment,dictionary_version,event_id) REFERENCES bench.event_definition,
  PRIMARY KEY(run_id,source,event_sequence)
);

CREATE TABLE rbpf.just_tx_config (
  config_id uuid PRIMARY KEY,
  condition_id uuid NOT NULL,
  experiment text NOT NULL DEFAULT 'just_tx' CHECK(experiment='just_tx'),
  instrumentation text NOT NULL CHECK(instrumentation IN ('counter_only','helper_hist','stage_hist','batch_hist','switch_hist','trace')),
  profile_hash text NOT NULL CHECK(profile_hash ~ '^[0-9a-f]{64}$'),
  calibration_ids uuid[] NOT NULL DEFAULT ARRAY[]::uuid[],
  parameters jsonb NOT NULL DEFAULT '{}',
  target_pps integer CHECK(target_pps>0),
  send_window_ns bench.u64,
  drain_ns bench.u64 NOT NULL,
  payload_bytes integer NOT NULL CHECK(payload_bytes BETWEEN 32 AND 1472),
  probe_version text NOT NULL CHECK(probe_version='JTX1'),
  capture_interface text NOT NULL,
  FOREIGN KEY(condition_id,experiment) REFERENCES bench.condition(condition_id,experiment),
  UNIQUE(config_id,condition_id)
);
CREATE TABLE rbpf.just_tx_metrics_data (
  run_id uuid NOT NULL,
  config_id uuid NOT NULL REFERENCES rbpf.just_tx_config,
  mode text NOT NULL DEFAULT 'rbpf' CHECK(mode='rbpf'),
  experiment text NOT NULL DEFAULT 'just_tx' CHECK(experiment='just_tx'),
  variant text NOT NULL DEFAULT 'metrics' CHECK(variant='metrics'),
  baseline_snapshot_id uuid,
  final_snapshot_id uuid,
  analysis_id uuid NOT NULL,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL CHECK(jsonb_typeof(issues)='array'),
  sent_success bench.u64, returned bench.u64,
  duplicate_rx bench.u64, late_rx bench.u64,
  valid_rtt_count bench.u64,
  CHECK(returned IS NULL OR sent_success IS NULL OR returned<=sent_success),
  CHECK(valid_rtt_count IS NULL OR returned IS NULL OR valid_rtt_count<=returned),
  PRIMARY KEY(run_id,analysis_id),
  FOREIGN KEY(run_id,mode,experiment,variant,config_id)
    REFERENCES bench.run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(baseline_snapshot_id,run_id) REFERENCES rbpf.metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(final_snapshot_id,run_id) REFERENCES rbpf.metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(analysis_id,run_id) REFERENCES bench.analysis(analysis_id,run_id),
  CHECK(NOT qualified OR (baseline_snapshot_id IS NOT NULL AND final_snapshot_id IS NOT NULL))
);
CREATE TABLE rbpf.just_tx_trace_data (
  run_id uuid PRIMARY KEY,
  config_id uuid NOT NULL REFERENCES rbpf.just_tx_config,
  mode text NOT NULL DEFAULT 'rbpf' CHECK(mode='rbpf'),
  experiment text NOT NULL DEFAULT 'just_tx' CHECK(experiment='just_tx'),
  variant text NOT NULL DEFAULT 'trace' CHECK(variant='trace'),
  epoch bigint NOT NULL CHECK(epoch>=0),
  dictionary_version integer NOT NULL CHECK(dictionary_version>=1),
  boot_id bench.u64 NOT NULL,
  clock_id text NOT NULL,
  capacity integer NOT NULL CHECK(capacity>0),
  written integer NOT NULL CHECK(written>=0 AND written<=capacity),
  dropped bigint NOT NULL CHECK(dropped>=0),
  flags bigint NOT NULL CHECK(flags>=0),
  complete boolean NOT NULL,
  result_checked boolean NOT NULL,
  result_ok boolean,
  raw_artifact_id uuid NOT NULL REFERENCES bench.artifact,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL CHECK(jsonb_typeof(issues)='array'),
  FOREIGN KEY(run_id,mode,experiment,variant,config_id)
    REFERENCES bench.run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(run_id,clock_id) REFERENCES bench.clock,
  CHECK(NOT complete OR (dropped=0 AND flags=0)),
  CHECK(NOT result_checked OR result_ok IS NOT NULL),
  CHECK(NOT qualified OR (complete AND (NOT result_checked OR result_ok)))
);
CREATE TABLE rbpf.just_tx_event (
  run_id uuid NOT NULL,
  mode text NOT NULL DEFAULT 'rbpf' CHECK(mode='rbpf'),
  experiment text NOT NULL DEFAULT 'just_tx' CHECK(experiment='just_tx'),
  source text NOT NULL CHECK(source IN ('dut','host','agent')),
  event_sequence bigint NOT NULL CHECK(event_sequence>=0),
  dictionary_version integer NOT NULL,
  event_id integer NOT NULL,
  clock_id text NOT NULL,
  timestamp_raw bench.u64 NOT NULL,
  kind text NOT NULL CHECK(kind IN ('begin','end','instant')),
  context_id integer NOT NULL CHECK(context_id BETWEEN 0 AND 255),
  probe_seq bigint CHECK(probe_seq>=1),
  span_id bigint CHECK(span_id>=0),
  parent_span_id bigint CHECK(parent_span_id>=0),
  arg0 bigint CHECK(arg0 BETWEEN 0 AND 4294967295),
  arg1 bigint CHECK(arg1 BETWEEN 0 AND 4294967295),
  FOREIGN KEY(run_id,mode,experiment) REFERENCES bench.run(run_id,mode,experiment),
  FOREIGN KEY(run_id,clock_id) REFERENCES bench.clock,
  FOREIGN KEY(experiment,dictionary_version,event_id) REFERENCES bench.event_definition,
  PRIMARY KEY(run_id,source,event_sequence)
);

CREATE TABLE rbpf.exec_bench_config (
  config_id uuid PRIMARY KEY,
  condition_id uuid NOT NULL,
  experiment text NOT NULL DEFAULT 'exec_bench' CHECK(experiment='exec_bench'),
  instrumentation text NOT NULL CHECK(instrumentation IN ('counter_only','helper_hist','stage_hist','batch_hist','switch_hist','trace')),
  profile_hash text NOT NULL CHECK(profile_hash ~ '^[0-9a-f]{64}$'),
  calibration_ids uuid[] NOT NULL DEFAULT ARRAY[]::uuid[],
  parameters jsonb NOT NULL DEFAULT '{}',
  workload text NOT NULL CHECK(workload IN ('incr','square','bitswap','fib','buf','memcpy','fletcher32','bsort')),
  work_param integer NOT NULL CHECK(work_param>=0),
  batch_count bigint NOT NULL CHECK(batch_count BETWEEN 1 AND 4294967295),
  samples_per_run integer NOT NULL CHECK(samples_per_run>0),
  input_bytes integer NOT NULL CHECK(input_bytes=88),
  input_protocol text NOT NULL CHECK(input_protocol='EXB2'),
  wifi_enabled boolean NOT NULL CHECK(NOT wifi_enabled),
  step_budget bench.u64,
  fixture_artifact_id uuid NOT NULL REFERENCES bench.artifact,
  FOREIGN KEY(condition_id,experiment) REFERENCES bench.condition(condition_id,experiment),
  UNIQUE(config_id,condition_id)
);
CREATE TABLE rbpf.exec_bench_metrics_data (
  run_id uuid NOT NULL,
  config_id uuid NOT NULL REFERENCES rbpf.exec_bench_config,
  mode text NOT NULL DEFAULT 'rbpf' CHECK(mode='rbpf'),
  experiment text NOT NULL DEFAULT 'exec_bench' CHECK(experiment='exec_bench'),
  variant text NOT NULL DEFAULT 'metrics' CHECK(variant='metrics'),
  baseline_snapshot_id uuid,
  final_snapshot_id uuid,
  analysis_id uuid NOT NULL,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL CHECK(jsonb_typeof(issues)='array'),
  batches_expected integer NOT NULL CHECK(batches_expected>0),
  batches_completed integer NOT NULL CHECK(batches_completed>=0),
  valid_batches integer NOT NULL CHECK(valid_batches>=0),
  CHECK(valid_batches<=batches_completed AND batches_completed<=batches_expected),
  PRIMARY KEY(run_id,analysis_id),
  FOREIGN KEY(run_id,mode,experiment,variant,config_id)
    REFERENCES bench.run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(baseline_snapshot_id,run_id) REFERENCES rbpf.metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(final_snapshot_id,run_id) REFERENCES rbpf.metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(analysis_id,run_id) REFERENCES bench.analysis(analysis_id,run_id),
  CHECK(NOT qualified OR (baseline_snapshot_id IS NOT NULL AND final_snapshot_id IS NOT NULL))
);
CREATE TABLE rbpf.exec_bench_trace_data (
  run_id uuid PRIMARY KEY,
  config_id uuid NOT NULL REFERENCES rbpf.exec_bench_config,
  mode text NOT NULL DEFAULT 'rbpf' CHECK(mode='rbpf'),
  experiment text NOT NULL DEFAULT 'exec_bench' CHECK(experiment='exec_bench'),
  variant text NOT NULL DEFAULT 'trace' CHECK(variant='trace'),
  epoch bigint NOT NULL CHECK(epoch>=0),
  dictionary_version integer NOT NULL CHECK(dictionary_version>=1),
  boot_id bench.u64 NOT NULL,
  clock_id text NOT NULL,
  capacity integer NOT NULL CHECK(capacity>0),
  written integer NOT NULL CHECK(written>=0 AND written<=capacity),
  dropped bigint NOT NULL CHECK(dropped>=0),
  flags bigint NOT NULL CHECK(flags>=0),
  complete boolean NOT NULL,
  result_checked boolean NOT NULL,
  result_ok boolean,
  raw_artifact_id uuid NOT NULL REFERENCES bench.artifact,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL CHECK(jsonb_typeof(issues)='array'),
  FOREIGN KEY(run_id,mode,experiment,variant,config_id)
    REFERENCES bench.run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(run_id,clock_id) REFERENCES bench.clock,
  CHECK(NOT complete OR (dropped=0 AND flags=0)),
  CHECK(NOT result_checked OR result_ok IS NOT NULL),
  CHECK(NOT qualified OR (complete AND (NOT result_checked OR result_ok)))
);
CREATE TABLE rbpf.exec_bench_event (
  run_id uuid NOT NULL,
  mode text NOT NULL DEFAULT 'rbpf' CHECK(mode='rbpf'),
  experiment text NOT NULL DEFAULT 'exec_bench' CHECK(experiment='exec_bench'),
  source text NOT NULL CHECK(source IN ('dut','host','agent')),
  event_sequence bigint NOT NULL CHECK(event_sequence>=0),
  dictionary_version integer NOT NULL,
  event_id integer NOT NULL,
  clock_id text NOT NULL,
  timestamp_raw bench.u64 NOT NULL,
  kind text NOT NULL CHECK(kind IN ('begin','end','instant')),
  context_id integer NOT NULL CHECK(context_id BETWEEN 0 AND 255),
  probe_seq bigint CHECK(probe_seq>=1),
  span_id bigint CHECK(span_id>=0),
  parent_span_id bigint CHECK(parent_span_id>=0),
  arg0 bigint CHECK(arg0 BETWEEN 0 AND 4294967295),
  arg1 bigint CHECK(arg1 BETWEEN 0 AND 4294967295),
  FOREIGN KEY(run_id,mode,experiment) REFERENCES bench.run(run_id,mode,experiment),
  FOREIGN KEY(run_id,clock_id) REFERENCES bench.clock,
  FOREIGN KEY(experiment,dictionary_version,event_id) REFERENCES bench.event_definition,
  PRIMARY KEY(run_id,source,event_sequence)
);

CREATE TABLE rbpf.case_config (
  config_id uuid PRIMARY KEY,
  condition_id uuid NOT NULL,
  experiment text NOT NULL DEFAULT 'case' CHECK(experiment='case'),
  instrumentation text NOT NULL CHECK(instrumentation IN ('counter_only','helper_hist','stage_hist','batch_hist','switch_hist','trace')),
  profile_hash text NOT NULL CHECK(profile_hash ~ '^[0-9a-f]{64}$'),
  calibration_ids uuid[] NOT NULL DEFAULT ARRAY[]::uuid[],
  parameters jsonb NOT NULL DEFAULT '{}',
  target_pps integer NOT NULL CHECK(target_pps>0),
  switch_count integer NOT NULL CHECK(switch_count>0),
  switch_kind text NOT NULL CHECK(switch_kind IN ('hot_publish','native_policy_select','stop_copy_start','cold_install')),
  quiesce_policy text NOT NULL CHECK(quiesce_policy IN ('drop','pass','native')),
  pre_window_ns bench.u64 NOT NULL,
  post_window_ns bench.u64 NOT NULL,
  payload_bytes integer NOT NULL CHECK(payload_bytes=40),
  source_mode text NOT NULL,
  target_mode text NOT NULL,
  FOREIGN KEY(condition_id,experiment) REFERENCES bench.condition(condition_id,experiment),
  UNIQUE(config_id,condition_id)
);
CREATE TABLE rbpf.case_metrics_data (
  run_id uuid NOT NULL,
  config_id uuid NOT NULL REFERENCES rbpf.case_config,
  mode text NOT NULL DEFAULT 'rbpf' CHECK(mode='rbpf'),
  experiment text NOT NULL DEFAULT 'case' CHECK(experiment='case'),
  variant text NOT NULL DEFAULT 'metrics' CHECK(variant='metrics'),
  baseline_snapshot_id uuid,
  final_snapshot_id uuid,
  analysis_id uuid NOT NULL,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL CHECK(jsonb_typeof(issues)='array'),
  attempted integer NOT NULL CHECK(attempted>=0),
  successful integer NOT NULL CHECK(successful>=0 AND successful<=attempted),
  boot_unchanged boolean NOT NULL,
  old_after_new bench.u64,
  PRIMARY KEY(run_id,analysis_id),
  FOREIGN KEY(run_id,mode,experiment,variant,config_id)
    REFERENCES bench.run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(baseline_snapshot_id,run_id) REFERENCES rbpf.metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(final_snapshot_id,run_id) REFERENCES rbpf.metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(analysis_id,run_id) REFERENCES bench.analysis(analysis_id,run_id),
  CHECK(NOT qualified OR (baseline_snapshot_id IS NOT NULL AND final_snapshot_id IS NOT NULL))
);
CREATE TABLE rbpf.case_trace_data (
  run_id uuid PRIMARY KEY,
  config_id uuid NOT NULL REFERENCES rbpf.case_config,
  mode text NOT NULL DEFAULT 'rbpf' CHECK(mode='rbpf'),
  experiment text NOT NULL DEFAULT 'case' CHECK(experiment='case'),
  variant text NOT NULL DEFAULT 'trace' CHECK(variant='trace'),
  epoch bigint NOT NULL CHECK(epoch>=0),
  dictionary_version integer NOT NULL CHECK(dictionary_version>=1),
  boot_id bench.u64 NOT NULL,
  clock_id text NOT NULL,
  capacity integer NOT NULL CHECK(capacity>0),
  written integer NOT NULL CHECK(written>=0 AND written<=capacity),
  dropped bigint NOT NULL CHECK(dropped>=0),
  flags bigint NOT NULL CHECK(flags>=0),
  complete boolean NOT NULL,
  result_checked boolean NOT NULL,
  result_ok boolean,
  raw_artifact_id uuid NOT NULL REFERENCES bench.artifact,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL CHECK(jsonb_typeof(issues)='array'),
  FOREIGN KEY(run_id,mode,experiment,variant,config_id)
    REFERENCES bench.run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(run_id,clock_id) REFERENCES bench.clock,
  CHECK(NOT complete OR (dropped=0 AND flags=0)),
  CHECK(NOT result_checked OR result_ok IS NOT NULL),
  CHECK(NOT qualified OR (complete AND (NOT result_checked OR result_ok)))
);
CREATE TABLE rbpf.case_event (
  run_id uuid NOT NULL,
  mode text NOT NULL DEFAULT 'rbpf' CHECK(mode='rbpf'),
  experiment text NOT NULL DEFAULT 'case' CHECK(experiment='case'),
  source text NOT NULL CHECK(source IN ('dut','host','agent')),
  event_sequence bigint NOT NULL CHECK(event_sequence>=0),
  dictionary_version integer NOT NULL,
  event_id integer NOT NULL,
  clock_id text NOT NULL,
  timestamp_raw bench.u64 NOT NULL,
  kind text NOT NULL CHECK(kind IN ('begin','end','instant')),
  context_id integer NOT NULL CHECK(context_id BETWEEN 0 AND 255),
  probe_seq bigint CHECK(probe_seq>=1),
  span_id bigint CHECK(span_id>=0),
  parent_span_id bigint CHECK(parent_span_id>=0),
  arg0 bigint CHECK(arg0 BETWEEN 0 AND 4294967295),
  arg1 bigint CHECK(arg1 BETWEEN 0 AND 4294967295),
  FOREIGN KEY(run_id,mode,experiment) REFERENCES bench.run(run_id,mode,experiment),
  FOREIGN KEY(run_id,clock_id) REFERENCES bench.clock,
  FOREIGN KEY(experiment,dictionary_version,event_id) REFERENCES bench.event_definition,
  PRIMARY KEY(run_id,source,event_sequence)
);

CREATE TABLE rbpf.probe_result (
  analysis_id uuid NOT NULL,
  run_id uuid NOT NULL,
  mode text NOT NULL DEFAULT 'rbpf' CHECK(mode='rbpf'),
  seq bigint NOT NULL,
  returned boolean NOT NULL,
  duplicate_rx bigint NOT NULL CHECK(duplicate_rx>=0),
  late_rx bigint NOT NULL CHECK(late_rx>=0),
  tx_capture_id uuid,
  tx_record_index bigint,
  rx_capture_id uuid,
  rx_record_index bigint,
  rtt_ns bench.u64,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL,
  FOREIGN KEY(run_id,mode) REFERENCES bench.run(run_id,mode),
  FOREIGN KEY(analysis_id,run_id) REFERENCES bench.analysis(analysis_id,run_id),
  FOREIGN KEY(run_id,seq) REFERENCES bench.send_attempt,
  FOREIGN KEY(tx_capture_id,run_id) REFERENCES bench.capture(capture_id,run_id),
  FOREIGN KEY(rx_capture_id,run_id) REFERENCES bench.capture(capture_id,run_id),
  FOREIGN KEY(tx_capture_id,tx_record_index) REFERENCES bench.packet_fact,
  FOREIGN KEY(rx_capture_id,rx_record_index) REFERENCES bench.packet_fact,
  PRIMARY KEY(analysis_id,seq),
  CHECK((tx_capture_id IS NULL)=(tx_record_index IS NULL)),
  CHECK((rx_capture_id IS NULL)=(rx_record_index IS NULL)),
  CHECK(rtt_ns IS NULL OR (returned AND qualified AND tx_capture_id IS NOT NULL AND rx_capture_id IS NOT NULL))
);
CREATE TABLE rbpf.firewall_token (
  run_id uuid NOT NULL,
  mode text NOT NULL DEFAULT 'rbpf' CHECK(mode='rbpf'),
  experiment text NOT NULL DEFAULT 'firewall' CHECK(experiment='firewall'),
  seq bigint NOT NULL,
  sent boolean NOT NULL,
  observed boolean NOT NULL,
  received_bytes bytea,
  FOREIGN KEY(run_id,mode,experiment) REFERENCES bench.run(run_id,mode,experiment),
  FOREIGN KEY(run_id,seq) REFERENCES bench.send_attempt,
  PRIMARY KEY(run_id,seq)
);
CREATE TABLE rbpf.exec_batch (
  run_id uuid NOT NULL,
  mode text NOT NULL DEFAULT 'rbpf' CHECK(mode='rbpf'),
  experiment text NOT NULL DEFAULT 'exec_bench' CHECK(experiment='exec_bench'),
  sample_index integer NOT NULL CHECK(sample_index>=0),
  clock_id text NOT NULL,
  begin_tick bench.u64 NOT NULL,
  end_tick bench.u64 NOT NULL,
  batch_count bigint NOT NULL CHECK(batch_count>0),
  completed_iterations bigint NOT NULL CHECK(completed_iterations>=0),
  seed bench.u64 NOT NULL,
  input_bytes bytea NOT NULL,
  expected_bytes bytea NOT NULL,
  actual_bytes bytea NOT NULL,
  result_ok boolean NOT NULL,
  qualified boolean NOT NULL,
  FOREIGN KEY(run_id,mode,experiment) REFERENCES bench.run(run_id,mode,experiment),
  FOREIGN KEY(run_id,clock_id) REFERENCES bench.clock,
  PRIMARY KEY(run_id,sample_index),
  CHECK(end_tick>=begin_tick),
  CHECK(NOT qualified OR (result_ok AND completed_iterations=batch_count))
);
CREATE TABLE rbpf.case_switch (
  run_id uuid NOT NULL,
  mode text NOT NULL DEFAULT 'rbpf' CHECK(mode='rbpf'),
  experiment text NOT NULL DEFAULT 'case' CHECK(experiment='case'),
  switch_index integer NOT NULL CHECK(switch_index>=1),
  switch_kind text NOT NULL CHECK(switch_kind IN ('hot_publish','native_policy_select','stop_copy_start','cold_install')),
  source_version integer NOT NULL CHECK(source_version IN (1,2)),
  target_version integer NOT NULL CHECK(target_version IN (1,2)),
  source_slot integer,
  target_slot integer,
  source_generation bigint,
  target_generation bigint,
  host_clock_id text NOT NULL,
  host_command_begin_ns bench.u64 NOT NULL,
  host_reply_ns bench.u64 NOT NULL,
  dut_clock_id text,
  dut_begin_tick bench.u64,
  dut_end_tick bench.u64,
  quiesce_ticks bench.u64,
  rebind_ticks bench.u64,
  publish_ticks bench.u64,
  last_old_capture_id uuid,
  last_old_record_index bigint,
  first_new_capture_id uuid,
  first_new_record_index bigint,
  reply_transition_gap_ns bench.u64,
  old_after_new bigint CHECK(old_after_new>=0),
  command_ok boolean NOT NULL,
  error_code text,
  boot_unchanged boolean NOT NULL,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL,
  FOREIGN KEY(run_id,mode,experiment) REFERENCES bench.run(run_id,mode,experiment),
  FOREIGN KEY(run_id,host_clock_id) REFERENCES bench.clock,
  FOREIGN KEY(run_id,dut_clock_id) REFERENCES bench.clock,
  FOREIGN KEY(last_old_capture_id,run_id) REFERENCES bench.capture(capture_id,run_id),
  FOREIGN KEY(first_new_capture_id,run_id) REFERENCES bench.capture(capture_id,run_id),
  FOREIGN KEY(last_old_capture_id,last_old_record_index) REFERENCES bench.packet_fact,
  FOREIGN KEY(first_new_capture_id,first_new_record_index) REFERENCES bench.packet_fact,
  PRIMARY KEY(run_id,switch_index),
  CHECK(source_version<>target_version),
  CHECK(host_reply_ns>=host_command_begin_ns),
  CHECK(dut_end_tick IS NULL OR dut_begin_tick IS NULL OR dut_end_tick>=dut_begin_tick),
  CHECK((last_old_capture_id IS NULL)=(last_old_record_index IS NULL)),
  CHECK((first_new_capture_id IS NULL)=(first_new_record_index IS NULL)),
  CHECK(reply_transition_gap_ns IS NULL OR (last_old_capture_id IS NOT NULL AND first_new_capture_id IS NOT NULL)),
  CHECK(NOT qualified OR (command_ok AND boot_unchanged))
);
ALTER TABLE rbpf.case_config ADD CHECK(switch_kind<>'native_policy_select' AND source_mode='rbpf' AND target_mode='rbpf');
ALTER TABLE rbpf.case_switch ADD CHECK(switch_kind<>'native_policy_select');

-- Append-only evidence and analysis rows. A new analysis receives a new ID.
CREATE FUNCTION bench.reject_rewrite() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
  RAISE EXCEPTION 'append-only table: %', TG_TABLE_SCHEMA || '.' || TG_TABLE_NAME USING ERRCODE='55000';
END $$;
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON bench.artifact FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON bench.send_attempt FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON bench.capture FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON bench.packet_fact FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON bench.calibration_sample FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON bench.analysis FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON hal.metric_definition FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON hal.metric_series FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON hal.metric_snapshot FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON hal.scalar_sample FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON hal.histogram_sample FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON hal.histogram_bucket FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON hal.probe_result FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON hal.firewall_token FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON hal.exec_batch FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON hal.case_switch FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON hal.firewall_metrics_data FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON hal.firewall_trace_data FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON hal.firewall_event FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON hal.just_drop_metrics_data FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON hal.just_drop_trace_data FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON hal.just_drop_event FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON hal.just_tx_metrics_data FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON hal.just_tx_trace_data FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON hal.just_tx_event FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON hal.exec_bench_metrics_data FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON hal.exec_bench_trace_data FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON hal.exec_bench_event FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON hal.case_metrics_data FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON hal.case_trace_data FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON hal.case_event FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON aot.metric_definition FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON aot.metric_series FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON aot.metric_snapshot FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON aot.scalar_sample FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON aot.histogram_sample FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON aot.histogram_bucket FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON aot.probe_result FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON aot.firewall_token FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON aot.exec_batch FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON aot.case_switch FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON aot.firewall_metrics_data FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON aot.firewall_trace_data FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON aot.firewall_event FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON aot.just_drop_metrics_data FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON aot.just_drop_trace_data FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON aot.just_drop_event FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON aot.just_tx_metrics_data FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON aot.just_tx_trace_data FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON aot.just_tx_event FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON aot.exec_bench_metrics_data FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON aot.exec_bench_trace_data FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON aot.exec_bench_event FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON aot.case_metrics_data FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON aot.case_trace_data FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON aot.case_event FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON jit.metric_definition FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON jit.metric_series FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON jit.metric_snapshot FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON jit.scalar_sample FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON jit.histogram_sample FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON jit.histogram_bucket FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON jit.probe_result FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON jit.firewall_token FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON jit.exec_batch FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON jit.case_switch FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON jit.firewall_metrics_data FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON jit.firewall_trace_data FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON jit.firewall_event FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON jit.just_drop_metrics_data FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON jit.just_drop_trace_data FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON jit.just_drop_event FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON jit.just_tx_metrics_data FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON jit.just_tx_trace_data FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON jit.just_tx_event FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON jit.exec_bench_metrics_data FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON jit.exec_bench_trace_data FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON jit.exec_bench_event FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON jit.case_metrics_data FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON jit.case_trace_data FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON jit.case_event FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON rbpf.metric_definition FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON rbpf.metric_series FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON rbpf.metric_snapshot FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON rbpf.scalar_sample FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON rbpf.histogram_sample FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON rbpf.histogram_bucket FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON rbpf.probe_result FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON rbpf.firewall_token FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON rbpf.exec_batch FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON rbpf.case_switch FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON rbpf.firewall_metrics_data FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON rbpf.firewall_trace_data FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON rbpf.firewall_event FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON rbpf.just_drop_metrics_data FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON rbpf.just_drop_trace_data FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON rbpf.just_drop_event FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON rbpf.just_tx_metrics_data FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON rbpf.just_tx_trace_data FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON rbpf.just_tx_event FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON rbpf.exec_bench_metrics_data FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON rbpf.exec_bench_trace_data FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON rbpf.exec_bench_event FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON rbpf.case_metrics_data FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON rbpf.case_trace_data FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON rbpf.case_event FOR EACH ROW EXECUTE FUNCTION bench.reject_rewrite();

-- Config resolution is checked even before result tables are written.
CREATE FUNCTION bench.check_run_config() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE ok boolean;
BEGIN
  EXECUTE format('SELECT EXISTS(SELECT 1 FROM %I.%I WHERE config_id=$1 AND condition_id=$2)',
    NEW.mode,NEW.experiment || '_config') INTO ok USING NEW.config_id,NEW.condition_id;
  IF NOT ok THEN RAISE EXCEPTION 'run config or condition mismatch'; END IF;
  RETURN NEW;
END $$;
CREATE TRIGGER run_config_guard BEFORE INSERT OR UPDATE OF config_id,condition_id,mode,experiment
  ON bench.run FOR EACH ROW EXECUTE FUNCTION bench.check_run_config();
COMMIT;
