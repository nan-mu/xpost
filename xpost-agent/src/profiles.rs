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
    #[serde(default)]
    pub xdp: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct XdpProfile {
    pub name: String,
    pub fw: String,
    pub artifact: String,
    pub build: BuildSpec,
    #[serde(default)]
    pub target: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ProfilesFile {
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
}

impl Profiles {
    pub fn load_path(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path)
            .with_context(|| format!("read profiles {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("parse profiles {}", path.display()))
    }

    pub fn parse(text: &str) -> Result<Self> {
        let file: ProfilesFile = toml::from_str(text).context("profiles.toml")?;
        build_registry(file.fw, file.xdp)
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

    /// First fw partition table, if any (for SerialHandler open before flash).
    pub fn default_partition_table(&self) -> Option<&str> {
        self.fw.values().next().map(|f| f.partition_table.as_str())
    }

    /// Chip for the first fw entry, if any.
    pub fn default_chip(&self) -> Result<Option<xpost_serial::Chip>> {
        match self.fw.values().next() {
            Some(f) => Ok(Some(chip_for_target(&f.target)?)),
            None => Ok(None),
        }
    }

    /// Partition table for an XDP profile (from linked FW).
    pub fn partition_table_for_xdp(&self, xdp_name: &str) -> Result<&str> {
        let x = self
            .xdp(xdp_name)
            .with_context(|| format!("unknown xdp `{xdp_name}`"))?;
        let f = self
            .fw(&x.fw)
            .with_context(|| format!("xdp `{xdp_name}` links missing fw `{}`", x.fw))?;
        Ok(f.partition_table.as_str())
    }
}

/// Parse + validate fw/xdp lists into a registry.
pub fn build_registry(fws: Vec<FwProfile>, xdps: Vec<XdpProfile>) -> Result<Profiles> {
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
        if p.artifact.trim().is_empty() {
            bail!("xdp `{}`: empty artifact", p.name);
        }
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
            if x.fw != *fname {
                bail!(
                    "fw `{fname}` lists xdp `{xn}`, but that xdp.fw is `{}`",
                    x.fw
                );
            }
        }
    }

    Ok(Profiles {
        fw: fw_map,
        xdp: xdp_map,
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
artifact = "build/just_drop/just_drop.elf"
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
        assert_eq!(x.build.command[0], "pixi");
        assert_eq!(
            p.partition_table_for_xdp("just_drop").unwrap(),
            "examples/just_xdp/partitions.csv"
        );
    }

    #[test]
    fn reject_unknown_fw_link() {
        let toml = r#"
[[xdp]]
name = "just_drop"
fw = "missing"
artifact = "build/just_drop/just_drop.elf"
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
    fn reject_xdp_fw_mismatch_in_list() {
        let toml = r#"
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

[[xdp]]
name = "x"
fw = "b"
artifact = "build/x/x.elf"
build = { cwd = ".", command = ["pixi", "run", "build"] }
"#;
        let err = Profiles::parse(toml).unwrap_err().to_string();
        assert!(err.contains("lists xdp"), "{err}");
    }

    #[test]
    fn build_argv_preserved() {
        let p = Profiles::parse(FIXTURE).unwrap();
        let fw = p.fw("board-cag").unwrap();
        assert_eq!(fw.build.cwd, "examples/just_xdp");
        assert_eq!(
            fw.build.command,
            vec!["cargo", "build", "--release"]
        );
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
artifact = "build/x/x.elf"
build = { cwd = ".", command = ["pixi", "run", "build"] }
"#;
        let err = Profiles::parse(toml).unwrap_err().to_string();
        // unknown target on xdp, or mismatch — either is a rejection
        assert!(
            err.contains("unknown target") || err.contains("!="),
            "{err}"
        );
    }
}
