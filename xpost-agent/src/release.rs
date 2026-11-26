//! Read and check the deployment files from the pinned producer release.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const TAG: &str = "v0.1.1";
pub const COMMIT: &str = "07a4ea9a81a7fedf7bef2d6ced8d7fcfcccfd113";
const REPOSITORY: &str = "nan-mu/libbpf-mcu";
const ASSETS: &str = include_str!("libbpf-mcu-v0.1.1.sha256");
const MAX_ARCHIVE_BYTES: u64 = 1024 * 1024;

pub const PROGRAMS: [&str; 12] = [
    "firewall",
    "just_drop",
    "just_tx",
    "noop",
    "prime",
    "log2",
    "copy",
    "fletcher32",
    "bsort",
    "strcmp",
    "case_swap",
    "case_mark",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Aot,
    Jit,
    Rbpf,
}

impl Mode {
    pub const ALL: [Self; 3] = [Self::Aot, Self::Jit, Self::Rbpf];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Aot => "aot",
            Self::Jit => "jit",
            Self::Rbpf => "rbpf",
        }
    }

    fn index_name(self) -> &'static str {
        match self {
            Self::Aot => "llvm-aot",
            Self::Jit => "jit",
            Self::Rbpf => "ebpf",
        }
    }

    fn suffix(self) -> &'static str {
        match self {
            Self::Aot => "aot",
            Self::Jit => "jit",
            Self::Rbpf => "ebpf",
        }
    }
}

#[derive(Deserialize)]
struct Index {
    schema: String,
    program: String,
    source: Member,
    bpf: Member,
    #[serde(default, rename = "mode")]
    modes: Vec<DeploymentRecord>,
}

#[derive(Deserialize)]
struct DeploymentRecord {
    name: String,
    role: String,
    format: String,
    elf: Member,
    object: Option<Member>,
}

#[derive(Deserialize)]
struct Member {
    path: String,
    size_bytes: u64,
    sha256: String,
}

pub struct ProgramArchive {
    file: tempfile::NamedTempFile,
    index: Index,
    asset_sha256: &'static str,
}

#[derive(Serialize)]
pub struct DeploymentInfo {
    pub program: String,
    pub mode: Mode,
    pub member: String,
    pub asset_sha256: &'static str,
    pub release_sha256: String,
    pub size_bytes: usize,
    pub image_size: usize,
    pub slot_address: usize,
    pub helper_address: usize,
}

pub struct Deployment {
    pub info: DeploymentInfo,
    pub bytes: Vec<u8>,
    pub container: xpost_elf::ParsedContainer,
}

impl ProgramArchive {
    pub fn open(root: &Path, program: &str) -> Result<Self> {
        Self::open_inner(root, program)
            .with_context(|| format!("{REPOSITORY} {TAG} program {program}"))
    }

