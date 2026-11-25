//! Persist one case attempt and its evidence in the protocol database.

use anyhow::{Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
#[cfg(test)]
use tokio_postgres::NoTls;
use tokio_postgres::{Client, Transaction};
use uuid::Uuid;

use crate::{config::PostgresConfig, db::protocol};

use super::case_capture::{Evidence, POST, PPS, PRE, Packet};
use super::case_trace::Trace;

const RELEASE_COMMIT: &str = "07a4ea9a81a7fedf7bef2d6ced8d7fcfcccfd113";

pub(super) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub(super) async fn connect(cfg: &PostgresConfig) -> Result<Client> {
    let mut client = protocol::connect(cfg).await?;
    let tables = [
        "bench.run",
        "bench.condition",
        "bench.artifact",
        "bench.run_artifact",
        "bench.clock",
        "bench.capture",
        "bench.packet_fact",
        "bench.analysis",
        "bench.event_definition",
        "aot.case_config",
        "aot.case_switch",
        "aot.case_trace_data",
        "aot.case_event",
    ]
    .map(str::to_owned);
    protocol::check(&mut client, &tables).await?;
    Ok(client)
}

pub(super) struct Attempt {
    pub run: Uuid,
    config: Uuid,
}

impl Attempt {
    pub async fn start(
        client: &mut Client,
        run: Uuid,
        phase: &str,
        profile: &str,
        medium_id: Option<Uuid>,
        identity: Option<&Value>,
        retry_of: Option<Uuid>,
        order: i32,
    ) -> Result<Self> {
        let condition = Uuid::new_v4();
        let config = Uuid::new_v4();
        let tx = client.transaction().await?;
        let parameters = json!({ "port": 8080, "pps": PPS, "slots": [0,1], "versions": [2,1], "switches": 1, "medium_id": medium_id, "retry_of": retry_of,
            "stage": identity.and_then(|id| id["stage"].as_str()), "medium_identity": identity });
        tx.execute("INSERT INTO bench.condition(condition_id,experiment,protocol_version,parameters,planned_modes) VALUES($1,'case','1',$2,ARRAY['aot'])", &[&condition,&parameters]).await?;
        let pre = PRE.as_nanos().to_string();
        let post = POST.as_nanos().to_string();
        tx.execute("INSERT INTO aot.case_config(config_id,condition_id,instrumentation,profile_hash,target_pps,switch_count,switch_kind,quiesce_policy,pre_window_ns,post_window_ns,payload_bytes,source_mode,target_mode) VALUES($1,$2,'trace',$3,$4,1,'hot_publish','drop',$5::text::bench.u64,$6::text::bench.u64,40,'aot','aot')", &[&config,&condition,&profile,&PPS,&pre,&post]).await?;
        tx.execute("INSERT INTO bench.run(run_id,condition_id,config_id,mode,experiment,variant,phase,repeat_no,seed,plan_order,status,run_uuid,libbpf_commit,xpost_commit) VALUES($1,$2,$3,'aot','case','trace',$4,1,0,$6,'running',$1,$5,$7)", &[&run,&condition,&config,&phase,&RELEASE_COMMIT,&order,&super::medium::revision()]).await?;
        tx.commit().await?;
        Ok(Self { run, config })
    }

    pub async fn finish(
        &self,
        client: &mut Client,
        result: &Observation,
        iface: &str,
    ) -> Result<()> {
        let tx = client.transaction().await?;
        let issues = json!(result.issues);
        let boot = result.boot.map(|n| n.to_string());
        let updated = tx.execute("UPDATE bench.run SET status=$2, issues=$3, boot_id=$4::text::bench.u64 WHERE run_id=$1 AND status='running'", &[&self.run,&result.status(),&issues,&boot]).await?;
        ensure!(
            updated == 1,
            "case attempt {} has no running record to complete",
            self.run
        );
        if !result.preparation.is_empty() {
            self.artifact(
                &tx,
                "preparation",
                "application/json",
                &serde_json::to_vec(&result.preparation)?,
                &json!({}),
            )
            .await?;
        }
        if let Some(identity) = &result.flash {
            self.artifact(
                &tx,
                "loaded_images",
                "application/json",
                &serde_json::to_vec(identity)?,
                &json!({}),
            )
            .await?;
        }
        let capture = match &result.capture {
            Some(data) => Some(self.capture(&tx, iface, data).await?),
            None => None,
        };
        if let Some(receipt) = &result.freeze_receipt {
            self.artifact(
                &tx,
                "case_freeze_receipt",
                "application/json",
                &serde_json::to_vec(receipt)?,
                &json!({}),
            )
            .await?;
        }
        if let Some(bytes) = &result.trace_bytes {
            let id = self
                .artifact(
                    &tx,
                    "dut_trace",
                    "application/octet-stream",
                    bytes,
                    &json!({ "wire_version": 1 }),
                )
                .await?;
            if let Some(trace) = &result.trace {
                self.trace(&tx, trace, id, result.qualified(), &issues)
                    .await?;
            }
        }
        if let Some(switch) = &result.switch {
            self.switch(&tx, switch, result, capture, &issues).await?;
        }
        let evidence = serde_json::to_vec(&json!({
            "pcap_sha256": result.capture.as_ref().map(|capture| hex(&Sha256::digest(&capture.pcap))),
            "trace_sha256": result.trace_bytes.as_ref().map(|bytes| hex(&Sha256::digest(bytes))),
            "freeze_receipt": result.freeze_receipt, "boot_id": result.boot,
            "switch_reply": result.switch.as_ref().and_then(|switch| switch.reply.as_ref()),
            "published_state": result.switch.as_ref().and_then(|switch| switch.state_after.as_ref()),
        }))?;
        self.artifact(
            &tx,
            "case_observation",
            "application/json",
            &evidence,
            &json!({}),
        )
        .await?;
        tx.execute("INSERT INTO bench.analysis(analysis_id,run_id,algorithm_version,source_digest,qualified,issues)
            VALUES($1,$2,'case-trace-v1',$3,$4,$5)",
            &[&Uuid::new_v4(), &self.run, &hex(&Sha256::digest(&evidence)), &result.qualified(), &issues]).await?;
        tx.commit().await?;
        Ok(())
    }

    async fn artifact(
        &self,
        tx: &Transaction<'_>,
        role: &str,
        media: &str,
        bytes: &[u8],
        meta: &Value,
    ) -> Result<Uuid> {
        let id = Uuid::new_v4();
        let digest = hex(&Sha256::digest(bytes));
        tx.execute("INSERT INTO bench.artifact(artifact_id,role,sha256,size_bytes,original_bytes,media_type,metadata) VALUES($1,$2,$3,$4,$5,$6,$7)", &[&id,&role,&digest,&(bytes.len() as i64),&bytes,&media,&meta]).await?;
        tx.execute(
            "INSERT INTO bench.run_artifact(run_id,role,artifact_id) VALUES($1,$2,$3)",
            &[&self.run, &role, &id],
        )
        .await?;
        Ok(id)
    }

    async fn capture(&self, tx: &Transaction<'_>, iface: &str, data: &Evidence) -> Result<Uuid> {
        let id = Uuid::new_v4();
        let drop = data.kernel_dropped();
        let complete = drop == Some(0);
        let artifact = self
            .artifact(
                tx,
                "tcpdump",
                "application/vnd.tcpdump.pcap",
                &data.pcap,
                &json!({ "interface": iface, "includes_tx": true }),
            )
            .await?;
        tx.execute("INSERT INTO bench.clock(run_id,clock_id,domain,tick_hz,tick_bits,precision_description) VALUES($1,'pcap','host_pcap',$2,64,'pcap timestamp precision')", &[&self.run,&i64::from(data.hz)]).await?;
        tx.execute("INSERT INTO bench.capture(capture_id,run_id,direction,interface,clock_id,precision,listening,exited_by_controller,exit_code,kernel_dropped,complete,stderr_text,artifact_id) VALUES($1,$2,'rx',$3,'pcap',$4,true,true,0,$5,$6,$7,$8)", &[&id,&self.run,&iface,&data.hz,&drop,&complete,&data.stderr,&artifact]).await?;
        let insert = tx.prepare("INSERT INTO bench.packet_fact(capture_id,record_index,file_offset,captured_len,original_len,ts_sec,ts_subsec,timestamp_hz,observed_uuid,seq,parse_status,identity_ok,payload_ok,version_marker) VALUES($1,$2,$3,82,82,$4,$5,$6,$7,$8,'ok',true,true,$9)").await?;
        for p in &data.packets {
            tx.execute(
                &insert,
                &[
                    &id,
                    &p.index,
                    &p.offset,
                    &p.sec,
                    &p.subsec,
                    &data.hz,
                    &self.run,
                    &p.seq,
                    &i32::from(p.version),
                ],
            )
            .await?;
        }
        Ok(id)
    }

    async fn trace(
        &self,
        tx: &Transaction<'_>,
        trace: &Trace,
        id: Uuid,
        qualified: bool,
        issues: &Value,
    ) -> Result<()> {
        let boot = trace.boot_id.to_string();
        let complete = trace.dropped == 0 && trace.flags == 0;
        tx.execute("INSERT INTO bench.clock(run_id,clock_id,domain,tick_hz,tick_bits,boot_id,precision_description) VALUES($1,'dut','dut',1000000,64,$2::text::bench.u64,'ESP HAL microseconds since boot')", &[&self.run,&boot]).await?;
        tx.execute("INSERT INTO aot.case_trace_data(run_id,config_id,epoch,dictionary_version,boot_id,clock_id,capacity,written,dropped,flags,complete,result_checked,result_ok,raw_artifact_id,qualified,issues) VALUES($1,$2,$3,1,$4::text::bench.u64,'dut',$5,$6,$7,$8,$9,true,$10,$11,$10,$12)", &[&self.run,&self.config,&i64::from(trace.epoch),&boot,&trace.capacity,&(trace.events.len() as i32),&trace.dropped,&trace.flags,&complete,&qualified,&id,&issues]).await?;
        for (event_id, name, boundary) in [
            (1, "control", "serial switch handler"),
            (2, "quiesce", "pause to inflight drain"),
            (3, "rebind", "map binding update"),
            (4, "publish", "entry store to admission"),
        ] {
            tx.execute("INSERT INTO bench.event_definition(experiment,dictionary_version,event_id,name,semantics,boundary) VALUES('case',1,$1,$2,$3,$3) ON CONFLICT DO NOTHING", &[&event_id,&name,&boundary]).await?;
        }
        let insert = tx.prepare("INSERT INTO aot.case_event(run_id,source,event_sequence,dictionary_version,event_id,clock_id,timestamp_raw,kind,context_id,span_id,parent_span_id,arg0,arg1) VALUES($1,'dut',$2,1,$3,'dut',$4::text::bench.u64,$5,0,$6,$7,$8,$9)").await?;
        for event in &trace.events {
            let parent = i64::from(event.id != 1);
            tx.execute(
                &insert,
                &[
                    &self.run,
                    &event.sequence,
                    &event.id,
                    &event.tick.to_string(),
                    &event.kind,
                    &i64::from(event.id),
                    &parent,
                    &event.slot,
                    &event.generation,
                ],
            )
            .await?;
        }
        Ok(())
    }

    async fn switch(
        &self,
        tx: &Transaction<'_>,
        switch: &Switch,
        result: &Observation,
        capture: Option<Uuid>,
        issues: &Value,
    ) -> Result<()> {
        tx.execute("INSERT INTO bench.clock(run_id,clock_id,domain,tick_hz,tick_bits,precision_description) VALUES($1,'runner','host_monotonic',1000000000,64,'relative to capture start')", &[&self.run]).await?;
        let (last, first, late) = result.transitions();
        let pcap_hz = result.capture.as_ref().map(|data| data.hz).unwrap_or(1);
        let gap = first
            .zip(last)
            .and_then(|(b, a)| packet_ns(b, pcap_hz).checked_sub(packet_ns(a, pcap_hz)))
            .map(|ns| ns.to_string());
        let end_dut = result
            .trace
            .as_ref()
            .and_then(|t| t.events.last())
            .map(|event| event.tick.to_string());
        let begin_dut = result
            .trace
            .as_ref()
            .and_then(|t| t.events.first())
            .map(|event| event.tick.to_string());
        let stage = |id| {
            let trace = result.trace.as_ref()?;
            let mut events = trace.events.iter().filter(|event| event.id == id);
            let begin = events.next()?.tick;
            let end = events.next()?.tick;
            end.checked_sub(begin).map(|ticks| ticks.to_string())
        };
        let from = i64::from(switch.generation_a);
        let to = i64::from(switch.generation_b);
        tx.execute("INSERT INTO aot.case_switch(run_id,switch_index,switch_kind,source_version,target_version,source_slot,target_slot,source_generation,target_generation,host_clock_id,host_command_begin_ns,host_reply_ns,dut_clock_id,dut_begin_tick,dut_end_tick,quiesce_ticks,rebind_ticks,publish_ticks,last_old_capture_id,last_old_record_index,first_new_capture_id,first_new_record_index,reply_transition_gap_ns,old_after_new,command_ok,error_code,boot_unchanged,qualified,issues) VALUES($1,1,'hot_publish',2,1,0,1,$2,$3,'runner',$4::text::bench.u64,$5::text::bench.u64,$6,$7::text::bench.u64,$8::text::bench.u64,$9::text::bench.u64,$10::text::bench.u64,$11::text::bench.u64,$12,$13,$14,$15,$16::text::bench.u64,$17,$18,$19,$20,$21,$22)", &[&self.run,&from,&to,&switch.begin_ns.to_string(),&switch.end_ns.to_string(),&result.trace.as_ref().map(|_| "dut"),&begin_dut,&end_dut,&stage(2),&stage(3),&stage(4),&last.and(capture),&last.map(|p|p.index),&first.and(capture),&first.map(|p|p.index),&gap,&late,&switch.command_ok,&switch.error,&switch.boot_unchanged,&result.qualified(),&issues]).await?;
        self.artifact(
            tx,
            "switch_reply",
            "application/json",
            &serde_json::to_vec(
                &json!({ "reply": switch.reply, "state_after": switch.state_after }),
            )?,
            &json!({ "agent_rx_clock": "agent" }),
        )
        .await?;
        Ok(())
    }
}

#[derive(Default)]
pub(super) struct Observation {
    pub boot: Option<u64>,
    pub capture: Option<Evidence>,
    pub trace_bytes: Option<Vec<u8>>,
    pub freeze_receipt: Option<Value>,
    pub trace: Option<Trace>,
    pub switch: Option<Switch>,
    pub issues: Vec<String>,
    pub preparation: Vec<String>,
    pub flash: Option<Value>,
}

impl Observation {
    pub fn qualified(&self) -> bool {
        self.issues.is_empty()
    }

    pub fn status(&self) -> &'static str {
        if self.qualified() {
            "complete"
        } else if self.switch.is_some() {
            "inconclusive"
        } else {
            "failed"
        }
    }

    pub fn preserve(&self, run: Uuid) -> Result<std::path::PathBuf> {
        use std::fs;

        let root = std::path::Path::new("case-evidence");
        fs::create_dir_all(root)?;
        let path = root.join(run.to_string());
        fs::create_dir(&path)?;
        if let Some(capture) = &self.capture {
            fs::write(path.join("tcpdump.pcap"), &capture.pcap)?;
            fs::write(path.join("tcpdump.stderr"), &capture.stderr)?;
        }
        if let Some(trace) = &self.trace_bytes {
            fs::write(path.join("dut-trace.bin"), trace)?;
        }
        fs::write(
            path.join("attempt.json"),
            serde_json::to_vec_pretty(&json!({
                "run_id": run.to_string(), "boot_id": self.boot, "preparation": self.preparation,
                "loaded_images": self.flash, "switch": self.switch, "issues": self.issues,
                "freeze_receipt": self.freeze_receipt,
            }))?,
        )?;
        Ok(path)
    }

    pub fn transitions(&self) -> (Option<&Packet>, Option<&Packet>, i64) {
        let Some(data) = &self.capture else {
            return (None, None, 0);
        };
        let first = data.packets.iter().find(|p| p.version == 1);
        let last = data
            .packets
            .iter()
            .rfind(|p| p.version == 2 && first.is_some_and(|b| p.index < b.index));
        let late = data
            .packets
            .iter()
            .filter(|p| p.version == 2 && first.is_some_and(|b| p.index > b.index))
            .count() as i64;
        (last, first, late)
    }
}

