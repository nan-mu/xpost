//! xdp_forward_echo: VIP dst → same payload bounce; STA dst → FWSTIM+seq echo, no bounce.
//! Relies on firmware boot seed for `vip_ip` (no runtime VipMapOp).

use std::time::Duration;

use anyhow::Result;
use log::info;

use crate::{config::CaseConfig, harness::Harness, tests::ProbeTest};
use test_report::{CaseReport, TestReport};

pub struct XdpForwardEchoTest;

impl ProbeTest for XdpForwardEchoTest {
    fn name(&self) -> &'static str {
        "xdp_forward_echo"
    }

    fn run<'a>(
        &'a self,
        harness: &'a mut Harness,
        cases: &'a [CaseConfig],
    ) -> core::pin::Pin<Box<dyn core::future::Future<Output = Result<TestReport>> + Send + 'a>>
    {
        Box::pin(async move { run_xdp_forward_echo(harness, cases).await })
    }
}

async fn run_xdp_forward_echo(harness: &mut Harness, cases: &[CaseConfig]) -> Result<TestReport> {
    let mut reports = Vec::with_capacity(cases.len());
    for case in cases {
        info!(
            "xdp_forward_echo case={} src={} dst={:?} expect_bounce={} expect_echo={}",
            case.id, case.src_ip, case.dst_ip, case.expect_bounce, case.expect_echo
        );
        harness.reset_state().await;
        let seq = harness.alloc_seq();
        let mut report = CaseReport {
            test: "xdp_forward_echo".into(),
            case: case.id.clone(),
            expect_echo: case.expect_echo,
            got_echo: false,
            expect_bounce: case.expect_bounce,
            got_bounce: false,
            pass: false,
            t0_ns: None,
            t1_ns: None,
            delta_ns: None,
            seq,
            src_ip: case.src_ip.to_string(),
            dst_ip: case.dst_ip.unwrap_or(harness.esp_ip).to_string(),
            expect_sport: None,
            got_sport: None,
            expect_cell: None,
            got_cell: None,
            error: None,
        };

        if case.expect_bounce && case.dst_ip.is_none() && harness.vip_ip.is_none() {
            report.error = Some("vip_bounce case needs case.dst_ip or target.vip_ip".into());
            reports.push(report);
            continue;
        }

        let dst = if case.expect_bounce {
            case.dst_ip.or(harness.vip_ip)
        } else {
            case.dst_ip
        };

        if let Err(e) = harness.send_stimulus(case.src_ip, dst, seq).await {
            report.error = Some(format!("send_stimulus: {e:#}"));
            reports.push(report);
            continue;
        }

        match harness.wait_t0(seq, None).await {
            Ok(Some(t0)) => report.t0_ns = Some(t0),
            Ok(None) => {
                report.error = Some("timeout waiting for T0 (stim egress)".into());
                reports.push(report);
                continue;
            }
            Err(e) => {
                report.error = Some(format!("wait_t0: {e:#}"));
                reports.push(report);
                continue;
            }
        }

        if case.expect_bounce {
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
                    reports.push(report);
                    continue;
                }
            }
            let echoed = harness
                .wait_echo(seq, Some(Duration::from_millis(50)))
                .await;
            if let Ok(Some(_)) = echoed {
                report.got_echo = true;
            }
            report.pass = report.got_bounce && !report.got_echo;
            if !report.pass {
                report.error = Some(format!(
                    "expect_bounce got_bounce={} got_echo={} (echo must be absent)",
                    report.got_bounce, report.got_echo
                ));
            }
        } else {
            let echo_wait = if case.expect_echo {
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
                    reports.push(report);
                    continue;
                }
            }
            report.got_bounce = harness.saw_bounce(seq).await;
            report.pass = report.got_echo == case.expect_echo && !report.got_bounce;
            if !report.pass {
                report.error = Some(format!(
                    "expect_echo={} got_echo={} got_bounce={} (bounce must be absent)",
                    case.expect_echo, report.got_echo, report.got_bounce
                ));
            }
        }

        info!(
            "xdp_forward_echo case={} pass={} delta_ns={:?}",
            case.id, report.pass, report.delta_ns
        );
        reports.push(report);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let ok = reports.iter().all(|c| c.pass);
    Ok(TestReport {
        name: "xdp_forward_echo".into(),
        ok,
        cases: reports,
    })
}
