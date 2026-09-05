//! Profile registry for the Mac-side agent.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
struct ProfilesFile {
    profile: Vec<ProfileEntry>,
}

#[derive(Debug, Clone, Deserialize)]
struct ProfileEntry {
    id: String,
    path: String,
    description: String,
    bin: String,
    partition_table: String,
    chip: String,
}

#[derive(Debug, Clone)]
pub struct Profile {
    pub id: String,
    pub path: String,
    pub description: String,
    pub bin: PathBuf,
    pub partition_table: PathBuf,
    pub chip: String,
}

pub fn load_profiles(xpost_root: &Path) -> Result<Vec<Profile>> {
    let raw =
        std::fs::read_to_string(xpost_root.join("profiles.toml")).context("read profiles.toml")?;
    let file: ProfilesFile = toml::from_str(&raw).context("parse profiles.toml")?;
    let mut out = Vec::new();
    for p in file.profile {
        out.push(Profile {
            id: p.id,
            path: p.path,
            description: p.description,
            bin: xpost_root.join(p.bin),
            partition_table: xpost_root.join(p.partition_table),
            chip: p.chip,
        });
    }
    Ok(out)
}

pub fn find_profile<'a>(profiles: &'a [Profile], id: &str) -> Result<&'a Profile> {
    profiles
        .iter()
        .find(|p| p.id == id)
        .with_context(|| format!("unknown profile `{id}`"))
}
