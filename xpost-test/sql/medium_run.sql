-- Read one Protocol V1 attempt from the release schema.
-- psql -X -v ON_ERROR_STOP=1 -v run_id=<uuid> -f xpost-test/sql/medium_run.sql <database>
BEGIN READ ONLY;

SELECT mode, experiment, variant
FROM release.bench_run
WHERE run_id = :'run_id'::uuid
\gset

SELECT r.run_id, r.run_uuid, r.condition_id, r.config_id, r.experiment, r.mode,
       r.variant, r.phase, r.repeat_no, r.status, r.issues, r.boot_id,
       r.xpost_commit, r.libbpf_commit, r.send_elapsed_ns, r.export_elapsed_ns,
       c.protocol_version, c.parameters
FROM release.bench_run r
JOIN release.bench_condition c USING (condition_id, experiment)
WHERE r.run_id = :'run_id'::uuid;

SELECT ra.role, a.artifact_id, a.sha256, a.size_bytes, a.metadata
FROM release.bench_run_artifact ra
JOIN release.bench_artifact a USING (artifact_id)
WHERE ra.run_id = :'run_id'::uuid
ORDER BY ra.role, a.sha256;

SELECT clock_id, domain, tick_hz, tick_bits, boot_id
FROM release.bench_clock
WHERE run_id = :'run_id'::uuid
ORDER BY clock_id;

SELECT format(
  'SELECT c.* FROM release.%I c JOIN release.bench_run r USING (config_id, condition_id) WHERE r.run_id = %L',
  :'mode' || '_' || :'experiment' || '_config', :'run_id'
)
\gexec

SELECT format(
  'SELECT * FROM release.%I WHERE run_id = %L',
  :'mode' || '_' || :'experiment' || '_' || :'variant' || '_data', :'run_id'
)
\gexec

SELECT CASE :'variant'
  WHEN 'trace' THEN format(
    'SELECT * FROM release.%I WHERE run_id = %L ORDER BY source, event_sequence',
    :'mode' || '_' || :'experiment' || '_event', :'run_id'
  )
  WHEN 'metrics' THEN format(
    'SELECT snapshot_id, phase, boot_id, epoch, clock_id, at_tick, profile_hash
     FROM release.%I WHERE run_id = %L ORDER BY at_tick',
    :'mode' || '_metric_snapshot',
    :'run_id'
  )
END
\gexec

SELECT format(
  'SELECT s.phase, s.boot_id, s.epoch, d.name, d.kind, d.unit, d.boundary,
          series.labels, v.value
   FROM release.%1$I s
   JOIN release.%2$I v USING (snapshot_id)
   JOIN release.%3$I series USING (series_id)
   JOIN release.%4$I d USING (metric_id)
   WHERE s.run_id = %5$L ORDER BY s.at_tick, d.name, series.labels',
  :'mode' || '_metric_snapshot',
  :'mode' || '_scalar_sample',
  :'mode' || '_metric_series',
  :'mode' || '_metric_definition',
  :'run_id'
)
WHERE :'variant' = 'metrics'
\gexec

SELECT format(
  'SELECT s.phase, s.boot_id, s.epoch, d.name, d.unit, d.boundary, series.labels,
          h.calibration_id, h.sample_count, h.sample_sum,
          b.ordinal, b.upper_bound, b.cumulative_count
   FROM release.%1$I s
   JOIN release.%2$I h USING (snapshot_id)
   JOIN release.%3$I b USING (snapshot_id, series_id)
   JOIN release.%4$I series USING (series_id)
   JOIN release.%5$I d USING (metric_id)
   WHERE s.run_id = %6$L ORDER BY s.at_tick, d.name, series.labels, b.ordinal',
  :'mode' || '_metric_snapshot',
  :'mode' || '_histogram_sample',
  :'mode' || '_histogram_bucket',
  :'mode' || '_metric_series',
  :'mode' || '_metric_definition',
  :'run_id'
)
WHERE :'variant' = 'metrics'
\gexec

