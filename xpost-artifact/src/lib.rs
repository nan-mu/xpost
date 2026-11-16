//! Flash resolution for one program and one XDP mode.
//!
//! Pin: libbpf-mcu commit [`CONTRACT_COMMIT`], pull request 4, provisional
//! until merge. Do not add schema fields. Do not treat this pin as a frozen
//! format.
//!
//! The crate reads `index.toml`, `<program>/manifest.toml`, and
//! `<program>/<mode>/report.toml`. It does not read `profiles.toml`.
//! It does not flash, and it does not talk to the serial worker.

use std::path::{Component, Path, PathBuf};

use sha2::{Digest, Sha256};

/// Producer commit this reader understands. Provisional.
pub const CONTRACT_COMMIT: &str = "30d8f071";

const SCHEMA_INDEX: &str = "libbpf-mcu.artifacts.v1";
const SCHEMA_PROGRAM: &str = "libbpf-mcu.program.v1";
const SCHEMA_REPORT: &str = "libbpf-mcu.report.v1";

const ROLES: &[&str] = &[
    "bpf_object",
    "aot_object",
    "debug_elf",
    "image",
    "xpost_container",
];

const FORMATS: &[&str] = &[
    "elf-bpf-relocatable",
    "elf-riscv-relocatable",
    "elf-riscv-linked",
    "raw-rv32-image",
    "raw-ebpf-image",
    "xpost-compact-elf-v0",
];

/// XPOST mode that has an XDP container.
///
/// `hal` is not a variant. Firmware flash keeps using `fw.artifact`.
/// `bpf` is not a variant. `<program>_bpf.o` is not a container.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum XdpMode {
    /// Producer mode `rv32`. Primary file `<program>_rv32.elf`.
    Aot,
    /// Producer mode `jit`. Primary file `<program>_jit.elf`.
    Jit,
    /// Producer mode `bpf-xpost`. Primary file `<program>_bpf-xpost.elf`.
    Rbpf,
}

impl XdpMode {
    /// Database spelling already used by `xpost-test/manifest` `Mode::as_db`.
    pub const fn as_db(self) -> &'static str {
        match self {
            Self::Aot => "aot",
            Self::Jit => "jit",
            Self::Rbpf => "rbpf",
        }
    }

    fn producer(self) -> ProducerMode {
        match self {
            Self::Aot => ProducerMode::Rv32,
            Self::Jit => ProducerMode::Jit,
            Self::Rbpf => ProducerMode::BpfXpost,
        }
    }
}

/// Private producer spellings from `MODE_SPECS` at the pin. Not a public API.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProducerMode {
    Rv32,
    Jit,
    BpfXpost,
}

impl ProducerMode {
    const fn as_manifest(self) -> &'static str {
        match self {
            Self::Rv32 => "rv32",
            Self::Jit => "jit",
            Self::BpfXpost => "bpf-xpost",
        }
    }

    /// `MODE_SPECS` primary `xpost_container` file name. Not a directory scan.
    fn container_file_name(self, program: &str) -> String {
        match self {
            Self::Rv32 => format!("{program}_rv32.elf"),
            Self::Jit => format!("{program}_jit.elf"),
            Self::BpfXpost => format!("{program}_bpf-xpost.elf"),
        }
    }
}

/// One container the caller may hand to the existing serial flash.
///
/// The embedded name already matches `profile_name`. The bytes already match
/// the report hash and size. The caller does not open TOML.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FlashImage {
    path: PathBuf,
    len: u64,
    sha256: [u8; 32],
    profile_name: String,
}

impl FlashImage {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn sha256(&self) -> &[u8; 32] {
        &self.sha256
    }

    pub fn profile_name(&self) -> &str {
        &self.profile_name
    }
}

/// Why this program and mode must not be flashed.
#[derive(Debug)]
pub enum ResolveError {
    Unreadable {
        path: PathBuf,
    },
    SchemaMismatch {
        path: PathBuf,
    },
    ProgramNotInIndex {
        program: String,
    },
    /// `index.toml` programs were not sorted by name.
    IndexUnsorted,
    ManifestHashMismatch,
    /// No consumable `[[mode]]` row. Stale or failed does not count.
    ModeNotConsumable {
        program: String,
        mode: XdpMode,
    },
    ReportHashMismatch,
    ReportNotSuccess {
        status: String,
    },
    /// Hash, size, role, or format disagreed with the file beside the report.
    ArtifactMismatch {
        rel: String,
    },
    /// `success` report whose `verification.status` is not `accepted`.
    VerificationNotAccepted {
        status: String,
    },
    /// No single primary `xpost_container` in format `xpost-compact-elf-v0`.
    ContainerMissing,
    NameMismatch {
        profile: String,
        embedded: String,
    },
}

