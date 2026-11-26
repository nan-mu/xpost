//! Validate NMS1 boundaries and retain cumulative DUT counters in the protocol database.

use std::collections::BTreeMap;

use anyhow::{Context, Result, ensure};
use tokio_postgres::Transaction;
use uuid::Uuid;

use super::decode::{Kind, decode_payload};

const HEADER: usize = 84;

#[derive(Debug)]
pub struct Snapshot<'a> {
    raw: &'a [u8],
    pub run: Uuid,
    pub epoch: u32,
    pub boot: u64,
    pub profile: [u8; 32],
    pub tick: u64,
    pub hz: u32,
    pub bits: u8,
    pub counters: BTreeMap<String, u64>,
}

pub struct Pair<'a> {
    pub baseline: Snapshot<'a>,
    pub final_: Snapshot<'a>,
}

fn boundary(bytes: &[u8], phase: u8) -> Result<(Snapshot<'_>, &[u8])> {
    let header = bytes.get(..HEADER).context("short NMS1 header")?;
    ensure!(
        &header[..6] == b"NMS1\x01\0" && header[6] == phase,
        "invalid NMS1 boundary"
    );
    let le32 = |start| u32::from_le_bytes(header[start..start + 4].try_into().unwrap());
    let le64 = |start| u64::from_le_bytes(header[start..start + 8].try_into().unwrap());
    let (raw, rest) = bytes
        .split_at_checked(HEADER + le32(8) as usize)
        .context("short NMS1 report")?;
    let report = decode_payload(&raw[HEADER..])?;
    ensure!(
        report.entries > 0 && usize::from(report.entries) == report.metrics.len(),
        "NMS1 requires at least one counter and no events"
    );
    let mut counters = BTreeMap::new();
    for item in report.metrics {
        ensure!(
            item.kind == Kind::Counter,
            "NMS1 {} is not a counter",
            item.name
        );
        ensure!(
            counters.insert(item.name, item.count).is_none(),
            "duplicate NMS1 counter"
        );
    }
    let bits = header[7];
    let tick = le64(72);
    let hz = le32(80);
    ensure!(
        (1..=64).contains(&bits) && hz > 0 && (bits == 64 || tick >> bits == 0),
        "invalid NMS1 clock"
    );
    let snapshot = Snapshot {
        raw,
        run: Uuid::from_bytes(header[12..28].try_into()?),
        epoch: le32(28),
        boot: le64(32),
        profile: header[40..72].try_into()?,
        tick,
        hz,
        bits,
        counters,
    };
    ensure!(
        snapshot.epoch != 0 && snapshot.boot != 0,
        "invalid NMS1 identity"
    );
    Ok((snapshot, rest))
}

pub fn decode(bytes: &[u8]) -> Result<Pair<'_>> {
    let (baseline, remainder) = boundary(bytes, 1)?;
    let (final_, remainder) = boundary(remainder, 2)?;
    ensure!(remainder.is_empty(), "trailing NMS1 bytes");
    ensure!(
        baseline.run == final_.run
            && baseline.epoch == final_.epoch
            && baseline.boot == final_.boot
            && baseline.profile == final_.profile
            && baseline.hz == final_.hz
            && baseline.bits == final_.bits
            && baseline.tick <= final_.tick,
        "NMS1 boundaries have different identities or invalid tick order"
    );
    ensure!(
        baseline.counters.keys().eq(final_.counters.keys()),
        "NMS1 counter definitions changed"
    );
    for (name, value) in &baseline.counters {
        ensure!(
            final_.counters[name] >= *value,
            "NMS1 counter decreased: {name}"
        );
    }
    Ok(Pair { baseline, final_ })
}

impl Pair<'_> {
    pub fn require(
        &self,
        run: Uuid,
        epoch: u32,
        boot: u64,
        profile: &[u8; 32],
        arm: u64,
    ) -> Result<()> {
        let start = &self.baseline;
        ensure!(
            start.run == run
                && start.epoch == epoch
                && start.boot == boot
                && start.profile == *profile
                && start.tick == arm,
            "NMS1 disagrees with the prepared session"
        );
        Ok(())
    }

    pub fn delta(&self, name: &str) -> Option<u64> {
        Some(self.final_.counters.get(name)? - self.baseline.counters.get(name)?)
    }

    /// Save the clock, raw boundaries and exact counter samples in an existing transaction.
    pub async fn write(&self, tx: &Transaction<'_>, mode: &str) -> Result<(Uuid, Uuid)> {
        ensure!(
            matches!(mode, "hal" | "aot" | "jit" | "rbpf"),
            "unknown protocol mode"
        );
        let start = &self.baseline;
        tx.execute("INSERT INTO bench.clock(run_id,clock_id,domain,tick_hz,tick_bits,boot_id,precision_description) VALUES ($1,'dut','dut',$2,$3,$4::text::bench.u64,'Device perf ticks since boot')",
            &[&start.run,&i64::from(start.hz),&i32::from(start.bits),&start.boot.to_string()]).await?;
        let mut series = BTreeMap::new();
        for name in start.counters.keys() {
            let metric = Uuid::new_v4();
            tx.execute(&format!("INSERT INTO {mode}.metric_definition(metric_id,name,kind,unit,boundary,scope,definition_version) VALUES ($1,$2,'counter','count',$2,'dut','nms1-v1') ON CONFLICT(name,definition_version) DO NOTHING"), &[&metric,name]).await?;
            let row = tx.query_one(&format!("SELECT metric_id FROM {mode}.metric_definition WHERE name=$1 AND definition_version='nms1-v1'"), &[name]).await?;
            let metric: Uuid = row.get(0);
            let id = Uuid::new_v4();
            tx.execute(&format!("INSERT INTO {mode}.metric_series(series_id,metric_id,kind,labels) VALUES ($1,$2,'counter','{{}}') ON CONFLICT(metric_id,labels) DO NOTHING"), &[&id,&metric]).await?;
            let row = tx.query_one(&format!("SELECT series_id FROM {mode}.metric_series WHERE metric_id=$1 AND labels='{{}}'"), &[&metric]).await?;
            series.insert(name, row.get::<_, Uuid>(0));
        }
        let baseline = self.baseline.write(tx, mode, "baseline", &series).await?;
        let final_ = self.final_.write(tx, mode, "final", &series).await?;
        Ok((baseline, final_))
    }
}

impl Snapshot<'_> {
    async fn write(
        &self,
        tx: &Transaction<'_>,
        mode: &str,
        phase: &str,
        series: &BTreeMap<&String, Uuid>,
    ) -> Result<Uuid> {
        let role = format!("metrics_{phase}");
        let raw_id = super::protocol::artifact(tx, self.run, &role, self.raw).await?;
        let id = Uuid::new_v4();
        let profile = super::protocol::hex(&self.profile);
        tx.execute(&format!("INSERT INTO {mode}.metric_snapshot(snapshot_id,run_id,phase,epoch,boot_id,clock_id,at_tick,profile_hash,raw_artifact_id) VALUES ($1,$2,$3,$4,$5::text::bench.u64,'dut',$6::text::bench.u64,$7,$8)"), &[&id,&self.run,&phase,&i64::from(self.epoch),&self.boot.to_string(),&self.tick.to_string(),&profile,&raw_id]).await?;
        for (name, value) in &self.counters {
            tx.execute(&format!("INSERT INTO {mode}.scalar_sample(snapshot_id,series_id,kind,value) VALUES ($1,$2,'counter',$3::text::numeric)"),
                &[&id,&series[name],&value.to_string()]).await?;
        }
        Ok(id)
    }
}