fn packet_ns(packet: &Packet, hz: i32) -> u64 {
    packet.sec as u64 * 1_000_000_000
        + u64::from(packet.subsec as u32) * (1_000_000_000 / hz as u64)
}

#[derive(serde::Serialize)]
pub(super) struct Switch {
    pub generation_a: u32,
    pub generation_b: u32,
    pub begin_ns: u128,
    pub end_ns: u128,
    pub command_ok: bool,
    pub boot_unchanged: bool,
    pub reply: Option<String>,
    pub state_after: Option<String>,
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exam::case_trace::Event;

    #[tokio::test]
    #[ignore = "requires a disposable protocol database supplied as XPOST_CASE_TEST_PG"]
    async fn stores_a_case_switch_and_both_raw_artifacts_under_its_run_id() -> Result<()> {
        let dsn = std::env::var("XPOST_CASE_TEST_PG")?;
        let (mut client, connection) = tokio_postgres::connect(&dsn, NoTls).await?;
        tokio::spawn(connection);
        let run = Uuid::new_v4();
        let attempt = Attempt::start(
            &mut client,
            run,
            "qualification",
            &"a".repeat(64),
            None,
            None,
            None,
            0,
        )
        .await?;
        let packet = |index, version| Packet {
            index,
            offset: 24 + index * 98,
            sec: 123,
            subsec: index as i32 * 2000,
            seq: index + 1,
            version,
        };
        let event = |sequence, id, kind| Event {
            sequence,
            id,
            kind,
            tick: 10 + sequence as u64,
            slot: 1,
            generation: 8,
        };
        let result = Observation {
            boot: Some(123),
            capture: Some(Evidence {
                pcap: vec![1, 2, 3],
                stderr: "0 packets dropped by kernel\n".into(),
                hz: 1_000_000,
                packets: vec![packet(0, 2), packet(1, 1)],
            }),
            freeze_receipt: None,
            trace_bytes: Some(vec![9, 8, 7]),
            trace: Some(Trace {
                epoch: 2,
                boot_id: 123,
                capacity: 16,
                dropped: 0,
                flags: 0,
                events: vec![
                    event(0, 1, "begin"),
                    event(1, 2, "begin"),
                    event(2, 2, "end"),
                    event(3, 3, "begin"),
                    event(4, 3, "end"),
                    event(5, 4, "begin"),
                    event(6, 4, "end"),
                    event(7, 1, "end"),
                ],
            }),
            switch: Some(Switch {
                generation_a: 7,
                generation_b: 8,
                begin_ns: 10,
                end_ns: 100,
                command_ok: true,
                boot_unchanged: true,
                reply: Some("acknowledged".into()),
                state_after: None,
                error: None,
            }),
            issues: vec![],
            preparation: vec!["fixture".into()],
            flash: None,
        };
        attempt.finish(&mut client, &result, "fixture").await?;
        let duplicate = attempt
            .finish(&mut client, &result, "fixture")
            .await
            .unwrap_err();
        assert!(duplicate.to_string().contains("no running record"));
        for (role, bytes) in [("tcpdump", vec![1, 2, 3]), ("dut_trace", vec![9, 8, 7])] {
            let row = client.query_one("SELECT a.original_bytes FROM bench.run_artifact r JOIN bench.artifact a USING(artifact_id) WHERE r.run_id=$1 AND r.role=$2", &[&run,&role]).await?;
            assert_eq!(row.get::<_, Vec<u8>>(0), bytes);
        }
        let row = client.query_one("SELECT status, switch_index, qualified, reply_transition_gap_ns::text FROM bench.run JOIN aot.case_switch USING(run_id) WHERE run_id=$1", &[&run]).await?;
        assert_eq!(row.get::<_, String>(0), "complete");
        assert_eq!(row.get::<_, i32>(1), 1);
        assert!(row.get::<_, bool>(2));
        assert_eq!(row.get::<_, String>(3), "2000000");
        eprintln!("case writer fixture run_id={run}");
        Ok(())
    }
}