impl std::fmt::Display for ResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreadable { path } => write!(f, "unreadable {}", path.display()),
            Self::SchemaMismatch { path } => write!(f, "schema mismatch {}", path.display()),
            Self::ProgramNotInIndex { program } => {
                write!(f, "program `{program}` is not in the index")
            }
            Self::IndexUnsorted => write!(f, "index is not sorted by program"),
            Self::ManifestHashMismatch => write!(f, "manifest hash mismatch"),
            Self::ModeNotConsumable { program, mode } => write!(
                f,
                "{program} has no consumable {} mode",
                mode.producer().as_manifest()
            ),
            Self::ReportHashMismatch => write!(f, "report hash mismatch"),
            Self::ReportNotSuccess { status } => {
                write!(f, "report status is `{status}`, not success")
            }
            Self::ArtifactMismatch { rel } => {
                write!(f, "artifact `{rel}` hash, size, role, or format mismatch")
            }
            Self::VerificationNotAccepted { status } => {
                write!(f, "verification status is `{status}`, not accepted")
            }
            Self::ContainerMissing => {
                write!(f, "no single primary xpost_container")
            }
            Self::NameMismatch { profile, embedded } => {
                write!(f, "embedded name `{embedded}` != profile `{profile}`")
            }
        }
    }
}

impl std::error::Error for ResolveError {}

/// Resolve the one container for `program` and `mode` under `build_root`.
///
/// `profile_name` is the xdp profile name, not the C stem. `case_mark_idle`
/// passes program `case_mark` and profile name `case_mark_idle`.
/// `--image-name` is the build command's job. This function only reads.
///
/// Refuses a missing file, a hash or size mismatch, a schema mismatch, a
/// status other than `success`, a mode that is only stale or failed, a
/// verification that is not `accepted`, a file that is not the primary
/// `xpost_container`, and an embedded name other than `profile_name`.
pub fn resolve_flash_image(
    build_root: &Path,
    program: &str,
    mode: XdpMode,
    profile_name: &str,
) -> Result<FlashImage, ResolveError> {
    let index_path = build_root.join("index.toml");
    let index_bytes = read_bytes(&index_path)?;
    let index = parse_doc(&index_bytes, &index_path)?;
    require_schema(&index, SCHEMA_INDEX, &index_path)?;

    let programs = tables(&index, "program", &index_path)?;
    let mut names = Vec::with_capacity(programs.len());
    for row in &programs {
        names.push(req_str(row, "name", &index_path)?.to_owned());
    }
    let mut sorted = names.clone();
    sorted.sort();
    if names != sorted {
        return Err(ResolveError::IndexUnsorted);
    }

    let program_row = programs
        .into_iter()
        .find(|row| row.get("name").and_then(toml::Value::as_str) == Some(program))
        .ok_or_else(|| ResolveError::ProgramNotInIndex {
            program: program.to_owned(),
        })?;
    let manifest_rel = req_str(program_row, "manifest", &index_path)?;
    let manifest_sha = req_str(program_row, "manifest_sha256", &index_path)?;
    let manifest_path =
        rel_join(build_root, manifest_rel).map_err(|_| ResolveError::Unreadable {
            path: PathBuf::from(manifest_rel),
        })?;
    let manifest_bytes = read_bytes(&manifest_path)?;
    if hex_sha256(&manifest_bytes) != manifest_sha {
        return Err(ResolveError::ManifestHashMismatch);
    }

    let manifest = parse_doc(&manifest_bytes, &manifest_path)?;
    require_schema(&manifest, SCHEMA_PROGRAM, &manifest_path)?;
    let producer = mode.producer();
    let mode_rows = tables(&manifest, "mode", &manifest_path)?;
    let mode_row = mode_rows
        .into_iter()
        .find(|row| row.get("name").and_then(toml::Value::as_str) == Some(producer.as_manifest()))
        .ok_or_else(|| ResolveError::ModeNotConsumable {
            program: program.to_owned(),
            mode,
        })?;
    let report_rel = req_str(mode_row, "report", &manifest_path)?;
    let report_sha = req_str(mode_row, "report_sha256", &manifest_path)?;
    let report_base = manifest_path
        .parent()
        .ok_or_else(|| ResolveError::Unreadable {
            path: manifest_path.clone(),
        })?;
    let report_path = rel_join(report_base, report_rel).map_err(|_| ResolveError::Unreadable {
        path: PathBuf::from(report_rel),
    })?;
    let report_bytes = read_bytes(&report_path)?;
    if hex_sha256(&report_bytes) != report_sha {
        return Err(ResolveError::ReportHashMismatch);
    }

    let report = parse_doc(&report_bytes, &report_path)?;
    require_schema(&report, SCHEMA_REPORT, &report_path)?;
    let status = req_str(&report, "status", &report_path)?;
    if status != "success" {
        return Err(ResolveError::ReportNotSuccess {
            status: status.to_owned(),
        });
    }

    let expected_file = producer.container_file_name(program);
    let artifacts = tables(&report, "artifact", &report_path)?;
    let mut checked = Vec::with_capacity(artifacts.len());
    for art in artifacts {
        checked.push(check_artifact(art, &report_path)?);
    }

    let verification = match report.get("verification") {
        Some(table) => req_str(table, "status", &report_path)?,
        None => "missing",
    };
    if verification != "accepted" {
        return Err(ResolveError::VerificationNotAccepted {
            status: verification.to_owned(),
        });
    }

    let image = select_container(&checked, &expected_file)?;
    let embedded = embedded_name(&image.bytes).map_err(|_| ResolveError::Unreadable {
        path: image.path.clone(),
    })?;
    if embedded != profile_name {
        return Err(ResolveError::NameMismatch {
            profile: profile_name.to_owned(),
            embedded,
        });
    }

    Ok(FlashImage {
        path: absolute(&image.path)?,
        len: image.len,
        sha256: image.sha256,
        profile_name: profile_name.to_owned(),
    })
}

