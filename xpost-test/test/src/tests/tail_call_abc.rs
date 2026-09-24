//! tail_call_abc — cold-boot + A trampoline + B policy; prove bpf_tail_call via abc.
//!
//! Firmware must boot with `fw_us_cold_boot` (empty HOT_A, no bpf.toml maps/seeds).
//!
//! | 阶段 | 操作 | a | b | c |
//! |------|------|---|---|---|
//! | T0 | 空机 | E | E | E |
//! | T1 | CREATE maps; UPLOAD A trampoline | E | E | E |
//! | T2 | UPLOAD B bounce; UPDATE b | E | B | E |
//! | T3 | UPDATE c | E | B | B |
//! | T4 | UPLOAD B drop (A unchanged) | E | D | D |
//! | T5 | unload B | E | E | E |

use std::net::Ipv4Addr;

use anyhow::{Result, bail};
use log::info;
use test_common::hot_load_op::{self, HotLoadOp, STORAGE_SRAM_A, STORAGE_SRAM_B};
use test_common::map_crud_op::{self, MapCrudOp};

use crate::{config::CaseConfig, generated::tail_call, harness::Harness, tests::ProbeTest};
use test_report::{CaseReport, TestReport};

mod assets {
    include!(concat!(env!("OUT_DIR"), "/tail_call_assets.rs"));
}

pub struct TailCallAbcTest;

impl ProbeTest for TailCallAbcTest {
    fn name(&self) -> &'static str {
        "tail_call_abc"
    }

    fn run<'a>(
        &'a self,
        harness: &'a mut Harness,
        cases: &'a [CaseConfig],
    ) -> core::pin::Pin<Box<dyn core::future::Future<Output = Result<TestReport>> + Send + 'a>>
    {
        Box::pin(async move { run_tail_call_abc(harness, cases).await })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Expect {
    Echo,
    Bounce,
    Drop,
}

impl Expect {
    fn tag(self) -> &'static str {
        match self {
            Expect::Echo => "E",
            Expect::Bounce => "B",
            Expect::Drop => "D",
        }
    }
}

struct Abc {
    a: Ipv4Addr,
    b: Ipv4Addr,
    c: Ipv4Addr,
    ctrl: Ipv4Addr,
}

fn gen_lan_ip(esp: Ipv4Addr, avoid: &[Ipv4Addr]) -> Ipv4Addr {
    use rand::RngExt;
    let mut rng = rand::rng();
    let oct = esp.octets();
    loop {
        let host = rng.random_range(2..254u8);
        let ip = Ipv4Addr::new(oct[0], oct[1], oct[2], host);
        if ip != esp && !avoid.contains(&ip) {
            return ip;
        }
    }
}

fn ip_key(ip: Ipv4Addr) -> [u8; 4] {
    ip.octets()
}

fn got_tag(echo: bool, bounce: bool) -> &'static str {
    match (echo, bounce) {
        (true, false) => "E",
        (false, true) => "B",
        (false, false) => "D",
        (true, true) => "EB",
    }
}

fn empty_fail(stage: &str, err: String) -> CaseReport {
    CaseReport {
        test: "tail_call_abc".into(),
        case: stage.into(),
        expect_echo: false,
        got_echo: false,
        expect_bounce: false,
        got_bounce: false,
        pass: false,
        t0_ns: None,
        t1_ns: None,
        delta_ns: None,
        seq: 0,
        src_ip: String::new(),
        dst_ip: String::new(),
        expect_sport: None,
        got_sport: None,
        expect_cell: None,
        got_cell: None,
        error: Some(err),
    }
}

fn map_aot(maps: &[tail_call::MapMeta], name: &str) -> Result<u64> {
    maps.iter()
        .find(|m| m.name == name)
        .map(|m| m.aot_addr)
        .ok_or_else(|| anyhow::anyhow!("missing map `{name}` in asset meta"))
}