SELECT CASE :'experiment'
  WHEN 'just_tx' THEN format(
    'SELECT d.analysis_id, d.qualified,
            d.sent_success * 1e9 / NULLIF(r.send_elapsed_ns, 0) AS offered_pps,
            1 - d.returned / NULLIF(d.sent_success, 0) AS observed_loss,
            CASE WHEN d.qualified THEN 1 - d.returned / NULLIF(d.sent_success, 0) END AS qualified_loss,
            count(p.rtt_ns) AS rtt_samples,
            percentile_cont(0.50) WITHIN GROUP (ORDER BY p.rtt_ns) AS median_rtt_ns,
            percentile_cont(0.95) WITHIN GROUP (ORDER BY p.rtt_ns) AS p95_rtt_ns
     FROM release.%1$I d
     JOIN release.bench_run r USING (run_id)
     LEFT JOIN release.%2$I p ON (p.run_id, p.analysis_id) = (d.run_id, d.analysis_id) AND p.qualified
     WHERE d.run_id = %3$L
     GROUP BY d.analysis_id, d.qualified, d.sent_success, d.returned, r.send_elapsed_ns
     ORDER BY d.analysis_id',
    :'mode' || '_just_tx_metrics_data',
    :'mode' || '_probe_result',
    :'run_id'
  )
  WHEN 'just_drop' THEN format(
    'SELECT d.analysis_id, d.qualified,
            d.sent_success * 1e9 / NULLIF(r.send_elapsed_ns, 0) AS offered_pps,
            CASE WHEN d.qualified AND b.boot_id = f.boot_id AND b.epoch = f.epoch
              AND b.clock_id = f.clock_id AND b.profile_hash = f.profile_hash
              THEN d.ingress_seen * c.tick_hz / NULLIF(d.device_window_ticks, 0) END AS ingress_pps,
            CASE WHEN d.qualified AND b.boot_id = f.boot_id AND b.epoch = f.epoch
              AND b.clock_id = f.clock_id AND b.profile_hash = f.profile_hash
              THEN d.normal_drop * c.tick_hz / NULLIF(d.device_window_ticks, 0) END AS completed_drop_pps,
            d.runtime_fault
     FROM release.%1$I d
     JOIN release.bench_run r USING (run_id)
     LEFT JOIN release.%2$I b ON b.snapshot_id = d.baseline_snapshot_id AND b.phase = ''baseline''
     LEFT JOIN release.%2$I f ON f.snapshot_id = d.final_snapshot_id AND f.phase = ''final''
     LEFT JOIN release.bench_clock c ON (c.run_id, c.clock_id, c.boot_id) = (d.run_id, f.clock_id, f.boot_id)
       AND c.domain = ''dut''
     WHERE d.run_id = %3$L ORDER BY d.analysis_id',
    :'mode' || '_just_drop_metrics_data',
    :'mode' || '_metric_snapshot',
    :'run_id'
  )
  WHEN 'firewall' THEN format(
    'SELECT analysis_id, qualified, matched_legal, sent_legal, blocked_hit_delta, sent_illegal,
            matched_legal / NULLIF(sent_legal, 0) AS legal_delivery_ratio,
            blocked_hit_delta / NULLIF(sent_illegal, 0) AS illegal_hit_ratio, token_refused
     FROM release.%I WHERE run_id = %L ORDER BY analysis_id',
    :'mode' || '_firewall_metrics_data',
    :'run_id'
  )
  WHEN 'exec_bench' THEN format(
    'SELECT c.workload, c.input_protocol, b.sample_index, b.clock_id, b.execution_return,
            encode(substring(b.input_bytes for 64), ''hex'') AS request_header_hex,
            b.status, b.batch_count, b.completed_iterations, b.result, b.digest,
            b.result_ok, b.qualified, b.end_tick - b.begin_tick AS batch_ticks,
            CASE WHEN b.qualified AND r.status = ''complete''
              AND clock.domain = ''dut'' AND clock.boot_id = r.boot_id
              THEN (b.end_tick - b.begin_tick) / b.batch_count END AS ticks_per_round
     FROM release.%1$I b JOIN release.bench_run r USING (run_id)
     JOIN release.%2$I c USING (config_id, condition_id)
     JOIN release.bench_clock clock USING (run_id, clock_id)
     WHERE b.run_id = %3$L ORDER BY b.sample_index',
    :'mode' || '_exec_batch',
    :'mode' || '_exec_bench_config',
    :'run_id'
  )
  WHEN 'case' THEN format(
    'SELECT switch_index, switch_kind, source_version, target_version, source_slot, target_slot,
            source_generation, target_generation, dut_clock_id, dut_end_tick - dut_begin_tick AS dut_switch_ticks,
            quiesce_ticks, publish_ticks, host_reply_ns - host_command_begin_ns AS host_command_ns,
            reply_transition_gap_ns, old_after_new, command_ok, boot_unchanged, qualified, issues
     FROM release.%I WHERE run_id = %L ORDER BY switch_index',
    :'mode' || '_case_switch',
    :'run_id'
  )
END
\gexec

COMMIT;