    fn open_inner(root: &Path, program: &str) -> Result<Self> {
        let asset_name = format!("{program}.7z");
        let asset_sha256 = ASSETS
            .lines()
            .filter_map(|line| line.split_once("  "))
            .find_map(|(digest, name)| (name == asset_name).then_some(digest))
            .context("program archive is absent from the pinned release")?;
        let cache = cache_directory(root)?;
        let cache_path = cache.join(format!("{asset_sha256}.7z"));
        let (bytes, downloaded) = match read_cache(&cache_path)? {
            Some(bytes) => (bytes, false),
            None => (download_archive(&asset_name)?, true),
        };
        ensure!(
            digest(&bytes) == asset_sha256,
            "archive {asset_name} SHA-256 mismatch"
        );

        // Read archive members from an operation-owned copy of the checked bytes.
        let mut file = tempfile::NamedTempFile::new_in(&cache)?;
        file.write_all(&bytes)?;
        let index_bytes = archive_member(file.path(), "index.toml")?;
        let index: Index = toml::from_str(std::str::from_utf8(&index_bytes)?)
            .context("decode v3 program index")?;
        ensure!(
            index.schema == "libbpf-mcu.program-index.v3" && index.program == program,
            "v3 index schema or program mismatch"
        );
        let mut names = std::collections::HashSet::new();
        for row in &index.modes {
            ensure!(
                Mode::ALL.iter().any(|mode| mode.index_name() == row.name)
                    && names.insert(row.name.as_str()),
                "unknown or duplicate v3 mode {}",
                row.name
            );
        }
        checked_member(file.path(), &index.source, &format!("{program}.c"))?;
        let bpf_path = format!("objects/{program}_bpf.o");
        let bpf = checked_member(file.path(), &index.bpf, &bpf_path)?;
        check_relocatable(&bpf, 2, 247, &bpf_path)?;
        if downloaded {
            let mut entry = tempfile::NamedTempFile::new_in(&cache)?;
            entry.write_all(&bytes)?;
            match entry.persist_noclobber(&cache_path) {
                Ok(_) => {}
                Err(err) if err.error.kind() == std::io::ErrorKind::AlreadyExists => {
                    ensure!(
                        read_cache(&cache_path)?.is_some_and(|saved| saved == bytes),
                        "cache changed while publishing {asset_name}"
                    );
                }
                Err(err) => return Err(err.error).context("publish release archive"),
            }
        }
        Ok(Self {
            file,
            index,
            asset_sha256,
        })
    }

    pub fn deployment(&self, mode: Mode) -> Result<Deployment> {
        self.deployment_inner(mode).with_context(|| {
            format!(
                "{REPOSITORY} {TAG} program {} mode {}",
                self.index.program,
                mode.as_str()
            )
        })
    }

    fn deployment_inner(&self, mode: Mode) -> Result<Deployment> {
        let program = &self.index.program;
        let row = self
            .index
            .modes
            .iter()
            .find(|row| row.name == mode.index_name())
            .with_context(|| format!("no {} deployment in the v3 index", mode.index_name()))?;
        ensure!(
            row.role == "xpost_container" && row.format == "xpost-compact-elf-v0",
            "invalid deployment role or format"
        );
        match (mode, &row.object) {
            (Mode::Aot, Some(member)) => {
                let path = format!("objects/{program}_rv32.o");
                let object = checked_member(self.file.path(), member, &path)?;
                check_relocatable(&object, 1, 243, &path)?;
            }
            (Mode::Aot, None) | (_, Some(_)) => bail!("invalid native object record"),
            (_, None) => {}
        }
        let expected = format!("containers/{program}-vm-{}.elf", mode.suffix());
        let bytes = checked_member(self.file.path(), &row.elf, &expected)?;
        let xpost_elf::ScanOutcome::Ok(container) = xpost_elf::scan_at(&mut bytes.as_slice(), 0)
        else {
            bail!("invalid XDP ELF in {}", row.elf.path);
        };
        ensure!(
            container.file_len == bytes.len() && container.name.as_str() == program,
            "XDP ELF size or embedded program name mismatch in {}",
            row.elf.path
        );
        let info = DeploymentInfo {
            program: program.clone(),
            mode,
            member: row.elf.path.clone(),
            asset_sha256: self.asset_sha256,
            release_sha256: row.elf.sha256.clone(),
            size_bytes: bytes.len(),
            image_size: container.image_size,
            slot_address: container.slot_address,
            helper_address: container.helper_address,
        };
        Ok(Deployment {
            info,
            bytes,
            container,
        })
    }
}

pub fn format_sha256(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn digest(bytes: &[u8]) -> String {
    format_sha256(&Sha256::digest(bytes).into())
}

fn cache_directory(root: &Path) -> Result<PathBuf> {
    let mut path = fs::canonicalize(root)?;
    for component in [".cache", "xpost-agent", "releases", TAG] {
        path.push(component);
        match fs::create_dir(&path) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(err) => return Err(err).with_context(|| format!("create {}", path.display())),
        }
        ensure!(
            fs::symlink_metadata(&path)?.is_dir(),
            "invalid cache directory {}",
            path.display()
        );
    }
    Ok(path)
}