async fn check_one(
    harness: &mut Harness,
    stage: &str,
    which: &str,
    src_ip: Ipv4Addr,
    expect: Expect,
) -> CaseReport {
    harness.reset_state().await;
    let seq = harness.alloc_seq();
    let mut report = CaseReport {
        test: "tail_call_abc".into(),
        case: format!("{stage}/{which}"),
        expect_echo: expect == Expect::Echo,
        got_echo: false,
        expect_bounce: expect == Expect::Bounce,
        got_bounce: false,
        pass: false,
        t0_ns: None,
        t1_ns: None,
        delta_ns: None,
        seq,
        src_ip: src_ip.to_string(),
        dst_ip: harness.esp_ip.to_string(),
        expect_sport: None,
        got_sport: None,
        expect_cell: None,
        got_cell: None,
        error: None,
    };

    if let Err(e) = harness.send_stimulus(src_ip, None, seq).await {
        report.error = Some(format!("send_stimulus: {e:#}"));
        info!("[{stage}] {which}={src_ip} send_fail={e:#}");
        return report;
    }

    match harness.wait_t0(seq, None).await {
        Ok(Some(t0)) => report.t0_ns = Some(t0),
        Ok(None) => {
            report.error = Some("timeout waiting for T0".into());
            info!("[{stage}] {which}={src_ip} T0=timeout");
            return report;
        }
        Err(e) => {
            report.error = Some(format!("wait_t0: {e:#}"));
            info!("[{stage}] {which}={src_ip} T0_err={e:#}");
            return report;
        }
    }

    match expect {
        Expect::Echo => {
            match harness.wait_echo(seq, None).await {
                Ok(Some(t1)) => {
                    report.got_echo = true;
                    report.t1_ns = Some(t1);
                    if let Some(t0) = report.t0_ns {
                        report.delta_ns = Some(t1.saturating_sub(t0));
                    }
                }
                Ok(None) => report.got_echo = false,
                Err(e) => {
                    report.error = Some(format!("wait_echo: {e:#}"));
                    return report;
                }
            }
            report.got_bounce = harness.saw_bounce(seq).await;
        }
        Expect::Bounce => {
            match harness.wait_bounce(seq, None).await {
                Ok(Some(t1)) => {
                    report.got_bounce = true;
                    report.t1_ns = Some(t1);
                    if let Some(t0) = report.t0_ns {
                        report.delta_ns = Some(t1.saturating_sub(t0));
                    }
                }
                Ok(None) => report.got_bounce = false,
                Err(e) => {
                    report.error = Some(format!("wait_bounce: {e:#}"));
                    return report;
                }
            }
            report.got_echo = harness
                .wait_echo(seq, Some(std::time::Duration::from_millis(50)))
                .await
                .ok()
                .flatten()
                .is_some();
        }
        Expect::Drop => {
            let to = Some(harness.pair_timeout());
            report.got_echo = harness.wait_echo(seq, to).await.ok().flatten().is_some();
            report.got_bounce = harness.saw_bounce(seq).await
                || harness
                    .wait_bounce(seq, Some(std::time::Duration::from_millis(50)))
                    .await
                    .ok()
                    .flatten()
                    .is_some();
        }
    }

    report.pass = match expect {
        Expect::Echo => report.got_echo && !report.got_bounce,
        Expect::Bounce => report.got_bounce && !report.got_echo,
        Expect::Drop => !report.got_echo && !report.got_bounce,
    };
    if !report.pass {
        report.error = Some(format!(
            "expect={} got={}",
            expect.tag(),
            got_tag(report.got_echo, report.got_bounce)
        ));
    }

    let delta = report
        .delta_ns
        .map(|d| format!("{d}ns"))
        .unwrap_or_else(|| "-".into());
    info!(
        "[{stage}] {which}={src_ip} expect={} got={} Δ={delta} pass={}",
        expect.tag(),
        got_tag(report.got_echo, report.got_bounce),
        report.pass
    );
    report
}

async fn check_abc(
    harness: &mut Harness,
    abc: &Abc,
    ea: Expect,
    eb: Expect,
    ec: Expect,
    stage: &str,
    reports: &mut Vec<CaseReport>,
) -> bool {
    info!(
        "[{stage}] check_abc expect=a:{} b:{} c:{}",
        ea.tag(),
        eb.tag(),
        ec.tag()
    );
    let ra = check_one(harness, stage, "a", abc.a, ea).await;
    let ok_a = ra.pass;
    reports.push(ra);
    let rb = check_one(harness, stage, "b", abc.b, eb).await;
    let ok_b = rb.pass;
    reports.push(rb);
    let rc = check_one(harness, stage, "c", abc.c, ec).await;
    let ok_c = rc.pass;
    reports.push(rc);
    let ok = ok_a && ok_b && ok_c;
    info!(
        "[{stage}] result={} (a={} b={} c={})",
        if ok { "PASS" } else { "FAIL" },
        if ok_a { "ok" } else { "fail" },
        if ok_b { "ok" } else { "fail" },
        if ok_c { "ok" } else { "fail" },
    );
    ok
}