struct CheckedArtifact {
    rel: String,
    role: String,
    format: String,
    primary: bool,
    path: PathBuf,
    len: u64,
    sha256: [u8; 32],
    bytes: Vec<u8>,
}

fn check_artifact(art: &toml::Value, report_path: &Path) -> Result<CheckedArtifact, ResolveError> {
    let rel = req_str(art, "path", report_path)?.to_owned();
    let role = req_str(art, "role", report_path)?;
    let format = req_str(art, "format", report_path)?;
    let sha = req_str(art, "sha256", report_path)?;
    let size = req_u64(art, "size_bytes", report_path)?;
    let primary = req_bool(art, "primary", report_path)?;
    if !ROLES.contains(&role) || !FORMATS.contains(&format) {
        return Err(ResolveError::ArtifactMismatch { rel });
    }
    let base = report_path
        .parent()
        .ok_or_else(|| ResolveError::Unreadable {
            path: report_path.to_path_buf(),
        })?;
    let path =
        rel_join(base, &rel).map_err(|_| ResolveError::ArtifactMismatch { rel: rel.clone() })?;
    let bytes = read_bytes(&path)?;
    let digest = sha256_bytes(&bytes);
    if hex_of(&digest) != sha || bytes.len() as u64 != size {
        return Err(ResolveError::ArtifactMismatch { rel: rel.clone() });
    }
    Ok(CheckedArtifact {
        rel,
        role: role.to_owned(),
        format: format.to_owned(),
        primary,
        path,
        len: bytes.len() as u64,
        sha256: digest,
        bytes,
    })
}

/// Exactly one primary container, and no primary on any other role or file.
fn select_container<'a>(
    artifacts: &'a [CheckedArtifact],
    expected_file: &str,
) -> Result<&'a CheckedArtifact, ResolveError> {
    let mut chosen = None;
    for art in artifacts {
        if !art.primary {
            continue;
        }
        let is_container = art.role == "xpost_container"
            && art.format == "xpost-compact-elf-v0"
            && art.rel == expected_file;
        if !is_container {
            return Err(ResolveError::ContainerMissing);
        }
        if chosen.is_some() {
            return Err(ResolveError::ContainerMissing);
        }
        chosen = Some(art);
    }
    chosen.ok_or(ResolveError::ContainerMissing)
}

