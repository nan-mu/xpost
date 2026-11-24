//! Read-only checks for the published deployments and the target firmware.

use std::path::Path;

use anyhow::{Context, Result};
use object::Object;
use serde::Serialize;

use crate::deployment::{self, PreparedInfo, Target};
use crate::profiles::{Profiles, XdpProfile, abs_under_root};
use crate::release::{COMMIT, DeploymentInfo, Mode, PROGRAMS, ProgramArchive, TAG, format_sha256};

const HAL_FIRMWARE: [&str; 4] = [
    "firewall-hal",
    "just-drop-hal",
    "just-tx-hal",
    "exec-bench-hal",
];

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
    pub required_targets: usize,
    pub deployments: Vec<DeploymentInfo>,
    pub prepared: Vec<PreparedInfo>,
    pub hal_firmware: Vec<HalFirmware>,
    pub issues: Vec<String>,
}

#[derive(Serialize)]
pub struct HalFirmware {
    firmware_profile: &'static str,
    firmware_sha256: String,
}

impl Medium {
    pub fn deployment_ready(&self) -> bool {
        self.deployments.len() == self.required
            && self.prepared.len() == self.required_targets
            && self.issues.is_empty()
    }
}

pub fn medium(root: &Path, profiles: &Profiles) -> Medium {
    let mut report = inspect(root, Some(profiles));
    for name in HAL_FIRMWARE {
        match check_hal(root, profiles, name) {
            Ok(firmware) => report.hal_firmware.push(firmware),
            Err(error) => report.issues.push(format!("HAL {name}: {error:#}")),
        }
    }
    report
}

pub fn inventory(root: &Path) -> Medium {
    inspect(root, None)
}

fn modes(program: &str, medium: bool) -> &'static [Mode] {
    if medium && matches!(program, "case_swap" | "case_mark") {
        &[Mode::Aot]
    } else {
        &Mode::ALL
    }
}

fn trace_carrier(program: &str, mode: Mode) -> Option<String> {
    let family = match program {
        "just_tx" => "just-tx",
        "just_drop" => "just-drop",
        "firewall" => "firewall",
        _ => return None,
    };
    let engine = if mode == Mode::Rbpf { "-rbpf" } else { "" };
    Some(format!("{family}{engine}-trace"))
}

fn inspect(root: &Path, profiles: Option<&Profiles>) -> Medium {
    let medium = profiles.is_some();
    let mut report = Medium {
        release: TAG,
        commit: COMMIT,
        required: PROGRAMS
            .iter()
            .map(|program| modes(program, medium).len())
            .sum(),
        required_targets: 0,
        deployments: Vec::new(),
        prepared: Vec::new(),
        hal_firmware: Vec::new(),
        issues: Vec::new(),
    };
    for program in PROGRAMS {
        let modes = modes(program, medium);
        if medium {
            report.required_targets += modes
                .iter()
                .map(|&mode| 1 + usize::from(trace_carrier(program, mode).is_some()))
                .sum::<usize>();
        }
        let archive = match ProgramArchive::open(root, program) {
            Ok(archive) => archive,
            Err(error) => {
                for mode in modes {
                    report.issues.push(format!("{}: {error:#}", mode.as_str()));
                }
                continue;
            }
        };
        for &mode in modes {
            match archive.deployment(mode) {
                Ok(image) => report.deployments.push(image.info),
                Err(error) => {
                    report.issues.push(format!("{error:#}"));
                    continue;
                }
            }
            let Some(profiles) = profiles else { continue };
            let (primary, slot) = if program == "case_mark" {
                ("case_mark_idle".to_owned(), 1)
            } else {
                (profile_name(program, mode), 0)
            };
            check_target(
                root,
                profiles,
                &archive,
                &primary,
                program,
                mode,
                slot,
                None,
                &mut report,
            );
            if let Some(firmware) = trace_carrier(program, mode) {
                check_target(
                    root,
                    profiles,
                    &archive,
                    &primary,
                    program,
                    mode,
                    slot,
                    Some(&firmware),
                    &mut report,
                );
            }
        }
    }
    report
}

fn check_hal(root: &Path, profiles: &Profiles, name: &'static str) -> Result<HalFirmware> {
    let profile = profiles.fw(name).context("missing HAL firmware profile")?;
    anyhow::ensure!(profile.xdp.is_empty(), "HAL profile lists XDP images");
    let firmware = abs_under_root(root, &profile.artifact);
    let partitions = abs_under_root(root, &profile.partition_table);
    let snapshot = deployment::firmware_snapshot(&firmware, &partitions)?;
    let elf = object::File::parse(snapshot.elf.as_slice()).context("parse HAL Firmware ELF")?;
    anyhow::ensure!(
        elf.architecture() == object::Architecture::Riscv32
            && elf.is_little_endian()
            && elf.kind() == object::ObjectKind::Executable
            && elf.section_by_name(xpost_elf::SECTION_NAME).is_none(),
        "invalid HAL Firmware ELF"
    );
    Ok(HalFirmware {
        firmware_profile: name,
        firmware_sha256: format_sha256(&snapshot.sha256),
    })
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
    firmware: Option<&str>,
    report: &mut Medium,
) {
    let result: Result<PreparedInfo> = (|| {
        let xdp = profiles
            .xdp(name)
            .filter(|profile: &&XdpProfile| {
                (profile.program.as_str(), profile.mode, profile.slot) == (program, mode, slot)
            })
            .with_context(|| format!("missing target profile {name} for slot {slot}"))?;
        let profile = firmware.unwrap_or(&xdp.fw);
        let fw = profiles
            .fw(profile)
            .with_context(|| format!("missing firmware profile {profile}"))?;
        let target = Target::read(root, fw).with_context(|| format!("target {}", fw.name))?;
        Ok(target
            .prepare(archive.deployment(mode)?, xdp)
            .with_context(|| format!("target {profile}"))?
            .info)
    })();
    match result {
        Ok(image) => report.prepared.push(image),
        Err(error) => report
            .issues
            .push(format!("{TAG} {program}/{}: {error:#}", mode.as_str())),
    }
}
