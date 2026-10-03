use std::{env, fs, path::PathBuf};

use anyhow::{Context as _, anyhow};
use aya_build::Toolchain;

fn main() -> anyhow::Result<()> {
    println!("cargo:rerun-if-changed=../config/default.toml");
    println!("cargo:rerun-if-changed=../config/just_tx.toml");
    println!("cargo:rerun-if-changed=../config/just_drop.toml");

    let out_dir = PathBuf::from(env::var("OUT_DIR")?);

    let cargo_metadata::Metadata { packages, .. } = cargo_metadata::MetadataCommand::new()
        .no_deps()
        .exec()
        .context("MetadataCommand::exec")?;
    let ebpf_package = packages
        .into_iter()
        .find(|cargo_metadata::Package { name, .. }| name.as_str() == "test-ebpf")
        .ok_or_else(|| anyhow!("test-ebpf package not found"))?;
    let cargo_metadata::Package {
        name,
        manifest_path,
        ..
    } = ebpf_package;

    // Host builds without bpfel toolchain can set TEST_SKIP_EBPF=1 (stub only; not for TR3000).
    if env::var_os("TEST_SKIP_EBPF").is_some() {
        let stub = out_dir.join("test_bpf");
        if stub.is_dir() {
            fs::remove_dir_all(&stub).with_context(|| format!("remove {}", stub.display()))?;
        }
        fs::write(&stub, b"\0").with_context(|| format!("write stub {}", stub.display()))?;
        println!(
            "cargo:warning=TEST_SKIP_EBPF set; wrote stub {}",
            stub.display()
        );
        return Ok(());
    }

    let ebpf_package = aya_build::Package {
        name: name.as_str(),
        root_dir: manifest_path
            .parent()
            .ok_or_else(|| anyhow!("no parent for {manifest_path}"))?
            .as_str(),
        ..Default::default()
    };
    aya_build::build_ebpf([ebpf_package], Toolchain::default())
}
