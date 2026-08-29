//! Map factory — unify registration styles; ringbufs expose [`RingbufMap::into_channel`].
//!
//! # MCU
//!
//! ```ignore
//! let rb = MapBuilder::new_ringbuf()
//!     .name("dut_evt")
//!     .byte_capacity(4096)
//!     .register()
//!     .await?;
//! let mut rx = rb.into_channel();
//! ```
//!
//! # `bpf.toml` manifest
//!
//! Parse with [`MapBuilder::from_toml`], then [`MapManifest::register_runtime`].
//! Map names must be resolved through an embedder [`NameIntern`] — parsed TOML
//! strings are not `'static` on their own.

use alloc::string::String;
use alloc::vec::Vec;

use super::channel::RingbufReceiver;
use super::ctrl::{MapCreateCmd, MapCtrlError, MapKind, register_map, register_ringbuf};
use super::pool::ResourceId;

/// Embedder hook: map parsed / dynamic names to firmware `&'static str` literals.
pub trait NameIntern {
    fn intern(&self, name: &str) -> Option<&'static str>;
}

/// Factory entry — pick a map kind or load a manifest.
pub struct MapBuilder;

impl MapBuilder {
    pub fn new_ringbuf() -> RingbufBuilder {
        RingbufBuilder::default()
    }

    /// Register maps declared in `bpf.toml` via build-time [`super::init_firmware_maps`].
    pub async fn register_boot_maps() -> Result<(), MapCtrlError> {
        super::init_firmware_maps().await
    }

