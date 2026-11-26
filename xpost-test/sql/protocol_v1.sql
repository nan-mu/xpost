-- XPOST experiment design v1. PostgreSQL 14+.
-- Use this DDL as the Protocol V1 structure definition in release.
-- The runner reconciles managed tables before new collection.
-- Direct execution creates all objects and requires an empty release schema.
BEGIN;
CREATE DOMAIN release.bench_u64 AS numeric(20,0)
  CHECK (VALUE >= 0 AND VALUE <= 18446744073709551615);
CREATE TABLE release.bench_condition (
  condition_id uuid PRIMARY KEY,
  experiment text NOT NULL CHECK (experiment IN ('firewall','just_drop','just_tx','exec_bench','case')),
  protocol_version text NOT NULL,
  parameters jsonb NOT NULL CHECK (jsonb_typeof(parameters)='object'),
  planned_modes text[] NOT NULL DEFAULT ARRAY['hal','aot','jit','rbpf'],
  UNIQUE(condition_id, experiment)
);
CREATE TABLE release.bench_artifact (
  artifact_id uuid PRIMARY KEY,
  role text NOT NULL,
  sha256 text NOT NULL CHECK (sha256 ~ '^[0-9a-f]{64}$'),
  size_bytes bigint NOT NULL CHECK (size_bytes>=0),
  original_bytes bytea NOT NULL,
  media_type text NOT NULL,
  metadata jsonb NOT NULL DEFAULT '{}',
  CHECK (octet_length(original_bytes)=size_bytes)
);
CREATE TABLE release.bench_run (
  run_id uuid PRIMARY KEY,
  condition_id uuid NOT NULL,
  config_id uuid NOT NULL,
  mode text NOT NULL CHECK (mode IN ('hal','aot','jit','rbpf')),
  experiment text NOT NULL CHECK (experiment IN ('firewall','just_drop','just_tx','exec_bench','case')),
  variant text NOT NULL CHECK (variant IN ('metrics','trace')),
  phase text NOT NULL CHECK (phase IN ('formal','calibration','qualification','pilot')),
  repeat_no integer NOT NULL CHECK(repeat_no>=1),
  seed release.bench_u64 NOT NULL,
  plan_order integer NOT NULL CHECK(plan_order>=0),
  status text NOT NULL CHECK(status IN ('planned','running','complete','failed','unsupported','inconclusive','blocked')),
  run_uuid uuid NOT NULL UNIQUE,
  boot_id release.bench_u64,
  xpost_commit text,
  libbpf_commit text,
  toolchain jsonb NOT NULL DEFAULT '{}',
  host_send_begin_ns release.bench_u64,
  host_send_end_ns release.bench_u64,
  host_drain_end_ns release.bench_u64,
  send_elapsed_ns release.bench_u64,
  export_elapsed_ns release.bench_u64,
  issues jsonb NOT NULL DEFAULT '[]' CHECK(jsonb_typeof(issues)='array'),
  FOREIGN KEY(condition_id,experiment) REFERENCES release.bench_condition(condition_id,experiment),
  UNIQUE(run_id, mode),
  UNIQUE(run_id, mode, experiment),
  UNIQUE(run_id, mode, experiment, variant, config_id),
  CHECK(host_send_end_ns IS NULL OR host_send_begin_ns IS NULL OR host_send_end_ns>=host_send_begin_ns),
  CHECK(host_drain_end_ns IS NULL OR host_send_end_ns IS NULL OR host_drain_end_ns>=host_send_end_ns)
);
CREATE TABLE release.bench_run_artifact (
  run_id uuid NOT NULL REFERENCES release.bench_run,
  role text NOT NULL,
  artifact_id uuid NOT NULL REFERENCES release.bench_artifact,
  PRIMARY KEY(run_id,role,artifact_id)
);
CREATE TABLE release.bench_clock (
  run_id uuid NOT NULL REFERENCES release.bench_run,
  clock_id text NOT NULL,
  domain text NOT NULL CHECK(domain IN ('dut','host_monotonic','host_pcap','agent')),
  tick_hz bigint NOT NULL CHECK(tick_hz>0),
  tick_bits integer NOT NULL CHECK(tick_bits BETWEEN 1 AND 64),
  boot_id release.bench_u64,
  precision_description text NOT NULL,
  PRIMARY KEY(run_id,clock_id)
);
CREATE FUNCTION release.bench_valid_bounds(b bigint[]) RETURNS boolean
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
CREATE TABLE release.bench_calibration (
  calibration_id uuid PRIMARY KEY,
  condition_id uuid NOT NULL REFERENCES release.bench_condition,
  metric_name text NOT NULL,
  boundary text NOT NULL,
  tick_hz bigint NOT NULL CHECK(tick_hz>0),
  finite_bounds bigint[] NOT NULL CHECK(release.bench_valid_bounds(finite_bounds)),
  profile_hash text NOT NULL CHECK(profile_hash ~ '^[0-9a-f]{64}$'),
  algorithm_version text NOT NULL,
  seed release.bench_u64 NOT NULL,
  status text NOT NULL CHECK(status IN ('candidate','frozen','incomplete','failed','resolution_limited')),
  planned_modes text[] NOT NULL,
  available_modes text[] NOT NULL,
  diagnostics jsonb NOT NULL,
  source_artifact_id uuid NOT NULL REFERENCES release.bench_artifact
);
CREATE TABLE release.bench_calibration_sample (
  calibration_id uuid NOT NULL REFERENCES release.bench_calibration,
  mode text NOT NULL CHECK(mode IN ('hal','aot','jit','rbpf')),
  split text NOT NULL CHECK(split IN ('train','validation','overhead')),
  ordinal integer NOT NULL CHECK(ordinal>=0),
  duration_ticks release.bench_u64 NOT NULL,
  sample_context jsonb NOT NULL DEFAULT '{}',
  PRIMARY KEY(calibration_id,mode,split,ordinal)
);
CREATE TABLE release.bench_event_definition (
  experiment text NOT NULL,
  dictionary_version integer NOT NULL CHECK(dictionary_version>=1),
  event_id integer NOT NULL CHECK(event_id BETWEEN 0 AND 65535),
  name text NOT NULL,
  semantics text NOT NULL,
  boundary text NOT NULL,
  PRIMARY KEY(experiment,dictionary_version,event_id)
);
CREATE TABLE release.bench_send_attempt (
  run_id uuid NOT NULL REFERENCES release.bench_run,
  seq bigint NOT NULL CHECK(seq>=1),
  mono_ns release.bench_u64 NOT NULL,
  wall_ns release.bench_u64 NOT NULL,
  submitted boolean NOT NULL,
  error_code integer,
  traffic_class text CHECK(traffic_class IN ('legal','illegal','probe')),
  payload bytea NOT NULL,
  source_ip inet,
  PRIMARY KEY(run_id,seq),
  CHECK(NOT submitted OR error_code IS NULL)
);
CREATE TABLE release.bench_capture (
  capture_id uuid PRIMARY KEY,
  run_id uuid NOT NULL REFERENCES release.bench_run,
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
  artifact_id uuid NOT NULL REFERENCES release.bench_artifact,
  FOREIGN KEY(run_id,clock_id) REFERENCES release.bench_clock,
  UNIQUE(capture_id,run_id)
);
CREATE TABLE release.bench_packet_fact (
  capture_id uuid NOT NULL REFERENCES release.bench_capture,
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
CREATE TABLE release.bench_analysis (
  analysis_id uuid PRIMARY KEY,
  run_id uuid NOT NULL REFERENCES release.bench_run,
  algorithm_version text NOT NULL,
  source_digest text NOT NULL CHECK(source_digest ~ '^[0-9a-f]{64}$'),
  qualified boolean NOT NULL,
  issues jsonb NOT NULL CHECK(jsonb_typeof(issues)='array'),
  created_at timestamptz NOT NULL DEFAULT now(),
  UNIQUE(analysis_id,run_id)
);

-- hal tables use literal mode checks.
CREATE TABLE release.hal_metric_definition (
  metric_id uuid PRIMARY KEY,
  name text NOT NULL,
  kind text NOT NULL CHECK(kind IN ('counter','gauge','histogram')),
  unit text NOT NULL,
  boundary text NOT NULL,
  scope text NOT NULL,
  definition_version text NOT NULL,
  UNIQUE(metric_id,kind), UNIQUE(name,definition_version)
);
CREATE TABLE release.hal_metric_series (
  series_id uuid PRIMARY KEY,
  metric_id uuid NOT NULL,
  kind text NOT NULL,
  labels jsonb NOT NULL CHECK(jsonb_typeof(labels)='object'),
  FOREIGN KEY(metric_id,kind) REFERENCES release.hal_metric_definition(metric_id,kind),
  UNIQUE(series_id,kind), UNIQUE(metric_id,labels)
);
CREATE TABLE release.hal_metric_snapshot (
  snapshot_id uuid PRIMARY KEY,
  run_id uuid NOT NULL,
  mode text NOT NULL DEFAULT 'hal' CHECK(mode='hal'),
  phase text NOT NULL CHECK(phase IN ('baseline','final')),
  epoch bigint NOT NULL CHECK(epoch>=0),
  boot_id release.bench_u64 NOT NULL,
  clock_id text NOT NULL,
  at_tick release.bench_u64 NOT NULL,
  profile_hash text NOT NULL CHECK(profile_hash ~ '^[0-9a-f]{64}$'),
  raw_artifact_id uuid NOT NULL REFERENCES release.bench_artifact,
  FOREIGN KEY(run_id,mode) REFERENCES release.bench_run(run_id,mode),
  FOREIGN KEY(run_id,clock_id) REFERENCES release.bench_clock,
  UNIQUE(snapshot_id,run_id), UNIQUE(run_id,epoch,phase)
);
CREATE TABLE release.hal_scalar_sample (
  snapshot_id uuid NOT NULL REFERENCES release.hal_metric_snapshot,
  series_id uuid NOT NULL,
  kind text NOT NULL CHECK(kind IN ('counter','gauge')),
  value numeric NOT NULL,
  FOREIGN KEY(series_id,kind) REFERENCES release.hal_metric_series(series_id,kind),
  PRIMARY KEY(snapshot_id,series_id)
);
CREATE TABLE release.hal_histogram_sample (
  snapshot_id uuid NOT NULL REFERENCES release.hal_metric_snapshot,
  series_id uuid NOT NULL,
  kind text NOT NULL DEFAULT 'histogram' CHECK(kind='histogram'),
  calibration_id uuid NOT NULL REFERENCES release.bench_calibration,
  sample_count release.bench_u64 NOT NULL,
  sample_sum release.bench_u64 NOT NULL,
  FOREIGN KEY(series_id,kind) REFERENCES release.hal_metric_series(series_id,kind),
  PRIMARY KEY(snapshot_id,series_id),
  UNIQUE(snapshot_id,series_id,sample_count)
);
CREATE TABLE release.hal_histogram_bucket (
  snapshot_id uuid NOT NULL,
  series_id uuid NOT NULL,
  ordinal integer NOT NULL CHECK(ordinal>=1),
  upper_bound bigint CHECK(upper_bound>=0), -- NULL represents +Inf.
  cumulative_count release.bench_u64 NOT NULL,
  sample_count release.bench_u64 NOT NULL,
  FOREIGN KEY(snapshot_id,series_id,sample_count)
    REFERENCES release.hal_histogram_sample(snapshot_id,series_id,sample_count),
  PRIMARY KEY(snapshot_id,series_id,ordinal),
  CHECK(cumulative_count<=sample_count),
  CHECK(upper_bound IS NOT NULL OR cumulative_count=sample_count)
);
CREATE UNIQUE INDEX hal_hist_one_inf ON release.hal_histogram_bucket(snapshot_id,series_id)
  WHERE upper_bound IS NULL;

CREATE TABLE release.hal_firewall_config (
  config_id uuid PRIMARY KEY,
  condition_id uuid NOT NULL,
  experiment text NOT NULL DEFAULT 'firewall' CHECK(experiment='firewall'),
  instrumentation text NOT NULL CHECK(instrumentation IN ('counter_only','helper_hist','stage_hist','batch_hist','switch_hist','trace')),
  profile_hash text NOT NULL CHECK(profile_hash ~ '^[0-9a-f]{64}$'),
  calibration_ids uuid[] NOT NULL DEFAULT ARRAY[]::uuid[],
  parameters jsonb NOT NULL DEFAULT '{}',
  target_pps integer CHECK(target_pps>0),
  send_window_ns release.bench_u64,
  drain_ns release.bench_u64 NOT NULL,
  payload_bytes integer NOT NULL CHECK(payload_bytes>=32),
  blacklist_capacity integer NOT NULL CHECK(blacklist_capacity=256),
  blacklist_entries integer NOT NULL CHECK(blacklist_entries BETWEEN 1 AND 256),
  legal_numerator integer NOT NULL CHECK(legal_numerator BETWEEN 0 AND 10),
  legal_denominator integer NOT NULL CHECK(legal_denominator=10),
  lookup_case text NOT NULL CHECK(lookup_case IN ('mixed','hit','miss')),
  map_fixture_artifact_id uuid NOT NULL REFERENCES release.bench_artifact,
  FOREIGN KEY(condition_id,experiment) REFERENCES release.bench_condition(condition_id,experiment),
  UNIQUE(config_id,condition_id)
);
CREATE TABLE release.hal_firewall_metrics_data (
  run_id uuid NOT NULL,
  config_id uuid NOT NULL REFERENCES release.hal_firewall_config,
  mode text NOT NULL DEFAULT 'hal' CHECK(mode='hal'),
  experiment text NOT NULL DEFAULT 'firewall' CHECK(experiment='firewall'),
  variant text NOT NULL DEFAULT 'metrics' CHECK(variant='metrics'),
  baseline_snapshot_id uuid,
  final_snapshot_id uuid,
  analysis_id uuid NOT NULL,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL CHECK(jsonb_typeof(issues)='array'),
  sent_legal release.bench_u64, sent_illegal release.bench_u64,
  matched_legal release.bench_u64, blocked_hit_delta release.bench_u64,
  token_refused release.bench_u64,
  CHECK(matched_legal IS NULL OR sent_legal IS NULL OR matched_legal<=sent_legal),
  PRIMARY KEY(run_id,analysis_id),
  FOREIGN KEY(run_id,mode,experiment,variant,config_id)
    REFERENCES release.bench_run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(baseline_snapshot_id,run_id) REFERENCES release.hal_metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(final_snapshot_id,run_id) REFERENCES release.hal_metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(analysis_id,run_id) REFERENCES release.bench_analysis(analysis_id,run_id),
  CHECK(NOT qualified OR (baseline_snapshot_id IS NOT NULL AND final_snapshot_id IS NOT NULL))
);
CREATE TABLE release.hal_firewall_trace_data (
  run_id uuid PRIMARY KEY,
  config_id uuid NOT NULL REFERENCES release.hal_firewall_config,
  mode text NOT NULL DEFAULT 'hal' CHECK(mode='hal'),
  experiment text NOT NULL DEFAULT 'firewall' CHECK(experiment='firewall'),
  variant text NOT NULL DEFAULT 'trace' CHECK(variant='trace'),
  epoch bigint NOT NULL CHECK(epoch>=0),
  dictionary_version integer NOT NULL CHECK(dictionary_version>=1),
  boot_id release.bench_u64 NOT NULL,
  clock_id text NOT NULL,
  capacity integer NOT NULL CHECK(capacity>0),
  written integer NOT NULL CHECK(written>=0 AND written<=capacity),
  dropped bigint NOT NULL CHECK(dropped>=0),
  flags bigint NOT NULL CHECK(flags>=0),
  complete boolean NOT NULL,
  result_checked boolean NOT NULL,
  result_ok boolean,
  raw_artifact_id uuid NOT NULL REFERENCES release.bench_artifact,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL CHECK(jsonb_typeof(issues)='array'),
  FOREIGN KEY(run_id,mode,experiment,variant,config_id)
    REFERENCES release.bench_run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(run_id,clock_id) REFERENCES release.bench_clock,
  CHECK(NOT complete OR (dropped=0 AND flags=0)),
  CHECK(NOT result_checked OR result_ok IS NOT NULL),
  CHECK(NOT qualified OR (complete AND (NOT result_checked OR result_ok)))
);
CREATE TABLE release.hal_firewall_event (
  run_id uuid NOT NULL,
  mode text NOT NULL DEFAULT 'hal' CHECK(mode='hal'),
  experiment text NOT NULL DEFAULT 'firewall' CHECK(experiment='firewall'),
  source text NOT NULL CHECK(source IN ('dut','host','agent')),
  event_sequence bigint NOT NULL CHECK(event_sequence>=0),
  dictionary_version integer NOT NULL,
  event_id integer NOT NULL,
  clock_id text NOT NULL,
  timestamp_raw release.bench_u64 NOT NULL,
  kind text NOT NULL CHECK(kind IN ('begin','end','instant')),
  context_id integer NOT NULL CHECK(context_id BETWEEN 0 AND 255),
  probe_seq bigint CHECK(probe_seq>=1),
  span_id bigint CHECK(span_id>=0),
  parent_span_id bigint CHECK(parent_span_id>=0),
  arg0 bigint CHECK(arg0 BETWEEN 0 AND 4294967295),
  arg1 bigint CHECK(arg1 BETWEEN 0 AND 4294967295),
  FOREIGN KEY(run_id,mode,experiment) REFERENCES release.bench_run(run_id,mode,experiment),
  FOREIGN KEY(run_id,clock_id) REFERENCES release.bench_clock,
  FOREIGN KEY(experiment,dictionary_version,event_id) REFERENCES release.bench_event_definition,
  PRIMARY KEY(run_id,source,event_sequence)
);

CREATE TABLE release.hal_just_drop_config (
  config_id uuid PRIMARY KEY,
  condition_id uuid NOT NULL,
  experiment text NOT NULL DEFAULT 'just_drop' CHECK(experiment='just_drop'),
  instrumentation text NOT NULL CHECK(instrumentation IN ('counter_only','helper_hist','stage_hist','batch_hist','switch_hist','trace')),
  profile_hash text NOT NULL CHECK(profile_hash ~ '^[0-9a-f]{64}$'),
  calibration_ids uuid[] NOT NULL DEFAULT ARRAY[]::uuid[],
  parameters jsonb NOT NULL DEFAULT '{}',
  target_pps integer CHECK(target_pps>0),
  send_window_ns release.bench_u64,
  drain_ns release.bench_u64 NOT NULL,
  payload_bytes integer NOT NULL CHECK(payload_bytes>=32),
  workload_version text NOT NULL CHECK(workload_version='drop-minimal-v1'),
  neighbor_policy text NOT NULL,
  saturation_grid boolean NOT NULL,
  FOREIGN KEY(condition_id,experiment) REFERENCES release.bench_condition(condition_id,experiment),
  UNIQUE(config_id,condition_id)
);
CREATE TABLE release.hal_just_drop_metrics_data (
  run_id uuid NOT NULL,
  config_id uuid NOT NULL REFERENCES release.hal_just_drop_config,
  mode text NOT NULL DEFAULT 'hal' CHECK(mode='hal'),
  experiment text NOT NULL DEFAULT 'just_drop' CHECK(experiment='just_drop'),
  variant text NOT NULL DEFAULT 'metrics' CHECK(variant='metrics'),
  baseline_snapshot_id uuid,
  final_snapshot_id uuid,
  analysis_id uuid NOT NULL,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL CHECK(jsonb_typeof(issues)='array'),
  sent_success release.bench_u64, ingress_seen release.bench_u64,
  policy_enter release.bench_u64, normal_drop release.bench_u64,
  runtime_fault release.bench_u64, device_window_ticks release.bench_u64,
  sender_limited boolean NOT NULL,
  plateau_observed boolean NOT NULL,
  PRIMARY KEY(run_id,analysis_id),
  FOREIGN KEY(run_id,mode,experiment,variant,config_id)
    REFERENCES release.bench_run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(baseline_snapshot_id,run_id) REFERENCES release.hal_metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(final_snapshot_id,run_id) REFERENCES release.hal_metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(analysis_id,run_id) REFERENCES release.bench_analysis(analysis_id,run_id),
  CHECK(NOT qualified OR (baseline_snapshot_id IS NOT NULL AND final_snapshot_id IS NOT NULL))
);
CREATE TABLE release.hal_just_drop_trace_data (
  run_id uuid PRIMARY KEY,
  config_id uuid NOT NULL REFERENCES release.hal_just_drop_config,
  mode text NOT NULL DEFAULT 'hal' CHECK(mode='hal'),
  experiment text NOT NULL DEFAULT 'just_drop' CHECK(experiment='just_drop'),
  variant text NOT NULL DEFAULT 'trace' CHECK(variant='trace'),
  epoch bigint NOT NULL CHECK(epoch>=0),
  dictionary_version integer NOT NULL CHECK(dictionary_version>=1),
  boot_id release.bench_u64 NOT NULL,
  clock_id text NOT NULL,
  capacity integer NOT NULL CHECK(capacity>0),
  written integer NOT NULL CHECK(written>=0 AND written<=capacity),
  dropped bigint NOT NULL CHECK(dropped>=0),
  flags bigint NOT NULL CHECK(flags>=0),
  complete boolean NOT NULL,
  result_checked boolean NOT NULL,
  result_ok boolean,
  raw_artifact_id uuid NOT NULL REFERENCES release.bench_artifact,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL CHECK(jsonb_typeof(issues)='array'),
  FOREIGN KEY(run_id,mode,experiment,variant,config_id)
    REFERENCES release.bench_run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(run_id,clock_id) REFERENCES release.bench_clock,
  CHECK(NOT complete OR (dropped=0 AND flags=0)),
  CHECK(NOT result_checked OR result_ok IS NOT NULL),
  CHECK(NOT qualified OR (complete AND (NOT result_checked OR result_ok)))
);
CREATE TABLE release.hal_just_drop_event (
  run_id uuid NOT NULL,
  mode text NOT NULL DEFAULT 'hal' CHECK(mode='hal'),
  experiment text NOT NULL DEFAULT 'just_drop' CHECK(experiment='just_drop'),
  source text NOT NULL CHECK(source IN ('dut','host','agent')),
  event_sequence bigint NOT NULL CHECK(event_sequence>=0),
  dictionary_version integer NOT NULL,
  event_id integer NOT NULL,
  clock_id text NOT NULL,
  timestamp_raw release.bench_u64 NOT NULL,
  kind text NOT NULL CHECK(kind IN ('begin','end','instant')),
  context_id integer NOT NULL CHECK(context_id BETWEEN 0 AND 255),
  probe_seq bigint CHECK(probe_seq>=1),
  span_id bigint CHECK(span_id>=0),
  parent_span_id bigint CHECK(parent_span_id>=0),
  arg0 bigint CHECK(arg0 BETWEEN 0 AND 4294967295),
  arg1 bigint CHECK(arg1 BETWEEN 0 AND 4294967295),
  FOREIGN KEY(run_id,mode,experiment) REFERENCES release.bench_run(run_id,mode,experiment),
  FOREIGN KEY(run_id,clock_id) REFERENCES release.bench_clock,
  FOREIGN KEY(experiment,dictionary_version,event_id) REFERENCES release.bench_event_definition,
  PRIMARY KEY(run_id,source,event_sequence)
);

CREATE TABLE release.hal_just_tx_config (
  config_id uuid PRIMARY KEY,
  condition_id uuid NOT NULL,
  experiment text NOT NULL DEFAULT 'just_tx' CHECK(experiment='just_tx'),
  instrumentation text NOT NULL CHECK(instrumentation IN ('counter_only','helper_hist','stage_hist','batch_hist','switch_hist','trace')),
  profile_hash text NOT NULL CHECK(profile_hash ~ '^[0-9a-f]{64}$'),
  calibration_ids uuid[] NOT NULL DEFAULT ARRAY[]::uuid[],
  parameters jsonb NOT NULL DEFAULT '{}',
  target_pps integer CHECK(target_pps>0),
  send_window_ns release.bench_u64,
  drain_ns release.bench_u64 NOT NULL,
  payload_bytes integer NOT NULL CHECK(payload_bytes BETWEEN 32 AND 1472),
  probe_version text NOT NULL CHECK(probe_version='JTX1'),
  capture_interface text NOT NULL,
  FOREIGN KEY(condition_id,experiment) REFERENCES release.bench_condition(condition_id,experiment),
  UNIQUE(config_id,condition_id)
);
CREATE TABLE release.hal_just_tx_metrics_data (
  run_id uuid NOT NULL,
  config_id uuid NOT NULL REFERENCES release.hal_just_tx_config,
  mode text NOT NULL DEFAULT 'hal' CHECK(mode='hal'),
  experiment text NOT NULL DEFAULT 'just_tx' CHECK(experiment='just_tx'),
  variant text NOT NULL DEFAULT 'metrics' CHECK(variant='metrics'),
  baseline_snapshot_id uuid,
  final_snapshot_id uuid,
  analysis_id uuid NOT NULL,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL CHECK(jsonb_typeof(issues)='array'),
  sent_success release.bench_u64, returned release.bench_u64,
  duplicate_rx release.bench_u64, late_rx release.bench_u64,
  valid_rtt_count release.bench_u64,
  CHECK(returned IS NULL OR sent_success IS NULL OR returned<=sent_success),
  CHECK(valid_rtt_count IS NULL OR returned IS NULL OR valid_rtt_count<=returned),
  PRIMARY KEY(run_id,analysis_id),
  FOREIGN KEY(run_id,mode,experiment,variant,config_id)
    REFERENCES release.bench_run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(baseline_snapshot_id,run_id) REFERENCES release.hal_metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(final_snapshot_id,run_id) REFERENCES release.hal_metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(analysis_id,run_id) REFERENCES release.bench_analysis(analysis_id,run_id),
  CHECK(NOT qualified OR (baseline_snapshot_id IS NOT NULL AND final_snapshot_id IS NOT NULL))
);
CREATE TABLE release.hal_just_tx_trace_data (
  run_id uuid PRIMARY KEY,
  config_id uuid NOT NULL REFERENCES release.hal_just_tx_config,
  mode text NOT NULL DEFAULT 'hal' CHECK(mode='hal'),
  experiment text NOT NULL DEFAULT 'just_tx' CHECK(experiment='just_tx'),
  variant text NOT NULL DEFAULT 'trace' CHECK(variant='trace'),
  epoch bigint NOT NULL CHECK(epoch>=0),
  dictionary_version integer NOT NULL CHECK(dictionary_version>=1),
  boot_id release.bench_u64 NOT NULL,
  clock_id text NOT NULL,
  capacity integer NOT NULL CHECK(capacity>0),
  written integer NOT NULL CHECK(written>=0 AND written<=capacity),
  dropped bigint NOT NULL CHECK(dropped>=0),
  flags bigint NOT NULL CHECK(flags>=0),
  complete boolean NOT NULL,
  result_checked boolean NOT NULL,
  result_ok boolean,
  raw_artifact_id uuid NOT NULL REFERENCES release.bench_artifact,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL CHECK(jsonb_typeof(issues)='array'),
  FOREIGN KEY(run_id,mode,experiment,variant,config_id)
    REFERENCES release.bench_run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(run_id,clock_id) REFERENCES release.bench_clock,
  CHECK(NOT complete OR (dropped=0 AND flags=0)),
  CHECK(NOT result_checked OR result_ok IS NOT NULL),
  CHECK(NOT qualified OR (complete AND (NOT result_checked OR result_ok)))
);
CREATE TABLE release.hal_just_tx_event (
  run_id uuid NOT NULL,
  mode text NOT NULL DEFAULT 'hal' CHECK(mode='hal'),
  experiment text NOT NULL DEFAULT 'just_tx' CHECK(experiment='just_tx'),
  source text NOT NULL CHECK(source IN ('dut','host','agent')),
  event_sequence bigint NOT NULL CHECK(event_sequence>=0),
  dictionary_version integer NOT NULL,
  event_id integer NOT NULL,
  clock_id text NOT NULL,
  timestamp_raw release.bench_u64 NOT NULL,
  kind text NOT NULL CHECK(kind IN ('begin','end','instant')),
  context_id integer NOT NULL CHECK(context_id BETWEEN 0 AND 255),
  probe_seq bigint CHECK(probe_seq>=1),
  span_id bigint CHECK(span_id>=0),
  parent_span_id bigint CHECK(parent_span_id>=0),
  arg0 bigint CHECK(arg0 BETWEEN 0 AND 4294967295),
  arg1 bigint CHECK(arg1 BETWEEN 0 AND 4294967295),
  FOREIGN KEY(run_id,mode,experiment) REFERENCES release.bench_run(run_id,mode,experiment),
  FOREIGN KEY(run_id,clock_id) REFERENCES release.bench_clock,
  FOREIGN KEY(experiment,dictionary_version,event_id) REFERENCES release.bench_event_definition,
  PRIMARY KEY(run_id,source,event_sequence)
);

CREATE TABLE release.hal_exec_bench_config (
  config_id uuid PRIMARY KEY,
  condition_id uuid NOT NULL,
  experiment text NOT NULL DEFAULT 'exec_bench' CHECK(experiment='exec_bench'),
  instrumentation text NOT NULL CHECK(instrumentation IN ('counter_only','helper_hist','stage_hist','batch_hist','switch_hist','trace')),
  profile_hash text NOT NULL CHECK(profile_hash ~ '^[0-9a-f]{64}$'),
  calibration_ids uuid[] NOT NULL DEFAULT ARRAY[]::uuid[],
  parameters jsonb NOT NULL DEFAULT '{}',
  workload text NOT NULL CHECK(workload IN ('noop','prime','log2','copy','fletcher32','bsort','strcmp')),
  work_param integer NOT NULL CHECK(work_param>=0),
  batch_count bigint NOT NULL CHECK(batch_count BETWEEN 1 AND 65536),
  samples_per_run integer NOT NULL CHECK(samples_per_run>0),
  input_bytes integer NOT NULL CHECK(input_bytes=8258),
  input_protocol text NOT NULL CHECK(input_protocol='EXB2'),
  wifi_enabled boolean NOT NULL CHECK(NOT wifi_enabled),
  step_budget release.bench_u64,
  fixture_artifact_id uuid NOT NULL REFERENCES release.bench_artifact,
  FOREIGN KEY(condition_id,experiment) REFERENCES release.bench_condition(condition_id,experiment),
  UNIQUE(config_id,condition_id)
);
CREATE TABLE release.hal_exec_bench_metrics_data (
  run_id uuid NOT NULL,
  config_id uuid NOT NULL REFERENCES release.hal_exec_bench_config,
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
    REFERENCES release.bench_run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(baseline_snapshot_id,run_id) REFERENCES release.hal_metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(final_snapshot_id,run_id) REFERENCES release.hal_metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(analysis_id,run_id) REFERENCES release.bench_analysis(analysis_id,run_id),
  CHECK(NOT qualified OR (baseline_snapshot_id IS NOT NULL AND final_snapshot_id IS NOT NULL))
);
CREATE TABLE release.hal_exec_bench_trace_data (
  run_id uuid PRIMARY KEY,
  config_id uuid NOT NULL REFERENCES release.hal_exec_bench_config,
  mode text NOT NULL DEFAULT 'hal' CHECK(mode='hal'),
  experiment text NOT NULL DEFAULT 'exec_bench' CHECK(experiment='exec_bench'),
  variant text NOT NULL DEFAULT 'trace' CHECK(variant='trace'),
  epoch bigint NOT NULL CHECK(epoch>=0),
  dictionary_version integer NOT NULL CHECK(dictionary_version>=1),
  boot_id release.bench_u64 NOT NULL,
  clock_id text NOT NULL,
  capacity integer NOT NULL CHECK(capacity>0),
  written integer NOT NULL CHECK(written>=0 AND written<=capacity),
  dropped bigint NOT NULL CHECK(dropped>=0),
  flags bigint NOT NULL CHECK(flags>=0),
  complete boolean NOT NULL,
  result_checked boolean NOT NULL,
  result_ok boolean,
  raw_artifact_id uuid NOT NULL REFERENCES release.bench_artifact,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL CHECK(jsonb_typeof(issues)='array'),
  FOREIGN KEY(run_id,mode,experiment,variant,config_id)
    REFERENCES release.bench_run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(run_id,clock_id) REFERENCES release.bench_clock,
  CHECK(NOT complete OR (dropped=0 AND flags=0)),
  CHECK(NOT result_checked OR result_ok IS NOT NULL),
  CHECK(NOT qualified OR (complete AND (NOT result_checked OR result_ok)))
);
CREATE TABLE release.hal_exec_bench_event (
  run_id uuid NOT NULL,
  mode text NOT NULL DEFAULT 'hal' CHECK(mode='hal'),
  experiment text NOT NULL DEFAULT 'exec_bench' CHECK(experiment='exec_bench'),
  source text NOT NULL CHECK(source IN ('dut','host','agent')),
  event_sequence bigint NOT NULL CHECK(event_sequence>=0),
  dictionary_version integer NOT NULL,
  event_id integer NOT NULL,
  clock_id text NOT NULL,
  timestamp_raw release.bench_u64 NOT NULL,
  kind text NOT NULL CHECK(kind IN ('begin','end','instant')),
  context_id integer NOT NULL CHECK(context_id BETWEEN 0 AND 255),
  probe_seq bigint CHECK(probe_seq>=1),
  span_id bigint CHECK(span_id>=0),
  parent_span_id bigint CHECK(parent_span_id>=0),
  arg0 bigint CHECK(arg0 BETWEEN 0 AND 4294967295),
  arg1 bigint CHECK(arg1 BETWEEN 0 AND 4294967295),
  FOREIGN KEY(run_id,mode,experiment) REFERENCES release.bench_run(run_id,mode,experiment),
  FOREIGN KEY(run_id,clock_id) REFERENCES release.bench_clock,
  FOREIGN KEY(experiment,dictionary_version,event_id) REFERENCES release.bench_event_definition,
  PRIMARY KEY(run_id,source,event_sequence)
);

CREATE TABLE release.hal_case_config (
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
  pre_window_ns release.bench_u64 NOT NULL,
  post_window_ns release.bench_u64 NOT NULL,
  payload_bytes integer NOT NULL CHECK(payload_bytes=40),
  source_mode text NOT NULL,
  target_mode text NOT NULL,
  FOREIGN KEY(condition_id,experiment) REFERENCES release.bench_condition(condition_id,experiment),
  UNIQUE(config_id,condition_id)
);
CREATE TABLE release.hal_case_metrics_data (
  run_id uuid NOT NULL,
  config_id uuid NOT NULL REFERENCES release.hal_case_config,
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
  old_after_new release.bench_u64,
  PRIMARY KEY(run_id,analysis_id),
  FOREIGN KEY(run_id,mode,experiment,variant,config_id)
    REFERENCES release.bench_run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(baseline_snapshot_id,run_id) REFERENCES release.hal_metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(final_snapshot_id,run_id) REFERENCES release.hal_metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(analysis_id,run_id) REFERENCES release.bench_analysis(analysis_id,run_id),
  CHECK(NOT qualified OR (baseline_snapshot_id IS NOT NULL AND final_snapshot_id IS NOT NULL))
);
CREATE TABLE release.hal_case_trace_data (
  run_id uuid PRIMARY KEY,
  config_id uuid NOT NULL REFERENCES release.hal_case_config,
  mode text NOT NULL DEFAULT 'hal' CHECK(mode='hal'),
  experiment text NOT NULL DEFAULT 'case' CHECK(experiment='case'),
  variant text NOT NULL DEFAULT 'trace' CHECK(variant='trace'),
  epoch bigint NOT NULL CHECK(epoch>=0),
  dictionary_version integer NOT NULL CHECK(dictionary_version>=1),
  boot_id release.bench_u64 NOT NULL,
  clock_id text NOT NULL,
  capacity integer NOT NULL CHECK(capacity>0),
  written integer NOT NULL CHECK(written>=0 AND written<=capacity),
  dropped bigint NOT NULL CHECK(dropped>=0),
  flags bigint NOT NULL CHECK(flags>=0),
  complete boolean NOT NULL,
  result_checked boolean NOT NULL,
  result_ok boolean,
  raw_artifact_id uuid NOT NULL REFERENCES release.bench_artifact,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL CHECK(jsonb_typeof(issues)='array'),
  FOREIGN KEY(run_id,mode,experiment,variant,config_id)
    REFERENCES release.bench_run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(run_id,clock_id) REFERENCES release.bench_clock,
  CHECK(NOT complete OR (dropped=0 AND flags=0)),
  CHECK(NOT result_checked OR result_ok IS NOT NULL),
  CHECK(NOT qualified OR (complete AND (NOT result_checked OR result_ok)))
);
CREATE TABLE release.hal_case_event (
  run_id uuid NOT NULL,
  mode text NOT NULL DEFAULT 'hal' CHECK(mode='hal'),
  experiment text NOT NULL DEFAULT 'case' CHECK(experiment='case'),
  source text NOT NULL CHECK(source IN ('dut','host','agent')),
  event_sequence bigint NOT NULL CHECK(event_sequence>=0),
  dictionary_version integer NOT NULL,
  event_id integer NOT NULL,
  clock_id text NOT NULL,
  timestamp_raw release.bench_u64 NOT NULL,
  kind text NOT NULL CHECK(kind IN ('begin','end','instant')),
  context_id integer NOT NULL CHECK(context_id BETWEEN 0 AND 255),
  probe_seq bigint CHECK(probe_seq>=1),
  span_id bigint CHECK(span_id>=0),
  parent_span_id bigint CHECK(parent_span_id>=0),
  arg0 bigint CHECK(arg0 BETWEEN 0 AND 4294967295),
  arg1 bigint CHECK(arg1 BETWEEN 0 AND 4294967295),
  FOREIGN KEY(run_id,mode,experiment) REFERENCES release.bench_run(run_id,mode,experiment),
  FOREIGN KEY(run_id,clock_id) REFERENCES release.bench_clock,
  FOREIGN KEY(experiment,dictionary_version,event_id) REFERENCES release.bench_event_definition,
  PRIMARY KEY(run_id,source,event_sequence)
);

CREATE TABLE release.hal_probe_result (
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
  rtt_ns release.bench_u64,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL,
  FOREIGN KEY(run_id,mode) REFERENCES release.bench_run(run_id,mode),
  FOREIGN KEY(analysis_id,run_id) REFERENCES release.bench_analysis(analysis_id,run_id),
  FOREIGN KEY(run_id,seq) REFERENCES release.bench_send_attempt,
  FOREIGN KEY(tx_capture_id,run_id) REFERENCES release.bench_capture(capture_id,run_id),
  FOREIGN KEY(rx_capture_id,run_id) REFERENCES release.bench_capture(capture_id,run_id),
  FOREIGN KEY(tx_capture_id,tx_record_index) REFERENCES release.bench_packet_fact,
  FOREIGN KEY(rx_capture_id,rx_record_index) REFERENCES release.bench_packet_fact,
  PRIMARY KEY(analysis_id,seq),
  CHECK((tx_capture_id IS NULL)=(tx_record_index IS NULL)),
  CHECK((rx_capture_id IS NULL)=(rx_record_index IS NULL)),
  CHECK(rtt_ns IS NULL OR (returned AND qualified AND tx_capture_id IS NOT NULL AND rx_capture_id IS NOT NULL))
);
CREATE TABLE release.hal_firewall_token (
  run_id uuid NOT NULL,
  mode text NOT NULL DEFAULT 'hal' CHECK(mode='hal'),
  experiment text NOT NULL DEFAULT 'firewall' CHECK(experiment='firewall'),
  seq bigint NOT NULL,
  sent boolean NOT NULL,
  observed boolean NOT NULL,
  received_bytes bytea,
  FOREIGN KEY(run_id,mode,experiment) REFERENCES release.bench_run(run_id,mode,experiment),
  FOREIGN KEY(run_id,seq) REFERENCES release.bench_send_attempt,
  PRIMARY KEY(run_id,seq)
);
CREATE TABLE release.hal_exec_batch (
  run_id uuid NOT NULL,
  mode text NOT NULL DEFAULT 'hal' CHECK(mode='hal'),
  experiment text NOT NULL DEFAULT 'exec_bench' CHECK(experiment='exec_bench'),
  sample_index integer NOT NULL CHECK(sample_index>=0),
  clock_id text NOT NULL,
  begin_tick release.bench_u64 NOT NULL,
  end_tick release.bench_u64 NOT NULL,
  batch_count bigint NOT NULL CHECK(batch_count BETWEEN 1 AND 65536),
  completed_iterations bigint NOT NULL CHECK(completed_iterations BETWEEN 0 AND batch_count),
  seed release.bench_u64 NOT NULL,
  input_bytes bytea NOT NULL CHECK(octet_length(input_bytes)=8258),
  expected_bytes bytea NOT NULL CHECK(octet_length(expected_bytes)=8258),
  actual_bytes bytea NOT NULL CHECK(octet_length(actual_bytes)=8258),
  execution_return bigint NOT NULL CHECK(execution_return BETWEEN 0 AND 4294967295),
  status bigint NOT NULL CHECK(status BETWEEN 0 AND 4294967295),
  result release.bench_u64 NOT NULL,
  digest bigint NOT NULL CHECK(digest BETWEEN 0 AND 4294967295),
  result_ok boolean NOT NULL,
  qualified boolean NOT NULL,
  FOREIGN KEY(run_id,mode,experiment) REFERENCES release.bench_run(run_id,mode,experiment),
  FOREIGN KEY(run_id,clock_id) REFERENCES release.bench_clock,
  PRIMARY KEY(run_id,sample_index),
  CHECK(end_tick>=begin_tick),
  CHECK(NOT result_ok OR actual_bytes=expected_bytes),
  CHECK(NOT qualified OR (
    result_ok AND status=0 AND execution_return=0
    AND completed_iterations=batch_count
  ))
);
CREATE TABLE release.hal_case_switch (
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
  host_command_begin_ns release.bench_u64 NOT NULL,
  host_reply_ns release.bench_u64 NOT NULL,
  dut_clock_id text,
  dut_begin_tick release.bench_u64,
  dut_end_tick release.bench_u64,
  quiesce_ticks release.bench_u64,
  rebind_ticks release.bench_u64,
  publish_ticks release.bench_u64,
  last_old_capture_id uuid,
  last_old_record_index bigint,
  first_new_capture_id uuid,
  first_new_record_index bigint,
  reply_transition_gap_ns release.bench_u64,
  old_after_new bigint CHECK(old_after_new>=0),
  command_ok boolean NOT NULL,
  error_code text,
  boot_unchanged boolean NOT NULL,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL,
  FOREIGN KEY(run_id,mode,experiment) REFERENCES release.bench_run(run_id,mode,experiment),
  FOREIGN KEY(run_id,host_clock_id) REFERENCES release.bench_clock,
  FOREIGN KEY(run_id,dut_clock_id) REFERENCES release.bench_clock,
  FOREIGN KEY(last_old_capture_id,run_id) REFERENCES release.bench_capture(capture_id,run_id),
  FOREIGN KEY(first_new_capture_id,run_id) REFERENCES release.bench_capture(capture_id,run_id),
  FOREIGN KEY(last_old_capture_id,last_old_record_index) REFERENCES release.bench_packet_fact,
  FOREIGN KEY(first_new_capture_id,first_new_record_index) REFERENCES release.bench_packet_fact,
  PRIMARY KEY(run_id,switch_index),
  CHECK(source_version<>target_version),
  CHECK(host_reply_ns>=host_command_begin_ns),
  CHECK(dut_end_tick IS NULL OR dut_begin_tick IS NULL OR dut_end_tick>=dut_begin_tick),
  CHECK((last_old_capture_id IS NULL)=(last_old_record_index IS NULL)),
  CHECK((first_new_capture_id IS NULL)=(first_new_record_index IS NULL)),
  CHECK(reply_transition_gap_ns IS NULL OR (last_old_capture_id IS NOT NULL AND first_new_capture_id IS NOT NULL)),
  CHECK(NOT qualified OR (command_ok AND boot_unchanged))
);
ALTER TABLE release.hal_case_config ADD CHECK(switch_kind='native_policy_select' AND quiesce_policy='native');
ALTER TABLE release.hal_case_switch ADD CHECK(switch_kind='native_policy_select' AND source_slot IS NULL AND target_slot IS NULL AND rebind_ticks IS NULL);

-- aot tables use literal mode checks.
CREATE TABLE release.aot_metric_definition (
  metric_id uuid PRIMARY KEY,
  name text NOT NULL,
  kind text NOT NULL CHECK(kind IN ('counter','gauge','histogram')),
  unit text NOT NULL,
  boundary text NOT NULL,
  scope text NOT NULL,
  definition_version text NOT NULL,
  UNIQUE(metric_id,kind), UNIQUE(name,definition_version)
);
CREATE TABLE release.aot_metric_series (
  series_id uuid PRIMARY KEY,
  metric_id uuid NOT NULL,
  kind text NOT NULL,
  labels jsonb NOT NULL CHECK(jsonb_typeof(labels)='object'),
  FOREIGN KEY(metric_id,kind) REFERENCES release.aot_metric_definition(metric_id,kind),
  UNIQUE(series_id,kind), UNIQUE(metric_id,labels)
);
CREATE TABLE release.aot_metric_snapshot (
  snapshot_id uuid PRIMARY KEY,
  run_id uuid NOT NULL,
  mode text NOT NULL DEFAULT 'aot' CHECK(mode='aot'),
  phase text NOT NULL CHECK(phase IN ('baseline','final')),
  epoch bigint NOT NULL CHECK(epoch>=0),
  boot_id release.bench_u64 NOT NULL,
  clock_id text NOT NULL,
  at_tick release.bench_u64 NOT NULL,
  profile_hash text NOT NULL CHECK(profile_hash ~ '^[0-9a-f]{64}$'),
  raw_artifact_id uuid NOT NULL REFERENCES release.bench_artifact,
  FOREIGN KEY(run_id,mode) REFERENCES release.bench_run(run_id,mode),
  FOREIGN KEY(run_id,clock_id) REFERENCES release.bench_clock,
  UNIQUE(snapshot_id,run_id), UNIQUE(run_id,epoch,phase)
);
CREATE TABLE release.aot_scalar_sample (
  snapshot_id uuid NOT NULL REFERENCES release.aot_metric_snapshot,
  series_id uuid NOT NULL,
  kind text NOT NULL CHECK(kind IN ('counter','gauge')),
  value numeric NOT NULL,
  FOREIGN KEY(series_id,kind) REFERENCES release.aot_metric_series(series_id,kind),
  PRIMARY KEY(snapshot_id,series_id)
);
CREATE TABLE release.aot_histogram_sample (
  snapshot_id uuid NOT NULL REFERENCES release.aot_metric_snapshot,
  series_id uuid NOT NULL,
  kind text NOT NULL DEFAULT 'histogram' CHECK(kind='histogram'),
  calibration_id uuid NOT NULL REFERENCES release.bench_calibration,
  sample_count release.bench_u64 NOT NULL,
  sample_sum release.bench_u64 NOT NULL,
  FOREIGN KEY(series_id,kind) REFERENCES release.aot_metric_series(series_id,kind),
  PRIMARY KEY(snapshot_id,series_id),
  UNIQUE(snapshot_id,series_id,sample_count)
);
CREATE TABLE release.aot_histogram_bucket (
  snapshot_id uuid NOT NULL,
  series_id uuid NOT NULL,
  ordinal integer NOT NULL CHECK(ordinal>=1),
  upper_bound bigint CHECK(upper_bound>=0), -- NULL represents +Inf.
  cumulative_count release.bench_u64 NOT NULL,
  sample_count release.bench_u64 NOT NULL,
  FOREIGN KEY(snapshot_id,series_id,sample_count)
    REFERENCES release.aot_histogram_sample(snapshot_id,series_id,sample_count),
  PRIMARY KEY(snapshot_id,series_id,ordinal),
  CHECK(cumulative_count<=sample_count),
  CHECK(upper_bound IS NOT NULL OR cumulative_count=sample_count)
);
CREATE UNIQUE INDEX aot_hist_one_inf ON release.aot_histogram_bucket(snapshot_id,series_id)
  WHERE upper_bound IS NULL;

CREATE TABLE release.aot_firewall_config (
  config_id uuid PRIMARY KEY,
  condition_id uuid NOT NULL,
  experiment text NOT NULL DEFAULT 'firewall' CHECK(experiment='firewall'),
  instrumentation text NOT NULL CHECK(instrumentation IN ('counter_only','helper_hist','stage_hist','batch_hist','switch_hist','trace')),
  profile_hash text NOT NULL CHECK(profile_hash ~ '^[0-9a-f]{64}$'),
  calibration_ids uuid[] NOT NULL DEFAULT ARRAY[]::uuid[],
  parameters jsonb NOT NULL DEFAULT '{}',
  target_pps integer CHECK(target_pps>0),
  send_window_ns release.bench_u64,
  drain_ns release.bench_u64 NOT NULL,
  payload_bytes integer NOT NULL CHECK(payload_bytes>=32),
  blacklist_capacity integer NOT NULL CHECK(blacklist_capacity=256),
  blacklist_entries integer NOT NULL CHECK(blacklist_entries BETWEEN 1 AND 256),
  legal_numerator integer NOT NULL CHECK(legal_numerator BETWEEN 0 AND 10),
  legal_denominator integer NOT NULL CHECK(legal_denominator=10),
  lookup_case text NOT NULL CHECK(lookup_case IN ('mixed','hit','miss')),
  map_fixture_artifact_id uuid NOT NULL REFERENCES release.bench_artifact,
  FOREIGN KEY(condition_id,experiment) REFERENCES release.bench_condition(condition_id,experiment),
  UNIQUE(config_id,condition_id)
);
CREATE TABLE release.aot_firewall_metrics_data (
  run_id uuid NOT NULL,
  config_id uuid NOT NULL REFERENCES release.aot_firewall_config,
  mode text NOT NULL DEFAULT 'aot' CHECK(mode='aot'),
  experiment text NOT NULL DEFAULT 'firewall' CHECK(experiment='firewall'),
  variant text NOT NULL DEFAULT 'metrics' CHECK(variant='metrics'),
  baseline_snapshot_id uuid,
  final_snapshot_id uuid,
  analysis_id uuid NOT NULL,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL CHECK(jsonb_typeof(issues)='array'),
  sent_legal release.bench_u64, sent_illegal release.bench_u64,
  matched_legal release.bench_u64, blocked_hit_delta release.bench_u64,
  token_refused release.bench_u64,
  CHECK(matched_legal IS NULL OR sent_legal IS NULL OR matched_legal<=sent_legal),
  PRIMARY KEY(run_id,analysis_id),
  FOREIGN KEY(run_id,mode,experiment,variant,config_id)
    REFERENCES release.bench_run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(baseline_snapshot_id,run_id) REFERENCES release.aot_metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(final_snapshot_id,run_id) REFERENCES release.aot_metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(analysis_id,run_id) REFERENCES release.bench_analysis(analysis_id,run_id),
  CHECK(NOT qualified OR (baseline_snapshot_id IS NOT NULL AND final_snapshot_id IS NOT NULL))
);
CREATE TABLE release.aot_firewall_trace_data (
  run_id uuid PRIMARY KEY,
  config_id uuid NOT NULL REFERENCES release.aot_firewall_config,
  mode text NOT NULL DEFAULT 'aot' CHECK(mode='aot'),
  experiment text NOT NULL DEFAULT 'firewall' CHECK(experiment='firewall'),
  variant text NOT NULL DEFAULT 'trace' CHECK(variant='trace'),
  epoch bigint NOT NULL CHECK(epoch>=0),
  dictionary_version integer NOT NULL CHECK(dictionary_version>=1),
  boot_id release.bench_u64 NOT NULL,
  clock_id text NOT NULL,
  capacity integer NOT NULL CHECK(capacity>0),
  written integer NOT NULL CHECK(written>=0 AND written<=capacity),
  dropped bigint NOT NULL CHECK(dropped>=0),
  flags bigint NOT NULL CHECK(flags>=0),
  complete boolean NOT NULL,
  result_checked boolean NOT NULL,
  result_ok boolean,
  raw_artifact_id uuid NOT NULL REFERENCES release.bench_artifact,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL CHECK(jsonb_typeof(issues)='array'),
  FOREIGN KEY(run_id,mode,experiment,variant,config_id)
    REFERENCES release.bench_run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(run_id,clock_id) REFERENCES release.bench_clock,
  CHECK(NOT complete OR (dropped=0 AND flags=0)),
  CHECK(NOT result_checked OR result_ok IS NOT NULL),
  CHECK(NOT qualified OR (complete AND (NOT result_checked OR result_ok)))
);
CREATE TABLE release.aot_firewall_event (
  run_id uuid NOT NULL,
  mode text NOT NULL DEFAULT 'aot' CHECK(mode='aot'),
  experiment text NOT NULL DEFAULT 'firewall' CHECK(experiment='firewall'),
  source text NOT NULL CHECK(source IN ('dut','host','agent')),
  event_sequence bigint NOT NULL CHECK(event_sequence>=0),
  dictionary_version integer NOT NULL,
  event_id integer NOT NULL,
  clock_id text NOT NULL,
  timestamp_raw release.bench_u64 NOT NULL,
  kind text NOT NULL CHECK(kind IN ('begin','end','instant')),
  context_id integer NOT NULL CHECK(context_id BETWEEN 0 AND 255),
  probe_seq bigint CHECK(probe_seq>=1),
  span_id bigint CHECK(span_id>=0),
  parent_span_id bigint CHECK(parent_span_id>=0),
  arg0 bigint CHECK(arg0 BETWEEN 0 AND 4294967295),
  arg1 bigint CHECK(arg1 BETWEEN 0 AND 4294967295),
  FOREIGN KEY(run_id,mode,experiment) REFERENCES release.bench_run(run_id,mode,experiment),
  FOREIGN KEY(run_id,clock_id) REFERENCES release.bench_clock,
  FOREIGN KEY(experiment,dictionary_version,event_id) REFERENCES release.bench_event_definition,
  PRIMARY KEY(run_id,source,event_sequence)
);

CREATE TABLE release.aot_just_drop_config (
  config_id uuid PRIMARY KEY,
  condition_id uuid NOT NULL,
  experiment text NOT NULL DEFAULT 'just_drop' CHECK(experiment='just_drop'),
  instrumentation text NOT NULL CHECK(instrumentation IN ('counter_only','helper_hist','stage_hist','batch_hist','switch_hist','trace')),
  profile_hash text NOT NULL CHECK(profile_hash ~ '^[0-9a-f]{64}$'),
  calibration_ids uuid[] NOT NULL DEFAULT ARRAY[]::uuid[],
  parameters jsonb NOT NULL DEFAULT '{}',
  target_pps integer CHECK(target_pps>0),
  send_window_ns release.bench_u64,
  drain_ns release.bench_u64 NOT NULL,
  payload_bytes integer NOT NULL CHECK(payload_bytes>=32),
  workload_version text NOT NULL CHECK(workload_version='drop-minimal-v1'),
  neighbor_policy text NOT NULL,
  saturation_grid boolean NOT NULL,
  FOREIGN KEY(condition_id,experiment) REFERENCES release.bench_condition(condition_id,experiment),
  UNIQUE(config_id,condition_id)
);
CREATE TABLE release.aot_just_drop_metrics_data (
  run_id uuid NOT NULL,
  config_id uuid NOT NULL REFERENCES release.aot_just_drop_config,
  mode text NOT NULL DEFAULT 'aot' CHECK(mode='aot'),
  experiment text NOT NULL DEFAULT 'just_drop' CHECK(experiment='just_drop'),
  variant text NOT NULL DEFAULT 'metrics' CHECK(variant='metrics'),
  baseline_snapshot_id uuid,
  final_snapshot_id uuid,
  analysis_id uuid NOT NULL,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL CHECK(jsonb_typeof(issues)='array'),
  sent_success release.bench_u64, ingress_seen release.bench_u64,
  policy_enter release.bench_u64, normal_drop release.bench_u64,
  runtime_fault release.bench_u64, device_window_ticks release.bench_u64,
  sender_limited boolean NOT NULL,
  plateau_observed boolean NOT NULL,
  PRIMARY KEY(run_id,analysis_id),
  FOREIGN KEY(run_id,mode,experiment,variant,config_id)
    REFERENCES release.bench_run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(baseline_snapshot_id,run_id) REFERENCES release.aot_metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(final_snapshot_id,run_id) REFERENCES release.aot_metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(analysis_id,run_id) REFERENCES release.bench_analysis(analysis_id,run_id),
  CHECK(NOT qualified OR (baseline_snapshot_id IS NOT NULL AND final_snapshot_id IS NOT NULL))
);
CREATE TABLE release.aot_just_drop_trace_data (
  run_id uuid PRIMARY KEY,
  config_id uuid NOT NULL REFERENCES release.aot_just_drop_config,
  mode text NOT NULL DEFAULT 'aot' CHECK(mode='aot'),
  experiment text NOT NULL DEFAULT 'just_drop' CHECK(experiment='just_drop'),
  variant text NOT NULL DEFAULT 'trace' CHECK(variant='trace'),
  epoch bigint NOT NULL CHECK(epoch>=0),
  dictionary_version integer NOT NULL CHECK(dictionary_version>=1),
  boot_id release.bench_u64 NOT NULL,
  clock_id text NOT NULL,
  capacity integer NOT NULL CHECK(capacity>0),
  written integer NOT NULL CHECK(written>=0 AND written<=capacity),
  dropped bigint NOT NULL CHECK(dropped>=0),
  flags bigint NOT NULL CHECK(flags>=0),
  complete boolean NOT NULL,
  result_checked boolean NOT NULL,
  result_ok boolean,
  raw_artifact_id uuid NOT NULL REFERENCES release.bench_artifact,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL CHECK(jsonb_typeof(issues)='array'),
  FOREIGN KEY(run_id,mode,experiment,variant,config_id)
    REFERENCES release.bench_run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(run_id,clock_id) REFERENCES release.bench_clock,
  CHECK(NOT complete OR (dropped=0 AND flags=0)),
  CHECK(NOT result_checked OR result_ok IS NOT NULL),
  CHECK(NOT qualified OR (complete AND (NOT result_checked OR result_ok)))
);
CREATE TABLE release.aot_just_drop_event (
  run_id uuid NOT NULL,
  mode text NOT NULL DEFAULT 'aot' CHECK(mode='aot'),
  experiment text NOT NULL DEFAULT 'just_drop' CHECK(experiment='just_drop'),
  source text NOT NULL CHECK(source IN ('dut','host','agent')),
  event_sequence bigint NOT NULL CHECK(event_sequence>=0),
  dictionary_version integer NOT NULL,
  event_id integer NOT NULL,
  clock_id text NOT NULL,
  timestamp_raw release.bench_u64 NOT NULL,
  kind text NOT NULL CHECK(kind IN ('begin','end','instant')),
  context_id integer NOT NULL CHECK(context_id BETWEEN 0 AND 255),
  probe_seq bigint CHECK(probe_seq>=1),
  span_id bigint CHECK(span_id>=0),
  parent_span_id bigint CHECK(parent_span_id>=0),
  arg0 bigint CHECK(arg0 BETWEEN 0 AND 4294967295),
  arg1 bigint CHECK(arg1 BETWEEN 0 AND 4294967295),
  FOREIGN KEY(run_id,mode,experiment) REFERENCES release.bench_run(run_id,mode,experiment),
  FOREIGN KEY(run_id,clock_id) REFERENCES release.bench_clock,
  FOREIGN KEY(experiment,dictionary_version,event_id) REFERENCES release.bench_event_definition,
  PRIMARY KEY(run_id,source,event_sequence)
);

CREATE TABLE release.aot_just_tx_config (
  config_id uuid PRIMARY KEY,
  condition_id uuid NOT NULL,
  experiment text NOT NULL DEFAULT 'just_tx' CHECK(experiment='just_tx'),
  instrumentation text NOT NULL CHECK(instrumentation IN ('counter_only','helper_hist','stage_hist','batch_hist','switch_hist','trace')),
  profile_hash text NOT NULL CHECK(profile_hash ~ '^[0-9a-f]{64}$'),
  calibration_ids uuid[] NOT NULL DEFAULT ARRAY[]::uuid[],
  parameters jsonb NOT NULL DEFAULT '{}',
  target_pps integer CHECK(target_pps>0),
  send_window_ns release.bench_u64,
  drain_ns release.bench_u64 NOT NULL,
  payload_bytes integer NOT NULL CHECK(payload_bytes BETWEEN 32 AND 1472),
  probe_version text NOT NULL CHECK(probe_version='JTX1'),
  capture_interface text NOT NULL,
  FOREIGN KEY(condition_id,experiment) REFERENCES release.bench_condition(condition_id,experiment),
  UNIQUE(config_id,condition_id)
);
CREATE TABLE release.aot_just_tx_metrics_data (
  run_id uuid NOT NULL,
  config_id uuid NOT NULL REFERENCES release.aot_just_tx_config,
  mode text NOT NULL DEFAULT 'aot' CHECK(mode='aot'),
  experiment text NOT NULL DEFAULT 'just_tx' CHECK(experiment='just_tx'),
  variant text NOT NULL DEFAULT 'metrics' CHECK(variant='metrics'),
  baseline_snapshot_id uuid,
  final_snapshot_id uuid,
  analysis_id uuid NOT NULL,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL CHECK(jsonb_typeof(issues)='array'),
  sent_success release.bench_u64, returned release.bench_u64,
  duplicate_rx release.bench_u64, late_rx release.bench_u64,
  valid_rtt_count release.bench_u64,
  CHECK(returned IS NULL OR sent_success IS NULL OR returned<=sent_success),
  CHECK(valid_rtt_count IS NULL OR returned IS NULL OR valid_rtt_count<=returned),
  PRIMARY KEY(run_id,analysis_id),
  FOREIGN KEY(run_id,mode,experiment,variant,config_id)
    REFERENCES release.bench_run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(baseline_snapshot_id,run_id) REFERENCES release.aot_metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(final_snapshot_id,run_id) REFERENCES release.aot_metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(analysis_id,run_id) REFERENCES release.bench_analysis(analysis_id,run_id),
  CHECK(NOT qualified OR (baseline_snapshot_id IS NOT NULL AND final_snapshot_id IS NOT NULL))
);
CREATE TABLE release.aot_just_tx_trace_data (
  run_id uuid PRIMARY KEY,
  config_id uuid NOT NULL REFERENCES release.aot_just_tx_config,
  mode text NOT NULL DEFAULT 'aot' CHECK(mode='aot'),
  experiment text NOT NULL DEFAULT 'just_tx' CHECK(experiment='just_tx'),
  variant text NOT NULL DEFAULT 'trace' CHECK(variant='trace'),
  epoch bigint NOT NULL CHECK(epoch>=0),
  dictionary_version integer NOT NULL CHECK(dictionary_version>=1),
  boot_id release.bench_u64 NOT NULL,
  clock_id text NOT NULL,
  capacity integer NOT NULL CHECK(capacity>0),
  written integer NOT NULL CHECK(written>=0 AND written<=capacity),
  dropped bigint NOT NULL CHECK(dropped>=0),
  flags bigint NOT NULL CHECK(flags>=0),
  complete boolean NOT NULL,
  result_checked boolean NOT NULL,
  result_ok boolean,
  raw_artifact_id uuid NOT NULL REFERENCES release.bench_artifact,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL CHECK(jsonb_typeof(issues)='array'),
  FOREIGN KEY(run_id,mode,experiment,variant,config_id)
    REFERENCES release.bench_run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(run_id,clock_id) REFERENCES release.bench_clock,
  CHECK(NOT complete OR (dropped=0 AND flags=0)),
  CHECK(NOT result_checked OR result_ok IS NOT NULL),
  CHECK(NOT qualified OR (complete AND (NOT result_checked OR result_ok)))
);
CREATE TABLE release.aot_just_tx_event (
  run_id uuid NOT NULL,
  mode text NOT NULL DEFAULT 'aot' CHECK(mode='aot'),
  experiment text NOT NULL DEFAULT 'just_tx' CHECK(experiment='just_tx'),
  source text NOT NULL CHECK(source IN ('dut','host','agent')),
  event_sequence bigint NOT NULL CHECK(event_sequence>=0),
  dictionary_version integer NOT NULL,
  event_id integer NOT NULL,
  clock_id text NOT NULL,
  timestamp_raw release.bench_u64 NOT NULL,
  kind text NOT NULL CHECK(kind IN ('begin','end','instant')),
  context_id integer NOT NULL CHECK(context_id BETWEEN 0 AND 255),
  probe_seq bigint CHECK(probe_seq>=1),
  span_id bigint CHECK(span_id>=0),
  parent_span_id bigint CHECK(parent_span_id>=0),
  arg0 bigint CHECK(arg0 BETWEEN 0 AND 4294967295),
  arg1 bigint CHECK(arg1 BETWEEN 0 AND 4294967295),
  FOREIGN KEY(run_id,mode,experiment) REFERENCES release.bench_run(run_id,mode,experiment),
  FOREIGN KEY(run_id,clock_id) REFERENCES release.bench_clock,
  FOREIGN KEY(experiment,dictionary_version,event_id) REFERENCES release.bench_event_definition,
  PRIMARY KEY(run_id,source,event_sequence)
);

CREATE TABLE release.aot_exec_bench_config (
  config_id uuid PRIMARY KEY,
  condition_id uuid NOT NULL,
  experiment text NOT NULL DEFAULT 'exec_bench' CHECK(experiment='exec_bench'),
  instrumentation text NOT NULL CHECK(instrumentation IN ('counter_only','helper_hist','stage_hist','batch_hist','switch_hist','trace')),
  profile_hash text NOT NULL CHECK(profile_hash ~ '^[0-9a-f]{64}$'),
  calibration_ids uuid[] NOT NULL DEFAULT ARRAY[]::uuid[],
  parameters jsonb NOT NULL DEFAULT '{}',
  workload text NOT NULL CHECK(workload IN ('noop','prime','log2','copy','fletcher32','bsort','strcmp')),
  work_param integer NOT NULL CHECK(work_param>=0),
  batch_count bigint NOT NULL CHECK(batch_count BETWEEN 1 AND 65536),
  samples_per_run integer NOT NULL CHECK(samples_per_run>0),
  input_bytes integer NOT NULL CHECK(input_bytes=8258),
  input_protocol text NOT NULL CHECK(input_protocol='EXB2'),
  wifi_enabled boolean NOT NULL CHECK(NOT wifi_enabled),
  step_budget release.bench_u64,
  fixture_artifact_id uuid NOT NULL REFERENCES release.bench_artifact,
  FOREIGN KEY(condition_id,experiment) REFERENCES release.bench_condition(condition_id,experiment),
  UNIQUE(config_id,condition_id)
);
CREATE TABLE release.aot_exec_bench_metrics_data (
  run_id uuid NOT NULL,
  config_id uuid NOT NULL REFERENCES release.aot_exec_bench_config,
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
    REFERENCES release.bench_run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(baseline_snapshot_id,run_id) REFERENCES release.aot_metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(final_snapshot_id,run_id) REFERENCES release.aot_metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(analysis_id,run_id) REFERENCES release.bench_analysis(analysis_id,run_id),
  CHECK(NOT qualified OR (baseline_snapshot_id IS NOT NULL AND final_snapshot_id IS NOT NULL))
);
CREATE TABLE release.aot_exec_bench_trace_data (
  run_id uuid PRIMARY KEY,
  config_id uuid NOT NULL REFERENCES release.aot_exec_bench_config,
  mode text NOT NULL DEFAULT 'aot' CHECK(mode='aot'),
  experiment text NOT NULL DEFAULT 'exec_bench' CHECK(experiment='exec_bench'),
  variant text NOT NULL DEFAULT 'trace' CHECK(variant='trace'),
  epoch bigint NOT NULL CHECK(epoch>=0),
  dictionary_version integer NOT NULL CHECK(dictionary_version>=1),
  boot_id release.bench_u64 NOT NULL,
  clock_id text NOT NULL,
  capacity integer NOT NULL CHECK(capacity>0),
  written integer NOT NULL CHECK(written>=0 AND written<=capacity),
  dropped bigint NOT NULL CHECK(dropped>=0),
  flags bigint NOT NULL CHECK(flags>=0),
  complete boolean NOT NULL,
  result_checked boolean NOT NULL,
  result_ok boolean,
  raw_artifact_id uuid NOT NULL REFERENCES release.bench_artifact,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL CHECK(jsonb_typeof(issues)='array'),
  FOREIGN KEY(run_id,mode,experiment,variant,config_id)
    REFERENCES release.bench_run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(run_id,clock_id) REFERENCES release.bench_clock,
  CHECK(NOT complete OR (dropped=0 AND flags=0)),
  CHECK(NOT result_checked OR result_ok IS NOT NULL),
  CHECK(NOT qualified OR (complete AND (NOT result_checked OR result_ok)))
);
CREATE TABLE release.aot_exec_bench_event (
  run_id uuid NOT NULL,
  mode text NOT NULL DEFAULT 'aot' CHECK(mode='aot'),
  experiment text NOT NULL DEFAULT 'exec_bench' CHECK(experiment='exec_bench'),
  source text NOT NULL CHECK(source IN ('dut','host','agent')),
  event_sequence bigint NOT NULL CHECK(event_sequence>=0),
  dictionary_version integer NOT NULL,
  event_id integer NOT NULL,
  clock_id text NOT NULL,
  timestamp_raw release.bench_u64 NOT NULL,
  kind text NOT NULL CHECK(kind IN ('begin','end','instant')),
  context_id integer NOT NULL CHECK(context_id BETWEEN 0 AND 255),
  probe_seq bigint CHECK(probe_seq>=1),
  span_id bigint CHECK(span_id>=0),
  parent_span_id bigint CHECK(parent_span_id>=0),
  arg0 bigint CHECK(arg0 BETWEEN 0 AND 4294967295),
  arg1 bigint CHECK(arg1 BETWEEN 0 AND 4294967295),
  FOREIGN KEY(run_id,mode,experiment) REFERENCES release.bench_run(run_id,mode,experiment),
  FOREIGN KEY(run_id,clock_id) REFERENCES release.bench_clock,
  FOREIGN KEY(experiment,dictionary_version,event_id) REFERENCES release.bench_event_definition,
  PRIMARY KEY(run_id,source,event_sequence)
);

CREATE TABLE release.aot_case_config (
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
  pre_window_ns release.bench_u64 NOT NULL,
  post_window_ns release.bench_u64 NOT NULL,
  payload_bytes integer NOT NULL CHECK(payload_bytes=40),
  source_mode text NOT NULL,
  target_mode text NOT NULL,
  FOREIGN KEY(condition_id,experiment) REFERENCES release.bench_condition(condition_id,experiment),
  UNIQUE(config_id,condition_id)
);
CREATE TABLE release.aot_case_metrics_data (
  run_id uuid NOT NULL,
  config_id uuid NOT NULL REFERENCES release.aot_case_config,
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
  old_after_new release.bench_u64,
  PRIMARY KEY(run_id,analysis_id),
  FOREIGN KEY(run_id,mode,experiment,variant,config_id)
    REFERENCES release.bench_run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(baseline_snapshot_id,run_id) REFERENCES release.aot_metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(final_snapshot_id,run_id) REFERENCES release.aot_metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(analysis_id,run_id) REFERENCES release.bench_analysis(analysis_id,run_id),
  CHECK(NOT qualified OR (baseline_snapshot_id IS NOT NULL AND final_snapshot_id IS NOT NULL))
);
CREATE TABLE release.aot_case_trace_data (
  run_id uuid PRIMARY KEY,
  config_id uuid NOT NULL REFERENCES release.aot_case_config,
  mode text NOT NULL DEFAULT 'aot' CHECK(mode='aot'),
  experiment text NOT NULL DEFAULT 'case' CHECK(experiment='case'),
  variant text NOT NULL DEFAULT 'trace' CHECK(variant='trace'),
  epoch bigint NOT NULL CHECK(epoch>=0),
  dictionary_version integer NOT NULL CHECK(dictionary_version>=1),
  boot_id release.bench_u64 NOT NULL,
  clock_id text NOT NULL,
  capacity integer NOT NULL CHECK(capacity>0),
  written integer NOT NULL CHECK(written>=0 AND written<=capacity),
  dropped bigint NOT NULL CHECK(dropped>=0),
  flags bigint NOT NULL CHECK(flags>=0),
  complete boolean NOT NULL,
  result_checked boolean NOT NULL,
  result_ok boolean,
  raw_artifact_id uuid NOT NULL REFERENCES release.bench_artifact,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL CHECK(jsonb_typeof(issues)='array'),
  FOREIGN KEY(run_id,mode,experiment,variant,config_id)
    REFERENCES release.bench_run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(run_id,clock_id) REFERENCES release.bench_clock,
  CHECK(NOT complete OR (dropped=0 AND flags=0)),
  CHECK(NOT result_checked OR result_ok IS NOT NULL),
  CHECK(NOT qualified OR (complete AND (NOT result_checked OR result_ok)))
);
CREATE TABLE release.aot_case_event (
  run_id uuid NOT NULL,
  mode text NOT NULL DEFAULT 'aot' CHECK(mode='aot'),
  experiment text NOT NULL DEFAULT 'case' CHECK(experiment='case'),
  source text NOT NULL CHECK(source IN ('dut','host','agent')),
  event_sequence bigint NOT NULL CHECK(event_sequence>=0),
  dictionary_version integer NOT NULL,
  event_id integer NOT NULL,
  clock_id text NOT NULL,
  timestamp_raw release.bench_u64 NOT NULL,
  kind text NOT NULL CHECK(kind IN ('begin','end','instant')),
  context_id integer NOT NULL CHECK(context_id BETWEEN 0 AND 255),
  probe_seq bigint CHECK(probe_seq>=1),
  span_id bigint CHECK(span_id>=0),
  parent_span_id bigint CHECK(parent_span_id>=0),
  arg0 bigint CHECK(arg0 BETWEEN 0 AND 4294967295),
  arg1 bigint CHECK(arg1 BETWEEN 0 AND 4294967295),
  FOREIGN KEY(run_id,mode,experiment) REFERENCES release.bench_run(run_id,mode,experiment),
  FOREIGN KEY(run_id,clock_id) REFERENCES release.bench_clock,
  FOREIGN KEY(experiment,dictionary_version,event_id) REFERENCES release.bench_event_definition,
  PRIMARY KEY(run_id,source,event_sequence)
);

CREATE TABLE release.aot_probe_result (
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
  rtt_ns release.bench_u64,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL,
  FOREIGN KEY(run_id,mode) REFERENCES release.bench_run(run_id,mode),
  FOREIGN KEY(analysis_id,run_id) REFERENCES release.bench_analysis(analysis_id,run_id),
  FOREIGN KEY(run_id,seq) REFERENCES release.bench_send_attempt,
  FOREIGN KEY(tx_capture_id,run_id) REFERENCES release.bench_capture(capture_id,run_id),
  FOREIGN KEY(rx_capture_id,run_id) REFERENCES release.bench_capture(capture_id,run_id),
  FOREIGN KEY(tx_capture_id,tx_record_index) REFERENCES release.bench_packet_fact,
  FOREIGN KEY(rx_capture_id,rx_record_index) REFERENCES release.bench_packet_fact,
  PRIMARY KEY(analysis_id,seq),
  CHECK((tx_capture_id IS NULL)=(tx_record_index IS NULL)),
  CHECK((rx_capture_id IS NULL)=(rx_record_index IS NULL)),
  CHECK(rtt_ns IS NULL OR (returned AND qualified AND tx_capture_id IS NOT NULL AND rx_capture_id IS NOT NULL))
);
CREATE TABLE release.aot_firewall_token (
  run_id uuid NOT NULL,
  mode text NOT NULL DEFAULT 'aot' CHECK(mode='aot'),
  experiment text NOT NULL DEFAULT 'firewall' CHECK(experiment='firewall'),
  seq bigint NOT NULL,
  sent boolean NOT NULL,
  observed boolean NOT NULL,
  received_bytes bytea,
  FOREIGN KEY(run_id,mode,experiment) REFERENCES release.bench_run(run_id,mode,experiment),
  FOREIGN KEY(run_id,seq) REFERENCES release.bench_send_attempt,
  PRIMARY KEY(run_id,seq)
);
CREATE TABLE release.aot_exec_batch (
  run_id uuid NOT NULL,
  mode text NOT NULL DEFAULT 'aot' CHECK(mode='aot'),
  experiment text NOT NULL DEFAULT 'exec_bench' CHECK(experiment='exec_bench'),
  sample_index integer NOT NULL CHECK(sample_index>=0),
  clock_id text NOT NULL,
  begin_tick release.bench_u64 NOT NULL,
  end_tick release.bench_u64 NOT NULL,
  batch_count bigint NOT NULL CHECK(batch_count BETWEEN 1 AND 65536),
  completed_iterations bigint NOT NULL CHECK(completed_iterations BETWEEN 0 AND batch_count),
  seed release.bench_u64 NOT NULL,
  input_bytes bytea NOT NULL CHECK(octet_length(input_bytes)=8258),
  expected_bytes bytea NOT NULL CHECK(octet_length(expected_bytes)=8258),
  actual_bytes bytea NOT NULL CHECK(octet_length(actual_bytes)=8258),
  execution_return bigint NOT NULL CHECK(execution_return BETWEEN 0 AND 4294967295),
  status bigint NOT NULL CHECK(status BETWEEN 0 AND 4294967295),
  result release.bench_u64 NOT NULL,
  digest bigint NOT NULL CHECK(digest BETWEEN 0 AND 4294967295),
  result_ok boolean NOT NULL,
  qualified boolean NOT NULL,
  FOREIGN KEY(run_id,mode,experiment) REFERENCES release.bench_run(run_id,mode,experiment),
  FOREIGN KEY(run_id,clock_id) REFERENCES release.bench_clock,
  PRIMARY KEY(run_id,sample_index),
  CHECK(end_tick>=begin_tick),
  CHECK(NOT result_ok OR actual_bytes=expected_bytes),
  CHECK(NOT qualified OR (
    result_ok AND status=0 AND execution_return=2
    AND completed_iterations=batch_count
  ))
);
CREATE TABLE release.aot_case_switch (
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
  host_command_begin_ns release.bench_u64 NOT NULL,
  host_reply_ns release.bench_u64 NOT NULL,
  dut_clock_id text,
  dut_begin_tick release.bench_u64,
  dut_end_tick release.bench_u64,
  quiesce_ticks release.bench_u64,
  rebind_ticks release.bench_u64,
  publish_ticks release.bench_u64,
  last_old_capture_id uuid,
  last_old_record_index bigint,
  first_new_capture_id uuid,
  first_new_record_index bigint,
  reply_transition_gap_ns release.bench_u64,
  old_after_new bigint CHECK(old_after_new>=0),
  command_ok boolean NOT NULL,
  error_code text,
  boot_unchanged boolean NOT NULL,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL,
  FOREIGN KEY(run_id,mode,experiment) REFERENCES release.bench_run(run_id,mode,experiment),
  FOREIGN KEY(run_id,host_clock_id) REFERENCES release.bench_clock,
  FOREIGN KEY(run_id,dut_clock_id) REFERENCES release.bench_clock,
  FOREIGN KEY(last_old_capture_id,run_id) REFERENCES release.bench_capture(capture_id,run_id),
  FOREIGN KEY(first_new_capture_id,run_id) REFERENCES release.bench_capture(capture_id,run_id),
  FOREIGN KEY(last_old_capture_id,last_old_record_index) REFERENCES release.bench_packet_fact,
  FOREIGN KEY(first_new_capture_id,first_new_record_index) REFERENCES release.bench_packet_fact,
  PRIMARY KEY(run_id,switch_index),
  CHECK(source_version<>target_version),
  CHECK(host_reply_ns>=host_command_begin_ns),
  CHECK(dut_end_tick IS NULL OR dut_begin_tick IS NULL OR dut_end_tick>=dut_begin_tick),
  CHECK((last_old_capture_id IS NULL)=(last_old_record_index IS NULL)),
  CHECK((first_new_capture_id IS NULL)=(first_new_record_index IS NULL)),
  CHECK(reply_transition_gap_ns IS NULL OR (last_old_capture_id IS NOT NULL AND first_new_capture_id IS NOT NULL)),
  CHECK(NOT qualified OR (command_ok AND boot_unchanged))
);
ALTER TABLE release.aot_case_config ADD CHECK(switch_kind<>'native_policy_select' AND source_mode='aot' AND target_mode='aot');
ALTER TABLE release.aot_case_switch ADD CHECK(switch_kind<>'native_policy_select');

-- jit tables use literal mode checks.
CREATE TABLE release.jit_metric_definition (
  metric_id uuid PRIMARY KEY,
  name text NOT NULL,
  kind text NOT NULL CHECK(kind IN ('counter','gauge','histogram')),
  unit text NOT NULL,
  boundary text NOT NULL,
  scope text NOT NULL,
  definition_version text NOT NULL,
  UNIQUE(metric_id,kind), UNIQUE(name,definition_version)
);
CREATE TABLE release.jit_metric_series (
  series_id uuid PRIMARY KEY,
  metric_id uuid NOT NULL,
  kind text NOT NULL,
  labels jsonb NOT NULL CHECK(jsonb_typeof(labels)='object'),
  FOREIGN KEY(metric_id,kind) REFERENCES release.jit_metric_definition(metric_id,kind),
  UNIQUE(series_id,kind), UNIQUE(metric_id,labels)
);
CREATE TABLE release.jit_metric_snapshot (
  snapshot_id uuid PRIMARY KEY,
  run_id uuid NOT NULL,
  mode text NOT NULL DEFAULT 'jit' CHECK(mode='jit'),
  phase text NOT NULL CHECK(phase IN ('baseline','final')),
  epoch bigint NOT NULL CHECK(epoch>=0),
  boot_id release.bench_u64 NOT NULL,
  clock_id text NOT NULL,
  at_tick release.bench_u64 NOT NULL,
  profile_hash text NOT NULL CHECK(profile_hash ~ '^[0-9a-f]{64}$'),
  raw_artifact_id uuid NOT NULL REFERENCES release.bench_artifact,
  FOREIGN KEY(run_id,mode) REFERENCES release.bench_run(run_id,mode),
  FOREIGN KEY(run_id,clock_id) REFERENCES release.bench_clock,
  UNIQUE(snapshot_id,run_id), UNIQUE(run_id,epoch,phase)
);
CREATE TABLE release.jit_scalar_sample (
  snapshot_id uuid NOT NULL REFERENCES release.jit_metric_snapshot,
  series_id uuid NOT NULL,
  kind text NOT NULL CHECK(kind IN ('counter','gauge')),
  value numeric NOT NULL,
  FOREIGN KEY(series_id,kind) REFERENCES release.jit_metric_series(series_id,kind),
  PRIMARY KEY(snapshot_id,series_id)
);
CREATE TABLE release.jit_histogram_sample (
  snapshot_id uuid NOT NULL REFERENCES release.jit_metric_snapshot,
  series_id uuid NOT NULL,
  kind text NOT NULL DEFAULT 'histogram' CHECK(kind='histogram'),
  calibration_id uuid NOT NULL REFERENCES release.bench_calibration,
  sample_count release.bench_u64 NOT NULL,
  sample_sum release.bench_u64 NOT NULL,
  FOREIGN KEY(series_id,kind) REFERENCES release.jit_metric_series(series_id,kind),
  PRIMARY KEY(snapshot_id,series_id),
  UNIQUE(snapshot_id,series_id,sample_count)
);
CREATE TABLE release.jit_histogram_bucket (
  snapshot_id uuid NOT NULL,
  series_id uuid NOT NULL,
  ordinal integer NOT NULL CHECK(ordinal>=1),
  upper_bound bigint CHECK(upper_bound>=0), -- NULL represents +Inf.
  cumulative_count release.bench_u64 NOT NULL,
  sample_count release.bench_u64 NOT NULL,
  FOREIGN KEY(snapshot_id,series_id,sample_count)
    REFERENCES release.jit_histogram_sample(snapshot_id,series_id,sample_count),
  PRIMARY KEY(snapshot_id,series_id,ordinal),
  CHECK(cumulative_count<=sample_count),
  CHECK(upper_bound IS NOT NULL OR cumulative_count=sample_count)
);
CREATE UNIQUE INDEX jit_hist_one_inf ON release.jit_histogram_bucket(snapshot_id,series_id)
  WHERE upper_bound IS NULL;

CREATE TABLE release.jit_firewall_config (
  config_id uuid PRIMARY KEY,
  condition_id uuid NOT NULL,
  experiment text NOT NULL DEFAULT 'firewall' CHECK(experiment='firewall'),
  instrumentation text NOT NULL CHECK(instrumentation IN ('counter_only','helper_hist','stage_hist','batch_hist','switch_hist','trace')),
  profile_hash text NOT NULL CHECK(profile_hash ~ '^[0-9a-f]{64}$'),
  calibration_ids uuid[] NOT NULL DEFAULT ARRAY[]::uuid[],
  parameters jsonb NOT NULL DEFAULT '{}',
  target_pps integer CHECK(target_pps>0),
  send_window_ns release.bench_u64,
  drain_ns release.bench_u64 NOT NULL,
  payload_bytes integer NOT NULL CHECK(payload_bytes>=32),
  blacklist_capacity integer NOT NULL CHECK(blacklist_capacity=256),
  blacklist_entries integer NOT NULL CHECK(blacklist_entries BETWEEN 1 AND 256),
  legal_numerator integer NOT NULL CHECK(legal_numerator BETWEEN 0 AND 10),
  legal_denominator integer NOT NULL CHECK(legal_denominator=10),
  lookup_case text NOT NULL CHECK(lookup_case IN ('mixed','hit','miss')),
  map_fixture_artifact_id uuid NOT NULL REFERENCES release.bench_artifact,
  FOREIGN KEY(condition_id,experiment) REFERENCES release.bench_condition(condition_id,experiment),
  UNIQUE(config_id,condition_id)
);
CREATE TABLE release.jit_firewall_metrics_data (
  run_id uuid NOT NULL,
  config_id uuid NOT NULL REFERENCES release.jit_firewall_config,
  mode text NOT NULL DEFAULT 'jit' CHECK(mode='jit'),
  experiment text NOT NULL DEFAULT 'firewall' CHECK(experiment='firewall'),
  variant text NOT NULL DEFAULT 'metrics' CHECK(variant='metrics'),
  baseline_snapshot_id uuid,
  final_snapshot_id uuid,
  analysis_id uuid NOT NULL,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL CHECK(jsonb_typeof(issues)='array'),
  sent_legal release.bench_u64, sent_illegal release.bench_u64,
  matched_legal release.bench_u64, blocked_hit_delta release.bench_u64,
  token_refused release.bench_u64,
  CHECK(matched_legal IS NULL OR sent_legal IS NULL OR matched_legal<=sent_legal),
  PRIMARY KEY(run_id,analysis_id),
  FOREIGN KEY(run_id,mode,experiment,variant,config_id)
    REFERENCES release.bench_run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(baseline_snapshot_id,run_id) REFERENCES release.jit_metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(final_snapshot_id,run_id) REFERENCES release.jit_metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(analysis_id,run_id) REFERENCES release.bench_analysis(analysis_id,run_id),
  CHECK(NOT qualified OR (baseline_snapshot_id IS NOT NULL AND final_snapshot_id IS NOT NULL))
);
CREATE TABLE release.jit_firewall_trace_data (
  run_id uuid PRIMARY KEY,
  config_id uuid NOT NULL REFERENCES release.jit_firewall_config,
  mode text NOT NULL DEFAULT 'jit' CHECK(mode='jit'),
  experiment text NOT NULL DEFAULT 'firewall' CHECK(experiment='firewall'),
  variant text NOT NULL DEFAULT 'trace' CHECK(variant='trace'),
  epoch bigint NOT NULL CHECK(epoch>=0),
  dictionary_version integer NOT NULL CHECK(dictionary_version>=1),
  boot_id release.bench_u64 NOT NULL,
  clock_id text NOT NULL,
  capacity integer NOT NULL CHECK(capacity>0),
  written integer NOT NULL CHECK(written>=0 AND written<=capacity),
  dropped bigint NOT NULL CHECK(dropped>=0),
  flags bigint NOT NULL CHECK(flags>=0),
  complete boolean NOT NULL,
  result_checked boolean NOT NULL,
  result_ok boolean,
  raw_artifact_id uuid NOT NULL REFERENCES release.bench_artifact,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL CHECK(jsonb_typeof(issues)='array'),
  FOREIGN KEY(run_id,mode,experiment,variant,config_id)
    REFERENCES release.bench_run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(run_id,clock_id) REFERENCES release.bench_clock,
  CHECK(NOT complete OR (dropped=0 AND flags=0)),
  CHECK(NOT result_checked OR result_ok IS NOT NULL),
  CHECK(NOT qualified OR (complete AND (NOT result_checked OR result_ok)))
);
CREATE TABLE release.jit_firewall_event (
  run_id uuid NOT NULL,
  mode text NOT NULL DEFAULT 'jit' CHECK(mode='jit'),
  experiment text NOT NULL DEFAULT 'firewall' CHECK(experiment='firewall'),
  source text NOT NULL CHECK(source IN ('dut','host','agent')),
  event_sequence bigint NOT NULL CHECK(event_sequence>=0),
  dictionary_version integer NOT NULL,
  event_id integer NOT NULL,
  clock_id text NOT NULL,
  timestamp_raw release.bench_u64 NOT NULL,
  kind text NOT NULL CHECK(kind IN ('begin','end','instant')),
  context_id integer NOT NULL CHECK(context_id BETWEEN 0 AND 255),
  probe_seq bigint CHECK(probe_seq>=1),
  span_id bigint CHECK(span_id>=0),
  parent_span_id bigint CHECK(parent_span_id>=0),
  arg0 bigint CHECK(arg0 BETWEEN 0 AND 4294967295),
  arg1 bigint CHECK(arg1 BETWEEN 0 AND 4294967295),
  FOREIGN KEY(run_id,mode,experiment) REFERENCES release.bench_run(run_id,mode,experiment),
  FOREIGN KEY(run_id,clock_id) REFERENCES release.bench_clock,
  FOREIGN KEY(experiment,dictionary_version,event_id) REFERENCES release.bench_event_definition,
  PRIMARY KEY(run_id,source,event_sequence)
);

CREATE TABLE release.jit_just_drop_config (
  config_id uuid PRIMARY KEY,
  condition_id uuid NOT NULL,
  experiment text NOT NULL DEFAULT 'just_drop' CHECK(experiment='just_drop'),
  instrumentation text NOT NULL CHECK(instrumentation IN ('counter_only','helper_hist','stage_hist','batch_hist','switch_hist','trace')),
  profile_hash text NOT NULL CHECK(profile_hash ~ '^[0-9a-f]{64}$'),
  calibration_ids uuid[] NOT NULL DEFAULT ARRAY[]::uuid[],
  parameters jsonb NOT NULL DEFAULT '{}',
  target_pps integer CHECK(target_pps>0),
  send_window_ns release.bench_u64,
  drain_ns release.bench_u64 NOT NULL,
  payload_bytes integer NOT NULL CHECK(payload_bytes>=32),
  workload_version text NOT NULL CHECK(workload_version='drop-minimal-v1'),
  neighbor_policy text NOT NULL,
  saturation_grid boolean NOT NULL,
  FOREIGN KEY(condition_id,experiment) REFERENCES release.bench_condition(condition_id,experiment),
  UNIQUE(config_id,condition_id)
);
CREATE TABLE release.jit_just_drop_metrics_data (
  run_id uuid NOT NULL,
  config_id uuid NOT NULL REFERENCES release.jit_just_drop_config,
  mode text NOT NULL DEFAULT 'jit' CHECK(mode='jit'),
  experiment text NOT NULL DEFAULT 'just_drop' CHECK(experiment='just_drop'),
  variant text NOT NULL DEFAULT 'metrics' CHECK(variant='metrics'),
  baseline_snapshot_id uuid,
  final_snapshot_id uuid,
  analysis_id uuid NOT NULL,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL CHECK(jsonb_typeof(issues)='array'),
  sent_success release.bench_u64, ingress_seen release.bench_u64,
  policy_enter release.bench_u64, normal_drop release.bench_u64,
  runtime_fault release.bench_u64, device_window_ticks release.bench_u64,
  sender_limited boolean NOT NULL,
  plateau_observed boolean NOT NULL,
  PRIMARY KEY(run_id,analysis_id),
  FOREIGN KEY(run_id,mode,experiment,variant,config_id)
    REFERENCES release.bench_run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(baseline_snapshot_id,run_id) REFERENCES release.jit_metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(final_snapshot_id,run_id) REFERENCES release.jit_metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(analysis_id,run_id) REFERENCES release.bench_analysis(analysis_id,run_id),
  CHECK(NOT qualified OR (baseline_snapshot_id IS NOT NULL AND final_snapshot_id IS NOT NULL))
);
CREATE TABLE release.jit_just_drop_trace_data (
  run_id uuid PRIMARY KEY,
  config_id uuid NOT NULL REFERENCES release.jit_just_drop_config,
  mode text NOT NULL DEFAULT 'jit' CHECK(mode='jit'),
  experiment text NOT NULL DEFAULT 'just_drop' CHECK(experiment='just_drop'),
  variant text NOT NULL DEFAULT 'trace' CHECK(variant='trace'),
  epoch bigint NOT NULL CHECK(epoch>=0),
  dictionary_version integer NOT NULL CHECK(dictionary_version>=1),
  boot_id release.bench_u64 NOT NULL,
  clock_id text NOT NULL,
  capacity integer NOT NULL CHECK(capacity>0),
  written integer NOT NULL CHECK(written>=0 AND written<=capacity),
  dropped bigint NOT NULL CHECK(dropped>=0),
  flags bigint NOT NULL CHECK(flags>=0),
  complete boolean NOT NULL,
  result_checked boolean NOT NULL,
  result_ok boolean,
  raw_artifact_id uuid NOT NULL REFERENCES release.bench_artifact,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL CHECK(jsonb_typeof(issues)='array'),
  FOREIGN KEY(run_id,mode,experiment,variant,config_id)
    REFERENCES release.bench_run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(run_id,clock_id) REFERENCES release.bench_clock,
  CHECK(NOT complete OR (dropped=0 AND flags=0)),
  CHECK(NOT result_checked OR result_ok IS NOT NULL),
  CHECK(NOT qualified OR (complete AND (NOT result_checked OR result_ok)))
);
CREATE TABLE release.jit_just_drop_event (
  run_id uuid NOT NULL,
  mode text NOT NULL DEFAULT 'jit' CHECK(mode='jit'),
  experiment text NOT NULL DEFAULT 'just_drop' CHECK(experiment='just_drop'),
  source text NOT NULL CHECK(source IN ('dut','host','agent')),
  event_sequence bigint NOT NULL CHECK(event_sequence>=0),
  dictionary_version integer NOT NULL,
  event_id integer NOT NULL,
  clock_id text NOT NULL,
  timestamp_raw release.bench_u64 NOT NULL,
  kind text NOT NULL CHECK(kind IN ('begin','end','instant')),
  context_id integer NOT NULL CHECK(context_id BETWEEN 0 AND 255),
  probe_seq bigint CHECK(probe_seq>=1),
  span_id bigint CHECK(span_id>=0),
  parent_span_id bigint CHECK(parent_span_id>=0),
  arg0 bigint CHECK(arg0 BETWEEN 0 AND 4294967295),
  arg1 bigint CHECK(arg1 BETWEEN 0 AND 4294967295),
  FOREIGN KEY(run_id,mode,experiment) REFERENCES release.bench_run(run_id,mode,experiment),
  FOREIGN KEY(run_id,clock_id) REFERENCES release.bench_clock,
  FOREIGN KEY(experiment,dictionary_version,event_id) REFERENCES release.bench_event_definition,
  PRIMARY KEY(run_id,source,event_sequence)
);

CREATE TABLE release.jit_just_tx_config (
  config_id uuid PRIMARY KEY,
  condition_id uuid NOT NULL,
  experiment text NOT NULL DEFAULT 'just_tx' CHECK(experiment='just_tx'),
  instrumentation text NOT NULL CHECK(instrumentation IN ('counter_only','helper_hist','stage_hist','batch_hist','switch_hist','trace')),
  profile_hash text NOT NULL CHECK(profile_hash ~ '^[0-9a-f]{64}$'),
  calibration_ids uuid[] NOT NULL DEFAULT ARRAY[]::uuid[],
  parameters jsonb NOT NULL DEFAULT '{}',
  target_pps integer CHECK(target_pps>0),
  send_window_ns release.bench_u64,
  drain_ns release.bench_u64 NOT NULL,
  payload_bytes integer NOT NULL CHECK(payload_bytes BETWEEN 32 AND 1472),
  probe_version text NOT NULL CHECK(probe_version='JTX1'),
  capture_interface text NOT NULL,
  FOREIGN KEY(condition_id,experiment) REFERENCES release.bench_condition(condition_id,experiment),
  UNIQUE(config_id,condition_id)
);
CREATE TABLE release.jit_just_tx_metrics_data (
  run_id uuid NOT NULL,
  config_id uuid NOT NULL REFERENCES release.jit_just_tx_config,
  mode text NOT NULL DEFAULT 'jit' CHECK(mode='jit'),
  experiment text NOT NULL DEFAULT 'just_tx' CHECK(experiment='just_tx'),
  variant text NOT NULL DEFAULT 'metrics' CHECK(variant='metrics'),
  baseline_snapshot_id uuid,
  final_snapshot_id uuid,
  analysis_id uuid NOT NULL,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL CHECK(jsonb_typeof(issues)='array'),
  sent_success release.bench_u64, returned release.bench_u64,
  duplicate_rx release.bench_u64, late_rx release.bench_u64,
  valid_rtt_count release.bench_u64,
  CHECK(returned IS NULL OR sent_success IS NULL OR returned<=sent_success),
  CHECK(valid_rtt_count IS NULL OR returned IS NULL OR valid_rtt_count<=returned),
  PRIMARY KEY(run_id,analysis_id),
  FOREIGN KEY(run_id,mode,experiment,variant,config_id)
    REFERENCES release.bench_run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(baseline_snapshot_id,run_id) REFERENCES release.jit_metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(final_snapshot_id,run_id) REFERENCES release.jit_metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(analysis_id,run_id) REFERENCES release.bench_analysis(analysis_id,run_id),
  CHECK(NOT qualified OR (baseline_snapshot_id IS NOT NULL AND final_snapshot_id IS NOT NULL))
);
CREATE TABLE release.jit_just_tx_trace_data (
  run_id uuid PRIMARY KEY,
  config_id uuid NOT NULL REFERENCES release.jit_just_tx_config,
  mode text NOT NULL DEFAULT 'jit' CHECK(mode='jit'),
  experiment text NOT NULL DEFAULT 'just_tx' CHECK(experiment='just_tx'),
  variant text NOT NULL DEFAULT 'trace' CHECK(variant='trace'),
  epoch bigint NOT NULL CHECK(epoch>=0),
  dictionary_version integer NOT NULL CHECK(dictionary_version>=1),
  boot_id release.bench_u64 NOT NULL,
  clock_id text NOT NULL,
  capacity integer NOT NULL CHECK(capacity>0),
  written integer NOT NULL CHECK(written>=0 AND written<=capacity),
  dropped bigint NOT NULL CHECK(dropped>=0),
  flags bigint NOT NULL CHECK(flags>=0),
  complete boolean NOT NULL,
  result_checked boolean NOT NULL,
  result_ok boolean,
  raw_artifact_id uuid NOT NULL REFERENCES release.bench_artifact,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL CHECK(jsonb_typeof(issues)='array'),
  FOREIGN KEY(run_id,mode,experiment,variant,config_id)
    REFERENCES release.bench_run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(run_id,clock_id) REFERENCES release.bench_clock,
  CHECK(NOT complete OR (dropped=0 AND flags=0)),
  CHECK(NOT result_checked OR result_ok IS NOT NULL),
  CHECK(NOT qualified OR (complete AND (NOT result_checked OR result_ok)))
);
CREATE TABLE release.jit_just_tx_event (
  run_id uuid NOT NULL,
  mode text NOT NULL DEFAULT 'jit' CHECK(mode='jit'),
  experiment text NOT NULL DEFAULT 'just_tx' CHECK(experiment='just_tx'),
  source text NOT NULL CHECK(source IN ('dut','host','agent')),
  event_sequence bigint NOT NULL CHECK(event_sequence>=0),
  dictionary_version integer NOT NULL,
  event_id integer NOT NULL,
  clock_id text NOT NULL,
  timestamp_raw release.bench_u64 NOT NULL,
  kind text NOT NULL CHECK(kind IN ('begin','end','instant')),
  context_id integer NOT NULL CHECK(context_id BETWEEN 0 AND 255),
  probe_seq bigint CHECK(probe_seq>=1),
  span_id bigint CHECK(span_id>=0),
  parent_span_id bigint CHECK(parent_span_id>=0),
  arg0 bigint CHECK(arg0 BETWEEN 0 AND 4294967295),
  arg1 bigint CHECK(arg1 BETWEEN 0 AND 4294967295),
  FOREIGN KEY(run_id,mode,experiment) REFERENCES release.bench_run(run_id,mode,experiment),
  FOREIGN KEY(run_id,clock_id) REFERENCES release.bench_clock,
  FOREIGN KEY(experiment,dictionary_version,event_id) REFERENCES release.bench_event_definition,
  PRIMARY KEY(run_id,source,event_sequence)
);

CREATE TABLE release.jit_exec_bench_config (
  config_id uuid PRIMARY KEY,
  condition_id uuid NOT NULL,
  experiment text NOT NULL DEFAULT 'exec_bench' CHECK(experiment='exec_bench'),
  instrumentation text NOT NULL CHECK(instrumentation IN ('counter_only','helper_hist','stage_hist','batch_hist','switch_hist','trace')),
  profile_hash text NOT NULL CHECK(profile_hash ~ '^[0-9a-f]{64}$'),
  calibration_ids uuid[] NOT NULL DEFAULT ARRAY[]::uuid[],
  parameters jsonb NOT NULL DEFAULT '{}',
  workload text NOT NULL CHECK(workload IN ('noop','prime','log2','copy','fletcher32','bsort','strcmp')),
  work_param integer NOT NULL CHECK(work_param>=0),
  batch_count bigint NOT NULL CHECK(batch_count BETWEEN 1 AND 65536),
  samples_per_run integer NOT NULL CHECK(samples_per_run>0),
  input_bytes integer NOT NULL CHECK(input_bytes=8258),
  input_protocol text NOT NULL CHECK(input_protocol='EXB2'),
  wifi_enabled boolean NOT NULL CHECK(NOT wifi_enabled),
  step_budget release.bench_u64,
  fixture_artifact_id uuid NOT NULL REFERENCES release.bench_artifact,
  FOREIGN KEY(condition_id,experiment) REFERENCES release.bench_condition(condition_id,experiment),
  UNIQUE(config_id,condition_id)
);
CREATE TABLE release.jit_exec_bench_metrics_data (
  run_id uuid NOT NULL,
  config_id uuid NOT NULL REFERENCES release.jit_exec_bench_config,
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
    REFERENCES release.bench_run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(baseline_snapshot_id,run_id) REFERENCES release.jit_metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(final_snapshot_id,run_id) REFERENCES release.jit_metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(analysis_id,run_id) REFERENCES release.bench_analysis(analysis_id,run_id),
  CHECK(NOT qualified OR (baseline_snapshot_id IS NOT NULL AND final_snapshot_id IS NOT NULL))
);
CREATE TABLE release.jit_exec_bench_trace_data (
  run_id uuid PRIMARY KEY,
  config_id uuid NOT NULL REFERENCES release.jit_exec_bench_config,
  mode text NOT NULL DEFAULT 'jit' CHECK(mode='jit'),
  experiment text NOT NULL DEFAULT 'exec_bench' CHECK(experiment='exec_bench'),
  variant text NOT NULL DEFAULT 'trace' CHECK(variant='trace'),
  epoch bigint NOT NULL CHECK(epoch>=0),
  dictionary_version integer NOT NULL CHECK(dictionary_version>=1),
  boot_id release.bench_u64 NOT NULL,
  clock_id text NOT NULL,
  capacity integer NOT NULL CHECK(capacity>0),
  written integer NOT NULL CHECK(written>=0 AND written<=capacity),
  dropped bigint NOT NULL CHECK(dropped>=0),
  flags bigint NOT NULL CHECK(flags>=0),
  complete boolean NOT NULL,
  result_checked boolean NOT NULL,
  result_ok boolean,
  raw_artifact_id uuid NOT NULL REFERENCES release.bench_artifact,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL CHECK(jsonb_typeof(issues)='array'),
  FOREIGN KEY(run_id,mode,experiment,variant,config_id)
    REFERENCES release.bench_run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(run_id,clock_id) REFERENCES release.bench_clock,
  CHECK(NOT complete OR (dropped=0 AND flags=0)),
  CHECK(NOT result_checked OR result_ok IS NOT NULL),
  CHECK(NOT qualified OR (complete AND (NOT result_checked OR result_ok)))
);
CREATE TABLE release.jit_exec_bench_event (
  run_id uuid NOT NULL,
  mode text NOT NULL DEFAULT 'jit' CHECK(mode='jit'),
  experiment text NOT NULL DEFAULT 'exec_bench' CHECK(experiment='exec_bench'),
  source text NOT NULL CHECK(source IN ('dut','host','agent')),
  event_sequence bigint NOT NULL CHECK(event_sequence>=0),
  dictionary_version integer NOT NULL,
  event_id integer NOT NULL,
  clock_id text NOT NULL,
  timestamp_raw release.bench_u64 NOT NULL,
  kind text NOT NULL CHECK(kind IN ('begin','end','instant')),
  context_id integer NOT NULL CHECK(context_id BETWEEN 0 AND 255),
  probe_seq bigint CHECK(probe_seq>=1),
  span_id bigint CHECK(span_id>=0),
  parent_span_id bigint CHECK(parent_span_id>=0),
  arg0 bigint CHECK(arg0 BETWEEN 0 AND 4294967295),
  arg1 bigint CHECK(arg1 BETWEEN 0 AND 4294967295),
  FOREIGN KEY(run_id,mode,experiment) REFERENCES release.bench_run(run_id,mode,experiment),
  FOREIGN KEY(run_id,clock_id) REFERENCES release.bench_clock,
  FOREIGN KEY(experiment,dictionary_version,event_id) REFERENCES release.bench_event_definition,
  PRIMARY KEY(run_id,source,event_sequence)
);

CREATE TABLE release.jit_case_config (
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
  pre_window_ns release.bench_u64 NOT NULL,
  post_window_ns release.bench_u64 NOT NULL,
  payload_bytes integer NOT NULL CHECK(payload_bytes=40),
  source_mode text NOT NULL,
  target_mode text NOT NULL,
  FOREIGN KEY(condition_id,experiment) REFERENCES release.bench_condition(condition_id,experiment),
  UNIQUE(config_id,condition_id)
);
CREATE TABLE release.jit_case_metrics_data (
  run_id uuid NOT NULL,
  config_id uuid NOT NULL REFERENCES release.jit_case_config,
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
  old_after_new release.bench_u64,
  PRIMARY KEY(run_id,analysis_id),
  FOREIGN KEY(run_id,mode,experiment,variant,config_id)
    REFERENCES release.bench_run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(baseline_snapshot_id,run_id) REFERENCES release.jit_metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(final_snapshot_id,run_id) REFERENCES release.jit_metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(analysis_id,run_id) REFERENCES release.bench_analysis(analysis_id,run_id),
  CHECK(NOT qualified OR (baseline_snapshot_id IS NOT NULL AND final_snapshot_id IS NOT NULL))
);
CREATE TABLE release.jit_case_trace_data (
  run_id uuid PRIMARY KEY,
  config_id uuid NOT NULL REFERENCES release.jit_case_config,
  mode text NOT NULL DEFAULT 'jit' CHECK(mode='jit'),
  experiment text NOT NULL DEFAULT 'case' CHECK(experiment='case'),
  variant text NOT NULL DEFAULT 'trace' CHECK(variant='trace'),
  epoch bigint NOT NULL CHECK(epoch>=0),
  dictionary_version integer NOT NULL CHECK(dictionary_version>=1),
  boot_id release.bench_u64 NOT NULL,
  clock_id text NOT NULL,
  capacity integer NOT NULL CHECK(capacity>0),
  written integer NOT NULL CHECK(written>=0 AND written<=capacity),
  dropped bigint NOT NULL CHECK(dropped>=0),
  flags bigint NOT NULL CHECK(flags>=0),
  complete boolean NOT NULL,
  result_checked boolean NOT NULL,
  result_ok boolean,
  raw_artifact_id uuid NOT NULL REFERENCES release.bench_artifact,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL CHECK(jsonb_typeof(issues)='array'),
  FOREIGN KEY(run_id,mode,experiment,variant,config_id)
    REFERENCES release.bench_run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(run_id,clock_id) REFERENCES release.bench_clock,
  CHECK(NOT complete OR (dropped=0 AND flags=0)),
  CHECK(NOT result_checked OR result_ok IS NOT NULL),
  CHECK(NOT qualified OR (complete AND (NOT result_checked OR result_ok)))
);
CREATE TABLE release.jit_case_event (
  run_id uuid NOT NULL,
  mode text NOT NULL DEFAULT 'jit' CHECK(mode='jit'),
  experiment text NOT NULL DEFAULT 'case' CHECK(experiment='case'),
  source text NOT NULL CHECK(source IN ('dut','host','agent')),
  event_sequence bigint NOT NULL CHECK(event_sequence>=0),
  dictionary_version integer NOT NULL,
  event_id integer NOT NULL,
  clock_id text NOT NULL,
  timestamp_raw release.bench_u64 NOT NULL,
  kind text NOT NULL CHECK(kind IN ('begin','end','instant')),
  context_id integer NOT NULL CHECK(context_id BETWEEN 0 AND 255),
  probe_seq bigint CHECK(probe_seq>=1),
  span_id bigint CHECK(span_id>=0),
  parent_span_id bigint CHECK(parent_span_id>=0),
  arg0 bigint CHECK(arg0 BETWEEN 0 AND 4294967295),
  arg1 bigint CHECK(arg1 BETWEEN 0 AND 4294967295),
  FOREIGN KEY(run_id,mode,experiment) REFERENCES release.bench_run(run_id,mode,experiment),
  FOREIGN KEY(run_id,clock_id) REFERENCES release.bench_clock,
  FOREIGN KEY(experiment,dictionary_version,event_id) REFERENCES release.bench_event_definition,
  PRIMARY KEY(run_id,source,event_sequence)
);

CREATE TABLE release.jit_probe_result (
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
  rtt_ns release.bench_u64,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL,
  FOREIGN KEY(run_id,mode) REFERENCES release.bench_run(run_id,mode),
  FOREIGN KEY(analysis_id,run_id) REFERENCES release.bench_analysis(analysis_id,run_id),
  FOREIGN KEY(run_id,seq) REFERENCES release.bench_send_attempt,
  FOREIGN KEY(tx_capture_id,run_id) REFERENCES release.bench_capture(capture_id,run_id),
  FOREIGN KEY(rx_capture_id,run_id) REFERENCES release.bench_capture(capture_id,run_id),
  FOREIGN KEY(tx_capture_id,tx_record_index) REFERENCES release.bench_packet_fact,
  FOREIGN KEY(rx_capture_id,rx_record_index) REFERENCES release.bench_packet_fact,
  PRIMARY KEY(analysis_id,seq),
  CHECK((tx_capture_id IS NULL)=(tx_record_index IS NULL)),
  CHECK((rx_capture_id IS NULL)=(rx_record_index IS NULL)),
  CHECK(rtt_ns IS NULL OR (returned AND qualified AND tx_capture_id IS NOT NULL AND rx_capture_id IS NOT NULL))
);
CREATE TABLE release.jit_firewall_token (
  run_id uuid NOT NULL,
  mode text NOT NULL DEFAULT 'jit' CHECK(mode='jit'),
  experiment text NOT NULL DEFAULT 'firewall' CHECK(experiment='firewall'),
  seq bigint NOT NULL,
  sent boolean NOT NULL,
  observed boolean NOT NULL,
  received_bytes bytea,
  FOREIGN KEY(run_id,mode,experiment) REFERENCES release.bench_run(run_id,mode,experiment),
  FOREIGN KEY(run_id,seq) REFERENCES release.bench_send_attempt,
  PRIMARY KEY(run_id,seq)
);
CREATE TABLE release.jit_exec_batch (
  run_id uuid NOT NULL,
  mode text NOT NULL DEFAULT 'jit' CHECK(mode='jit'),
  experiment text NOT NULL DEFAULT 'exec_bench' CHECK(experiment='exec_bench'),
  sample_index integer NOT NULL CHECK(sample_index>=0),
  clock_id text NOT NULL,
  begin_tick release.bench_u64 NOT NULL,
  end_tick release.bench_u64 NOT NULL,
  batch_count bigint NOT NULL CHECK(batch_count BETWEEN 1 AND 65536),
  completed_iterations bigint NOT NULL CHECK(completed_iterations BETWEEN 0 AND batch_count),
  seed release.bench_u64 NOT NULL,
  input_bytes bytea NOT NULL CHECK(octet_length(input_bytes)=8258),
  expected_bytes bytea NOT NULL CHECK(octet_length(expected_bytes)=8258),
  actual_bytes bytea NOT NULL CHECK(octet_length(actual_bytes)=8258),
  execution_return bigint NOT NULL CHECK(execution_return BETWEEN 0 AND 4294967295),
  status bigint NOT NULL CHECK(status BETWEEN 0 AND 4294967295),
  result release.bench_u64 NOT NULL,
  digest bigint NOT NULL CHECK(digest BETWEEN 0 AND 4294967295),
  result_ok boolean NOT NULL,
  qualified boolean NOT NULL,
  FOREIGN KEY(run_id,mode,experiment) REFERENCES release.bench_run(run_id,mode,experiment),
  FOREIGN KEY(run_id,clock_id) REFERENCES release.bench_clock,
  PRIMARY KEY(run_id,sample_index),
  CHECK(end_tick>=begin_tick),
  CHECK(NOT result_ok OR actual_bytes=expected_bytes),
  CHECK(NOT qualified OR (
    result_ok AND status=0 AND execution_return=2
    AND completed_iterations=batch_count
  ))
);
CREATE TABLE release.jit_case_switch (
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
  host_command_begin_ns release.bench_u64 NOT NULL,
  host_reply_ns release.bench_u64 NOT NULL,
  dut_clock_id text,
  dut_begin_tick release.bench_u64,
  dut_end_tick release.bench_u64,
  quiesce_ticks release.bench_u64,
  rebind_ticks release.bench_u64,
  publish_ticks release.bench_u64,
  last_old_capture_id uuid,
  last_old_record_index bigint,
  first_new_capture_id uuid,
  first_new_record_index bigint,
  reply_transition_gap_ns release.bench_u64,
  old_after_new bigint CHECK(old_after_new>=0),
  command_ok boolean NOT NULL,
  error_code text,
  boot_unchanged boolean NOT NULL,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL,
  FOREIGN KEY(run_id,mode,experiment) REFERENCES release.bench_run(run_id,mode,experiment),
  FOREIGN KEY(run_id,host_clock_id) REFERENCES release.bench_clock,
  FOREIGN KEY(run_id,dut_clock_id) REFERENCES release.bench_clock,
  FOREIGN KEY(last_old_capture_id,run_id) REFERENCES release.bench_capture(capture_id,run_id),
  FOREIGN KEY(first_new_capture_id,run_id) REFERENCES release.bench_capture(capture_id,run_id),
  FOREIGN KEY(last_old_capture_id,last_old_record_index) REFERENCES release.bench_packet_fact,
  FOREIGN KEY(first_new_capture_id,first_new_record_index) REFERENCES release.bench_packet_fact,
  PRIMARY KEY(run_id,switch_index),
  CHECK(source_version<>target_version),
  CHECK(host_reply_ns>=host_command_begin_ns),
  CHECK(dut_end_tick IS NULL OR dut_begin_tick IS NULL OR dut_end_tick>=dut_begin_tick),
  CHECK((last_old_capture_id IS NULL)=(last_old_record_index IS NULL)),
  CHECK((first_new_capture_id IS NULL)=(first_new_record_index IS NULL)),
  CHECK(reply_transition_gap_ns IS NULL OR (last_old_capture_id IS NOT NULL AND first_new_capture_id IS NOT NULL)),
  CHECK(NOT qualified OR (command_ok AND boot_unchanged))
);
ALTER TABLE release.jit_case_config ADD CHECK(switch_kind<>'native_policy_select' AND source_mode='jit' AND target_mode='jit');
ALTER TABLE release.jit_case_switch ADD CHECK(switch_kind<>'native_policy_select');

-- rbpf tables use literal mode checks.
CREATE TABLE release.rbpf_metric_definition (
  metric_id uuid PRIMARY KEY,
  name text NOT NULL,
  kind text NOT NULL CHECK(kind IN ('counter','gauge','histogram')),
  unit text NOT NULL,
  boundary text NOT NULL,
  scope text NOT NULL,
  definition_version text NOT NULL,
  UNIQUE(metric_id,kind), UNIQUE(name,definition_version)
);
CREATE TABLE release.rbpf_metric_series (
  series_id uuid PRIMARY KEY,
  metric_id uuid NOT NULL,
  kind text NOT NULL,
  labels jsonb NOT NULL CHECK(jsonb_typeof(labels)='object'),
  FOREIGN KEY(metric_id,kind) REFERENCES release.rbpf_metric_definition(metric_id,kind),
  UNIQUE(series_id,kind), UNIQUE(metric_id,labels)
);
CREATE TABLE release.rbpf_metric_snapshot (
  snapshot_id uuid PRIMARY KEY,
  run_id uuid NOT NULL,
  mode text NOT NULL DEFAULT 'rbpf' CHECK(mode='rbpf'),
  phase text NOT NULL CHECK(phase IN ('baseline','final')),
  epoch bigint NOT NULL CHECK(epoch>=0),
  boot_id release.bench_u64 NOT NULL,
  clock_id text NOT NULL,
  at_tick release.bench_u64 NOT NULL,
  profile_hash text NOT NULL CHECK(profile_hash ~ '^[0-9a-f]{64}$'),
  raw_artifact_id uuid NOT NULL REFERENCES release.bench_artifact,
  FOREIGN KEY(run_id,mode) REFERENCES release.bench_run(run_id,mode),
  FOREIGN KEY(run_id,clock_id) REFERENCES release.bench_clock,
  UNIQUE(snapshot_id,run_id), UNIQUE(run_id,epoch,phase)
);
CREATE TABLE release.rbpf_scalar_sample (
  snapshot_id uuid NOT NULL REFERENCES release.rbpf_metric_snapshot,
  series_id uuid NOT NULL,
  kind text NOT NULL CHECK(kind IN ('counter','gauge')),
  value numeric NOT NULL,
  FOREIGN KEY(series_id,kind) REFERENCES release.rbpf_metric_series(series_id,kind),
  PRIMARY KEY(snapshot_id,series_id)
);
CREATE TABLE release.rbpf_histogram_sample (
  snapshot_id uuid NOT NULL REFERENCES release.rbpf_metric_snapshot,
  series_id uuid NOT NULL,
  kind text NOT NULL DEFAULT 'histogram' CHECK(kind='histogram'),
  calibration_id uuid NOT NULL REFERENCES release.bench_calibration,
  sample_count release.bench_u64 NOT NULL,
  sample_sum release.bench_u64 NOT NULL,
  FOREIGN KEY(series_id,kind) REFERENCES release.rbpf_metric_series(series_id,kind),
  PRIMARY KEY(snapshot_id,series_id),
  UNIQUE(snapshot_id,series_id,sample_count)
);
CREATE TABLE release.rbpf_histogram_bucket (
  snapshot_id uuid NOT NULL,
  series_id uuid NOT NULL,
  ordinal integer NOT NULL CHECK(ordinal>=1),
  upper_bound bigint CHECK(upper_bound>=0), -- NULL represents +Inf.
  cumulative_count release.bench_u64 NOT NULL,
  sample_count release.bench_u64 NOT NULL,
  FOREIGN KEY(snapshot_id,series_id,sample_count)
    REFERENCES release.rbpf_histogram_sample(snapshot_id,series_id,sample_count),
  PRIMARY KEY(snapshot_id,series_id,ordinal),
  CHECK(cumulative_count<=sample_count),
  CHECK(upper_bound IS NOT NULL OR cumulative_count=sample_count)
);
CREATE UNIQUE INDEX rbpf_hist_one_inf ON release.rbpf_histogram_bucket(snapshot_id,series_id)
  WHERE upper_bound IS NULL;

CREATE TABLE release.rbpf_firewall_config (
  config_id uuid PRIMARY KEY,
  condition_id uuid NOT NULL,
  experiment text NOT NULL DEFAULT 'firewall' CHECK(experiment='firewall'),
  instrumentation text NOT NULL CHECK(instrumentation IN ('counter_only','helper_hist','stage_hist','batch_hist','switch_hist','trace')),
  profile_hash text NOT NULL CHECK(profile_hash ~ '^[0-9a-f]{64}$'),
  calibration_ids uuid[] NOT NULL DEFAULT ARRAY[]::uuid[],
  parameters jsonb NOT NULL DEFAULT '{}',
  target_pps integer CHECK(target_pps>0),
  send_window_ns release.bench_u64,
  drain_ns release.bench_u64 NOT NULL,
  payload_bytes integer NOT NULL CHECK(payload_bytes>=32),
  blacklist_capacity integer NOT NULL CHECK(blacklist_capacity=256),
  blacklist_entries integer NOT NULL CHECK(blacklist_entries BETWEEN 1 AND 256),
  legal_numerator integer NOT NULL CHECK(legal_numerator BETWEEN 0 AND 10),
  legal_denominator integer NOT NULL CHECK(legal_denominator=10),
  lookup_case text NOT NULL CHECK(lookup_case IN ('mixed','hit','miss')),
  map_fixture_artifact_id uuid NOT NULL REFERENCES release.bench_artifact,
  FOREIGN KEY(condition_id,experiment) REFERENCES release.bench_condition(condition_id,experiment),
  UNIQUE(config_id,condition_id)
);
CREATE TABLE release.rbpf_firewall_metrics_data (
  run_id uuid NOT NULL,
  config_id uuid NOT NULL REFERENCES release.rbpf_firewall_config,
  mode text NOT NULL DEFAULT 'rbpf' CHECK(mode='rbpf'),
  experiment text NOT NULL DEFAULT 'firewall' CHECK(experiment='firewall'),
  variant text NOT NULL DEFAULT 'metrics' CHECK(variant='metrics'),
  baseline_snapshot_id uuid,
  final_snapshot_id uuid,
  analysis_id uuid NOT NULL,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL CHECK(jsonb_typeof(issues)='array'),
  sent_legal release.bench_u64, sent_illegal release.bench_u64,
  matched_legal release.bench_u64, blocked_hit_delta release.bench_u64,
  token_refused release.bench_u64,
  CHECK(matched_legal IS NULL OR sent_legal IS NULL OR matched_legal<=sent_legal),
  PRIMARY KEY(run_id,analysis_id),
  FOREIGN KEY(run_id,mode,experiment,variant,config_id)
    REFERENCES release.bench_run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(baseline_snapshot_id,run_id) REFERENCES release.rbpf_metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(final_snapshot_id,run_id) REFERENCES release.rbpf_metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(analysis_id,run_id) REFERENCES release.bench_analysis(analysis_id,run_id),
  CHECK(NOT qualified OR (baseline_snapshot_id IS NOT NULL AND final_snapshot_id IS NOT NULL))
);
CREATE TABLE release.rbpf_firewall_trace_data (
  run_id uuid PRIMARY KEY,
  config_id uuid NOT NULL REFERENCES release.rbpf_firewall_config,
  mode text NOT NULL DEFAULT 'rbpf' CHECK(mode='rbpf'),
  experiment text NOT NULL DEFAULT 'firewall' CHECK(experiment='firewall'),
  variant text NOT NULL DEFAULT 'trace' CHECK(variant='trace'),
  epoch bigint NOT NULL CHECK(epoch>=0),
  dictionary_version integer NOT NULL CHECK(dictionary_version>=1),
  boot_id release.bench_u64 NOT NULL,
  clock_id text NOT NULL,
  capacity integer NOT NULL CHECK(capacity>0),
  written integer NOT NULL CHECK(written>=0 AND written<=capacity),
  dropped bigint NOT NULL CHECK(dropped>=0),
  flags bigint NOT NULL CHECK(flags>=0),
  complete boolean NOT NULL,
  result_checked boolean NOT NULL,
  result_ok boolean,
  raw_artifact_id uuid NOT NULL REFERENCES release.bench_artifact,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL CHECK(jsonb_typeof(issues)='array'),
  FOREIGN KEY(run_id,mode,experiment,variant,config_id)
    REFERENCES release.bench_run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(run_id,clock_id) REFERENCES release.bench_clock,
  CHECK(NOT complete OR (dropped=0 AND flags=0)),
  CHECK(NOT result_checked OR result_ok IS NOT NULL),
  CHECK(NOT qualified OR (complete AND (NOT result_checked OR result_ok)))
);
CREATE TABLE release.rbpf_firewall_event (
  run_id uuid NOT NULL,
  mode text NOT NULL DEFAULT 'rbpf' CHECK(mode='rbpf'),
  experiment text NOT NULL DEFAULT 'firewall' CHECK(experiment='firewall'),
  source text NOT NULL CHECK(source IN ('dut','host','agent')),
  event_sequence bigint NOT NULL CHECK(event_sequence>=0),
  dictionary_version integer NOT NULL,
  event_id integer NOT NULL,
  clock_id text NOT NULL,
  timestamp_raw release.bench_u64 NOT NULL,
  kind text NOT NULL CHECK(kind IN ('begin','end','instant')),
  context_id integer NOT NULL CHECK(context_id BETWEEN 0 AND 255),
  probe_seq bigint CHECK(probe_seq>=1),
  span_id bigint CHECK(span_id>=0),
  parent_span_id bigint CHECK(parent_span_id>=0),
  arg0 bigint CHECK(arg0 BETWEEN 0 AND 4294967295),
  arg1 bigint CHECK(arg1 BETWEEN 0 AND 4294967295),
  FOREIGN KEY(run_id,mode,experiment) REFERENCES release.bench_run(run_id,mode,experiment),
  FOREIGN KEY(run_id,clock_id) REFERENCES release.bench_clock,
  FOREIGN KEY(experiment,dictionary_version,event_id) REFERENCES release.bench_event_definition,
  PRIMARY KEY(run_id,source,event_sequence)
);

CREATE TABLE release.rbpf_just_drop_config (
  config_id uuid PRIMARY KEY,
  condition_id uuid NOT NULL,
  experiment text NOT NULL DEFAULT 'just_drop' CHECK(experiment='just_drop'),
  instrumentation text NOT NULL CHECK(instrumentation IN ('counter_only','helper_hist','stage_hist','batch_hist','switch_hist','trace')),
  profile_hash text NOT NULL CHECK(profile_hash ~ '^[0-9a-f]{64}$'),
  calibration_ids uuid[] NOT NULL DEFAULT ARRAY[]::uuid[],
  parameters jsonb NOT NULL DEFAULT '{}',
  target_pps integer CHECK(target_pps>0),
  send_window_ns release.bench_u64,
  drain_ns release.bench_u64 NOT NULL,
  payload_bytes integer NOT NULL CHECK(payload_bytes>=32),
  workload_version text NOT NULL CHECK(workload_version='drop-minimal-v1'),
  neighbor_policy text NOT NULL,
  saturation_grid boolean NOT NULL,
  FOREIGN KEY(condition_id,experiment) REFERENCES release.bench_condition(condition_id,experiment),
  UNIQUE(config_id,condition_id)
);
CREATE TABLE release.rbpf_just_drop_metrics_data (
  run_id uuid NOT NULL,
  config_id uuid NOT NULL REFERENCES release.rbpf_just_drop_config,
  mode text NOT NULL DEFAULT 'rbpf' CHECK(mode='rbpf'),
  experiment text NOT NULL DEFAULT 'just_drop' CHECK(experiment='just_drop'),
  variant text NOT NULL DEFAULT 'metrics' CHECK(variant='metrics'),
  baseline_snapshot_id uuid,
  final_snapshot_id uuid,
  analysis_id uuid NOT NULL,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL CHECK(jsonb_typeof(issues)='array'),
  sent_success release.bench_u64, ingress_seen release.bench_u64,
  policy_enter release.bench_u64, normal_drop release.bench_u64,
  runtime_fault release.bench_u64, device_window_ticks release.bench_u64,
  sender_limited boolean NOT NULL,
  plateau_observed boolean NOT NULL,
  PRIMARY KEY(run_id,analysis_id),
  FOREIGN KEY(run_id,mode,experiment,variant,config_id)
    REFERENCES release.bench_run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(baseline_snapshot_id,run_id) REFERENCES release.rbpf_metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(final_snapshot_id,run_id) REFERENCES release.rbpf_metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(analysis_id,run_id) REFERENCES release.bench_analysis(analysis_id,run_id),
  CHECK(NOT qualified OR (baseline_snapshot_id IS NOT NULL AND final_snapshot_id IS NOT NULL))
);
CREATE TABLE release.rbpf_just_drop_trace_data (
  run_id uuid PRIMARY KEY,
  config_id uuid NOT NULL REFERENCES release.rbpf_just_drop_config,
  mode text NOT NULL DEFAULT 'rbpf' CHECK(mode='rbpf'),
  experiment text NOT NULL DEFAULT 'just_drop' CHECK(experiment='just_drop'),
  variant text NOT NULL DEFAULT 'trace' CHECK(variant='trace'),
  epoch bigint NOT NULL CHECK(epoch>=0),
  dictionary_version integer NOT NULL CHECK(dictionary_version>=1),
  boot_id release.bench_u64 NOT NULL,
  clock_id text NOT NULL,
  capacity integer NOT NULL CHECK(capacity>0),
  written integer NOT NULL CHECK(written>=0 AND written<=capacity),
  dropped bigint NOT NULL CHECK(dropped>=0),
  flags bigint NOT NULL CHECK(flags>=0),
  complete boolean NOT NULL,
  result_checked boolean NOT NULL,
  result_ok boolean,
  raw_artifact_id uuid NOT NULL REFERENCES release.bench_artifact,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL CHECK(jsonb_typeof(issues)='array'),
  FOREIGN KEY(run_id,mode,experiment,variant,config_id)
    REFERENCES release.bench_run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(run_id,clock_id) REFERENCES release.bench_clock,
  CHECK(NOT complete OR (dropped=0 AND flags=0)),
  CHECK(NOT result_checked OR result_ok IS NOT NULL),
  CHECK(NOT qualified OR (complete AND (NOT result_checked OR result_ok)))
);
CREATE TABLE release.rbpf_just_drop_event (
  run_id uuid NOT NULL,
  mode text NOT NULL DEFAULT 'rbpf' CHECK(mode='rbpf'),
  experiment text NOT NULL DEFAULT 'just_drop' CHECK(experiment='just_drop'),
  source text NOT NULL CHECK(source IN ('dut','host','agent')),
  event_sequence bigint NOT NULL CHECK(event_sequence>=0),
  dictionary_version integer NOT NULL,
  event_id integer NOT NULL,
  clock_id text NOT NULL,
  timestamp_raw release.bench_u64 NOT NULL,
  kind text NOT NULL CHECK(kind IN ('begin','end','instant')),
  context_id integer NOT NULL CHECK(context_id BETWEEN 0 AND 255),
  probe_seq bigint CHECK(probe_seq>=1),
  span_id bigint CHECK(span_id>=0),
  parent_span_id bigint CHECK(parent_span_id>=0),
  arg0 bigint CHECK(arg0 BETWEEN 0 AND 4294967295),
  arg1 bigint CHECK(arg1 BETWEEN 0 AND 4294967295),
  FOREIGN KEY(run_id,mode,experiment) REFERENCES release.bench_run(run_id,mode,experiment),
  FOREIGN KEY(run_id,clock_id) REFERENCES release.bench_clock,
  FOREIGN KEY(experiment,dictionary_version,event_id) REFERENCES release.bench_event_definition,
  PRIMARY KEY(run_id,source,event_sequence)
);

CREATE TABLE release.rbpf_just_tx_config (
  config_id uuid PRIMARY KEY,
  condition_id uuid NOT NULL,
  experiment text NOT NULL DEFAULT 'just_tx' CHECK(experiment='just_tx'),
  instrumentation text NOT NULL CHECK(instrumentation IN ('counter_only','helper_hist','stage_hist','batch_hist','switch_hist','trace')),
  profile_hash text NOT NULL CHECK(profile_hash ~ '^[0-9a-f]{64}$'),
  calibration_ids uuid[] NOT NULL DEFAULT ARRAY[]::uuid[],
  parameters jsonb NOT NULL DEFAULT '{}',
  target_pps integer CHECK(target_pps>0),
  send_window_ns release.bench_u64,
  drain_ns release.bench_u64 NOT NULL,
  payload_bytes integer NOT NULL CHECK(payload_bytes BETWEEN 32 AND 1472),
  probe_version text NOT NULL CHECK(probe_version='JTX1'),
  capture_interface text NOT NULL,
  FOREIGN KEY(condition_id,experiment) REFERENCES release.bench_condition(condition_id,experiment),
  UNIQUE(config_id,condition_id)
);
CREATE TABLE release.rbpf_just_tx_metrics_data (
  run_id uuid NOT NULL,
  config_id uuid NOT NULL REFERENCES release.rbpf_just_tx_config,
  mode text NOT NULL DEFAULT 'rbpf' CHECK(mode='rbpf'),
  experiment text NOT NULL DEFAULT 'just_tx' CHECK(experiment='just_tx'),
  variant text NOT NULL DEFAULT 'metrics' CHECK(variant='metrics'),
  baseline_snapshot_id uuid,
  final_snapshot_id uuid,
  analysis_id uuid NOT NULL,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL CHECK(jsonb_typeof(issues)='array'),
  sent_success release.bench_u64, returned release.bench_u64,
  duplicate_rx release.bench_u64, late_rx release.bench_u64,
  valid_rtt_count release.bench_u64,
  CHECK(returned IS NULL OR sent_success IS NULL OR returned<=sent_success),
  CHECK(valid_rtt_count IS NULL OR returned IS NULL OR valid_rtt_count<=returned),
  PRIMARY KEY(run_id,analysis_id),
  FOREIGN KEY(run_id,mode,experiment,variant,config_id)
    REFERENCES release.bench_run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(baseline_snapshot_id,run_id) REFERENCES release.rbpf_metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(final_snapshot_id,run_id) REFERENCES release.rbpf_metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(analysis_id,run_id) REFERENCES release.bench_analysis(analysis_id,run_id),
  CHECK(NOT qualified OR (baseline_snapshot_id IS NOT NULL AND final_snapshot_id IS NOT NULL))
);
CREATE TABLE release.rbpf_just_tx_trace_data (
  run_id uuid PRIMARY KEY,
  config_id uuid NOT NULL REFERENCES release.rbpf_just_tx_config,
  mode text NOT NULL DEFAULT 'rbpf' CHECK(mode='rbpf'),
  experiment text NOT NULL DEFAULT 'just_tx' CHECK(experiment='just_tx'),
  variant text NOT NULL DEFAULT 'trace' CHECK(variant='trace'),
  epoch bigint NOT NULL CHECK(epoch>=0),
  dictionary_version integer NOT NULL CHECK(dictionary_version>=1),
  boot_id release.bench_u64 NOT NULL,
  clock_id text NOT NULL,
  capacity integer NOT NULL CHECK(capacity>0),
  written integer NOT NULL CHECK(written>=0 AND written<=capacity),
  dropped bigint NOT NULL CHECK(dropped>=0),
  flags bigint NOT NULL CHECK(flags>=0),
  complete boolean NOT NULL,
  result_checked boolean NOT NULL,
  result_ok boolean,
  raw_artifact_id uuid NOT NULL REFERENCES release.bench_artifact,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL CHECK(jsonb_typeof(issues)='array'),
  FOREIGN KEY(run_id,mode,experiment,variant,config_id)
    REFERENCES release.bench_run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(run_id,clock_id) REFERENCES release.bench_clock,
  CHECK(NOT complete OR (dropped=0 AND flags=0)),
  CHECK(NOT result_checked OR result_ok IS NOT NULL),
  CHECK(NOT qualified OR (complete AND (NOT result_checked OR result_ok)))
);
CREATE TABLE release.rbpf_just_tx_event (
  run_id uuid NOT NULL,
  mode text NOT NULL DEFAULT 'rbpf' CHECK(mode='rbpf'),
  experiment text NOT NULL DEFAULT 'just_tx' CHECK(experiment='just_tx'),
  source text NOT NULL CHECK(source IN ('dut','host','agent')),
  event_sequence bigint NOT NULL CHECK(event_sequence>=0),
  dictionary_version integer NOT NULL,
  event_id integer NOT NULL,
  clock_id text NOT NULL,
  timestamp_raw release.bench_u64 NOT NULL,
  kind text NOT NULL CHECK(kind IN ('begin','end','instant')),
  context_id integer NOT NULL CHECK(context_id BETWEEN 0 AND 255),
  probe_seq bigint CHECK(probe_seq>=1),
  span_id bigint CHECK(span_id>=0),
  parent_span_id bigint CHECK(parent_span_id>=0),
  arg0 bigint CHECK(arg0 BETWEEN 0 AND 4294967295),
  arg1 bigint CHECK(arg1 BETWEEN 0 AND 4294967295),
  FOREIGN KEY(run_id,mode,experiment) REFERENCES release.bench_run(run_id,mode,experiment),
  FOREIGN KEY(run_id,clock_id) REFERENCES release.bench_clock,
  FOREIGN KEY(experiment,dictionary_version,event_id) REFERENCES release.bench_event_definition,
  PRIMARY KEY(run_id,source,event_sequence)
);

CREATE TABLE release.rbpf_exec_bench_config (
  config_id uuid PRIMARY KEY,
  condition_id uuid NOT NULL,
  experiment text NOT NULL DEFAULT 'exec_bench' CHECK(experiment='exec_bench'),
  instrumentation text NOT NULL CHECK(instrumentation IN ('counter_only','helper_hist','stage_hist','batch_hist','switch_hist','trace')),
  profile_hash text NOT NULL CHECK(profile_hash ~ '^[0-9a-f]{64}$'),
  calibration_ids uuid[] NOT NULL DEFAULT ARRAY[]::uuid[],
  parameters jsonb NOT NULL DEFAULT '{}',
  workload text NOT NULL CHECK(workload IN ('noop','prime','log2','copy','fletcher32','bsort','strcmp')),
  work_param integer NOT NULL CHECK(work_param>=0),
  batch_count bigint NOT NULL CHECK(batch_count BETWEEN 1 AND 65536),
  samples_per_run integer NOT NULL CHECK(samples_per_run>0),
  input_bytes integer NOT NULL CHECK(input_bytes=8258),
  input_protocol text NOT NULL CHECK(input_protocol='EXB2'),
  wifi_enabled boolean NOT NULL CHECK(NOT wifi_enabled),
  step_budget release.bench_u64,
  fixture_artifact_id uuid NOT NULL REFERENCES release.bench_artifact,
  FOREIGN KEY(condition_id,experiment) REFERENCES release.bench_condition(condition_id,experiment),
  UNIQUE(config_id,condition_id)
);
CREATE TABLE release.rbpf_exec_bench_metrics_data (
  run_id uuid NOT NULL,
  config_id uuid NOT NULL REFERENCES release.rbpf_exec_bench_config,
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
    REFERENCES release.bench_run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(baseline_snapshot_id,run_id) REFERENCES release.rbpf_metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(final_snapshot_id,run_id) REFERENCES release.rbpf_metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(analysis_id,run_id) REFERENCES release.bench_analysis(analysis_id,run_id),
  CHECK(NOT qualified OR (baseline_snapshot_id IS NOT NULL AND final_snapshot_id IS NOT NULL))
);
CREATE TABLE release.rbpf_exec_bench_trace_data (
  run_id uuid PRIMARY KEY,
  config_id uuid NOT NULL REFERENCES release.rbpf_exec_bench_config,
  mode text NOT NULL DEFAULT 'rbpf' CHECK(mode='rbpf'),
  experiment text NOT NULL DEFAULT 'exec_bench' CHECK(experiment='exec_bench'),
  variant text NOT NULL DEFAULT 'trace' CHECK(variant='trace'),
  epoch bigint NOT NULL CHECK(epoch>=0),
  dictionary_version integer NOT NULL CHECK(dictionary_version>=1),
  boot_id release.bench_u64 NOT NULL,
  clock_id text NOT NULL,
  capacity integer NOT NULL CHECK(capacity>0),
  written integer NOT NULL CHECK(written>=0 AND written<=capacity),
  dropped bigint NOT NULL CHECK(dropped>=0),
  flags bigint NOT NULL CHECK(flags>=0),
  complete boolean NOT NULL,
  result_checked boolean NOT NULL,
  result_ok boolean,
  raw_artifact_id uuid NOT NULL REFERENCES release.bench_artifact,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL CHECK(jsonb_typeof(issues)='array'),
  FOREIGN KEY(run_id,mode,experiment,variant,config_id)
    REFERENCES release.bench_run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(run_id,clock_id) REFERENCES release.bench_clock,
  CHECK(NOT complete OR (dropped=0 AND flags=0)),
  CHECK(NOT result_checked OR result_ok IS NOT NULL),
  CHECK(NOT qualified OR (complete AND (NOT result_checked OR result_ok)))
);
CREATE TABLE release.rbpf_exec_bench_event (
  run_id uuid NOT NULL,
  mode text NOT NULL DEFAULT 'rbpf' CHECK(mode='rbpf'),
  experiment text NOT NULL DEFAULT 'exec_bench' CHECK(experiment='exec_bench'),
  source text NOT NULL CHECK(source IN ('dut','host','agent')),
  event_sequence bigint NOT NULL CHECK(event_sequence>=0),
  dictionary_version integer NOT NULL,
  event_id integer NOT NULL,
  clock_id text NOT NULL,
  timestamp_raw release.bench_u64 NOT NULL,
  kind text NOT NULL CHECK(kind IN ('begin','end','instant')),
  context_id integer NOT NULL CHECK(context_id BETWEEN 0 AND 255),
  probe_seq bigint CHECK(probe_seq>=1),
  span_id bigint CHECK(span_id>=0),
  parent_span_id bigint CHECK(parent_span_id>=0),
  arg0 bigint CHECK(arg0 BETWEEN 0 AND 4294967295),
  arg1 bigint CHECK(arg1 BETWEEN 0 AND 4294967295),
  FOREIGN KEY(run_id,mode,experiment) REFERENCES release.bench_run(run_id,mode,experiment),
  FOREIGN KEY(run_id,clock_id) REFERENCES release.bench_clock,
  FOREIGN KEY(experiment,dictionary_version,event_id) REFERENCES release.bench_event_definition,
  PRIMARY KEY(run_id,source,event_sequence)
);

CREATE TABLE release.rbpf_case_config (
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
  pre_window_ns release.bench_u64 NOT NULL,
  post_window_ns release.bench_u64 NOT NULL,
  payload_bytes integer NOT NULL CHECK(payload_bytes=40),
  source_mode text NOT NULL,
  target_mode text NOT NULL,
  FOREIGN KEY(condition_id,experiment) REFERENCES release.bench_condition(condition_id,experiment),
  UNIQUE(config_id,condition_id)
);
CREATE TABLE release.rbpf_case_metrics_data (
  run_id uuid NOT NULL,
  config_id uuid NOT NULL REFERENCES release.rbpf_case_config,
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
  old_after_new release.bench_u64,
  PRIMARY KEY(run_id,analysis_id),
  FOREIGN KEY(run_id,mode,experiment,variant,config_id)
    REFERENCES release.bench_run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(baseline_snapshot_id,run_id) REFERENCES release.rbpf_metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(final_snapshot_id,run_id) REFERENCES release.rbpf_metric_snapshot(snapshot_id,run_id),
  FOREIGN KEY(analysis_id,run_id) REFERENCES release.bench_analysis(analysis_id,run_id),
  CHECK(NOT qualified OR (baseline_snapshot_id IS NOT NULL AND final_snapshot_id IS NOT NULL))
);
CREATE TABLE release.rbpf_case_trace_data (
  run_id uuid PRIMARY KEY,
  config_id uuid NOT NULL REFERENCES release.rbpf_case_config,
  mode text NOT NULL DEFAULT 'rbpf' CHECK(mode='rbpf'),
  experiment text NOT NULL DEFAULT 'case' CHECK(experiment='case'),
  variant text NOT NULL DEFAULT 'trace' CHECK(variant='trace'),
  epoch bigint NOT NULL CHECK(epoch>=0),
  dictionary_version integer NOT NULL CHECK(dictionary_version>=1),
  boot_id release.bench_u64 NOT NULL,
  clock_id text NOT NULL,
  capacity integer NOT NULL CHECK(capacity>0),
  written integer NOT NULL CHECK(written>=0 AND written<=capacity),
  dropped bigint NOT NULL CHECK(dropped>=0),
  flags bigint NOT NULL CHECK(flags>=0),
  complete boolean NOT NULL,
  result_checked boolean NOT NULL,
  result_ok boolean,
  raw_artifact_id uuid NOT NULL REFERENCES release.bench_artifact,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL CHECK(jsonb_typeof(issues)='array'),
  FOREIGN KEY(run_id,mode,experiment,variant,config_id)
    REFERENCES release.bench_run(run_id,mode,experiment,variant,config_id),
  FOREIGN KEY(run_id,clock_id) REFERENCES release.bench_clock,
  CHECK(NOT complete OR (dropped=0 AND flags=0)),
  CHECK(NOT result_checked OR result_ok IS NOT NULL),
  CHECK(NOT qualified OR (complete AND (NOT result_checked OR result_ok)))
);
CREATE TABLE release.rbpf_case_event (
  run_id uuid NOT NULL,
  mode text NOT NULL DEFAULT 'rbpf' CHECK(mode='rbpf'),
  experiment text NOT NULL DEFAULT 'case' CHECK(experiment='case'),
  source text NOT NULL CHECK(source IN ('dut','host','agent')),
  event_sequence bigint NOT NULL CHECK(event_sequence>=0),
  dictionary_version integer NOT NULL,
  event_id integer NOT NULL,
  clock_id text NOT NULL,
  timestamp_raw release.bench_u64 NOT NULL,
  kind text NOT NULL CHECK(kind IN ('begin','end','instant')),
  context_id integer NOT NULL CHECK(context_id BETWEEN 0 AND 255),
  probe_seq bigint CHECK(probe_seq>=1),
  span_id bigint CHECK(span_id>=0),
  parent_span_id bigint CHECK(parent_span_id>=0),
  arg0 bigint CHECK(arg0 BETWEEN 0 AND 4294967295),
  arg1 bigint CHECK(arg1 BETWEEN 0 AND 4294967295),
  FOREIGN KEY(run_id,mode,experiment) REFERENCES release.bench_run(run_id,mode,experiment),
  FOREIGN KEY(run_id,clock_id) REFERENCES release.bench_clock,
  FOREIGN KEY(experiment,dictionary_version,event_id) REFERENCES release.bench_event_definition,
  PRIMARY KEY(run_id,source,event_sequence)
);

CREATE TABLE release.rbpf_probe_result (
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
  rtt_ns release.bench_u64,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL,
  FOREIGN KEY(run_id,mode) REFERENCES release.bench_run(run_id,mode),
  FOREIGN KEY(analysis_id,run_id) REFERENCES release.bench_analysis(analysis_id,run_id),
  FOREIGN KEY(run_id,seq) REFERENCES release.bench_send_attempt,
  FOREIGN KEY(tx_capture_id,run_id) REFERENCES release.bench_capture(capture_id,run_id),
  FOREIGN KEY(rx_capture_id,run_id) REFERENCES release.bench_capture(capture_id,run_id),
  FOREIGN KEY(tx_capture_id,tx_record_index) REFERENCES release.bench_packet_fact,
  FOREIGN KEY(rx_capture_id,rx_record_index) REFERENCES release.bench_packet_fact,
  PRIMARY KEY(analysis_id,seq),
  CHECK((tx_capture_id IS NULL)=(tx_record_index IS NULL)),
  CHECK((rx_capture_id IS NULL)=(rx_record_index IS NULL)),
  CHECK(rtt_ns IS NULL OR (returned AND qualified AND tx_capture_id IS NOT NULL AND rx_capture_id IS NOT NULL))
);
CREATE TABLE release.rbpf_firewall_token (
  run_id uuid NOT NULL,
  mode text NOT NULL DEFAULT 'rbpf' CHECK(mode='rbpf'),
  experiment text NOT NULL DEFAULT 'firewall' CHECK(experiment='firewall'),
  seq bigint NOT NULL,
  sent boolean NOT NULL,
  observed boolean NOT NULL,
  received_bytes bytea,
  FOREIGN KEY(run_id,mode,experiment) REFERENCES release.bench_run(run_id,mode,experiment),
  FOREIGN KEY(run_id,seq) REFERENCES release.bench_send_attempt,
  PRIMARY KEY(run_id,seq)
);
CREATE TABLE release.rbpf_exec_batch (
  run_id uuid NOT NULL,
  mode text NOT NULL DEFAULT 'rbpf' CHECK(mode='rbpf'),
  experiment text NOT NULL DEFAULT 'exec_bench' CHECK(experiment='exec_bench'),
  sample_index integer NOT NULL CHECK(sample_index>=0),
  clock_id text NOT NULL,
  begin_tick release.bench_u64 NOT NULL,
  end_tick release.bench_u64 NOT NULL,
  batch_count bigint NOT NULL CHECK(batch_count BETWEEN 1 AND 65536),
  completed_iterations bigint NOT NULL CHECK(completed_iterations BETWEEN 0 AND batch_count),
  seed release.bench_u64 NOT NULL,
  input_bytes bytea NOT NULL CHECK(octet_length(input_bytes)=8258),
  expected_bytes bytea NOT NULL CHECK(octet_length(expected_bytes)=8258),
  actual_bytes bytea NOT NULL CHECK(octet_length(actual_bytes)=8258),
  execution_return bigint NOT NULL CHECK(execution_return BETWEEN 0 AND 4294967295),
  status bigint NOT NULL CHECK(status BETWEEN 0 AND 4294967295),
  result release.bench_u64 NOT NULL,
  digest bigint NOT NULL CHECK(digest BETWEEN 0 AND 4294967295),
  result_ok boolean NOT NULL,
  qualified boolean NOT NULL,
  FOREIGN KEY(run_id,mode,experiment) REFERENCES release.bench_run(run_id,mode,experiment),
  FOREIGN KEY(run_id,clock_id) REFERENCES release.bench_clock,
  PRIMARY KEY(run_id,sample_index),
  CHECK(end_tick>=begin_tick),
  CHECK(NOT result_ok OR actual_bytes=expected_bytes),
  CHECK(NOT qualified OR (
    result_ok AND status=0 AND execution_return=2
    AND completed_iterations=batch_count
  ))
);
CREATE TABLE release.rbpf_case_switch (
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
  host_command_begin_ns release.bench_u64 NOT NULL,
  host_reply_ns release.bench_u64 NOT NULL,
  dut_clock_id text,
  dut_begin_tick release.bench_u64,
  dut_end_tick release.bench_u64,
  quiesce_ticks release.bench_u64,
  rebind_ticks release.bench_u64,
  publish_ticks release.bench_u64,
  last_old_capture_id uuid,
  last_old_record_index bigint,
  first_new_capture_id uuid,
  first_new_record_index bigint,
  reply_transition_gap_ns release.bench_u64,
  old_after_new bigint CHECK(old_after_new>=0),
  command_ok boolean NOT NULL,
  error_code text,
  boot_unchanged boolean NOT NULL,
  qualified boolean NOT NULL,
  issues jsonb NOT NULL,
  FOREIGN KEY(run_id,mode,experiment) REFERENCES release.bench_run(run_id,mode,experiment),
  FOREIGN KEY(run_id,host_clock_id) REFERENCES release.bench_clock,
  FOREIGN KEY(run_id,dut_clock_id) REFERENCES release.bench_clock,
  FOREIGN KEY(last_old_capture_id,run_id) REFERENCES release.bench_capture(capture_id,run_id),
  FOREIGN KEY(first_new_capture_id,run_id) REFERENCES release.bench_capture(capture_id,run_id),
  FOREIGN KEY(last_old_capture_id,last_old_record_index) REFERENCES release.bench_packet_fact,
  FOREIGN KEY(first_new_capture_id,first_new_record_index) REFERENCES release.bench_packet_fact,
  PRIMARY KEY(run_id,switch_index),
  CHECK(source_version<>target_version),
  CHECK(host_reply_ns>=host_command_begin_ns),
  CHECK(dut_end_tick IS NULL OR dut_begin_tick IS NULL OR dut_end_tick>=dut_begin_tick),
  CHECK((last_old_capture_id IS NULL)=(last_old_record_index IS NULL)),
  CHECK((first_new_capture_id IS NULL)=(first_new_record_index IS NULL)),
  CHECK(reply_transition_gap_ns IS NULL OR (last_old_capture_id IS NOT NULL AND first_new_capture_id IS NOT NULL)),
  CHECK(NOT qualified OR (command_ok AND boot_unchanged))
);
ALTER TABLE release.rbpf_case_config ADD CHECK(switch_kind<>'native_policy_select' AND source_mode='rbpf' AND target_mode='rbpf');
ALTER TABLE release.rbpf_case_switch ADD CHECK(switch_kind<>'native_policy_select');

-- Append-only evidence and analysis rows. A new analysis receives a new ID.
CREATE FUNCTION release.bench_reject_rewrite() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
  RAISE EXCEPTION 'append-only table: %', TG_TABLE_SCHEMA || '.' || TG_TABLE_NAME USING ERRCODE='55000';
END $$;
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.bench_artifact FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.bench_send_attempt FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.bench_capture FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.bench_packet_fact FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.bench_calibration_sample FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.bench_analysis FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.hal_metric_definition FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.hal_metric_series FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.hal_metric_snapshot FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.hal_scalar_sample FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.hal_histogram_sample FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.hal_histogram_bucket FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.hal_probe_result FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.hal_firewall_token FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.hal_exec_batch FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.hal_case_switch FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.hal_firewall_metrics_data FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.hal_firewall_trace_data FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.hal_firewall_event FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.hal_just_drop_metrics_data FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.hal_just_drop_trace_data FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.hal_just_drop_event FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.hal_just_tx_metrics_data FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.hal_just_tx_trace_data FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.hal_just_tx_event FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.hal_exec_bench_metrics_data FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.hal_exec_bench_trace_data FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.hal_exec_bench_event FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.hal_case_metrics_data FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.hal_case_trace_data FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.hal_case_event FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.aot_metric_definition FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.aot_metric_series FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.aot_metric_snapshot FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.aot_scalar_sample FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.aot_histogram_sample FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.aot_histogram_bucket FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.aot_probe_result FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.aot_firewall_token FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.aot_exec_batch FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.aot_case_switch FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.aot_firewall_metrics_data FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.aot_firewall_trace_data FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.aot_firewall_event FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.aot_just_drop_metrics_data FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.aot_just_drop_trace_data FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.aot_just_drop_event FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.aot_just_tx_metrics_data FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.aot_just_tx_trace_data FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.aot_just_tx_event FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.aot_exec_bench_metrics_data FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.aot_exec_bench_trace_data FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.aot_exec_bench_event FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.aot_case_metrics_data FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.aot_case_trace_data FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.aot_case_event FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.jit_metric_definition FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.jit_metric_series FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.jit_metric_snapshot FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.jit_scalar_sample FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.jit_histogram_sample FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.jit_histogram_bucket FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.jit_probe_result FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.jit_firewall_token FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.jit_exec_batch FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.jit_case_switch FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.jit_firewall_metrics_data FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.jit_firewall_trace_data FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.jit_firewall_event FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.jit_just_drop_metrics_data FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.jit_just_drop_trace_data FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.jit_just_drop_event FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.jit_just_tx_metrics_data FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.jit_just_tx_trace_data FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.jit_just_tx_event FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.jit_exec_bench_metrics_data FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.jit_exec_bench_trace_data FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.jit_exec_bench_event FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.jit_case_metrics_data FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.jit_case_trace_data FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.jit_case_event FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.rbpf_metric_definition FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.rbpf_metric_series FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.rbpf_metric_snapshot FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.rbpf_scalar_sample FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.rbpf_histogram_sample FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.rbpf_histogram_bucket FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.rbpf_probe_result FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.rbpf_firewall_token FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.rbpf_exec_batch FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.rbpf_case_switch FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.rbpf_firewall_metrics_data FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.rbpf_firewall_trace_data FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.rbpf_firewall_event FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.rbpf_just_drop_metrics_data FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.rbpf_just_drop_trace_data FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.rbpf_just_drop_event FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.rbpf_just_tx_metrics_data FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.rbpf_just_tx_trace_data FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.rbpf_just_tx_event FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.rbpf_exec_bench_metrics_data FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.rbpf_exec_bench_trace_data FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.rbpf_exec_bench_event FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.rbpf_case_metrics_data FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.rbpf_case_trace_data FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();
CREATE TRIGGER immutable_evidence BEFORE UPDATE OR DELETE ON release.rbpf_case_event FOR EACH ROW EXECUTE FUNCTION release.bench_reject_rewrite();

-- Config resolution is checked even before result tables are written.
CREATE FUNCTION release.bench_check_run_config() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE ok boolean;
BEGIN
  EXECUTE format('SELECT EXISTS(SELECT 1 FROM release.%I WHERE config_id=$1 AND condition_id=$2)',
    NEW.mode || '_' || NEW.experiment || '_config') INTO ok USING NEW.config_id,NEW.condition_id;
  IF NOT ok THEN RAISE EXCEPTION 'run config or condition mismatch'; END IF;
  RETURN NEW;
END $$;
CREATE TRIGGER run_config_guard BEFORE INSERT OR UPDATE OF config_id,condition_id,mode,experiment
  ON release.bench_run FOR EACH ROW EXECUTE FUNCTION release.bench_check_run_config();
COMMIT;
