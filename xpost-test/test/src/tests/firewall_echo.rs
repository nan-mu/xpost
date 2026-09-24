//! firewall_echo: blocked src → no echo; allowed src → echo.

use std::time::Duration;

use anyhow::Result;
use log::info;

use crate::{config::CaseConfig, harness::Harness, tests::ProbeTest};
use test_report::{CaseReport, TestReport};

pub struct FirewallEchoTest;

impl ProbeTest for FirewallEchoTest {
    fn name(&self) -> &'static str {
        "firewall_echo"
    }

    fn run<'a>(
        &'a self,
        harness: &'a mut Harness,
        cases: &'a [CaseConfig],
    ) -> core::pin::Pin<Box<dyn core::future::Future<Output = Result<TestReport>> + Send + 'a>>
    {
        Box::pin(async move { run_firewall_echo(harness, cases).await })
    }
}

async fn run_firewall_echo(harness: &mut Harness, cases: &[CaseConfig]) -> Result<TestReport> {
    let mut reports = Vec::with_capacity(cases.len());
    for case in cases {
        info!(
            "firewall_echo case={} src={} expect_echo={}",
            case.id, case.src_ip, case.expect_echo
        );
        harness.reset_state().await;
        let seq = harness.alloc_seq();
        let mut report = CaseReport {
            test: "firewall_echo".into(),
            case: case.id.clone(),
            expect_echo: case.expect_echo,
            got_echo: false,
            expect_bounce: false,
            got_bounce: false,
            pass: false,
            t0_ns: None,
            t1_ns: None,
            delta_ns: None,
            seq,
            src_ip: case.src_ip.to_string(),
            dst_ip: String::new(),
            expect_sport: None,
            got_sport: None,
            expect_cell: None,
            got_cell: None,
            error: None,
        };

        if let Err(e) = harness.send_stimulus(case.src_ip, None, seq).await {
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

        let echo_wait = if case.expect_echo {
            None
        } else {
            // Still wait full pair timeout to confirm absence.
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
            Ok(None) => {
                report.got_echo = false;
            }
            Err(e) => {
                report.error = Some(format!("wait_echo: {e:#}"));
                reports.push(report);
                continue;
            }
        }

        report.pass = report.got_echo == case.expect_echo;
        if !report.pass {
            report.error = Some(format!(
                "expect_echo={} got_echo={}",
                case.expect_echo, report.got_echo
            ));
        }
        info!(
            "firewall_echo case={} pass={} delta_ns={:?}",
            case.id, report.pass, report.delta_ns
        );
        reports.push(report);

        // Small gap between cases.
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let ok = reports.iter().all(|c| c.pass);
    Ok(TestReport {
        name: "firewall_echo".into(),
        ok,
        cases: reports,
    })
}