fn read_cache(path: &Path) -> Result<Option<Vec<u8>>> {
    let info = match fs::symlink_metadata(path) {
        Ok(info) => info,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err).with_context(|| format!("read cache {}", path.display())),
    };
    ensure!(
        info.is_file() && info.len() <= MAX_ARCHIVE_BYTES,
        "invalid cached release archive {}",
        path.display()
    );
    Ok(Some(fs::read(path)?))
}

fn download_archive(asset: &str) -> Result<Vec<u8>> {
    let output = Command::new("gh")
        .args([
            "release",
            "download",
            TAG,
            "--repo",
            REPOSITORY,
            "--pattern",
            asset,
            "--output",
            "-",
        ])
        .env("GH_PROMPT_DISABLED", "1")
        .output()
        .context("run gh to download the release archive")?;
    ensure!(
        output.status.success(),
        "download {asset}: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    ensure!(
        output.stdout.len() as u64 <= MAX_ARCHIVE_BYTES,
        "archive exceeds the download limit"
    );
    Ok(output.stdout)
}

fn archive_member(archive: &Path, member: &str) -> Result<Vec<u8>> {
    // macOS tar is libarchive. Linux hosts can use its bsdtar executable.
    let tar = if cfg!(target_os = "macos") {
        "/usr/bin/tar"
    } else {
        "bsdtar"
    };
    let output = Command::new(tar)
        .arg("-xOf")
        .arg(archive)
        .args(["--", member])
        .output()
        .context("run libarchive tar to read the release member")?;
    ensure!(
        output.status.success(),
        "read {member}: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(output.stdout)
}

fn checked_member(archive: &Path, member: &Member, expected: &str) -> Result<Vec<u8>> {
    ensure!(
        member.path == expected,
        "invalid v3 member path {}",
        member.path
    );
    let bytes = archive_member(archive, &member.path)?;
    ensure!(
        bytes.len() as u64 == member.size_bytes,
        "size mismatch for {}",
        member.path
    );
    ensure!(
        digest(&bytes) == member.sha256,
        "SHA-256 mismatch for {}",
        member.path
    );
    Ok(bytes)
}

fn check_relocatable(bytes: &[u8], class: u8, machine: u8, member: &str) -> Result<()> {
    let header_len = if class == 2 { 64 } else { 52 };
    ensure!(
        bytes.len() >= header_len
            && bytes[..7] == [0x7f, b'E', b'L', b'F', class, 1, 1]
            && bytes[16..20] == [1, 0, machine, 0],
        "invalid relocatable ELF {member}"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn damaged_cache_fails_before_archive_use() {
        let root = tempfile::tempdir().unwrap();
        let cache = cache_directory(root.path()).unwrap();
        fs::write(
            cache.join("4288a9611cd12b3759c9d1256304a68d855ce3df7e13ad9ffb564fe8a3fe34a8.7z"),
            b"changed archive bytes",
        )
        .unwrap();
        let Err(error) = ProgramArchive::open(root.path(), "just_tx") else {
            panic!("a corrupted archive became available");
        };
        let message = format!("{error:#}");
        assert!(message.contains("v0.1.1 program just_tx"), "{message}");
        assert!(message.contains("SHA-256 mismatch"), "{message}");
    }

    #[test]
    fn an_absent_index_row_cannot_use_an_unindexed_elf() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let index = toml::from_str(
            r#"
schema = "libbpf-mcu.program-index.v3"
program = "just_tx"
source = { path = "just_tx.c", size_bytes = 1, sha256 = "fixture" }
bpf = { path = "objects/just_tx_bpf.o", size_bytes = 1, sha256 = "fixture" }
"#,
        )
        .unwrap();
        let archive = ProgramArchive {
            file,
            index,
            asset_sha256: "fixture",
        };
        let Err(error) = archive.deployment(Mode::Jit) else {
            panic!("a missing mode became available");
        };
        let message = format!("{error:#}");
        assert!(
            message.contains("v0.1.1 program just_tx mode jit"),
            "{message}"
        );
        assert!(
            message.contains("no jit deployment in the v3 index"),
            "{message}"
        );
    }
}
