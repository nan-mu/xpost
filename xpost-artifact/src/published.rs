//! Inventory check for a libbpf-mcu v3 program archive.
//!
//! A v3 index identifies the published files. It has no firmware digest or
//! relocation record. A checked deployment is not a firmware-bound FlashImage.

use std::path::{Path, PathBuf};

use crate::{
    ResolveError, XdpMode, hex_sha256, parse_doc, read_bytes, req_str, req_u64, require_schema,
    tables, valid_segment,
};

pub const SCHEMA: &str = "libbpf-mcu.program-index.v3";

/// Verified XDP ELF identity and layout from the published archive.
#[derive(Debug)]
pub struct Deployment {
    pub mode: XdpMode,
    pub path: PathBuf,
    pub size_bytes: u64,
    pub sha256: String,
    pub embedded_name: String,
    pub image_size: usize,
    pub slot_address: usize,
    pub helper_address: usize,
}

/// Validate every indexed file under an extracted program root.
///
/// A raw BPF release is valid and returns no deployments.
pub fn verify_program(root: &Path, program: &str) -> Result<Vec<Deployment>, ResolveError> {
    let index_path = checked_path(root, "index.toml")?;
    let index = parse_doc(&read_bytes(&index_path)?, &index_path)?;
    require_schema(&index, SCHEMA, &index_path)?;
    if !valid_segment(program) || req_str(&index, "program", &index_path)? != program {
        return Err(ResolveError::SchemaMismatch { path: index_path });
    }
    let invalid_index = || ResolveError::SchemaMismatch {
        path: index_path.clone(),
    };

    let source = index.get("source").ok_or_else(invalid_index)?;
    read_member(root, source, &format!("{program}.c"), &index_path)?;
    let bpf = index.get("bpf").ok_or_else(invalid_index)?;
    let bpf_path = format!("objects/{program}_bpf.o");
    let bpf_bytes = read_member(root, bpf, &bpf_path, &index_path)?;
    check_relocatable(&bpf_bytes, 2, 247, &bpf_path)?;

    let mut deployments: Vec<Deployment> = Vec::new();
    for row in tables(&index, "mode", &index_path)? {
        let (mode, suffix) = match req_str(row, "name", &index_path)? {
            "llvm-aot" => (XdpMode::Aot, "aot"),
            "ebpf" => (XdpMode::Rbpf, "ebpf"),
            "jit" => (XdpMode::Jit, "jit"),
            _ => return Err(invalid_index()),
        };
        if deployments.iter().any(|d| d.mode == mode)
            || req_str(row, "role", &index_path)? != "xpost_container"
            || req_str(row, "format", &index_path)? != "xpost-compact-elf-v0"
        {
            return Err(invalid_index());
        }
        match (mode, row.get("object")) {
            (XdpMode::Aot, Some(object)) => {
                let object_path = format!("objects/{program}_rv32.o");
                let bytes = read_member(root, object, &object_path, &index_path)?;
                check_relocatable(&bytes, 1, 243, &object_path)?;
            }
            (XdpMode::Aot, None) | (_, Some(_)) => return Err(invalid_index()),
            (_, None) => {}
        }

        let elf = row.get("elf").ok_or_else(invalid_index)?;
        let relative = format!("containers/{program}-vm-{suffix}.elf");
        let bytes = read_member(root, elf, &relative, &index_path)?;
        let xpost_elf::ScanOutcome::Ok(container) = xpost_elf::scan_at(&mut bytes.as_slice(), 0)
        else {
            return Err(ResolveError::ArtifactMismatch { rel: relative });
        };
        if container.file_len != bytes.len() || !valid_segment(container.name.as_str()) {
            return Err(ResolveError::ArtifactMismatch { rel: relative });
        }
        deployments.push(Deployment {
            mode,
            path: root.join(relative),
            size_bytes: bytes.len() as u64,
            sha256: req_str(elf, "sha256", &index_path)?.to_owned(),
            embedded_name: container.name.as_str().to_owned(),
            image_size: container.image_size,
            slot_address: container.slot_address,
            helper_address: container.helper_address,
        });
    }
    Ok(deployments)
}

fn read_member(
    root: &Path,
    record: &toml::Value,
    expected_path: &str,
    index: &Path,
) -> Result<Vec<u8>, ResolveError> {
    let relative = req_str(record, "path", index)?;
    let wrong_member = || ResolveError::ArtifactMismatch {
        rel: relative.to_owned(),
    };
    if relative != expected_path {
        return Err(wrong_member());
    }
    let bytes = read_bytes(&checked_path(root, relative)?)?;
    if bytes.len() as u64 != req_u64(record, "size_bytes", index)?
        || hex_sha256(&bytes) != req_str(record, "sha256", index)?
    {
        return Err(wrong_member());
    }
    Ok(bytes)
}

fn checked_path(root: &Path, relative: &str) -> Result<PathBuf, ResolveError> {
    let mut path = root.to_path_buf();
    if !path.is_dir() || path.is_symlink() {
        return Err(ResolveError::Unreadable { path });
    }
    for part in relative.split('/') {
        if !valid_segment(part) {
            return Err(ResolveError::ArtifactMismatch {
                rel: relative.to_owned(),
            });
        }
        path.push(part);
        if path.is_symlink() {
            return Err(ResolveError::Unreadable { path });
        }
    }
    if !path.is_file() {
        return Err(ResolveError::Unreadable { path });
    }
    Ok(path)
}

fn check_relocatable(
    bytes: &[u8],
    class: u8,
    machine: u8,
    relative: &str,
) -> Result<(), ResolveError> {
    let header_len = if class == 2 { 64 } else { 52 };
    if bytes.len() < header_len
        || bytes[..7] != [0x7f, b'E', b'L', b'F', class, 1, 1]
        || bytes[16..20] != [1, 0, machine, 0]
    {
        return Err(ResolveError::ArtifactMismatch {
            rel: relative.to_owned(),
        });
    }
    Ok(())
}
