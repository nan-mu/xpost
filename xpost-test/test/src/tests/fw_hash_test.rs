//! fw_hash_test — 每个控制面操作后立刻用 a/b/c 三个 IP 测反应。
//!
//! 日志主键：`[S0]` / `[S1]` / …；每阶段先打配置，再打 a/b/c 回包。
//!
//! | 阶段 | 操作 | a | b | c |
//! |------|------|---|---|---|
//! | S0 | boot（bounce + map={b}） | E | B | E |
//! | S1 | UPDATE c | E | B | B |
//! | S2 | DELETE b | E | E | B |
//! | S3 | UPLOAD drop → 1号位 | E | E | B |
//! | S4 | COPY A→2号位; COPY 1号位→A | E | E | D |
//! | S5 | UPDATE b | E | D | D |
//! | S6 | COPY 2号位→A | E | B | B |
//! | S7 | READBACK 1号位+2号位 | — | — | — |

use std::net::Ipv4Addr;

use anyhow::{Result, bail};
use log::info;
use test_common::hot_load_op::{self, HotLoadOp, STORAGE_DRAM_0, STORAGE_DRAM_1, STORAGE_SRAM_A};
use test_common::map_crud_op::{self, MapCrudOp};

use crate::{config::CaseConfig, harness::Harness, tests::ProbeTest};
use test_report::{CaseReport, TestReport};

/// 1号位 / 2号位（DRAM staging）。
const SLOT_1: u32 = STORAGE_DRAM_0;
const SLOT_2: u32 = STORAGE_DRAM_1;

pub struct FwHashTest;

impl ProbeTest for FwHashTest {
    fn name(&self) -> &'static str {
        "fw_hash_test"
    }

    fn run<'a>(
        &'a self,
        harness: &'a mut Harness,
        cases: &'a [CaseConfig],
    ) -> core::pin::Pin<Box<dyn core::future::Future<Output = Result<TestReport>> + Send + 'a>>
    {
        Box::pin(async move { run_fw_hash_test(harness, cases).await })
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
    dut: Ipv4Addr,
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
        test: "fw_hash_test".into(),
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
        test: "fw_hash_test".into(),
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
        "[{stage}] probe expect a={} b={} c={}",
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

async fn map_delete(harness: &Harness, ctrl_src: Ipv4Addr, key_ip: Ipv4Addr) -> Result<()> {
    let key = ip_key(key_ip);
    let ack = harness
        .map_crud_op(
            ctrl_src,
            MapCrudOp::simple(
                map_crud_op::KIND_DELETE,
                "fw_rules",
                map_crud_op::MAP_TYPE_HASH,
                &key,
                &[],
                0,
            ),
        )
        .await?;
    if ack.status != map_crud_op::STATUS_OK {
        bail!("MapCrud DELETE {key_ip} status={}", ack.status);
    }
    Ok(())
}

async fn hot_op(
    harness: &Harness,
    ctrl_src: Ipv4Addr,
    op: HotLoadOp<'_>,
) -> Result<test_common::hot_load_op::HotLoadOpAck<'static>> {
    harness.hot_load_op(ctrl_src, op).await
}

