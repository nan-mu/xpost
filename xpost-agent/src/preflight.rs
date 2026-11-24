//! Read-only checks for the published deployments and the target firmware.

use std::path::Path;

use anyhow::{Context, Result};
use serde::Serialize;

use crate::deployment::{self, PreparedInfo, Target};
use crate::profiles::{Profiles, XdpProfile};
use crate::release::{COMMIT, DeploymentInfo, Mode, PROGRAMS, ProgramArchive, TAG};

pub fn profile(root: &Path, profiles: &Profiles, name: &str) -> Result<PreparedInfo> {
    let xdp = profiles
        .xdp(name)
        .with_context(|| format!("unknown xdp `{name}`"))?;
    let fw = profiles
        .fw(&xdp.fw)
        .with_context(|| format!("unknown fw `{}`", xdp.fw))?;
    let target = Target::read(root, fw).with_context(|| {
        format!(
            "libbpf-mcu {TAG} program {} mode {} target {}",
            xdp.program,
            xdp.mode.as_str(),
            fw.name
        )
    })?;
    Ok(deployment::prepare(root, &target, xdp)?.info)
}

#[derive(Serialize)]
pub struct Medium {
    pub release: &'static str,
    pub commit: &'static str,
    pub required: usize,
    pub deployments: Vec<DeploymentInfo>,
    pub prepared: Vec<PreparedInfo>,
    pub issues: Vec<String>,
}

impl Medium {
    pub fn deployment_ready(&self) -> bool {
        self.deployments.len() == self.required && self.issues.is_empty()
    }
}

pub fn medium(root: &Path, profiles: &Profiles) -> Medium {
    let mut report = Medium {
        release: TAG,
        commit: COMMIT,
        required: PROGRAMS.len() * Mode::ALL.len(),
        deployments: Vec::new(),
        prepared: Vec::new(),
        issues: Vec::new(),
    };
    for program in PROGRAMS {
        let archive = match ProgramArchive::open(root, program) {
            Ok(archive) => archive,
            Err(error) => {
                for mode in Mode::ALL {
                    report.issues.push(format!("{}: {error:#}", mode.as_str()));
                }
                continue;
            }
        };
        for mode in Mode::ALL {
            match archive.deployment(mode) {
                Ok(image) => report.deployments.push(image.info),
                Err(error) => {
                    report.issues.push(format!("{error:#}"));
                    continue;
                }
            }
            let primary = profile_name(program, mode);
            check_target(
                root,
                profiles,
                &archive,
                &primary,
                program,
                mode,
                0,
                &mut report,
            );
            if program == "case_mark" {
                let idle = profile_name("case_mark_idle", mode);
                check_target(
                    root,
                    profiles,
                    &archive,
                    &idle,
                    program,
                    mode,
                    1,
                    &mut report,
                );
            }
        }
    }
    report
}

fn profile_name(program: &str, mode: Mode) -> String {
    match mode {
        Mode::Aot => program.to_owned(),
        _ => format!("{program}_{}", mode.as_str()),
    }
}

fn check_target(
    root: &Path,
    profiles: &Profiles,
    archive: &ProgramArchive,
    name: &str,
    program: &str,
    mode: Mode,
    slot: u8,
    report: &mut Medium,
) {
    let result: Result<PreparedInfo> = (|| {
        let xdp = profiles
            .xdp(name)
            .filter(|profile: &&XdpProfile| {
                (profile.program.as_str(), profile.mode, profile.slot) == (program, mode, slot)
            })
            .with_context(|| format!("missing target profile {name} for slot {slot}"))?;
        let fw = profiles
            .fw(&xdp.fw)
            .context("missing target firmware profile")?;
        let target = Target::read(root, fw).with_context(|| format!("target {}", fw.name))?;
        Ok(target.prepare(archive.deployment(mode)?, xdp)?.info)
    })();
    match result {
        Ok(image) => report.prepared.push(image),
        Err(error) => report
            .issues
            .push(format!("{TAG} {program}/{}: {error:#}", mode.as_str())),
    }
}
