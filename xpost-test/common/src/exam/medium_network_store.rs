//! Store network attempts and their evidence in the release schema.

use std::{
    fs,
    io::{BufWriter, Write},
    net::Ipv4Addr,
    path::PathBuf,
};

use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use tokio_postgres::{Client, Transaction, types::ToSql};
use uuid::Uuid;

use crate::db::{Collection, metrics, protocol};

use super::{Analysis, Cell, Link, Observation, Stimulus, Workload, seed, sent_token};

// Bound JSON construction and PostgreSQL binding memory on TR3000.
// The enclosing transaction still commits or rolls back the entire run.
const EVIDENCE_ROWS_PER_BIND: usize = 4096;

pub(super) struct Attempt {
    run: Uuid,
    config: Uuid,
}

impl Attempt {
    pub(super) async fn finish_recovering(
        &self,
        client: &mut Client,
        database: &Collection<'_>,
        cell: &Cell<'_>,
        link: &Link,
        out: &Observation,
        result: &Analysis,
        intended: &str,
    ) -> Result<(String, bool)> {
        let initial = match self.finish(client, cell, link, out, result, intended).await {
            Ok(()) => return Ok((intended.into(), false)),
            Err(error) => error,
        };
        *client = database
            .connect()
            .await
            .with_context(|| format!("reconnect after finish failed: {initial:#}"))?;
        let status = self.status(client).await?;
        if terminal_status(&status) {
            return Ok((status, true));
        }
        ensure!(
            status == "running",
            "cannot recover network attempt with status {status}"
        );
        match self.finish(client, cell, link, out, result, intended).await {
            Ok(()) => Ok((intended.into(), true)),
            Err(error) => {
                if client.is_closed() {
                    *client = database.connect().await?;
                }
                let status = self.status(client).await?;
                ensure!(
                    terminal_status(&status),
                    "finish retry failed: {error:#} ({status})"
                );
                Ok((status, true))
            }
        }
    }

    async fn status(&self, client: &Client) -> Result<String> {
        Ok(client
            .query_one(
                "SELECT status FROM release.bench_run WHERE run_id=$1",
                &[&self.run],
            )
            .await
            .context("query network attempt after failed finish")?
            .get(0))
    }