    /// Parse `[maps.*]` from a `bpf.toml` fragment (no I/O; use `include_str!` on MCU).
    pub fn from_toml(toml: &str) -> Result<MapManifest, BuilderError> {
        MapManifest::parse(toml)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BuilderError {
    MissingName,
    MissingCapacity,
    Toml(String),
    Map(MapCtrlError),
}

impl From<MapCtrlError> for BuilderError {
    fn from(e: MapCtrlError) -> Self {
        Self::Map(e)
    }
}

/// Parsed `bpf.toml` map section (runtime entries only for now).
#[derive(Clone, Debug, Default)]
pub struct MapManifest {
    pub runtime_ringbufs: Vec<RuntimeRingbufSpec>,
}

#[derive(Clone, Debug)]
pub struct RuntimeRingbufSpec {
    pub name: String,
    pub byte_capacity: usize,
}

impl MapManifest {
    /// Scan `bpf.toml` for `[maps.*]` with `runtime = true` and `BPF_MAP_TYPE_RINGBUF`.
    pub fn parse(toml: &str) -> Result<Self, BuilderError> {
        let mut out = Self::default();
        let mut current: Option<String> = None;
        let mut runtime = false;
        let mut map_type: Option<String> = None;
        let mut max_entries = 0usize;

        let flush = |out: &mut Self,
                     name: Option<String>,
                     runtime: bool,
                     map_type: &Option<String>,
                     max_entries: usize| {
            if !runtime {
                return;
            }
            if map_type.as_deref() != Some("BPF_MAP_TYPE_RINGBUF") {
                return;
            }
            let Some(name) = name else {
                return;
            };
            if max_entries == 0 {
                return;
            }
            out.runtime_ringbufs.push(RuntimeRingbufSpec {
                name,
                byte_capacity: max_entries,
            });
        };

        for line in toml.lines() {
            let line = line.split('#').next().unwrap_or(line).trim();
            if line.is_empty() {
                continue;
            }
            if let Some(rest) = line.strip_prefix("[maps.") {
                flush(
                    &mut out,
                    current.take(),
                    runtime,
                    &map_type,
                    max_entries,
                );
                runtime = false;
                map_type = None;
                max_entries = 0;
                let name = rest.trim_end_matches(']').trim();
                if !name.is_empty() {
                    current = Some(name.to_string());
                }
                continue;
            }
            if current.is_none() {
                continue;
            }
            if let Some((_, v)) = line.split_once('=') {
                let v = v.trim().trim_matches('"');
                if line.starts_with("runtime") {
                    runtime = v == "true";
                } else if line.starts_with("map_type") {
                    map_type = Some(v.to_string());
                } else if line.starts_with("max_entries") {
                    max_entries = v.parse().unwrap_or(0);
                }
            }
        }
        flush(
            &mut out,
            current,
            runtime,
            &map_type,
            max_entries,
        );
        Ok(out)
    }

    /// Register every `runtime = true` ringbuf via [`register_ringbuf`].
    pub async fn register_runtime<I: NameIntern>(
        &self,
        intern: &I,
    ) -> Result<Vec<RingbufMap>, BuilderError> {
        let mut maps = Vec::new();
        for spec in &self.runtime_ringbufs {
            let Some(name) = intern.intern(&spec.name) else {
                return Err(BuilderError::Toml(format!(
                    "unknown runtime map name {:?}",
                    spec.name
                )));
            };
            let rb = MapBuilder::new_ringbuf()
                .name(name)
                .byte_capacity(spec.byte_capacity)
                .register()
                .await?;
            maps.push(rb);
        }
        Ok(maps)
    }
}

/// Fluent builder for `BPF_MAP_TYPE_RINGBUF`.
#[derive(Clone, Debug)]
pub struct RingbufBuilder {
    name: Option<&'static str>,
    byte_capacity: Option<usize>,
}

impl Default for RingbufBuilder {
    fn default() -> Self {
        Self {
            name: None,
            byte_capacity: Some(4096),
        }
    }
}

impl RingbufBuilder {
    pub fn name(mut self, name: &'static str) -> Self {
        self.name = Some(name);
        self
    }

    pub fn byte_capacity(mut self, bytes: usize) -> Self {
        self.byte_capacity = Some(bytes);
        self
    }

    /// Register in the pool (`name` + capacity). No VMA.
    pub async fn register(self) -> Result<RingbufMap, BuilderError> {
        let name = self.name.ok_or(BuilderError::MissingName)?;
        let cap = self.byte_capacity.ok_or(BuilderError::MissingCapacity)?;
        let id = register_ringbuf(name, cap).await?;
        Ok(RingbufMap { name, id })
    }
}

/// Registered ringbuf — convert to async RX with [`into_channel`](Self::into_channel).
pub struct RingbufMap {
    name: &'static str,
    id: ResourceId,
}

impl RingbufMap {
    pub fn name(&self) -> &'static str {
        self.name
    }

    pub fn id(&self) -> ResourceId {
        self.id
    }

    /// Embassy-async reader for `select!` / `.await`.
    pub fn into_channel(self) -> RingbufReceiver {
        super::channel::attach_rx(self.id)
    }
}

/// Fluent builder for HASH (extensible; same factory module).
pub struct HashBuilder {
    name: Option<&'static str>,
    key_size: usize,
    value_size: usize,
    max_entries: usize,
}

impl MapBuilder {
    pub fn new_hash() -> HashBuilder {
        HashBuilder {
            name: None,
            key_size: 0,
            value_size: 0,
            max_entries: 0,
        }
    }
}

impl HashBuilder {
    pub fn name(mut self, name: &'static str) -> Self {
        self.name = Some(name);
        self
    }

    pub fn key_size(mut self, bytes: usize) -> Self {
        self.key_size = bytes;
        self
    }

    pub fn value_size(mut self, bytes: usize) -> Self {
        self.value_size = bytes;
        self
    }

    pub fn max_entries(mut self, n: usize) -> Self {
        self.max_entries = n;
        self
    }

    pub async fn register(self) -> Result<HashMap, BuilderError> {
        let name = self.name.ok_or(BuilderError::MissingName)?;
        let id = register_map(
            name,
            MapCreateCmd {
                kind: MapKind::Hash,
                key_size: self.key_size,
                value_size: self.value_size,
                max_entries: self.max_entries,
                id: None,
            },
        )
        .await?;
        Ok(HashMap { name, id })
    }
}

/// Registered HASH map (no async channel — use [`super::with_hash_map`]).
pub struct HashMap {
    name: &'static str,
    id: ResourceId,
}

impl HashMap {
    pub fn name(&self) -> &'static str {
        self.name
    }

    pub fn id(&self) -> ResourceId {
        self.id
    }
}
