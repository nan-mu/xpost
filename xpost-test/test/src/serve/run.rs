//! Re-exec `./test <bench>` with stdout/stderr tee → terminal + job log.

use std::env;
use std::process::Stdio;

use anyhow::{Context, Result, bail};
use test_common::fw_agent::{JOB_DONE, JOB_FAILED, JOB_RUNNING};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

use super::job_log::JobLog;
use super::jobs::Jobs;

pub async fn run_test_job(
    jobs: &Jobs,
    job_id: u64,
    agent_endpoint: Option<&str>,
    log: JobLog,
) -> Result<()> {
    let job = jobs.get(job_id).await.context("job vanished")?;
    let bench = job.bench.as_str();
    if bench.is_empty() {
        jobs.set_state(job_id, JOB_FAILED, "empty bench").await;
        return Ok(());
    }

    jobs.set_state(job_id, JOB_RUNNING, format!("running test {bench}"))
        .await;

    let exe = env::current_exe().context("current_exe")?;
    let mut cmd = Command::new(&exe);
    cmd.arg(bench);
    if job.smoke {
        cmd.arg("--smoke");
    }
    if job.formal {
        cmd.arg("--formal");
    }
    if let Some(ep) = agent_endpoint.filter(|s| !s.is_empty()) {
        cmd.env("XPOST_AGENT_ENDPOINT", ep);
    }

    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("spawn {}", exe.display()))?;

    let stdout = child.stdout.take().context("stdout pipe")?;
    let stderr = child.stderr.take().context("stderr pipe")?;

    let log_out = log.clone();
    let log_err = log.clone();
    let tee_out = tokio::spawn(async move { tee_stream(stdout, log_out, false).await });
    let tee_err = tokio::spawn(async move { tee_stream(stderr, log_err, true).await });

    let status = child.wait().await.context("wait test")?;
    let _ = tee_out.await;
    let _ = tee_err.await;

    if status.success() {
        jobs.set_state(job_id, JOB_DONE, "ok").await;
    } else {
        jobs.set_state(job_id, JOB_FAILED, format!("exit {:?}", status.code()))
            .await;
    }
    Ok(())
}

async fn tee_stream<R>(mut reader: R, log: JobLog, is_stderr: bool) -> Result<()>
where
    R: AsyncReadExt + Unpin,
{
    let mut buf = [0u8; 4096];
    loop {
        let n = reader.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        let chunk = &buf[..n];
        log.append(chunk).await;
        if is_stderr {
            tokio::io::stderr().write_all(chunk).await?;
        } else {
            tokio::io::stdout().write_all(chunk).await?;
        }
    }
    Ok(())
}

pub fn validate_bench(bench: &str) -> Result<()> {
    match bench {
        "all" | "suite" | "drop-xdp" | "drop-hal" | "echo" | "wl-rtt" | "wl-loss" | "rate-rtt"
        | "rate-loss" | "just-drop" | "just-drop-pps" | "bench" | "bench-loss" => Ok(()),
        other => bail!("unknown bench `{other}`"),
    }
}
