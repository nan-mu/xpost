//! USB serial: `espflash flash` / `monitor` with isolated process groups + clean shutdown.

use std::process::Stdio;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, Notify};
use tokio::time::{Duration, sleep};

use crate::child::{isolate_process_group, terminate_child};
use crate::profiles::Profile;

const PORT_RELEASE: Duration = Duration::from_millis(400);

pub struct SerialSupervisor {
    chip: String,
    monitor_enabled: bool,
    deployed: Mutex<bool>,
    /// Exclusive short critical sections (spawn/stop handoff).
    op: Mutex<()>,
    monitor: Mutex<Option<Child>>,
    flash: Mutex<Option<Child>>,
    shutdown: Arc<Notify>,
}

impl SerialSupervisor {
    pub fn new(chip: impl Into<String>, monitor_enabled: bool) -> Self {
        Self {
            chip: chip.into(),
            monitor_enabled,
            deployed: Mutex::new(false),
            op: Mutex::new(()),
            monitor: Mutex::new(None),
            flash: Mutex::new(None),
            shutdown: Arc::new(Notify::new()),
        }
    }

    pub fn monitor_enabled(&self) -> bool {
        self.monitor_enabled
    }

    /// Stop monitor + any in-flight flash (Ctrl+C / agent exit).
    pub async fn shutdown(&self) {
        self.shutdown.notify_waiters();
        let _ = self.stop_monitor().await;
        self.stop_flash().await;
    }

    pub async fn start_monitor(&self) -> Result<()> {
        let _guard = self.op.lock().await;
        self.start_monitor_locked().await
    }

    async fn start_monitor_locked(&self) -> Result<()> {
        if !self.monitor_enabled {
            return Ok(());
        }
        let mut slot = self.monitor.lock().await;
        if slot.is_some() {
            return Ok(());
        }
        eprintln!("xpost-agent: espflash monitor (chip={})", self.chip);
        let mut cmd = Command::new("espflash");
        cmd.arg("monitor")
            .arg("--chip")
            .arg(&self.chip)
            .arg("--no-addresses")
            .arg("--non-interactive")
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        isolate_process_group(&mut cmd);
        let child = cmd.spawn().context("spawn espflash monitor")?;
        *slot = Some(child);
        Ok(())
    }

    pub async fn stop_monitor(&self) -> Result<()> {
        let _guard = self.op.lock().await;
        self.stop_monitor_locked().await;
        Ok(())
    }

    async fn stop_monitor_locked(&self) {
        let mut slot = self.monitor.lock().await;
        if let Some(child) = slot.take() {
            eprintln!("xpost-agent: stopping monitor …");
            terminate_child(child, "espflash monitor").await;
            sleep(PORT_RELEASE).await;
        }
    }

    async fn stop_flash(&self) {
        let _guard = self.op.lock().await;
        self.stop_flash_locked().await;
    }

    async fn stop_flash_locked(&self) {
        let mut slot = self.flash.lock().await;
        if let Some(child) = slot.take() {
            eprintln!("xpost-agent: stopping flash …");
            terminate_child(child, "espflash flash").await;
            sleep(PORT_RELEASE).await;
        }
    }

    pub async fn monitor_active(&self) -> bool {
        if !self.monitor_enabled {
            return *self.deployed.lock().await;
        }
        if !*self.deployed.lock().await {
            return false;
        }
        let mut slot = self.monitor.lock().await;
        let Some(child) = slot.as_mut() else {
            return false;
        };
        match child.try_wait() {
            Ok(None) => true,
            Ok(Some(_)) | Err(_) => {
                *slot = None;
                false
            }
        }
    }

    pub async fn flash_profile(&self, profile: &Profile) -> Result<()> {
        let child = {
            let _guard = self.op.lock().await;
            if !profile.bin.is_file() {
                bail!("profile bin missing: {}", profile.bin.display());
            }
            if !profile.partition_table.is_file() {
                bail!(
                    "partition table missing: {}",
                    profile.partition_table.display()
                );
            }
            self.stop_monitor_locked().await;
            self.stop_flash_locked().await;

            eprintln!(
                "xpost-agent: espflash flash profile={} — {}",
                profile.id, profile.description
            );
            let mut cmd = Command::new("espflash");
            cmd.arg("flash")
                .arg("--chip")
                .arg(&profile.chip)
                .arg("--partition-table")
                .arg(&profile.partition_table)
                .arg(&profile.bin)
                .stdin(Stdio::null())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit());
            isolate_process_group(&mut cmd);
            cmd.spawn().context("spawn espflash flash")?
        };

        {
            let mut slot = self.flash.lock().await;
            *slot = Some(child);
        }

        let shutdown = self.shutdown.clone();
        let wait = async {
            let mut slot = self.flash.lock().await;
            let Some(mut c) = slot.take() else {
                bail!("flash child missing");
            };
            c.wait().await.context("wait espflash flash")
        };

        let status = tokio::select! {
            s = wait => s?,
            _ = shutdown.notified() => {
                self.stop_flash().await;
                bail!("agent shutdown during flash");
            }
        };

        if !status.success() {
            bail!("espflash flash failed");
        }
        *self.deployed.lock().await = true;
        eprintln!("xpost-agent: flash OK; resuming monitor …");
        self.start_monitor().await?;
        Ok(())
    }

    pub async fn reboot_device(&self) -> Result<()> {
        if !*self.deployed.lock().await {
            bail!("reboot: no profile deployed yet (FlashProfile first)");
        }
        let mut child = {
            let _guard = self.op.lock().await;
            self.stop_monitor_locked().await;
            self.stop_flash_locked().await;
            eprintln!("xpost-agent: espflash reset (chip={})", self.chip);
            let mut cmd = Command::new("espflash");
            cmd.arg("reset")
                .arg("--chip")
                .arg(&self.chip)
                .stdin(Stdio::null())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit());
            isolate_process_group(&mut cmd);
            cmd.spawn().context("spawn espflash reset")?
        };

        let shutdown = self.shutdown.clone();
        let status = tokio::select! {
            s = child.wait() => s.context("wait espflash reset")?,
            _ = shutdown.notified() => {
                terminate_child(child, "espflash reset").await;
                bail!("agent shutdown during reset");
            }
        };
        if !status.success() {
            bail!("espflash reset failed");
        }
        eprintln!("xpost-agent: reset OK; resuming monitor …");
        self.start_monitor().await?;
        Ok(())
    }
}