async fn map_create(
    harness: &Harness,
    ctrl_src: Ipv4Addr,
    name: &str,
    map_type: u32,
    key_size: u32,
    value_size: u32,
    max_entries: u32,
    aot_addr: u64,
) -> Result<()> {
    let ack = harness
        .map_crud_op(
            ctrl_src,
            MapCrudOp {
                kind: map_crud_op::KIND_CREATE,
                map_name: name,
                map_type,
                key: &[],
                value: &[],
                flags: 0,
                key_size,
                value_size,
                max_entries,
                aot_addr,
            },
        )
        .await?;
    if ack.status != map_crud_op::STATUS_OK {
        bail!("MapCrud CREATE {name} status={}", ack.status);
    }
    Ok(())
}

async fn map_update(harness: &Harness, ctrl_src: Ipv4Addr, key_ip: Ipv4Addr) -> Result<()> {
    let key = ip_key(key_ip);
    let value = [1u8];
    let ack = harness
        .map_crud_op(
            ctrl_src,
            MapCrudOp::simple(
                map_crud_op::KIND_UPDATE,
                "fw_rules",
                map_crud_op::MAP_TYPE_HASH,
                &key,
                &value,
                0,
            ),
        )
        .await?;
    if ack.status != map_crud_op::STATUS_OK {
        bail!("MapCrud UPDATE {key_ip} status={}", ack.status);
    }
    Ok(())
}

async fn hot_upload(harness: &Harness, ctrl_src: Ipv4Addr, dst: u32, payload: &[u8]) -> Result<()> {
    let ack = harness
        .hot_load_op(
            ctrl_src,
            HotLoadOp {
                kind: hot_load_op::KIND_UPLOAD,
                region: 0,
                dram_slot: 0,
                src: 0,
                dst,
                payload,
            },
        )
        .await?;
    if ack.status != hot_load_op::STATUS_OK {
        bail!("HotLoad UPLOAD dst={dst} status={}", ack.status);
    }
    Ok(())
}

async fn hot_readback_len(harness: &Harness, ctrl_src: Ipv4Addr, src: u32) -> Result<u32> {
    let ack = harness
        .hot_load_op(
            ctrl_src,
            HotLoadOp {
                kind: hot_load_op::KIND_READBACK,
                region: 0,
                dram_slot: 0,
                src,
                dst: 0,
                payload: &[],
            },
        )
        .await?;
    if ack.status != hot_load_op::STATUS_OK {
        bail!("HotLoad READBACK src={src} status={}", ack.status);
    }
    Ok(ack.readback_len)
}

fn done(reports: Vec<CaseReport>) -> TestReport {
    let ok = reports.iter().all(|c| c.pass);
    info!(
        "[DONE] tail_call_abc result={} cases={}",
        if ok { "PASS" } else { "FAIL" },
        reports.len()
    );
    TestReport {
        name: "tail_call_abc".into(),
        ok,
        cases: reports,
    }
}

