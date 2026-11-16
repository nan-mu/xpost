//! Firmware / XDP profile registry (`profiles.toml`).
//!
//! Two namespaces: [`FwProfile`] (`[[fw]]`) and [`XdpProfile`] (`[[xdp]]`).
//! Cross-links: `fw.xdp[]` → xdp names; `xdp.fw` → fw name.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

/// `{ cwd, command }` run before flash (paths relative to workspace root).
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct BuildSpec {
    pub cwd: String,
    pub command: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct FwProfile {
    pub name: String,
    pub target: String,
    pub partition_table: String,
    pub artifact: String,
    pub build: BuildSpec,
    /// Install limit for one XDP ELF, in bytes.
    pub xdp_image_capacity: Option<usize>,
    #[serde(default)]
    pub xdp: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct XdpProfile {
    pub name: String,
    pub fw: String,
    /// Producer stem (`-c`), not the profile name.
    pub program: String,
    /// `aot`, `jit`, or `rbpf`. No default.
    pub mode: String,
    pub build: BuildSpec,
    #[serde(default)]
    pub target: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ProfilesFile {
    #[serde(default)]
    build_root: String,
    #[serde(default)]
    fw: Vec<FwProfile>,
    #[serde(default)]
    xdp: Vec<XdpProfile>,
}

/// Loaded, validated registry.
#[derive(Debug, Clone, Default)]
pub struct Profiles {
    fw: HashMap<String, FwProfile>,
    xdp: HashMap<String, XdpProfile>,
    build_root: String,
}

impl Profiles {
    /// Producer index directory, relative to the workspace root.
    pub fn build_root(&self) -> &str {
        &self.build_root
    }

    pub fn load_path(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path)
            .with_context(|| format!("read profiles {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("parse profiles {}", path.display()))
    }

    pub fn parse(text: &str) -> Result<Self> {
        let file: ProfilesFile = toml::from_str(text).context("profiles.toml")?;
        build_registry(file.build_root, file.fw, file.xdp)
    }

    pub fn fw(&self, name: &str) -> Option<&FwProfile> {
        self.fw.get(name)
    }

    pub fn xdp(&self, name: &str) -> Option<&XdpProfile> {
        self.xdp.get(name)
    }

    pub fn fw_count(&self) -> usize {
        self.fw.len()
    }

    pub fn xdp_count(&self) -> usize {
        self.xdp.len()
    }

    pub fn is_empty(&self) -> bool {
        self.fw.is_empty() && self.xdp.is_empty()
    }

    /// Partition table for SerialHandler open (needs an `xdp` row for FlashConfig).
    /// Prefer a fw that lists cold XDP images; HAL-only boards omit that row.
    pub fn default_partition_table(&self) -> Option<&str> {
        self.fw
            .values()
            .find(|f| !f.xdp.is_empty())
            .or_else(|| self.fw.values().next())
            .map(|f| f.partition_table.as_str())
    }

    /// Chip for SerialHandler open; prefer a fw that owns cold XDP.
    pub fn default_chip(&self) -> Result<Option<xpost_serial::Chip>> {
        let fw = self
            .fw
            .values()
            .find(|f| !f.xdp.is_empty())
            .or_else(|| self.fw.values().next());
        match fw {
            Some(f) => Ok(Some(chip_for_target(&f.target)?)),
            None => Ok(None),
        }
    }

    /// Resolve the install limit from the firmware linked to this XDP profile.
    pub fn xdp_image_capacity(&self, xdp: &XdpProfile) -> Result<Option<usize>> {
        let fw = self
            .fw(&xdp.fw)
            .with_context(|| format!("xdp `{}` links missing fw `{}`", xdp.name, xdp.fw))?;
        Ok(fw.xdp_image_capacity)
    }
}

/// Parse + validate fw/xdp lists into a registry.
pub fn build_registry(
    build_root: String,
    fws: Vec<FwProfile>,
    xdps: Vec<XdpProfile>,
) -> Result<Profiles> {
    let mut fw_map = HashMap::new();
    for p in fws {
        validate_build(&p.build, &format!("fw `{}`", p.name))?;
        validate_target(&p.target)?;
        if p.name.trim().is_empty() {
            bail!("fw entry with empty name");
        }
        if p.artifact.trim().is_empty() {
            bail!("fw `{}`: empty artifact", p.name);
        }
        if p.partition_table.trim().is_empty() {
            bail!("fw `{}`: empty partition_table", p.name);
        }
        let name = p.name.clone();
        if fw_map.insert(name.clone(), p).is_some() {
            bail!("duplicate fw name `{name}`");
        }
    }

    let mut xdp_map = HashMap::new();
    for p in xdps {
        validate_build(&p.build, &format!("xdp `{}`", p.name))?;
        if p.name.trim().is_empty() {
            bail!("xdp entry with empty name");
        }
        validate_program(&p.program, &format!("xdp `{}`", p.name))?;
        validate_xdp_mode(&p.mode, &format!("xdp `{}`", p.name))?;
        if p.fw.trim().is_empty() {
            bail!("xdp `{}`: empty fw link", p.name);
        }
        if let Some(ref t) = p.target {
            validate_target(t)?;
        }
        let name = p.name.clone();
        if xdp_map.insert(name.clone(), p).is_some() {
            bail!("duplicate xdp name `{name}`");
        }
    }

    for (xname, x) in &xdp_map {
        let Some(f) = fw_map.get(&x.fw) else {
            bail!("xdp `{xname}` references unknown fw `{}`", x.fw);
        };
        if let Some(ref t) = x.target
            && t != &f.target
        {
            bail!(
                "xdp `{xname}` target `{t}` != fw `{}` target `{}`",
                x.fw,
                f.target
            );
        }
    }

    for (fname, f) in &fw_map {
        for xn in &f.xdp {
            if !xdp_map.contains_key(xn) {
                bail!("fw `{fname}` lists unknown xdp `{xn}`");
            }
            let x = &xdp_map[xn];
            // Canonical owner is xdp.fw; other fws may co-flash the same xdp when
            // they share that owner's partition_table (e.g. just-tx-cag / just-tx-t).
            if x.fw != *fname {
                let Some(owner) = fw_map.get(&x.fw) else {
                    bail!("xdp `{xn}` links missing fw `{}`", x.fw);
                };
                if owner.partition_table != f.partition_table {
                    bail!(
                        "fw `{fname}` lists xdp `{xn}`, but that xdp.fw is `{}` with different partition_table",
                        x.fw
                    );
                }
            }
        }
    }

    if !xdp_map.is_empty() && build_root.trim().is_empty() {
        bail!("profiles.toml: build_root is required");
    }

    Ok(Profiles {
        fw: fw_map,
        xdp: xdp_map,
        build_root,
    })
}

fn validate_build(b: &BuildSpec, ctx: &str) -> Result<()> {
    if b.command.is_empty() {
        bail!("{ctx}: build.command must be non-empty");
    }
    if b.command.iter().any(|t| t.is_empty()) {
        bail!("{ctx}: build.command contains empty token");
    }
    Ok(())
}

fn validate_target(target: &str) -> Result<()> {
    match target {
        "esp32c3" => Ok(()),
        other => bail!("unknown target `{other}` (supported: esp32c3)"),
    }
}

fn validate_program(program: &str, ctx: &str) -> Result<()> {
    if program.is_empty()
        || program == "."
        || program == ".."
        || program.contains('/')
        || program.contains('\\')
    {
        bail!("{ctx}: program must be a single path segment");
    }
    Ok(())
}

fn validate_xdp_mode(mode: &str, ctx: &str) -> Result<()> {
    match mode {
        "aot" | "jit" | "rbpf" => Ok(()),
        other => bail!("{ctx}: unknown mode `{other}`"),
    }
}

/// Map profile `target` string to serial [`Chip`](xpost_serial::Chip).
pub fn chip_for_target(target: &str) -> Result<xpost_serial::Chip> {
    match target {
        "esp32c3" => Ok(xpost_serial::Chip::Esp32c3),
        other => bail!("unknown target `{other}`"),
    }
}

/// Resolve `cwd` / `artifact` against workspace root.
pub fn abs_under_root(root: &Path, rel: &str) -> PathBuf {
    let p = Path::new(rel);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        root.join(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"
build_root = "../build"

[[fw]]
name = "board-cag"
target = "esp32c3"
partition_table = "examples/just_xdp/partitions.csv"
artifact = "examples/just_xdp/target/riscv32imc-unknown-none-elf/release/just_xdp"
build = { cwd = "examples/just_xdp", command = ["cargo", "build", "--release"] }
xdp = ["just_drop"]

[[xdp]]
name = "just_drop"
fw = "board-cag"
program = "just_drop"
mode = "aot"
build = { cwd = ".", command = ["pixi", "run", "build", "--firmware", "examples/just_xdp/target/riscv32imc-unknown-none-elf/release/just_xdp", "-c", "just_drop"] }
"#;

    #[test]
    fn parse_empty() {
        let p = Profiles::parse("").unwrap();
        assert!(p.is_empty());
    }

    #[test]
    fn parse_fixture_links() {
        let p = Profiles::parse(FIXTURE).unwrap();
        assert_eq!(p.fw_count(), 1);
        assert_eq!(p.xdp_count(), 1);
        let fw = p.fw("board-cag").unwrap();
        assert_eq!(fw.xdp, vec!["just_drop"]);
        assert_eq!(fw.build.command[0], "cargo");
        let x = p.xdp("just_drop").unwrap();
        assert_eq!(x.fw, "board-cag");
        assert_eq!(x.program, "just_drop");
        assert_eq!(x.mode, "aot");
        assert_eq!(p.build_root(), "../build");
        assert_eq!(x.build.command[0], "pixi");
        assert_eq!(fw.partition_table, "examples/just_xdp/partitions.csv");
    }

    #[test]
    fn exec_bench_cold_install_uses_the_firmware_capacity() {
        let profiles = Profiles::parse(include_str!("../../profiles.toml")).unwrap();
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let exec = profiles.xdp("bsort").unwrap();
        let install_limit = profiles.xdp_image_capacity(exec).unwrap().unwrap();
        let board: toml::Table = include_str!("../../examples/exec_bench/Cargo.toml")
            .parse()
            .unwrap();
        let capacity = board["package"]["metadata"]["xpost"]["xdp_table"]["image_capacity"]
            .as_integer()
            .unwrap();
        assert_eq!(install_limit, capacity as usize);
        assert_eq!(install_limit, 16 * 1024);
        let mut config = xpost_serial::FlashConfig::from_partitions_csv(
            root.join(profiles.default_partition_table().unwrap()),
        )
        .unwrap();
        assert_eq!(config.xdp_table().unwrap().image_capacity, 4 * 1024);
        config.image_capacity = install_limit;
        let table = config.xdp_table().unwrap();
        assert_eq!(table.image_capacity, install_limit);
        assert_eq!(table.len, 64 * 1024);
        let network = profiles.xdp("just_tx").unwrap();
        assert_eq!(profiles.xdp_image_capacity(network).unwrap(), None);
    }

    #[test]
    fn reject_unknown_fw_link() {
        let toml = r#"
build_root = "../build"
[[xdp]]
name = "just_drop"
fw = "missing"
program = "just_drop"
mode = "aot"
build = { cwd = ".", command = ["pixi", "run", "build"] }
"#;
        let err = Profiles::parse(toml).unwrap_err().to_string();
        assert!(err.contains("unknown fw"), "{err}");
    }

    #[test]
    fn reject_dangling_fw_xdp() {
        let toml = r#"
[[fw]]
name = "board"
target = "esp32c3"
partition_table = "examples/just_xdp/partitions.csv"
artifact = "fw.elf"
build = { cwd = ".", command = ["echo", "hi"] }
xdp = ["nope"]
"#;
        let err = Profiles::parse(toml).unwrap_err().to_string();
        assert!(err.contains("unknown xdp"), "{err}");
    }

    #[test]
    fn reject_duplicate_fw_name() {
        let toml = r#"
[[fw]]
name = "board"
target = "esp32c3"
partition_table = "p.csv"
artifact = "a.elf"
build = { cwd = ".", command = ["echo"] }

[[fw]]
name = "board"
target = "esp32c3"
partition_table = "p.csv"
artifact = "b.elf"
build = { cwd = ".", command = ["echo"] }
"#;
        let err = Profiles::parse(toml).unwrap_err().to_string();
        assert!(err.contains("duplicate fw"), "{err}");
    }

    #[test]
    fn allow_shared_xdp_same_partition() {
        let toml = r#"
build_root = "../build"
[[fw]]
name = "a"
target = "esp32c3"
partition_table = "p.csv"
artifact = "a.elf"
build = { cwd = ".", command = ["echo"] }
xdp = ["x"]

[[fw]]
name = "b"
target = "esp32c3"
partition_table = "p.csv"
artifact = "b.elf"
build = { cwd = ".", command = ["echo"] }
xdp = ["x"]

[[xdp]]
name = "x"
fw = "a"
program = "x"
mode = "aot"
build = { cwd = ".", command = ["pixi", "run", "build"] }
"#;
        let p = Profiles::parse(toml).unwrap();
        assert_eq!(p.fw("a").unwrap().xdp, vec!["x"]);
        assert_eq!(p.fw("b").unwrap().xdp, vec!["x"]);
        assert_eq!(p.xdp("x").unwrap().fw, "a");
    }

    #[test]
    fn reject_shared_xdp_different_partition() {
        let toml = r#"
build_root = "../build"
[[fw]]
name = "a"
target = "esp32c3"
partition_table = "p.csv"
artifact = "a.elf"
build = { cwd = ".", command = ["echo"] }
xdp = ["x"]

[[fw]]
name = "b"
target = "esp32c3"
partition_table = "other.csv"
artifact = "b.elf"
build = { cwd = ".", command = ["echo"] }
xdp = ["x"]

[[xdp]]
name = "x"
fw = "a"
program = "x"
mode = "aot"
build = { cwd = ".", command = ["pixi", "run", "build"] }
"#;
        let err = Profiles::parse(toml).unwrap_err().to_string();
        assert!(err.contains("different partition_table"), "{err}");
    }

    #[test]
    fn build_argv_preserved() {
        let p = Profiles::parse(FIXTURE).unwrap();
        let fw = p.fw("board-cag").unwrap();
        assert_eq!(fw.build.cwd, "examples/just_xdp");
        assert_eq!(fw.build.command, vec!["cargo", "build", "--release"]);
        let x = p.xdp("just_drop").unwrap();
        assert_eq!(x.build.cwd, ".");
        assert_eq!(
            x.build.command,
            vec![
                "pixi",
                "run",
                "build",
                "--firmware",
                "examples/just_xdp/target/riscv32imc-unknown-none-elf/release/just_xdp",
                "-c",
                "just_drop",
            ]
        );
    }

    #[test]
    fn reject_unknown_target() {
        let toml = r#"
[[fw]]
name = "board"
target = "esp32s3"
partition_table = "p.csv"
artifact = "a.elf"
build = { cwd = ".", command = ["echo"] }
"#;
        let err = Profiles::parse(toml).unwrap_err().to_string();
        assert!(err.contains("unknown target"), "{err}");
    }

    #[test]
    fn reject_xdp_target_mismatch() {
        let toml = r#"
build_root = "../build"
[[fw]]
name = "board"
target = "esp32c3"
partition_table = "p.csv"
artifact = "a.elf"
build = { cwd = ".", command = ["echo"] }

[[xdp]]
name = "x"
fw = "board"
target = "esp32s3"
program = "x"
mode = "aot"
build = { cwd = ".", command = ["pixi", "run", "build"] }
"#;
        let err = Profiles::parse(toml).unwrap_err().to_string();
        // unknown target on xdp, or mismatch — either is a rejection
        assert!(
            err.contains("unknown target") || err.contains("!="),
            "{err}"
        );
    }

    #[test]
    fn reject_unknown_xdp_mode() {
        let toml = r#"
build_root = "../build"
[[fw]]
name = "board"
target = "esp32c3"
partition_table = "p.csv"
artifact = "a.elf"
build = { cwd = ".", command = ["echo"] }

[[xdp]]
name = "x"
fw = "board"
program = "x"
mode = "hal"
build = { cwd = ".", command = ["pixi", "run", "build"] }
"#;
        let err = Profiles::parse(toml).unwrap_err().to_string();
        assert!(err.contains("unknown mode"), "{err}");
    }

    #[test]
    fn parse_repo_profiles() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../profiles.toml");
        let text = std::fs::read_to_string(&path).unwrap();
        let profiles = Profiles::parse(&text).unwrap();
        assert_eq!(profiles.build_root(), "../build");
        let idle = profiles.xdp("case_mark_idle").unwrap();
        assert_eq!(idle.program, "case_mark");
        assert_eq!(idle.mode, "aot");
        let rbpf = profiles.xdp("incr_rbpf").unwrap();
        assert_eq!(rbpf.program, "incr");
        assert_eq!(rbpf.mode, "rbpf");
        assert_eq!(profiles.xdp("incr").unwrap().mode, "aot");
    }
}
