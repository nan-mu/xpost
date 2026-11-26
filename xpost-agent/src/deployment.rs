//! Bind a release deployment to a linked firmware layout before a flash operation.

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result, bail, ensure};
use object::{Object, ObjectSection, ObjectSymbol};
use serde::Serialize;
use sha2::{Digest, Sha256};
use xpost_elf::{
    ImageMetadataV0, MapDescriptorV0, ParsedContainer, SECTION_NAME, build_container, decode,
};
use xpost_serial::client::FirmwareExpectation;
use xpost_serial::client::flash::{DEFAULT_XDP_PARTITION, XdpTable, xdp_table_from_bin};

use crate::profiles::{FwProfile, XdpProfile, abs_under_root};
use crate::release::{Deployment, DeploymentInfo, Mode, ProgramArchive, TAG, format_sha256};

pub struct Target {
    pub snapshot: FirmwareExpectation,
    pub table: XdpTable,
    profile: String,
}

impl Target {
    pub fn read(root: &Path, profile: &FwProfile) -> Result<Self> {
        let firmware = abs_under_root(root, &profile.artifact);
        let partitions = abs_under_root(root, &profile.partition_table);
        let snapshot = firmware_snapshot(&firmware, &partitions)?;
        let mut table = xdp_table_from_bin(&snapshot.partition_table, DEFAULT_XDP_PARTITION)?;
        if let Some(capacity) = profile.xdp_image_capacity {
            table.image_capacity = capacity.min(table.len as usize);
        }
        Ok(Self {
            snapshot,
            table,
            profile: profile.name.clone(),
        })
    }

    pub fn prepare(&self, deployment: Deployment, profile: &XdpProfile) -> Result<PreparedImage> {
        self.prepare_inner(deployment, profile).with_context(|| {
            format!(
                "libbpf-mcu {TAG} program {} mode {} profile {}",
                profile.program,
                profile.mode.as_str(),
                profile.name
            )
        })
    }

    fn prepare_inner(&self, deployment: Deployment, profile: &XdpProfile) -> Result<PreparedImage> {
        let file =
            object::File::parse(self.snapshot.elf.as_slice()).context("parse Firmware ELF")?;
        ensure!(
            file.architecture() == object::Architecture::Riscv32
                && file.is_little_endian()
                && file.kind() == object::ObjectKind::Executable,
            "firmware is not a linked little-endian RV32 ELF"
        );
        let header = file
            .section_by_name(SECTION_NAME)
            .context("firmware has no XPM1 section")?;
        let map = decode(header.data()?)
            .map_err(|error| anyhow::anyhow!("invalid firmware XPM1: {error:?}"))?;
        let helper = file
            .symbols()
            .find(|symbol| {
                symbol.is_definition()
                    && symbol.name().is_ok_and(|name| name == "helper_dispatcher")
            })
            .context("firmware has no linked helper dispatcher")?;
        ensure!(
            helper.address() == u64::from(map.helper_addr),
            "firmware XPM1 and helper symbol disagree"
        );
        let table = self.table;
        ensure!(
            (map.table_addr, map.table_len) == (table.base, table.len),
            "firmware XPM1 and partition table disagree on the XDP flash span"
        );
        let primary = file
            .section_by_name(".xdp.hotload_0")
            .context("firmware has no primary SRAM slot")?;
        ensure!(
            (primary.address(), primary.size()) == (map.slot_addr.into(), map.slot_len.into()),
            "firmware XPM1 and primary SRAM slot disagree"
        );
        let section = format!(".xdp.hotload_{}", profile.slot);
        let slot = file
            .section_by_name(&section)
            .with_context(|| format!("firmware has no SRAM slot {}", profile.slot))?;
        let address = u32::try_from(slot.address())?;
        let capacity = u32::try_from(slot.size())?;
        ensure!(
            address != 0 && address % 4 == 0 && capacity != 0,
            "invalid firmware SRAM slot"
        );
        let interpreter = file.symbols().any(|symbol| {
            symbol.name().is_ok_and(|name| {
                name == "xpost_rbpf_stack" || name.ends_with("5xpost3xdp6interp10EBPF_STACK")
            })
        });
        ensure!(
            interpreter == (profile.mode == Mode::Rbpf),
            "firmware execution engine does not match {}",
            profile.mode.as_str()
        );
        ensure!(
            deployment.info.mode == profile.mode && deployment.info.program == profile.program,
            "release deployment identity does not match the profile"
        );
        let container = deployment.container;
        ensure!(
            container.helper_address == map.helper_addr as usize,
            "firmware helper at {:#x}; release deployment uses {:#x}",
            map.helper_addr,
            container.helper_address
        );
        let same_slot = container.slot_address == address as usize;
        let payload_sha256 = format_sha256(&container.digest);
        if !same_slot
            && profile.mode != Mode::Rbpf
            && !verified_case_slot_b(
                &deployment.info,
                &container,
                &payload_sha256,
                address,
                capacity,
                map.slot_addr,
                map.slot_len,
            )
        {
            bail!(
                "slot {} at {address:#x}; release deployment links at {:#x}. No relocation data or matching payload and layout evidence is available",
                profile.slot,
                container.slot_address
            );
        }
        ensure!(
            container.image_size <= capacity as usize,
            "image size {} exceeds SRAM slot capacity {capacity}",
            container.image_size
        );
        let bytes = if same_slot && container.name.as_str() == profile.name {
            deployment.bytes
        } else {
            // For this release, rBPF carries map indexes in its instructions.
            // Moving those instructions leaves map resolution to the interpreter.
            // Verified case AOT images also retain their exact payload on slot B.
            let maps = container
                .maps
                .into_iter()
                .map(|entry| MapDescriptorV0 {
                    name: entry.name,
                    map_type: entry.map_type,
                    len: entry.len,
                    map_vma: entry.vma,
                    key_layout: entry.key,
                    value_layout: entry.value,
                })
                .collect();
            let metadata = ImageMetadataV0 {
                slot_address: address,
                helper_address: map.helper_addr,
                machine: 1,
                name: profile
                    .name
                    .as_str()
                    .try_into()
                    .map_err(|_| anyhow::anyhow!("invalid XDP alias"))?,
                maps,
            };
            let image = &deployment.bytes[container.image_offset..][..container.image_size];
            build_container(image, &metadata, Some(capacity))
                .map_err(|error| anyhow::anyhow!("encode XDP ELF for alias: {error:?}"))?
        };
        ensure!(
            bytes.len() <= table.image_capacity,
            "XDP ELF size {} exceeds cold storage capacity {}",
            bytes.len(),
            table.image_capacity
        );
        let info = PreparedInfo {
            deployment: deployment.info,
            profile: profile.name.clone(),
            firmware_profile: self.profile.clone(),
            firmware_sha256: format_sha256(&self.snapshot.sha256),
            container_sha256: format_sha256(&Sha256::digest(&bytes).into()),
            payload_sha256,
            target_address: address,
            slot: profile.slot,
            slot_capacity: capacity,
            cold_capacity: table.image_capacity,
        };
        Ok(PreparedImage {
            info,
            bytes: bytes.into(),
        })
    }
}

