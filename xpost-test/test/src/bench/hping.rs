//! Spawn `hping3` as the DDoS interferer (UDP flood with spoofed src).

use std::net::Ipv4Addr;
use std::process::{Child, Command, Stdio};

use anyhow::{Context, Result};
use log::{info, warn};

pub struct Hping {
    children: Vec<Child>,
}

impl Hping {
    /// `pps == 0` → no process. Otherwise UDP to `esp:dport` spoofing `spoof`.
    pub fn start(esp: Ipv4Addr, spoof: Ipv4Addr, dport: u16, pps: u32) -> Result<Self> {
        if pps == 0 {
            return Ok(Self {
                children: Vec::new(),
            });
        }
        let interval_us = (1_000_000u64 / u64::from(pps)).max(1);
        Self::spawn_one(
            &[
                "--udp",
                "-p",
                &dport.to_string(),
                "-a",
                &spoof.to_string(),
                "-i",
                &format!("u{interval_us}"),
                &esp.to_string(),
            ],
            format!("spoof={spoof} → {esp}:{dport} UDP ~{pps} pps"),
        )
    }

    fn spawn_one(args: &[&str], label: String) -> Result<Self> {
        Ok(Self {
            children: vec![Self::spawn_child(args, &label)?],
        })
    }

    fn spawn_child(args: &[&str], label: &str) -> Result<Child> {
        let child = Command::new("hping3")
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("spawn hping3 {label} (on TR3000: opkg install hping3)"))?;
        info!("hping3 pid={} {label}", child.id());
        Ok(child)
    }

    pub fn stop(mut self) {
        self.kill();
    }

    fn kill(&mut self) {
        if self.children.is_empty() {
            return;
        }
        for mut c in self.children.drain(..) {
            match c.kill() {
                Ok(()) => {
                    let _ = c.wait();
                }
                Err(e) => warn!("hping3 kill: {e}"),
            }
        }
        info!("hping3 stopped");
    }
}

impl Drop for Hping {
    fn drop(&mut self) {
        self.kill();
    }
}
