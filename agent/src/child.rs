//! Kill espflash children reliably (own session / process group on Unix).

use std::time::Duration;

use tokio::process::Child;
use tokio::time;

const TERM_WAIT: Duration = Duration::from_secs(3);
const KILL_WAIT: Duration = Duration::from_secs(2);

/// Spawn hook: new session so `kill(-pgid)` reaches the whole espflash tree.
#[cfg(unix)]
#[allow(unused_imports)]
pub fn isolate_process_group(cmd: &mut tokio::process::Command) {
    use std::os::unix::process::CommandExt as _;
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(not(unix))]
pub fn isolate_process_group(_cmd: &mut tokio::process::Command) {}

/// SIGTERM → wait → SIGKILL → wait. Does not hang forever on stuck serial I/O.
pub async fn terminate_child(mut child: Child, label: &str) {
    let pid = child.id();
    eprintln!("xpost-agent: terminate {label} (pid={pid:?}) …");

    #[cfg(unix)]
    if let Some(pid) = pid {
        signal_process_group(pid, libc::SIGTERM);
    }
    #[cfg(not(unix))]
    let _ = child.kill().await;

    if !wait_child(&mut child, TERM_WAIT).await {
        #[cfg(unix)]
        if let Some(pid) = pid {
            signal_process_group(pid, libc::SIGKILL);
        }
        let _ = child.kill().await;
        let _ = wait_child(&mut child, KILL_WAIT).await;
    }
}

#[cfg(unix)]
fn signal_process_group(pid: u32, sig: i32) {
    unsafe {
        let pgid = libc::getpgid(pid as libc::pid_t);
        if pgid > 0 {
            libc::kill(-pgid, sig);
        } else {
            libc::kill(pid as libc::pid_t, sig);
        }
    }
}

async fn wait_child(child: &mut Child, budget: Duration) -> bool {
    time::timeout(budget, child.wait())
        .await
        .ok()
        .and_then(|r| r.ok())
        .is_some()
}
