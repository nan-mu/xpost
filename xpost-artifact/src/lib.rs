//! Flash resolution for one program and one XDP mode.
//!
//! Pin: libbpf-mcu commit [`CONTRACT_COMMIT`], pull request 4, provisional
//! until merge. Do not add schema fields. Do not treat this pin as a frozen
//! format.
//!
//! The crate reads `index.toml`, `<program>/manifest.toml`, and
//! `<program>/<mode>/report.toml`. It does not read `profiles.toml`.
//! It does not flash, and it does not talk to the serial worker.

use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

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
    FirmwareMismatch,
    CacheInvalid {
        path: PathBuf,
    },
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
            Self::FirmwareMismatch => write!(f, "firmware SHA-256 mismatch"),
            Self::CacheInvalid { path } => write!(f, "invalid cache entry {}", path.display()),
            Self::NameMismatch { profile, embedded } => {
                write!(
                    f,
                    "embedded name `{embedded}` != profile `{profile}`; generate the requested profile via libbpf-mcu"
                )
            }
        }
    }
}

impl std::error::Error for ResolveError {}

/// Resolve the one container for `program` and `mode` under `build_root`.
///
/// `profile_name` is the xdp profile name, not the C stem. `case_mark_idle`
/// passes program `case_mark` and profile name `case_mark_idle`.
/// `--image-name` is the build command's job. Validated containers are copied
/// to `cache_root` and bound to `expected_firmware_sha256`.
///
/// Refuses a missing file, a hash or size mismatch, a schema mismatch, a
/// status other than `success`, a mode that is only stale or failed, a
/// verification that is not `accepted`, a file that is not the primary
/// `xpost_container`, a firmware mismatch, and invalid cache entries. A different
/// embedded name can use a previously cached image for the requested profile.
pub fn resolve_flash_image(
    build_root: &Path,
    cache_root: &Path,
    program: &str,
    mode: XdpMode,
    profile_name: &str,
    expected_firmware_sha256: [u8; 32],
) -> Result<FlashImage, ResolveError> {
    let build_root = std::fs::canonicalize(build_root).map_err(|_| ResolveError::Unreadable {
        path: build_root.to_path_buf(),
    })?;
    if !valid_segment(program) || !valid_segment(profile_name) {
        return Err(ResolveError::Unreadable {
            path: PathBuf::from("invalid program or profile name"),
        });
    }
    let index_path = confined(&build_root, build_root.join("index.toml"))?;
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
        rel_join(&build_root, manifest_rel).map_err(|_| ResolveError::Unreadable {
            path: PathBuf::from(manifest_rel),
        })?;
    let manifest_path = confined(&build_root, manifest_path)?;
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
    let report_path = confined(&build_root, report_path)?;
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

    let firmware = report
        .get("firmware")
        .ok_or_else(|| ResolveError::SchemaMismatch {
            path: report_path.clone(),
        })?;
    let firmware_sha = req_str(firmware, "sha256", &report_path)?;
    if firmware_sha != hex_of(&expected_firmware_sha256) {
        return Err(ResolveError::FirmwareMismatch);
    }

    let artifacts = tables(&report, "artifact", &report_path)?;
    let mut checked = Vec::with_capacity(artifacts.len());
    for art in artifacts {
        checked.push(check_artifact(art, &report_path, &build_root)?);
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

    let image = select_container(&checked)?;
    let embedded = embedded_name(&image.bytes).map_err(|_| ResolveError::Unreadable {
        path: image.path.clone(),
    })?;
    if !valid_segment(&embedded) {
        return Err(ResolveError::Unreadable {
            path: image.path.clone(),
        });
    }
    let cache = cache_dir(cache_root, program, mode, profile_name)?;
    let identity = CacheIdentity {
        program,
        mode: mode.as_db(),
        profile: profile_name,
        firmware: firmware_sha,
        manifest: manifest_sha,
        report: report_sha,
    };
    if embedded != profile_name {
        let mut candidates = Vec::new();
        for entry in std::fs::read_dir(&cache).map_err(|_| ResolveError::Unreadable {
            path: cache.clone(),
        })? {
            let path = entry
                .map_err(|_| ResolveError::Unreadable {
                    path: cache.clone(),
                })?
                .path();
            if path.extension().and_then(|s| s.to_str()) == Some("toml") {
                let (result, fw) = read_cache(&cache, &path, &identity, false)?;
                if fw == firmware_sha {
                    candidates.push(result);
                }
            }
        }
        candidates.sort_by(|a, b| a.path.cmp(&b.path));
        if candidates.len() > 1 {
            return Err(ResolveError::CacheInvalid { path: cache });
        }
        return candidates
            .into_iter()
            .next()
            .ok_or_else(|| ResolveError::NameMismatch {
                profile: profile_name.to_owned(),
                embedded,
            });
    }
    let digest = hex_of(&image.sha256);
    let meta_path = cache.join(format!("{digest}.toml"));
    let elf_path = cache.join(format!("{digest}.elf"));
    if meta_path.symlink_metadata().is_ok() {
        // Validate the existing snapshot before refreshing its provenance.
        // A verified republication may bind identical bytes to a new firmware
        // or update the manifest without changing the container digest.
        read_cache(&cache, &meta_path, &identity, false)?;
    }
    // An uncommitted artifact can be left by an interrupted write. Verify it
    // before completing the metadata commit; never overwrite existing bytes.
    if elf_path.symlink_metadata().is_ok() {
        let real = confined(&cache, elf_path.clone())?;
        if read_bytes(&real)? != image.bytes {
            return Err(ResolveError::CacheInvalid { path: elf_path });
        }
    } else {
        atomic_write(&elf_path, &image.bytes)?;
    }
    let mut meta = toml::map::Map::new();
    for (key, value) in [
        ("program", program),
        ("mode", mode.as_db()),
        ("profile_name", profile_name),
        ("artifact_sha256", digest.as_str()),
        ("manifest_sha256", manifest_sha),
        ("report_sha256", report_sha),
        ("firmware_sha256", firmware_sha),
    ] {
        meta.insert(key.to_owned(), toml::Value::String(value.to_owned()));
    }
    meta.insert("size_bytes".into(), toml::Value::Integer(image.len as i64));
    let bytes =
        toml::to_string(&toml::Value::Table(meta)).map_err(|_| ResolveError::CacheInvalid {
            path: meta_path.clone(),
        })?;
    atomic_write(&meta_path, bytes.as_bytes())?;
    let (cached, _) = read_cache(&cache, &meta_path, &identity, true)?;
    Ok(cached)
}

struct CacheIdentity<'a> {
    program: &'a str,
    mode: &'a str,
    profile: &'a str,
    firmware: &'a str,
    manifest: &'a str,
    report: &'a str,
}