fn verified_case_slot_b(
    info: &DeploymentInfo,
    container: &ParsedContainer,
    payload_sha256: &str,
    address: u32,
    capacity: u32,
    primary_address: u32,
    primary_capacity: u32,
) -> bool {
    // Direct-link and QEMU evidence, Gitea 20ab29f tests/case-v0.1.1.md.
    let expected = match info.program.as_str() {
        "case_swap" => "91a6278c06dca003a1a5c5029de84814d01f3a913fdde7d4a278b98083d536fd",
        "case_mark" => "7d74289e391827fedef4a6d56f62412323de79ab34dd87037721c2fb7a7464f5",
        _ => return false,
    };
    TAG == "v0.1.1"
        && info.mode == Mode::Aot
        && payload_sha256 == expected
        && container.image_size == 432
        && container.maps.is_empty()
        && container.slot_address == 0x40380200
        && container.helper_address == 0x40380000
        && (primary_address, primary_capacity) == (0x40380200, 0xe00)
        && (address, capacity) == (0x40381000, 0xe00)
}

#[derive(Serialize)]
pub struct PreparedInfo {
    #[serde(flatten)]
    pub deployment: DeploymentInfo,
    pub profile: String,
    pub firmware_profile: String,
    pub firmware_sha256: String,
    pub container_sha256: String,
    pub payload_sha256: String,
    pub target_address: u32,
    pub slot: u8,
    pub slot_capacity: u32,
    pub cold_capacity: usize,
}

pub struct PreparedImage {
    pub info: PreparedInfo,
    pub bytes: Arc<[u8]>,
}

pub fn prepare(root: &Path, target: &Target, profile: &XdpProfile) -> Result<PreparedImage> {
    let archive = ProgramArchive::open(root, &profile.program).with_context(|| {
        format!(
            "mode {} for profile {}",
            profile.mode.as_str(),
            profile.name
        )
    })?;
    let deployment = archive.deployment(profile.mode)?;
    target.prepare(deployment, profile)
}

pub fn firmware_snapshot(path: &Path, partitions: &Path) -> Result<FirmwareExpectation> {
    let elf = std::fs::read(path).with_context(|| format!("read firmware {}", path.display()))?;
    let sha256 = Sha256::digest(&elf).into();
    let partition_table = xpost_serial::client::flash::partition_table_bin(partitions)?;
    Ok(FirmwareExpectation {
        elf,
        sha256,
        partition_table,
    })
}