fn embedded_name(bytes: &[u8]) -> Result<String, ()> {
    let mut src = bytes;
    match xpost_elf::scan_at(&mut src, 0) {
        xpost_elf::ScanOutcome::Ok(container) => Ok(container.name.as_str().to_owned()),
        xpost_elf::ScanOutcome::SoftSkip { .. } | xpost_elf::ScanOutcome::HardFail => Err(()),
    }
}

fn read_bytes(path: &Path) -> Result<Vec<u8>, ResolveError> {
    std::fs::read(path).map_err(|_| ResolveError::Unreadable {
        path: path.to_path_buf(),
    })
}

fn parse_doc(bytes: &[u8], path: &Path) -> Result<toml::Value, ResolveError> {
    let text = std::str::from_utf8(bytes).map_err(|_| ResolveError::SchemaMismatch {
        path: path.to_path_buf(),
    })?;
    toml::from_str(text).map_err(|_| ResolveError::SchemaMismatch {
        path: path.to_path_buf(),
    })
}

fn require_schema(doc: &toml::Value, expected: &str, path: &Path) -> Result<(), ResolveError> {
    match doc.get("schema").and_then(toml::Value::as_str) {
        Some(schema) if schema == expected => Ok(()),
        _ => Err(ResolveError::SchemaMismatch {
            path: path.to_path_buf(),
        }),
    }
}

fn tables<'a>(
    doc: &'a toml::Value,
    key: &str,
    path: &Path,
) -> Result<Vec<&'a toml::Value>, ResolveError> {
    match doc.get(key) {
        None => Ok(Vec::new()),
        Some(toml::Value::Array(items)) if items.iter().all(toml::Value::is_table) => {
            Ok(items.iter().collect())
        }
        Some(_) => Err(ResolveError::SchemaMismatch {
            path: path.to_path_buf(),
        }),
    }
}

fn req_str<'a>(doc: &'a toml::Value, key: &str, path: &Path) -> Result<&'a str, ResolveError> {
    doc.get(key)
        .and_then(toml::Value::as_str)
        .ok_or_else(|| ResolveError::SchemaMismatch {
            path: path.to_path_buf(),
        })
}

fn req_bool(doc: &toml::Value, key: &str, path: &Path) -> Result<bool, ResolveError> {
    doc.get(key)
        .and_then(toml::Value::as_bool)
        .ok_or_else(|| ResolveError::SchemaMismatch {
            path: path.to_path_buf(),
        })
}

fn req_u64(doc: &toml::Value, key: &str, path: &Path) -> Result<u64, ResolveError> {
    let Some(value) = doc.get(key).and_then(toml::Value::as_integer) else {
        return Err(ResolveError::SchemaMismatch {
            path: path.to_path_buf(),
        });
    };
    u64::try_from(value).map_err(|_| ResolveError::ArtifactMismatch {
        rel: req_str(doc, "path", path).unwrap_or("").to_owned(),
    })
}

fn rel_join(base: &Path, rel: &str) -> Result<PathBuf, ()> {
    if rel.is_empty() || rel.starts_with('/') || rel.starts_with('\\') {
        return Err(());
    }
    let path = Path::new(rel);
    if path.is_absolute() {
        return Err(());
    }
    if path.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return Err(());
    }
    Ok(base.join(path))
}

fn absolute(path: &Path) -> Result<PathBuf, ResolveError> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    let cwd = std::env::current_dir().map_err(|_| ResolveError::Unreadable {
        path: path.to_path_buf(),
    })?;
    Ok(cwd.join(path))
}

fn sha256_bytes(bytes: &[u8]) -> [u8; 32] {
    let digest = Sha256::digest(bytes);
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    out
}

fn hex_of(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0xf) as usize] as char);
    }
    out
}