async fn run_tail_call_abc(harness: &mut Harness, cases: &[CaseConfig]) -> Result<TestReport> {
    let mut reports = Vec::new();
    let Some(case0) = cases.first() else {
        bail!("tail_call_abc needs at least one [[tests.cases]] with src_ip");
    };
    let b_ip = case0.src_ip;
    let esp = harness.esp_ip;
    let a_ip = gen_lan_ip(esp, &[b_ip]);
    let c_ip = gen_lan_ip(esp, &[b_ip, a_ip]);
    let ctrl = gen_lan_ip(esp, &[b_ip, a_ip, c_ip]);
    let abc = Abc {
        a: a_ip,
        b: b_ip,
        c: c_ip,
        ctrl,
    };
    info!(
        "[tail_call_abc] abc a={a_ip} b={b_ip} c={c_ip} ctrl={} esp={esp}",
        abc.ctrl
    );

    let a_bin = assets::A_TAILCALL_BIN;
    let bounce_bin = assets::B_BOUNCE_BIN;
    let drop_bin = assets::B_DROP_BIN;
    let jmp_aot = map_aot(tail_call::a_tailcall::MAPS, "jmp_table")?;
    let bounce_aot = map_aot(tail_call::b_bounce::MAPS, "fw_rules")?;
    let drop_aot = map_aot(tail_call::b_drop::MAPS, "fw_rules")?;

    // T0: empty machine
    if !check_abc(
        harness,
        &abc,
        Expect::Echo,
        Expect::Echo,
        Expect::Echo,
        "T0",
        &mut reports,
    )
    .await
    {
        return Ok(done(reports));
    }

    // CREATE maps (+ bounce/drop aot aliases for fw_rules)
    info!("[T1] CREATE jmp_table + fw_rules");
    if let Err(e) = map_create(
        harness,
        abc.ctrl,
        "jmp_table",
        map_crud_op::MAP_TYPE_PROG_ARRAY,
        4,
        4,
        4,
        jmp_aot,
    )
    .await
    {
        reports.push(empty_fail("T1/create_jmp", e.to_string()));
        return Ok(done(reports));
    }
    if let Err(e) = map_create(
        harness,
        abc.ctrl,
        "fw_rules",
        map_crud_op::MAP_TYPE_HASH,
        4,
        1,
        16,
        bounce_aot,
    )
    .await
    {
        reports.push(empty_fail("T1/create_fw", e.to_string()));
        return Ok(done(reports));
    }
    if let Err(e) = map_create(
        harness,
        abc.ctrl,
        "fw_rules",
        map_crud_op::MAP_TYPE_HASH,
        0,
        0,
        0,
        drop_aot,
    )
    .await
    {
        reports.push(empty_fail("T1/alias_fw", e.to_string()));
        return Ok(done(reports));
    }

    info!("[T1] UPLOAD A trampoline → SRAM_A");
    if let Err(e) = hot_upload(harness, abc.ctrl, STORAGE_SRAM_A, a_bin).await {
        reports.push(empty_fail("T1/upload_a", e.to_string()));
        return Ok(done(reports));
    }
    if !check_abc(
        harness,
        &abc,
        Expect::Echo,
        Expect::Echo,
        Expect::Echo,
        "T1",
        &mut reports,
    )
    .await
    {
        return Ok(done(reports));
    }

    // T2: B bounce + map={b}
    info!("[T2] UPLOAD B bounce → SRAM_B; UPDATE b");
    if let Err(e) = hot_upload(harness, abc.ctrl, STORAGE_SRAM_B, bounce_bin).await {
        reports.push(empty_fail("T2/upload_b", e.to_string()));
        return Ok(done(reports));
    }
    if let Err(e) = map_update(harness, abc.ctrl, b_ip).await {
        reports.push(empty_fail("T2/update_b", e.to_string()));
        return Ok(done(reports));
    }
    if !check_abc(
        harness,
        &abc,
        Expect::Echo,
        Expect::Bounce,
        Expect::Echo,
        "T2",
        &mut reports,
    )
    .await
    {
        return Ok(done(reports));
    }

    // T3: UPDATE c
    info!("[T3] UPDATE c");
    if let Err(e) = map_update(harness, abc.ctrl, c_ip).await {
        reports.push(empty_fail("T3/update_c", e.to_string()));
        return Ok(done(reports));
    }
    if !check_abc(
        harness,
        &abc,
        Expect::Echo,
        Expect::Bounce,
        Expect::Bounce,
        "T3",
        &mut reports,
    )
    .await
    {
        return Ok(done(reports));
    }

    // T4: swap B to drop; A length must stay
    info!("[T4] READBACK A; UPLOAD B drop; READBACK A again");
    let len_a = match hot_readback_len(harness, abc.ctrl, STORAGE_SRAM_A).await {
        Ok(n) => n,
        Err(e) => {
            reports.push(empty_fail("T4/readback_a0", e.to_string()));
            return Ok(done(reports));
        }
    };
    if len_a as usize != a_bin.len() {
        reports.push(empty_fail(
            "T4/readback_a0",
            format!("A len={len_a} want={}", a_bin.len()),
        ));
        return Ok(done(reports));
    }
    if let Err(e) = hot_upload(harness, abc.ctrl, STORAGE_SRAM_B, drop_bin).await {
        reports.push(empty_fail("T4/upload_drop", e.to_string()));
        return Ok(done(reports));
    }
    let len_a2 = match hot_readback_len(harness, abc.ctrl, STORAGE_SRAM_A).await {
        Ok(n) => n,
        Err(e) => {
            reports.push(empty_fail("T4/readback_a1", e.to_string()));
            return Ok(done(reports));
        }
    };
    if len_a2 != len_a {
        reports.push(empty_fail(
            "T4/a_changed",
            format!("A len changed {len_a} → {len_a2}"),
        ));
        return Ok(done(reports));
    }
    if !check_abc(
        harness,
        &abc,
        Expect::Echo,
        Expect::Drop,
        Expect::Drop,
        "T4",
        &mut reports,
    )
    .await
    {
        return Ok(done(reports));
    }

    // T5: unload B (empty UPLOAD) → clear prog_table[0]
    info!("[T5] unload B");
    if let Err(e) = hot_upload(harness, abc.ctrl, STORAGE_SRAM_B, &[]).await {
        reports.push(empty_fail("T5/unload_b", e.to_string()));
        return Ok(done(reports));
    }
    let _ = check_abc(
        harness,
        &abc,
        Expect::Echo,
        Expect::Echo,
        Expect::Echo,
        "T5",
        &mut reports,
    )
    .await;

    Ok(done(reports))
}