    pub(super) async fn start(
        client: &mut Client,
        cell: &Cell<'_>,
        link: &Link,
        run: Uuid,
        order: i32,
        repeat: i32,
        stimulus: Stimulus,
        profile: &[u8; 32],
        params: &Value,
        rules: &[Ipv4Addr],
    ) -> Result<Self> {
        let tx = client.transaction().await?;
        let condition = Uuid::new_v4();
        let config = Uuid::new_v4();
        let mode = cell.mode;
        let name = cell.experiment.name();
        let variant = cell.variant;
        let profile = protocol::hex(profile);
        let pps = stimulus.rate();
        let window = pps.map(|_| cell.timing.window.as_nanos().to_string());
        let drain = cell.timing.observation.as_nanos().to_string();
        let instrumentation = if variant == "trace" {
            "trace"
        } else {
            "counter_only"
        };
        tx.execute(
            "INSERT INTO release.bench_condition(condition_id,experiment,protocol_version,parameters,planned_modes)
             VALUES($1,$2,'1',$3,$4)",
            &[&condition, &name, params, &vec![mode]],
        ).await?;
        let fixture = match cell.experiment {
            Workload::Firewall => {
                let bytes = serde_json::to_vec(rules)?;
                let fixture = protocol::create_artifact(
                    &tx,
                    "firewall_fixture",
                    &bytes,
                    "application/json",
                    None,
                )
                .await?;
                let lookup = stimulus.case();
                let legal = match stimulus {
                    Stimulus::Miss => 10_i32,
                    Stimulus::Rate(_) => 1,
                    _ => 0,
                };
                tx.execute(&format!("INSERT INTO release.{mode}_firewall_config
                    (config_id,condition_id,instrumentation,profile_hash,parameters,target_pps,send_window_ns,
                     drain_ns,payload_bytes,blacklist_capacity,blacklist_entries,legal_numerator,legal_denominator,lookup_case,map_fixture_artifact_id)
                     VALUES($1,$2,$3,$4,$5,$6,$7::text::release.bench_u64,$8::text::release.bench_u64,42,256,$9,$10,10,$11,$12)"),
                    &[&config,&condition,&instrumentation,&profile,params,&pps,&window,&drain,
                      &(rules.len() as i32),&legal,&lookup,&fixture]).await?;
                Some(fixture)
            }
            Workload::Drop => {
                tx.execute(&format!("INSERT INTO release.{mode}_just_drop_config
                    (config_id,condition_id,instrumentation,profile_hash,parameters,target_pps,send_window_ns,
                     drain_ns,payload_bytes,workload_version,neighbor_policy,saturation_grid)
                     VALUES($1,$2,$3,$4,$5,$6,$7::text::release.bench_u64,$8::text::release.bench_u64,40,
                     'drop-minimal-v1','ARP announce and UDP ACK before enable',false)"),
                    &[&config,&condition,&instrumentation,&profile,params,&pps,&window,&drain]).await?;
                None
            }
            Workload::Tx => {
                tx.execute(&format!("INSERT INTO release.{mode}_just_tx_config
                    (config_id,condition_id,instrumentation,profile_hash,parameters,target_pps,send_window_ns,
                     drain_ns,payload_bytes,probe_version,capture_interface)
                     VALUES($1,$2,$3,$4,$5,$6,$7::text::release.bench_u64,$8::text::release.bench_u64,40,'JTX1',$9)"),
                    &[&config,&condition,&instrumentation,&profile,params,&pps,&window,&drain,&link.iface]).await?;
                None
            }
        };
        let release = (!cell.hal()).then_some("07a4ea9a81a7fedf7bef2d6ced8d7fcfcccfd113");
        tx.execute("INSERT INTO release.bench_run
            (run_id,condition_id,config_id,mode,experiment,variant,phase,repeat_no,seed,plan_order,status,run_uuid,libbpf_commit,xpost_commit)
             VALUES($1,$2,$3,$4,$5,$6,'formal',$7,$8::text::release.bench_u64,$9,'running',$1,$10,$11)",
            &[&run,&condition,&config,&mode,&name,&variant,&repeat,&seed(run).to_string(),&order,&release,&super::super::medium::revision()]).await?;
        if let Some(fixture) = fixture {
            protocol::link_artifact(&tx, run, "firewall_fixture", fixture).await?;
        }
        tx.commit().await?;
        Ok(Self { run, config })
    }

    pub(super) async fn finish(
        &self,
        client: &mut Client,
        cell: &Cell<'_>,
        link: &Link,
        out: &Observation,
        result: &Analysis,
        status: &str,
    ) -> Result<()> {
        let tx = client.transaction().await?;
        let run = self.run;
        let issues = json!(out.issues);
        let boot = number(out.boot);
        let begin = out.elapsed.map(|_| "0");
        let end = number(out.elapsed);
        let drain = number(out.drain_mono);
        let updated = tx
            .execute(
                "UPDATE release.bench_run SET status=$2,issues=$3,boot_id=$4::text::release.bench_u64,
                    host_send_begin_ns=$5::text::release.bench_u64,host_send_end_ns=$6::text::release.bench_u64,
                    send_elapsed_ns=$6::text::release.bench_u64,host_drain_end_ns=$7::text::release.bench_u64
                    WHERE run_id=$1 AND status='running'",
                &[&run, &status, &issues, &boot, &begin, &end, &drain],
            )
            .await?;
        ensure!(updated == 1, "network attempt {run} has no running record");
        self.send_attempts(&tx, out).await?;
        let captures = self.capture(&tx, link, out, result).await?;

        let meta = observation_json(out, &result.summary(out));
        let meta_bytes = serde_json::to_vec(&meta)?;
        protocol::artifact(&tx, run, "network_observation", &meta_bytes).await?;
        let raw_perf = match out.perf.as_deref() {
            Some(bytes) => {
                let role = if cell.variant == "trace" {
                    "network_trace"
                } else {
                    "network_metrics"
                };
                Some(protocol::artifact(&tx, run, role, bytes).await?)
            }
            None => None,
        };
        let id = Uuid::new_v4();
        let digest = protocol::digest(&meta_bytes);
        tx.execute("INSERT INTO release.bench_analysis(analysis_id,run_id,algorithm_version,source_digest,qualified,issues)
            VALUES($1,$2,$3,$4,$5,$6)",
            &[&id,&run,&cell.timing.analysis_version(cell.experiment),&digest,&result.qualified,&issues]).await?;
        if cell.variant == "metrics" {
            let (baseline, final_id) = if result.metrics_valid {
                let pair =
                    metrics::decode(out.perf.as_deref().context("missing validated Metrics")?)?;
                let (baseline, final_id) = pair.write(&tx, cell.mode).await?;
                (Some(baseline), Some(final_id))
            } else {
                (None, None)
            };
            self.metrics(&tx, cell, id, baseline, final_id, result, out, &issues)
                .await?;
        } else if let (Some(trace), Some(raw)) = (&result.trace, raw_perf) {
            self.trace(&tx, cell, trace, raw, result, &issues).await?;
        }
        match cell.experiment {
            Workload::Firewall => self.firewall_tokens(&tx, cell.mode, out).await?,
            Workload::Tx => self.probes(&tx, cell.mode, id, captures, result).await?,
            Workload::Drop => {}
        }
        tx.commit().await?;
        Ok(())
    }

    async fn send_attempts(&self, tx: &Transaction<'_>, out: &Observation) -> Result<()> {
        if out.sends.is_empty() {
            return Ok(());
        }
        let insert = tx.prepare("INSERT INTO release.bench_send_attempt
            (run_id,seq,mono_ns,wall_ns,submitted,traffic_class,source_ip,payload)
            SELECT $1,r.seq,r.mono::release.bench_u64,r.wall::release.bench_u64,r.submitted,r.class,r.source::inet,decode(r.payload,'hex')
            FROM jsonb_to_recordset($2) AS r(seq bigint,mono text,wall text,submitted bool,class text,source text,payload text)").await?;
        for chunk in out.sends.chunks(EVIDENCE_ROWS_PER_BIND) {
            let records = Value::Array(chunk.iter().map(|send| json!({
                "seq": send.seq, "mono": send.mono.to_string(), "wall": send.wall.to_string(),
                "submitted": send.submitted, "class": send.class, "source": send.source,
                "payload": protocol::hex(&send.payload),
            })).collect());
            tx.execute(&insert, &[&self.run, &records]).await?;
        }
        Ok(())
    }

    async fn capture(
        &self,
        tx: &Transaction<'_>,
        link: &Link,
        out: &Observation,
        result: &Analysis,
    ) -> Result<[Option<Uuid>; 2]> {
        let Some(evidence) = &out.capture else {
            return Ok([None; 2]);
        };
        let id = protocol::artifact(tx, self.run, "network_pcap", &evidence.bytes).await?;
        protocol::artifact(
            tx,
            self.run,
            "network_tcpdump_stderr",
            evidence.stderr.as_bytes(),
        )
        .await?;
        let Some(hz) = result.capture_hz else {
            return Ok([None; 2]);
        };
        let run = self.run;
        tx.execute("INSERT INTO release.bench_clock(run_id,clock_id,domain,tick_hz,tick_bits,precision_description)
                    VALUES($1,'pcap','host_pcap',$2,64,'Host realtime pcap timestamps in both directions')",
            &[&run,&i64::from(hz)]).await?;
        let insert = tx.prepare("INSERT INTO release.bench_packet_fact(capture_id,record_index,file_offset,captured_len,original_len,
                    ts_sec,ts_subsec,timestamp_hz,observed_uuid,seq,parse_status,identity_ok,payload_ok)
            SELECT $1,p.idx,p.off,p.cap,p.orig,p.sec,p.subsec,$2,$3,p.seq,p.status,p.identity,p.body
            FROM jsonb_to_recordset($4) AS p(idx bigint,off bigint,cap int,orig int,sec bigint,subsec int,
                seq bigint,status text,identity bool,body bool)").await?;
        let mut ids = [None; 2];
        for (index, direction) in ["tx", "rx"].into_iter().enumerate() {
            let capture = Uuid::new_v4();
            ids[index] = Some(capture);
            tx.execute("INSERT INTO release.bench_capture(capture_id,run_id,direction,interface,clock_id,precision,
                        listening,exited_by_controller,exit_code,kernel_dropped,complete,stderr_text,artifact_id)
                        VALUES($1,$2,$3,$4,'pcap',$5,$6,$7,$8,$9,$10,$11,$12)",
                &[&capture,&run,&direction,&link.iface,&hz,&evidence.listening,&evidence.stopped,
                  &evidence.exit,&evidence.dropped(),&evidence.complete(),&evidence.stderr,&id]).await?;
            for chunk in result.packets.chunks(EVIDENCE_ROWS_PER_BIND) {
                let packets: Vec<_> = chunk.iter().filter(|p| p.tx == (index == 0)).map(|p| json!({
                    "idx":p.index,"off":p.offset,"cap":p.captured,"orig":p.original,
                    "sec":p.sec,"subsec":p.subsec,"seq":p.seq,
                    "status":if p.valid() { "ok" } else { "damaged" },"identity":p.identity,"body":p.payload,
                })).collect();
                if !packets.is_empty() {
                    tx.execute(&insert, &[&capture, &hz, &run, &Value::Array(packets)])
                        .await?;
                }
            }
        }
        Ok(ids)
    }

    async fn metrics(
        &self,
        tx: &Transaction<'_>,
        cell: &Cell<'_>,
        analysis: Uuid,
        baseline: Option<Uuid>,
        final_id: Option<Uuid>,
        result: &Analysis,
        out: &Observation,
        issues: &Value,
    ) -> Result<()> {
        let mode = cell.mode;
        let table = cell.experiment.name();
        let common = format!(
            "INSERT INTO release.{mode}_{table}_metrics_data
            (run_id,config_id,baseline_snapshot_id,final_snapshot_id,analysis_id,qualified,issues,"
        );
        let fields: &[&(dyn ToSql + Sync)] = &[
            &self.run,
            &self.config,
            &baseline,
            &final_id,
            &analysis,
            &result.qualified,
            issues,
        ];
        let sent = result.sent.to_string();
        match cell.experiment {
            Workload::Firewall => {
                let legal = result.legal.to_string();
                let illegal = result.illegal.to_string();
                let matched = number(result.matched);
                let hits = number(result.hits);
                let refused = number(out.refused);
                let values: &[&(dyn ToSql + Sync)] = &[&legal, &illegal, &matched, &hits, &refused];
                tx.execute(&format!("{common}sent_legal,sent_illegal,matched_legal,blocked_hit_delta,token_refused)
                  VALUES($1,$2,$3,$4,$5,$6,$7,$8::text::release.bench_u64,$9::text::release.bench_u64,$10::text::release.bench_u64,
                         $11::text::release.bench_u64,$12::text::release.bench_u64)"),
                    &[fields, values].concat()).await?;
            }
            Workload::Drop => {
                let tagged = &result.drop_counts;
                let ingress = number(tagged.ingress);
                let policy = number(tagged.policy_enter());
                let normal = number(tagged.normal);
                let fault = number(tagged.fault);
                let ticks = number(result.device_ticks);
                let values: &[&(dyn ToSql + Sync)] = &[
                    &sent,
                    &ingress,
                    &policy,
                    &normal,
                    &fault,
                    &ticks,
                    &result.sender_limited,
                ];
                tx.execute(&format!("{common}sent_success,ingress_seen,policy_enter,normal_drop,runtime_fault,device_window_ticks,sender_limited,plateau_observed)
                    VALUES($1,$2,$3,$4,$5,$6,$7,$8::text::release.bench_u64,$9::text::release.bench_u64,$10::text::release.bench_u64,
                        $11::text::release.bench_u64,$12::text::release.bench_u64,$13::text::release.bench_u64,$14,false)"),
                    &[fields, values].concat()).await?;
            }
            Workload::Tx => {
                let returned = number(result.tx.as_ref().map(|tx| tx.returned));
                let duplicate = number(result.tx.as_ref().map(|tx| tx.duplicate));
                let late = number(result.tx.as_ref().map(|tx| tx.late));
                let rtt = number(result.tx.as_ref().map(|tx| tx.valid_rtt));
                let values: &[&(dyn ToSql + Sync)] = &[&sent, &returned, &duplicate, &late, &rtt];
                tx.execute(&format!("{common}sent_success,returned,duplicate_rx,late_rx,valid_rtt_count)
                    VALUES($1,$2,$3,$4,$5,$6,$7,$8::text::release.bench_u64,$9::text::release.bench_u64,$10::text::release.bench_u64,
                           $11::text::release.bench_u64,$12::text::release.bench_u64)"),
                    &[fields, values].concat()).await?;
            }
        }
        Ok(())
    }

    async fn trace(
        &self,
        tx: &Transaction<'_>,
        cell: &Cell<'_>,
        trace: &super::DutTrace,
        raw: Uuid,
        result: &Analysis,
        issues: &Value,
    ) -> Result<()> {
        let run = self.run;
        let mode = cell.mode;
        let table = cell.experiment.name();
        let boot = trace.boot.to_string();
        let dictionary = super::TRACE_DICTIONARY;
        tx.execute("INSERT INTO release.bench_clock(run_id,clock_id,domain,tick_hz,tick_bits,boot_id,precision_description)
                    VALUES($1,'dut','dut',1000000,64,$2::text::release.bench_u64,'ESP HAL microseconds since boot')",
            &[&run,&boot]).await?;
        tx.execute(&format!("INSERT INTO release.{mode}_{table}_trace_data(run_id,config_id,epoch,dictionary_version,
                    boot_id,clock_id,capacity,written,dropped,flags,complete,result_checked,result_ok,raw_artifact_id,qualified,issues)
                    VALUES($1,$2,$3,{dictionary},$4::text::release.bench_u64,'dut',$5,$6,$7,$8,$9,$10,$11,$12,$13,$14)"),
            &[&run,&self.config,&i64::from(trace.epoch),&boot,&trace.capacity,&(trace.events.len() as i32),
              &trace.dropped,&trace.flags,&trace.complete(),&result.data_ok.is_some(),&result.data_ok,
              &raw,&result.qualified,issues]).await?;
        let event_ids: Vec<i32> = (1..=super::EVENT_NAMES.len() as i32).collect();
        tx.execute("INSERT INTO release.bench_event_definition(experiment,dictionary_version,event_id,name,semantics,boundary)
                    SELECT $1,$4,e.id,e.name,e.name,e.name FROM unnest($2::int[],$3::text[]) AS e(id,name)
                    ON CONFLICT DO NOTHING",
            &[&table,&event_ids,&super::EVENT_NAMES.as_slice(),&dictionary]).await?;
        tx.execute(&format!("INSERT INTO release.{mode}_{table}_event(run_id,source,event_sequence,dictionary_version,event_id,
                    clock_id,timestamp_raw,kind,context_id,probe_seq,span_id,parent_span_id,arg0,arg1)
                    SELECT $1,'dut',e.sequence,{dictionary},e.id,'dut',e.tick::release.bench_u64,'instant',e.context,e.probe_seq,0,0,e.arg0,e.arg1
                    FROM jsonb_to_recordset($2) AS e(sequence bigint,id int,tick text,context int,probe_seq bigint,arg0 bigint,arg1 bigint)"),
            &[&run,&json!(trace.events)]).await?;
        Ok(())
    }

    async fn firewall_tokens(
        &self,
        tx: &Transaction<'_>,
        mode: &str,
        out: &Observation,
    ) -> Result<()> {
        let Some(tokens) = &out.tokens else {
            return Ok(());
        };
        let observed: std::collections::BTreeSet<_> = tokens.iter().copied().collect();
        let insert = tx.prepare(&format!("INSERT INTO release.{mode}_firewall_token(run_id,seq,sent,observed,received_bytes)
                    SELECT $1,t.seq,t.sent,t.observed,decode(t.received,'hex')
                    FROM jsonb_to_recordset($2) AS t(seq bigint,sent bool,observed bool,received text)")).await?;
        for chunk in out.sends.chunks(EVIDENCE_ROWS_PER_BIND) {
            let records = Value::Array(
                chunk
                    .iter()
                    .map(|send| {
                        let value = sent_token(send);
                        let got = observed.contains(&value);
                        json!({"seq":send.seq,"sent":send.submitted,"observed":got,
                "received":got.then(|| protocol::hex(&value.to_le_bytes()))})
                    })
                    .collect(),
            );
            tx.execute(&insert, &[&self.run, &records]).await?;
        }
        Ok(())
    }

    async fn probes(
        &self,
        tx: &Transaction<'_>,
        mode: &str,
        analysis: Uuid,
        [tx_id, rx_id]: [Option<Uuid>; 2],
        result: &Analysis,
    ) -> Result<()> {
        let insert = tx.prepare(&format!("INSERT INTO release.{mode}_probe_result(analysis_id,run_id,seq,returned,duplicate_rx,late_rx,
                    tx_capture_id,tx_record_index,rx_capture_id,rx_record_index,rtt_ns,qualified,issues)
                    SELECT $1,$2,p.seq,p.returned,p.duplicate,p.late,
                           CASE WHEN p.tx_index IS NOT NULL THEN $3::uuid END,p.tx_index,
                           CASE WHEN p.rx_index IS NOT NULL THEN $4::uuid END,p.rx_index,p.rtt::release.bench_u64,p.qualified,
                           CASE WHEN p.qualified THEN '[]'::jsonb ELSE jsonb_build_array('capture or clock evidence incomplete') END
                    FROM jsonb_to_recordset($5) AS p(seq bigint,returned bool,duplicate bigint,late bigint,
                         tx_index bigint,rx_index bigint,rtt text,qualified bool)")).await?;
        for chunk in result.probes.chunks(EVIDENCE_ROWS_PER_BIND) {
            tx.execute(
                &insert,
                &[&analysis, &self.run, &tx_id, &rx_id, &json!(chunk)],
            )
            .await?;
        }
        Ok(())
    }
}

fn terminal_status(status: &str) -> bool {
    matches!(
        status,
        "complete" | "failed" | "unsupported" | "inconclusive" | "blocked"
    )
}

fn number(value: Option<u64>) -> Option<String> {
    value.map(|x| x.to_string())
}

fn observation_json(out: &Observation, summary: &Value) -> Value {
    json!({
        "issues": out.issues, "result": summary, "boot_id": out.boot, "epoch": out.epoch,
        "arm_tick": out.arm, "freeze_tick":out.freeze.map(|freeze| freeze.tick),
        "freeze_flags":out.freeze.map(|freeze| freeze.flags), "drain_wall_ns": out.drain_wall,
        "before_blacklist":out.maps_before, "after_blacklist":out.maps_after,
        "firewall_tokens":out.tokens, "token_refused": out.refused,
        "rxcnt_before":out.rx_before, "rxcnt_after":out.rx_after,
        "send_errors":out.sends.iter().filter_map(|send| send.error.as_ref().map(|e| (send.seq,e))).collect::<Vec<_>>(),
    })
}

pub(super) fn recover(run: Uuid, out: &Observation, summary: &Value) -> Result<PathBuf> {
    let dir = PathBuf::from(format!("medium-network-{run}"));
    fs::create_dir(&dir).context("create network evidence directory")?;
    fs::write(
        dir.join("observation.json"),
        serde_json::to_vec(&observation_json(out, summary))?,
    )?;
    let mut sends = BufWriter::new(fs::File::create(dir.join("sends.json"))?);
    serde_json::to_writer(&mut sends, &out.sends)?;
    sends.flush()?;
    if let Some(bytes) = &out.perf {
        fs::write(dir.join("perf.bin"), bytes)?;
    }
    if let Some(capture) = &out.capture {
        fs::write(dir.join("network.pcap"), &capture.bytes)?;
        fs::write(dir.join("tcpdump.stderr"), &capture.stderr)?;
    }
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{db::Database, exam::medium_network::NetworkPlan};

    #[tokio::test]
    #[ignore = "requires initialized release in the disposable database"]
    async fn recovers_an_interrupted_network_write_and_keeps_its_artifacts() -> Result<()> {
        let settings = Database::load_fixture()?;
        let admin = settings.connect().await?;
        let name: String = admin
            .query_one("SELECT current_database()::text", &[])
            .await?
            .get(0);
        ensure!(
            name == "reconcile_fixture",
            "use the disposable reconcile_fixture database"
        );
        let database = settings.resume().await?;
        let mut writer = database.connect().await?;
        let cell = Cell {
            experiment: Workload::Firewall,
            mode: "hal",
            variant: "metrics",
            firmware: "fixture",
            firmware_sha256: "fixture",
            timing: NetworkPlan::MEDIUM,
        };
        let link = Link {
            iface: "fixture".into(),
            capture_rx_iface: None,
            src: Ipv4Addr::new(192, 0, 2, 1),
            dut: Ipv4Addr::new(192, 0, 2, 2),
            hit: Ipv4Addr::new(192, 0, 2, 3),
            src_mac: [1; 6],
            dut_mac: [2; 6],
            port: 8080,
            sport: 8081,
        };
        let run = Uuid::new_v4();
        let attempt = Attempt::start(
            &mut writer,
            &cell,
            &link,
            run,
            0,
            1,
            Stimulus::Rate(400),
            &[7; 32],
            &json!({}),
            &[link.hit],
        )
        .await?;
        let pid: i32 = writer
            .query_one("SELECT pg_backend_pid()", &[])
            .await?
            .get(0);
        admin
            .query_one("SELECT pg_terminate_backend($1, 1000)", &[&pid])
            .await?;
        let out = Observation::default();
        let analysis = Analysis::default();
        assert_eq!(
            attempt
                .finish_recovering(
                    &mut writer,
                    &database,
                    &cell,
                    &link,
                    &out,
                    &analysis,
                    "inconclusive"
                )
                .await?,
            ("inconclusive".into(), true)
        );
        let row = writer.query_one("SELECT a.original_bytes,a.media_type,a.sha256 FROM release.bench_run_artifact l
            JOIN release.bench_artifact a USING (artifact_id) WHERE l.run_id=$1 AND l.role='firewall_fixture'", &[&run]).await?;
        let bytes = serde_json::to_vec(&[link.hit])?;
        assert_eq!(row.get::<_, Vec<u8>>(0), bytes);
        assert_eq!(row.get::<_, String>(1), "application/json");
        assert_eq!(row.get::<_, String>(2), protocol::digest(&bytes));
        let row = writer
            .query_one(
                "SELECT status,qualified FROM release.bench_run
            JOIN release.hal_firewall_metrics_data USING (run_id) WHERE run_id=$1",
                &[&run],
            )
            .await?;
        assert_eq!(row.get::<_, String>(0), "inconclusive");
        assert!(!row.get::<_, bool>(1));
        // This retained row has NULL baseline/final references. It remains
        // valid when the referenced table is rebuilt and both FKs restored.
        let writer_pid: i32 = writer
            .query_one("SELECT pg_backend_pid()", &[])
            .await?
            .get(0);
        let guard_pid: i32 = database
            .client()
            .query_one("SELECT pg_backend_pid()", &[])
            .await?
            .get(0);
        for pid in [writer_pid, guard_pid] {
            admin
                .query_one("SELECT pg_terminate_backend($1, 1000)", &[&pid])
                .await?;
        }
        drop(writer);
        drop(database);
        admin
            .batch_execute(
                "ALTER TABLE release.hal_metric_snapshot ALTER COLUMN profile_hash DROP NOT NULL",
            )
            .await?;
        let prepared = settings.prepare().await?;
        let retained = prepared.client().query_one(
            "SELECT baseline_snapshot_id, final_snapshot_id FROM release.hal_firewall_metrics_data WHERE run_id=$1", &[&run],
        ).await?;
        assert_eq!(retained.get::<_, Option<Uuid>>(0), None);
        assert_eq!(retained.get::<_, Option<Uuid>>(1), None);
        let validated: i64 = prepared.client().query_one(
            "SELECT count(*) FROM pg_constraint WHERE conrelid='release.hal_firewall_metrics_data'::regclass
             AND confrelid='release.hal_metric_snapshot'::regclass AND convalidated",
            &[],
        ).await?.get(0);
        assert_eq!(validated, 2);

        Ok(())
    }
}