fn hex_sha256(bytes: &[u8]) -> String {
    hex_of(&sha256_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("xpost-artifact-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn write(path: &Path, bytes: &[u8]) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, bytes).unwrap();
    }

    fn container(embedded: &str) -> Vec<u8> {
        let meta = xpost_elf::ImageMetadataV0 {
            slot_address: 0x4038_0000,
            helper_address: 0x4038_1000,
            machine: 1,
            name: heapless::String::try_from(embedded).unwrap(),
            maps: heapless::Vec::new(),
        };
        let data = xpost_elf::build_container(b"\x01\x00\x82\x80\xff", &meta, None).unwrap();
        data.as_slice().to_vec()
    }

    struct Art {
        rel: String,
        role: &'static str,
        format: &'static str,
        bytes: Vec<u8>,
        primary: bool,
        sha_override: Option<String>,
        write_file: bool,
    }

    struct Mode {
        producer: &'static str,
        status: &'static str,
        verification: Option<&'static str>,
        artifacts: Vec<Art>,
        write_report: bool,
        /// `mode`, `stale`, or `failed`. Only `mode` is consumable.
        table: &'static str,
    }

    struct Prog {
        name: &'static str,
        modes: Vec<Mode>,
        manifest_sha_ok: bool,
    }

    fn publish(root: &Path, programs: &[Prog]) {
        let mut index = String::from("schema = \"libbpf-mcu.artifacts.v1\"\n\n");
        for prog in programs {
            let dir = root.join(prog.name);
            let mut manifest = String::from("schema = \"libbpf-mcu.program.v1\"\n\n");
            for mode in &prog.modes {
                let mut body = String::new();
                for art in &mode.artifacts {
                    let path = dir.join(mode.producer).join(art.rel.as_str());
                    if art.write_file {
                        write(&path, &art.bytes);
                    }
                    let sha = art
                        .sha_override
                        .clone()
                        .unwrap_or_else(|| hex_sha256(&art.bytes));
                    body.push_str(&format!(
                        "[[artifact]]\nrole = \"{}\"\npath = \"{}\"\nformat = \"{}\"\nsha256 = \"{}\"\nsize_bytes = {}\nprimary = {}\n\n",
                        art.role,
                        art.rel,
                        art.format,
                        sha,
                        art.bytes.len(),
                        art.primary
                    ));
                }
                let mut report = format!(
                    "schema = \"libbpf-mcu.report.v1\"\nstatus = \"{}\"\n\n",
                    mode.status
                );
                if let Some(status) = mode.verification {
                    report.push_str(&format!("[verification]\nstatus = \"{status}\"\n\n"));
                }
                report.push_str(&body);
                let rel = format!("{}/report.toml", mode.producer);
                if mode.write_report {
                    write(&dir.join(&rel), report.as_bytes());
                }
                manifest.push_str(&format!(
                    "[[{table}]]\nname = \"{name}\"\nreport = \"{rel}\"\nreport_sha256 = \"{sha}\"\n\n",
                    table = mode.table,
                    name = mode.producer,
                    sha = hex_sha256(report.as_bytes()),
                ));
            }
            write(&dir.join("manifest.toml"), manifest.as_bytes());
            let sha = if prog.manifest_sha_ok {
                hex_sha256(manifest.as_bytes())
            } else {
                "0".repeat(64)
            };
            index.push_str(&format!(
                "[[program]]\nname = \"{name}\"\nmanifest = \"{name}/manifest.toml\"\nmanifest_sha256 = \"{sha}\"\n\n",
                name = prog.name,
            ));
        }
        write(&root.join("index.toml"), index.as_bytes());
    }

    fn legal_mode(program: &str, producer: &'static str, embedded: &str) -> Mode {
        let file = match producer {
            "rv32" => format!("{program}_rv32.elf"),
            "jit" => format!("{program}_jit.elf"),
            "bpf-xpost" => format!("{program}_bpf-xpost.elf"),
            other => panic!("bad producer {other}"),
        };
        Mode {
            producer,
            status: "success",
            verification: Some("accepted"),
            artifacts: vec![
                Art {
                    rel: file,
                    role: "xpost_container",
                    format: "xpost-compact-elf-v0",
                    bytes: container(embedded),
                    primary: true,
                    sha_override: None,
                    write_file: true,
                },
                Art {
                    rel: "side_bpf.o".to_owned(),
                    role: "bpf_object",
                    format: "elf-bpf-relocatable",
                    bytes: b"not-a-container".to_vec(),
                    primary: false,
                    sha_override: None,
                    write_file: true,
                },
            ],
            write_report: true,
            table: "mode",
        }
    }

    fn prog(name: &'static str, modes: Vec<Mode>) -> Prog {
        Prog {
            name,
            modes,
            manifest_sha_ok: true,
        }
    }

    #[test]
    fn legal_containers_for_each_mode() {
        let cases = [
            (XdpMode::Aot, "rv32", "demo_rv32.elf"),
            (XdpMode::Jit, "jit", "demo_jit.elf"),
            (XdpMode::Rbpf, "bpf-xpost", "demo_bpf-xpost.elf"),
        ];
        let root = scratch("legal");
        publish(
            &root,
            &[prog(
                "demo",
                cases
                    .iter()
                    .map(|(_, producer, _)| legal_mode("demo", producer, "demo"))
                    .collect(),
            )],
        );
        for (mode, _, file) in cases {
            let image = resolve_flash_image(&root, "demo", mode, "demo").unwrap();
            assert_eq!(image.profile_name(), "demo");
            assert_eq!(image.path().file_name().unwrap(), file);
            assert!(image.path().is_absolute());
            let bytes = std::fs::read(image.path()).unwrap();
            assert_eq!(image.len(), bytes.len() as u64);
            assert_eq!(image.sha256(), &sha256_bytes(&bytes));
            assert!(!image.path().ends_with("side_bpf.o"));
        }
    }

    #[test]
    fn missing_report() {
        let root = scratch("missing-report");
        let mut mode = legal_mode("demo", "rv32", "demo");
        mode.write_report = false;
        publish(&root, &[prog("demo", vec![mode])]);
        let err = resolve_flash_image(&root, "demo", XdpMode::Aot, "demo").unwrap_err();
        assert!(
            matches!(err, ResolveError::Unreadable { ref path } if path.ends_with("report.toml")),
            "{err}"
        );
    }

    #[test]
    fn missing_artifact_file() {
        let root = scratch("missing-artifact");
        let mut mode = legal_mode("demo", "rv32", "demo");
        mode.artifacts[0].write_file = false;
        publish(&root, &[prog("demo", vec![mode])]);
        let err = resolve_flash_image(&root, "demo", XdpMode::Aot, "demo").unwrap_err();
        assert!(
            matches!(err, ResolveError::Unreadable { ref path } if path.ends_with("demo_rv32.elf")),
            "{err}"
        );
    }

    #[test]
    fn artifact_hash_mismatch() {
        let root = scratch("hash-mismatch");
        let mut mode = legal_mode("demo", "rv32", "demo");
        mode.artifacts[0].sha_override = Some("0".repeat(64));
        publish(&root, &[prog("demo", vec![mode])]);
        let err = resolve_flash_image(&root, "demo", XdpMode::Aot, "demo").unwrap_err();
        assert!(
            matches!(err, ResolveError::ArtifactMismatch { ref rel } if rel == "demo_rv32.elf"),
            "{err}"
        );
    }

    #[test]
    fn mode_only_stale_or_failed_is_not_consumable() {
        let root = scratch("not-consumable");
        let mut stale = legal_mode("demo", "rv32", "demo");
        stale.table = "stale";
        let mut failed = legal_mode("demo", "jit", "demo");
        failed.table = "failed";
        publish(&root, &[prog("demo", vec![stale, failed])]);
        let aot = resolve_flash_image(&root, "demo", XdpMode::Aot, "demo").unwrap_err();
        assert!(
            matches!(
                aot,
                ResolveError::ModeNotConsumable {
                    mode: XdpMode::Aot,
                    ..
                }
            ),
            "{aot}"
        );
        let jit = resolve_flash_image(&root, "demo", XdpMode::Jit, "demo").unwrap_err();
        assert!(
            matches!(
                jit,
                ResolveError::ModeNotConsumable {
                    mode: XdpMode::Jit,
                    ..
                }
            ),
            "{jit}"
        );
    }

    #[test]
    fn program_not_in_index() {
        let root = scratch("missing-program");
        publish(
            &root,
            &[prog("other", vec![legal_mode("other", "rv32", "other")])],
        );
        let err = resolve_flash_image(&root, "demo", XdpMode::Aot, "demo").unwrap_err();
        assert!(
            matches!(err, ResolveError::ProgramNotInIndex { ref program } if program == "demo"),
            "{err}"
        );
    }

    #[test]
    fn report_status_failed() {
        let root = scratch("status-failed");
        let mut mode = legal_mode("demo", "rv32", "demo");
        mode.status = "failed";
        publish(&root, &[prog("demo", vec![mode])]);
        let err = resolve_flash_image(&root, "demo", XdpMode::Aot, "demo").unwrap_err();
        assert!(
            matches!(err, ResolveError::ReportNotSuccess { ref status } if status == "failed"),
            "{err}"
        );
    }

    #[test]
    fn verification_rejected_while_success() {
        let root = scratch("verification-rejected");
        let mut mode = legal_mode("demo", "rv32", "demo");
        mode.verification = Some("rejected");
        publish(&root, &[prog("demo", vec![mode])]);
        let err = resolve_flash_image(&root, "demo", XdpMode::Aot, "demo").unwrap_err();
        assert!(
            matches!(err, ResolveError::VerificationNotAccepted { ref status } if status == "rejected"),
            "{err}"
        );
    }

    #[test]
    fn index_manifest_hash_disagrees() {
        let root = scratch("manifest-hash");
        let mut program = prog("demo", vec![legal_mode("demo", "rv32", "demo")]);
        program.manifest_sha_ok = false;
        publish(&root, &[program]);
        let err = resolve_flash_image(&root, "demo", XdpMode::Aot, "demo").unwrap_err();
        assert!(matches!(err, ResolveError::ManifestHashMismatch), "{err}");
    }

    #[test]
    fn raw_bpf_primary_is_not_a_container() {
        let root = scratch("raw-bpf");
        let modes = ["rv32", "jit", "bpf-xpost"]
            .into_iter()
            .map(|producer| {
                let mut mode = legal_mode("demo", producer, "demo");
                mode.artifacts[0].primary = true;
                mode.artifacts[1].primary = true;
                mode
            })
            .collect();
        publish(&root, &[prog("demo", modes)]);
        for mode in [XdpMode::Aot, XdpMode::Jit, XdpMode::Rbpf] {
            let err = resolve_flash_image(&root, "demo", mode, "demo").unwrap_err();
            assert!(
                matches!(err, ResolveError::ContainerMissing),
                "{mode:?} {err}"
            );
        }
    }

    #[test]
    fn embedded_name_must_match_profile() {
        let root = scratch("name-mismatch");
        publish(
            &root,
            &[prog(
                "case_mark",
                vec![legal_mode("case_mark", "rv32", "case_mark")],
            )],
        );
        let err =
            resolve_flash_image(&root, "case_mark", XdpMode::Aot, "case_mark_idle").unwrap_err();
        assert!(
            matches!(
                err,
                ResolveError::NameMismatch { ref profile, ref embedded }
                    if profile == "case_mark_idle" && embedded == "case_mark"
            ),
            "{err}"
        );
    }

    #[test]
    fn two_programs_do_not_share_paths() {
        let root = scratch("two-programs");
        publish(
            &root,
            &[
                prog("alpha", vec![legal_mode("alpha", "rv32", "alpha")]),
                prog("zed", vec![legal_mode("zed", "rv32", "zed")]),
            ],
        );
        let alpha = resolve_flash_image(&root, "alpha", XdpMode::Aot, "alpha").unwrap();
        let zed = resolve_flash_image(&root, "zed", XdpMode::Aot, "zed").unwrap();
        assert_ne!(alpha.path(), zed.path());
        assert_eq!(alpha.path().file_name().unwrap(), "alpha_rv32.elf");
        assert_eq!(zed.path().file_name().unwrap(), "zed_rv32.elf");
        assert!(!alpha.path().ends_with("zed_rv32.elf"));
        assert!(!zed.path().ends_with("alpha_rv32.elf"));
    }

    #[test]
    fn index_not_sorted() {
        let root = scratch("unsorted");
        publish(
            &root,
            &[
                prog("zed", vec![legal_mode("zed", "rv32", "zed")]),
                prog("alpha", vec![legal_mode("alpha", "rv32", "alpha")]),
            ],
        );
        let err = resolve_flash_image(&root, "alpha", XdpMode::Aot, "alpha").unwrap_err();
        assert!(matches!(err, ResolveError::IndexUnsorted), "{err}");
    }
}