async fn run_fw_hash_test(harness: &mut Harness, cases: &[CaseConfig]) -> Result<TestReport> {
    let Some(case) = cases.first() else {
        bail!("fw_hash_test needs [[tests.cases]] with src_ip = boot-seeded b_ip");
    };

    let b_ip = case.src_ip;
    let a_ip = gen_lan_ip(harness.esp_ip, &[b_ip]);
    let c_ip = gen_lan_ip(harness.esp_ip, &[b_ip, a_ip]);
    let ctrl_src = gen_lan_ip(harness.esp_ip, &[b_ip, a_ip, c_ip]);
    let abc = Abc {
        a: a_ip,
        b: b_ip,
        c: c_ip,
        ctrl: ctrl_src,
        dut: harness.esp_ip,
    };

    info!(
        "[S0] cfg a={} b={} c={} ctrl={} dut={}",
        abc.a, abc.b, abc.c, abc.ctrl, abc.dut
    );

    let drop_bin = include_bytes!("../../assets/hot_a/drop/esp32c3.bin");
    let bounce_bin = include_bytes!("../../assets/hot_a/bounce/esp32c3.bin");
    let mut reports = Vec::new();

    // S0 boot
    info!("[S0] op=boot xdp=fw_hash_bounce map={{b}}");
    if !check_abc(
        harness,
        &abc,
        Expect::Echo,
        Expect::Bounce,
        Expect::Echo,
        "S0",
        &mut reports,
    )
    .await
    {
        return Ok(done(reports));
    }

    // S1 UPDATE c
    info!("[S1] op=MapCrud UPDATE key=c={}", abc.c);
    if let Err(e) = map_update(harness, abc.ctrl, abc.c).await {
        info!("[S1] op_fail={e:#}");
        reports.push(empty_fail("S1", e.to_string()));
        return Ok(done(reports));
    }
    info!("[S1] cfg map={{b,c}} a={} b={} c={}", abc.a, abc.b, abc.c);
    if !check_abc(
        harness,
        &abc,
        Expect::Echo,
        Expect::Bounce,
        Expect::Bounce,
        "S1",
        &mut reports,
    )
    .await
    {
        return Ok(done(reports));
    }

    // S2 DELETE b
    info!("[S2] op=MapCrud DELETE key=b={}", abc.b);
    if let Err(e) = map_delete(harness, abc.ctrl, abc.b).await {
        info!("[S2] op_fail={e:#}");
        reports.push(empty_fail("S2", e.to_string()));
        return Ok(done(reports));
    }
    info!("[S2] cfg map={{c}} a={} b={} c={}", abc.a, abc.b, abc.c);
    if !check_abc(
        harness,
        &abc,
        Expect::Echo,
        Expect::Echo,
        Expect::Bounce,
        "S2",
        &mut reports,
    )
    .await
    {
        return Ok(done(reports));
    }

    // S3 UPLOAD drop → 1号位
    info!("[S3] op=UPLOAD drop.bin len={} dst=slot1", drop_bin.len());
    match hot_op(
        harness,
        abc.ctrl,
        HotLoadOp {
            kind: hot_load_op::KIND_UPLOAD,
            region: 0,
            dram_slot: 0,
            payload: drop_bin,
            src: 0,
            dst: SLOT_1,
        },
    )
    .await
    {
        Ok(ack) => info!("[S3] ack status={} loaded={}", ack.status, ack.loaded_len),
        Err(e) => {
            info!("[S3] op_fail={e:#}");
            reports.push(empty_fail("S3", e.to_string()));
            return Ok(done(reports));
        }
    }
    info!("[S3] cfg HOT_A=bounce map={{c}} (upload only)");
    if !check_abc(
        harness,
        &abc,
        Expect::Echo,
        Expect::Echo,
        Expect::Bounce,
        "S3",
        &mut reports,
    )
    .await
    {
        return Ok(done(reports));
    }

    // S4 COPY A→2; COPY 1→A
    info!("[S4] op=COPY HOT_A→slot2");
    if let Err(e) = hot_op(
        harness,
        abc.ctrl,
        HotLoadOp {
            kind: hot_load_op::KIND_COPY,
            region: 0,
            dram_slot: 0,
            payload: &[],
            src: STORAGE_SRAM_A,
            dst: SLOT_2,
        },
    )
    .await
    {
        info!("[S4] op_fail={e:#}");
        reports.push(empty_fail("S4", e.to_string()));
        return Ok(done(reports));
    }
    info!("[S4] op=COPY slot1→HOT_A");
    if let Err(e) = hot_op(
        harness,
        abc.ctrl,
        HotLoadOp {
            kind: hot_load_op::KIND_COPY,
            region: 0,
            dram_slot: 0,
            payload: &[],
            src: SLOT_1,
            dst: STORAGE_SRAM_A,
        },
    )
    .await
    {
        info!("[S4] op_fail={e:#}");
        reports.push(empty_fail("S4", e.to_string()));
        return Ok(done(reports));
    }
    info!(
        "[S4] cfg HOT_A=drop map={{c}} a={} b={} c={}",
        abc.a, abc.b, abc.c
    );
    if !check_abc(
        harness,
        &abc,
        Expect::Echo,
        Expect::Echo,
        Expect::Drop,
        "S4",
        &mut reports,
    )
    .await
    {
        return Ok(done(reports));
    }

    // S5 UPDATE b
    info!("[S5] op=MapCrud UPDATE key=b={}", abc.b);
    if let Err(e) = map_update(harness, abc.ctrl, abc.b).await {
        info!("[S5] op_fail={e:#}");
        reports.push(empty_fail("S5", e.to_string()));
        return Ok(done(reports));
    }
    info!(
        "[S5] cfg HOT_A=drop map={{b,c}} a={} b={} c={}",
        abc.a, abc.b, abc.c
    );
    if !check_abc(
        harness,
        &abc,
        Expect::Echo,
        Expect::Drop,
        Expect::Drop,
        "S5",
        &mut reports,
    )
    .await
    {
        return Ok(done(reports));
    }

    // S6 COPY 2→A
    info!("[S6] op=COPY slot2→HOT_A");
    if let Err(e) = hot_op(
        harness,
        abc.ctrl,
        HotLoadOp {
            kind: hot_load_op::KIND_COPY,
            region: 0,
            dram_slot: 0,
            payload: &[],
            src: SLOT_2,
            dst: STORAGE_SRAM_A,
        },
    )
    .await
    {
        info!("[S6] op_fail={e:#}");
        reports.push(empty_fail("S6", e.to_string()));
        return Ok(done(reports));
    }
    info!(
        "[S6] cfg HOT_A=bounce map={{b,c}} a={} b={} c={}",
        abc.a, abc.b, abc.c
    );
    if !check_abc(
        harness,
        &abc,
        Expect::Echo,
        Expect::Bounce,
        Expect::Bounce,
        "S6",
        &mut reports,
    )
    .await
    {
        return Ok(done(reports));
    }

    // S7 READBACK
    info!("[S7] op=READBACK slot1+slot2");
    let rb1 = match hot_op(
        harness,
        abc.ctrl,
        HotLoadOp {
            kind: hot_load_op::KIND_READBACK,
            region: 0,
            dram_slot: 0,
            payload: &[],
            src: SLOT_1,
            dst: 0,
        },
    )
    .await
    {
        Ok(ack) => ack,
        Err(e) => {
            info!("[S7] op_fail={e:#}");
            reports.push(empty_fail("S7", e.to_string()));
            return Ok(done(reports));
        }
    };
    let rb2 = match hot_op(
        harness,
        abc.ctrl,
        HotLoadOp {
            kind: hot_load_op::KIND_READBACK,
            region: 0,
            dram_slot: 0,
            payload: &[],
            src: SLOT_2,
            dst: 0,
        },
    )
    .await
    {
        Ok(ack) => ack,
        Err(e) => {
            info!("[S7] op_fail={e:#}");
            reports.push(empty_fail("S7", e.to_string()));
            return Ok(done(reports));
        }
    };

    let pass = rb1.readback_len as usize == drop_bin.len()
        && rb2.readback_len as usize == bounce_bin.len();
    info!(
        "[S7] slot1_len={} want_drop={} slot2_len={} want_bounce={} result={}",
        rb1.readback_len,
        drop_bin.len(),
        rb2.readback_len,
        bounce_bin.len(),
        if pass { "PASS" } else { "FAIL" }
    );
    reports.push(CaseReport {
        test: "fw_hash_test".into(),
        case: "S7".into(),
        expect_echo: false,
        got_echo: false,
        expect_bounce: false,
        got_bounce: false,
        pass,
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
        error: if pass {
            None
        } else {
            Some(format!(
                "len mismatch: slot1={} want_drop={} slot2={} want_bounce={}",
                rb1.readback_len,
                drop_bin.len(),
                rb2.readback_len,
                bounce_bin.len()
            ))
        },
    });

    Ok(done(reports))
}

fn done(reports: Vec<CaseReport>) -> TestReport {
    let ok = reports.iter().all(|c| c.pass);
    info!(
        "[DONE] fw_hash_test result={} cases={}",
        if ok { "PASS" } else { "FAIL" },
        reports.len()
    );
    TestReport {
        name: "fw_hash_test".into(),
        ok,
        cases: reports,
    }
}