fn cache_dir(
    root: &Path,
    program: &str,
    mode: XdpMode,
    profile: &str,
) -> Result<PathBuf, ResolveError> {
    std::fs::create_dir_all(root).map_err(|_| ResolveError::Unreadable {
        path: root.to_path_buf(),
    })?;
    let root = std::fs::canonicalize(root).map_err(|_| ResolveError::Unreadable {
        path: root.to_path_buf(),
    })?;
    let mut dir = root.clone();
    for segment in [program, mode.as_db(), profile] {
        dir.push(segment);
        match std::fs::create_dir(&dir) {
            Ok(()) => (),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(_) => return Err(ResolveError::Unreadable { path: dir }),
        }
        let real = std::fs::canonicalize(&dir)
            .map_err(|_| ResolveError::Unreadable { path: dir.clone() })?;
        if !real.starts_with(&root) || !real.is_dir() {
            return Err(ResolveError::Unreadable { path: dir });
        }
        dir = real;
    }
    Ok(dir)
}

fn read_cache(
    dir: &Path,
    meta_path: &Path,
    id: &CacheIdentity<'_>,
    exact_source: bool,
) -> Result<(FlashImage, String), ResolveError> {
    let invalid = || ResolveError::CacheInvalid {
        path: meta_path.to_path_buf(),
    };
    let real = confined(dir, meta_path.to_path_buf()).map_err(|_| invalid())?;
    let doc =
        parse_doc(&read_bytes(&real).map_err(|_| invalid())?, &real).map_err(|_| invalid())?;
    let get = |key| req_str(&doc, key, &real).map_err(|_| invalid());
    let digest = get("artifact_sha256")?;
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        || meta_path.file_name().and_then(|s| s.to_str()) != Some(&format!("{digest}.toml"))
        || get("program")? != id.program
        || get("mode")? != id.mode
        || get("profile_name")? != id.profile
    {
        return Err(invalid());
    }
    let firmware = get("firmware_sha256")?;
    let valid_hash = |v: &str| {
        v.len() == 64
            && v.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    };
    let manifest = get("manifest_sha256")?;
    let report = get("report_sha256")?;
    if !valid_hash(firmware)
        || !valid_hash(manifest)
        || !valid_hash(report)
        || (exact_source
            && (firmware != id.firmware || manifest != id.manifest || report != id.report))
    {
        return Err(invalid());
    }
    let size = req_u64(&doc, "size_bytes", &real).map_err(|_| invalid())?;
    let path = confined(dir, dir.join(format!("{digest}.elf"))).map_err(|_| invalid())?;
    let bytes = read_bytes(&path).map_err(|_| invalid())?;
    let sha = sha256_bytes(&bytes);
    if hex_of(&sha) != digest
        || bytes.len() as u64 != size
        || embedded_name(&bytes).ok().as_deref() != Some(id.profile)
    {
        return Err(invalid());
    }
    Ok((
        FlashImage {
            path,
            len: size,
            sha256: sha,
            profile_name: id.profile.to_owned(),
        },
        firmware.to_owned(),
    ))
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), ResolveError> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let parent = path.parent().ok_or_else(|| ResolveError::Unreadable {
        path: path.to_path_buf(),
    })?;
    loop {
        let tmp = parent.join(format!(
            ".tmp-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let mut file = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
        {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => return Err(ResolveError::Unreadable { path: tmp }),
        };
        let result = file
            .write_all(bytes)
            .and_then(|_| file.sync_all())
            .and_then(|_| std::fs::rename(&tmp, path));
        if result.is_err() {
            let _ = std::fs::remove_file(&tmp);
            return Err(ResolveError::Unreadable {
                path: path.to_path_buf(),
            });
        }
        return Ok(());
    }
}

struct CheckedArtifact {
    role: String,
    format: String,
    primary: bool,
    path: PathBuf,
    len: u64,
    sha256: [u8; 32],
    bytes: Vec<u8>,
}

fn check_artifact(
    art: &toml::Value,
    report_path: &Path,
    build_root: &Path,
) -> Result<CheckedArtifact, ResolveError> {
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
    let path = confined(build_root, path)?;
    let bytes = read_bytes(&path)?;
    let digest = sha256_bytes(&bytes);
    if hex_of(&digest) != sha || bytes.len() as u64 != size {
        return Err(ResolveError::ArtifactMismatch { rel: rel.clone() });
    }
    Ok(CheckedArtifact {
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
) -> Result<&'a CheckedArtifact, ResolveError> {
    let mut chosen = None;
    for art in artifacts {
        if !art.primary {
            continue;
        }
        let is_container = art.role == "xpost_container" && art.format == "xpost-compact-elf-v0";
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
    if rel.is_empty() || rel.contains('\\') || rel.contains(':') {
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

fn valid_segment(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && !value
            .chars()
            .any(|c| c == '/' || c == '\\' || c == ':' || c.is_control())
}

fn confined(root: &Path, path: PathBuf) -> Result<PathBuf, ResolveError> {
    let real = std::fs::canonicalize(&path)
        .map_err(|_| ResolveError::Unreadable { path: path.clone() })?;
    if !real.starts_with(root) || !real.is_file() {
        return Err(ResolveError::Unreadable { path });
    }
    Ok(real)
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
    const FW: [u8; 32] = [42; 32];
    fn resolve_flash_image(
        root: &Path,
        program: &str,
        mode: XdpMode,
        profile: &str,
    ) -> Result<FlashImage, ResolveError> {
        super::resolve_flash_image(root, &root.join("cache"), program, mode, profile, FW)
    }

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
        publish_for_firmware(root, programs, FW);
    }

    fn publish_for_firmware(root: &Path, programs: &[Prog], firmware: [u8; 32]) {
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
                report.push_str(&format!(
                    "[firmware]\nsha256 = \"{}\"\n\n",
                    hex_of(&firmware)
                ));
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
            assert_eq!(
                image.path().file_name().unwrap().to_str().unwrap(),
                format!("{}.elf", hex_of(image.sha256()))
            );
            let _ = file;
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

    #[cfg(unix)]
    #[test]
    fn rejects_index_symlink_outside_build_root() {
        use std::os::unix::fs::symlink;
        let root = scratch("index-escape");
        let outside = scratch("index-outside");
        write(
            &outside.join("index.toml"),
            b"schema = \"libbpf-mcu.artifacts.v1\"\n",
        );
        symlink(outside.join("index.toml"), root.join("index.toml")).unwrap();
        assert!(resolve_flash_image(&root, "demo", XdpMode::Aot, "demo").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_escape_for_each_producer_file() {
        use std::os::unix::fs::symlink;
        for (label, rel) in [
            ("manifest", "demo/manifest.toml"),
            ("report", "demo/rv32/report.toml"),
            ("artifact", "demo/rv32/demo_rv32.elf"),
        ] {
            let root = scratch(&format!("escape-{label}"));
            let outside = scratch(&format!("outside-{label}"));
            publish(
                &root,
                &[prog("demo", vec![legal_mode("demo", "rv32", "demo")])],
            );
            let original = root.join(rel);
            let external = outside.join("file");
            std::fs::rename(&original, &external).unwrap();
            symlink(&external, &original).unwrap();
            assert!(
                resolve_flash_image(&root, "demo", XdpMode::Aot, "demo").is_err(),
                "{label}"
            );
        }
    }

    #[test]
    fn rejects_invalid_names_and_paths() {
        let root = scratch("invalid-segments");
        for name in ["", ".", "..", "a/b", "a\\b", "c:d"] {
            assert!(resolve_flash_image(&root, name, XdpMode::Aot, "demo").is_err());
            assert!(resolve_flash_image(&root, "demo", XdpMode::Aot, name).is_err());
        }
        for path in ["../out", "/out", "a\\out", "C:out"] {
            assert!(rel_join(&root, path).is_err());
        }
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
    #[test]
    fn firmware_cache_and_sibling_profiles() {
        let root = scratch("siblings");
        let cache = root.join("cache");
        publish(
            &root,
            &[prog(
                "case_mark",
                vec![legal_mode("case_mark", "rv32", "case_mark_idle")],
            )],
        );
        let idle = resolve_flash_image(&root, "case_mark", XdpMode::Aot, "case_mark_idle").unwrap();
        let bytes = std::fs::read(idle.path()).unwrap();
        assert_eq!(
            resolve_flash_image(&root, "case_mark", XdpMode::Aot, "case_mark_idle").unwrap(),
            idle
        );
        assert!(matches!(
            super::resolve_flash_image(
                &root,
                &cache,
                "case_mark",
                XdpMode::Aot,
                "case_mark_idle",
                [1; 32]
            ),
            Err(ResolveError::FirmwareMismatch)
        ));
        publish(
            &root,
            &[prog(
                "case_mark",
                vec![legal_mode("case_mark", "rv32", "case_mark")],
            )],
        );
        let base = resolve_flash_image(&root, "case_mark", XdpMode::Aot, "case_mark").unwrap();
        assert_ne!(base.path(), idle.path());
        assert_eq!(
            std::fs::read(
                resolve_flash_image(&root, "case_mark", XdpMode::Aot, "case_mark_idle")
                    .unwrap()
                    .path()
            )
            .unwrap(),
            bytes
        );
        std::fs::write(idle.path(), b"corrupt").unwrap();
        assert!(matches!(
            resolve_flash_image(&root, "case_mark", XdpMode::Aot, "case_mark_idle"),
            Err(ResolveError::CacheInvalid { .. })
        ));
    }

    #[test]
    fn invalid_source_never_falls_back() {
        let root = scratch("fallback-source");
        publish(
            &root,
            &[prog("demo", vec![legal_mode("demo", "rv32", "demo")])],
        );
        resolve_flash_image(&root, "demo", XdpMode::Aot, "demo").unwrap();
        for table in ["stale", "failed"] {
            let mut mode = legal_mode("demo", "rv32", "demo");
            mode.table = table;
            publish(&root, &[prog("demo", vec![mode])]);
            assert!(matches!(
                resolve_flash_image(&root, "demo", XdpMode::Aot, "demo"),
                Err(ResolveError::ModeNotConsumable { .. })
            ));
        }
        let mut mode = legal_mode("demo", "rv32", "demo");
        mode.status = "failed";
        publish(&root, &[prog("demo", vec![mode])]);
        assert!(matches!(
            resolve_flash_image(&root, "demo", XdpMode::Aot, "demo"),
            Err(ResolveError::ReportNotSuccess { .. })
        ));
        publish(
            &root,
            &[prog("demo", vec![legal_mode("demo", "rv32", "demo")])],
        );
        std::fs::write(root.join("demo/rv32/demo_rv32.elf"), b"bad").unwrap();
        assert!(matches!(
            resolve_flash_image(&root, "demo", XdpMode::Aot, "demo"),
            Err(ResolveError::ArtifactMismatch { .. })
        ));
        std::fs::remove_file(root.join("demo/rv32/report.toml")).unwrap();
        assert!(resolve_flash_image(&root, "demo", XdpMode::Aot, "demo").is_err());
        std::fs::remove_file(root.join("demo/manifest.toml")).unwrap();
        assert!(resolve_flash_image(&root, "demo", XdpMode::Aot, "demo").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn cache_symlink_escape_is_rejected() {
        use std::os::unix::fs::symlink;
        let root = scratch("cache-escape");
        let outside = scratch("cache-outside");
        publish(
            &root,
            &[prog("demo", vec![legal_mode("demo", "rv32", "demo")])],
        );
        std::fs::create_dir(root.join("cache")).unwrap();
        symlink(&outside, root.join("cache/demo")).unwrap();
        assert!(resolve_flash_image(&root, "demo", XdpMode::Aot, "demo").is_err());
        assert!(!outside.join("aot").exists());
    }
    #[test]
    fn valid_republication_refreshes_source_provenance() {
        let root = scratch("republication");
        publish(
            &root,
            &[prog("demo", vec![legal_mode("demo", "rv32", "demo")])],
        );
        let first = resolve_flash_image(&root, "demo", XdpMode::Aot, "demo").unwrap();
        let original = std::fs::read(first.path()).unwrap();
        // Adding a second mode changes the manifest but not the AOT bytes.
        publish(
            &root,
            &[prog(
                "demo",
                vec![
                    legal_mode("demo", "rv32", "demo"),
                    legal_mode("demo", "jit", "demo"),
                ],
            )],
        );
        assert_eq!(
            resolve_flash_image(&root, "demo", XdpMode::Aot, "demo").unwrap(),
            first
        );
        // A newly verified producer report can bind identical container bytes
        // to another firmware. Old metadata alone cannot authorize that change.
        let next_firmware = [43; 32];
        publish_for_firmware(
            &root,
            &[prog("demo", vec![legal_mode("demo", "rv32", "demo")])],
            next_firmware,
        );
        let refreshed = super::resolve_flash_image(
            &root,
            &root.join("cache"),
            "demo",
            XdpMode::Aot,
            "demo",
            next_firmware,
        )
        .unwrap();
        assert_eq!(refreshed, first);
        assert_eq!(std::fs::read(refreshed.path()).unwrap(), original);
        let metadata = parse_doc(
            &std::fs::read(refreshed.path().with_extension("toml")).unwrap(),
            refreshed.path(),
        )
        .unwrap();
        assert_eq!(
            metadata["firmware_sha256"].as_str().unwrap(),
            hex_of(&next_firmware)
        );
        assert!(matches!(
            resolve_flash_image(&root, "demo", XdpMode::Aot, "demo"),
            Err(ResolveError::FirmwareMismatch)
        ));
    }

    #[test]
    fn committed_metadata_corruption_is_not_repaired() {
        let root = scratch("metadata-corruption");
        publish(
            &root,
            &[prog("demo", vec![legal_mode("demo", "rv32", "demo")])],
        );
        let image = resolve_flash_image(&root, "demo", XdpMode::Aot, "demo").unwrap();
        let metadata = image.path().with_extension("toml");
        std::fs::write(&metadata, b"bad = true").unwrap();
        assert!(matches!(
            resolve_flash_image(&root, "demo", XdpMode::Aot, "demo"),
            Err(ResolveError::CacheInvalid { .. })
        ));
        assert_eq!(std::fs::read(metadata).unwrap(), b"bad = true");
    }

    #[test]
    fn report_hash_mismatch_never_uses_cache() {
        let root = scratch("report-hash-with-cache");
        publish(
            &root,
            &[prog("demo", vec![legal_mode("demo", "rv32", "demo")])],
        );
        resolve_flash_image(&root, "demo", XdpMode::Aot, "demo").unwrap();
        std::fs::write(root.join("demo/rv32/report.toml"), b"bad").unwrap();
        assert!(matches!(
            resolve_flash_image(&root, "demo", XdpMode::Aot, "demo"),
            Err(ResolveError::ReportHashMismatch)
        ));
    }
}
