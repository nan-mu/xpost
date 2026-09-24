//! hot_load_ctrl: SWAP (切换) HOT_A DROP→PASS; then HOT_SWAP (热切换) B=DROP and back to A.

use std::time::Duration;

use anyhow::Result;
use log::info;
use test_common::hot_load_op::{REGION_A, REGION_B, hot_swap_rx_op, swap_rx_op};

use crate::{config::CaseConfig, generated, harness::Harness, tests::ProbeTest};
use test_report::{CaseReport, TestReport};

/// Compile-time embedded AOT (from `assets/hot_a/` via build.rs).
mod assets {
    include!(concat!(env!("OUT_DIR"), "/hot_a_assets.rs"));
}

pub struct HotLoadCtrlTest;

impl ProbeTest for HotLoadCtrlTest {
    fn name(&self) -> &'static str {
        "hot_load_ctrl"
    }

    fn run<'a>(
        &'a self,
        harness: &'a mut Harness,
        cases: &'a [CaseConfig],
    ) -> core::pin::Pin<Box<dyn core::future::Future<Output = Result<TestReport>> + Send + 'a>>
    {
        Box::pin(async move { run_hot_load_ctrl(harness, cases).await })
    }
}

async fn stim_echo_case(
    harness: &mut Harness,
    case: &CaseConfig,
    expect_echo: bool,
    label: &str,
) -> CaseReport {
    harness.reset_state().await;
    let seq = harness.alloc_seq();
    let mut report = CaseReport {
        test: "hot_load_ctrl".into(),
        case: format!("{}:{}", case.id, label),
        expect_echo,
        got_echo: false,
        expect_bounce: false,
        got_bounce: false,
        pass: false,
        t0_ns: None,
        t1_ns: None,
        delta_ns: None,
        seq,
        src_ip: case.src_ip.to_string(),
        dst_ip: harness.esp_ip.to_string(),
        expect_sport: None,
        got_sport: None,
        expect_cell: None,
        got_cell: None,
        error: None,
    };

    if let Err(e) = harness.send_stimulus(case.src_ip, None, seq).await {
        report.error = Some(format!("send_stimulus: {e:#}"));
        return report;
    }

    match harness.wait_t0(seq, None).await {
        Ok(Some(t0)) => report.t0_ns = Some(t0),
        Ok(None) => {
            report.error = Some("timeout waiting for T0".into());
            return report;
        }
        Err(e) => {
            report.error = Some(format!("wait_t0: {e:#}"));
            return report;
        }
    }

    let echo_wait = if expect_echo {
        None
    } else {
        Some(harness.pair_timeout())
    };
    match harness.wait_echo(seq, echo_wait).await {
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

    report.pass = report.got_echo == expect_echo;
    if !report.pass {
        report.error = Some(format!(
            "expect_echo={} got_echo={}",
            expect_echo, report.got_echo
        ));
    }
    report
}

fn ctrl_ack_report(
    harness: &Harness,
    case: &CaseConfig,
    label: &str,
    result: Result<test_common::hot_load_op::HotLoadOpAck<'static>>,
    want_loaded: usize,
) -> CaseReport {
    match result {
        Ok(ack) => {
            info!(
                "hot_load_ctrl {label} ack stashed={} loaded={} (want loaded={})",
                ack.stashed_len, ack.loaded_len, want_loaded
            );
            let mut r = CaseReport {
                test: "hot_load_ctrl".into(),
                case: format!("{}:{}", case.id, label),
                expect_echo: false,
                got_echo: false,
                expect_bounce: false,
                got_bounce: false,
                pass: ack.loaded_len as usize == want_loaded,
                t0_ns: None,
                t1_ns: None,
                delta_ns: None,
                seq: 0,
                src_ip: case.src_ip.to_string(),
                dst_ip: harness.esp_ip.to_string(),
                expect_sport: None,
                got_sport: None,
                expect_cell: None,
                got_cell: None,
                error: None,
            };
            if !r.pass {
                r.error = Some(format!(
                    "loaded_len={} want {}",
                    ack.loaded_len, want_loaded
                ));
            }
            r
        }
        Err(e) => CaseReport {
            test: "hot_load_ctrl".into(),
            case: format!("{}:{}", case.id, label),
            expect_echo: false,
            got_echo: false,
            expect_bounce: false,
            got_bounce: false,
            pass: false,
            t0_ns: None,
            t1_ns: None,
            delta_ns: None,
            seq: 0,
            src_ip: case.src_ip.to_string(),
            dst_ip: harness.esp_ip.to_string(),
            expect_sport: None,
            got_sport: None,
            expect_cell: None,
            got_cell: None,
            error: Some(format!("hot_load_op {label}: {e:#}")),
        },
    }
}

async fn run_hot_load_ctrl(harness: &mut Harness, cases: &[CaseConfig]) -> Result<TestReport> {
    let Some(case) = cases.first() else {
        return Ok(TestReport {
            name: "hot_load_ctrl".into(),
            ok: false,
            cases: vec![CaseReport {
                test: "hot_load_ctrl".into(),
                case: "missing".into(),
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
                error: Some("hot_load_ctrl needs at least one [[tests.cases]] (src_ip)".into()),
            }],
        });
    };

    let src = case.src_ip;
    let mut reports = Vec::new();

    // S1: SWAP DROP into current rx (HOT_A)
    info!(
        "hot_load_ctrl S1 SWAP DROP bin={} maps={} xdp_size={}",
        assets::DROP_BIN.len(),
        generated::drop::MAPS.len(),
        generated::drop::XDP_SIZE
    );
    let r = ctrl_ack_report(
        harness,
        case,
        "swap_drop",
        harness
            .hot_load_op(src, swap_rx_op(REGION_A, assets::DROP_BIN, 1))
            .await,
        assets::DROP_BIN.len(),
    );
    let s1_ok = r.pass;
    reports.push(r);
    if !s1_ok {
        return Ok(TestReport {
            name: "hot_load_ctrl".into(),
            ok: false,
            cases: reports,
        });
    }

    tokio::time::sleep(Duration::from_millis(50)).await;

    // S2: expect no echo
    let r = stim_echo_case(harness, case, false, "assert_drop").await;
    info!("hot_load_ctrl S2 assert_drop pass={}", r.pass);
    let s2_ok = r.pass;
    reports.push(r);
    if !s2_ok {
        return Ok(TestReport {
            name: "hot_load_ctrl".into(),
            ok: false,
            cases: reports,
        });
    }

    // S3: SWAP PASS
    info!(
        "hot_load_ctrl S3 SWAP PASS bin={} maps={} xdp_size={}",
        assets::PASS_BIN.len(),
        generated::pass::MAPS.len(),
        generated::pass::XDP_SIZE
    );
    let r = ctrl_ack_report(
        harness,
        case,
        "swap_pass",
        harness
            .hot_load_op(src, swap_rx_op(REGION_A, assets::PASS_BIN, 1))
            .await,
        assets::PASS_BIN.len(),
    );
    let s3_ok = r.pass;
    reports.push(r);
    if !s3_ok {
        return Ok(TestReport {
            name: "hot_load_ctrl".into(),
            ok: false,
            cases: reports,
        });
    }

    tokio::time::sleep(Duration::from_millis(50)).await;

    // S4: expect echo
    let r = stim_echo_case(harness, case, true, "assert_pass").await;
    info!("hot_load_ctrl S4 assert_pass pass={}", r.pass);
    let s4_ok = r.pass;
    reports.push(r);
    if !s4_ok {
        return Ok(TestReport {
            name: "hot_load_ctrl".into(),
            ok: false,
            cases: reports,
        });
    }

    // S5: HOT_SWAP DROP into REGION_B (call → rx)
    info!(
        "hot_load_ctrl S5 HOT_SWAP DROP B bin={}",
        assets::DROP_BIN.len()
    );
    let r = ctrl_ack_report(
        harness,
        case,
        "hot_swap_drop_b",
        harness
            .hot_load_op(src, hot_swap_rx_op(REGION_B, assets::DROP_BIN))
            .await,
        assets::DROP_BIN.len(),
    );
    let s5_ok = r.pass;
    reports.push(r);
    if !s5_ok {
        return Ok(TestReport {
            name: "hot_load_ctrl".into(),
            ok: false,
            cases: reports,
        });
    }

    tokio::time::sleep(Duration::from_millis(50)).await;

    // S6: expect no echo (B is rx DROP)
    let r = stim_echo_case(harness, case, false, "assert_hot_swap_drop").await;
    info!("hot_load_ctrl S6 assert_hot_swap_drop pass={}", r.pass);
    let s6_ok = r.pass;
    reports.push(r);
    if !s6_ok {
        return Ok(TestReport {
            name: "hot_load_ctrl".into(),
            ok: false,
            cases: reports,
        });
    }

    // S7: HOT_SWAP PASS back to REGION_A
    info!(
        "hot_load_ctrl S7 HOT_SWAP PASS A bin={}",
        assets::PASS_BIN.len()
    );
    let r = ctrl_ack_report(
        harness,
        case,
        "hot_swap_pass_a",
        harness
            .hot_load_op(src, hot_swap_rx_op(REGION_A, assets::PASS_BIN))
            .await,
        assets::PASS_BIN.len(),
    );
    let s7_ok = r.pass;
    reports.push(r);
    if !s7_ok {
        return Ok(TestReport {
            name: "hot_load_ctrl".into(),
            ok: false,
            cases: reports,
        });
    }

    tokio::time::sleep(Duration::from_millis(50)).await;

    // S8: expect echo
    let r = stim_echo_case(harness, case, true, "assert_hot_swap_pass").await;
    info!("hot_load_ctrl S8 assert_hot_swap_pass pass={}", r.pass);
    reports.push(r);

    let ok = reports.iter().all(|c| c.pass);
    Ok(TestReport {
        name: "hot_load_ctrl".into(),
        ok,
        cases: reports,
    })
}
