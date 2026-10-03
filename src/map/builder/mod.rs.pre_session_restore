//! Flat [`MapBuilder`] and map K/V traits.

pub(crate) mod traits;

pub use traits::{MapKey, MapValue};

use alloc::{string::String, vec::Vec};

use crate::map::builder::traits::layout_vec;

/// BPF map kind stored in [`super::MapMeta`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MapType {
    Array,
    Hash,
    #[cfg(feature = "ringbuf")]
    Ringbuf,
}

/// Map instance spec builder (no typestate).
#[derive(Clone, Debug)]
pub struct MapBuilder {
    pub(crate) name: Option<String>,
    pub(crate) map_type: MapType,
    pub(crate) key_layout: Option<Vec<u8>>,
    pub(crate) value_layout: Option<Vec<u8>>,
    pub(crate) len: usize,
}

impl MapBuilder {
    pub fn hash() -> Self {
        Self {
            name: None,
            map_type: MapType::Hash,
            key_layout: None,
            value_layout: None,
            len: 0,
        }
    }

    pub fn array() -> Self {
        Self {
            name: None,
            map_type: MapType::Array,
            key_layout: None,
            value_layout: None,
            len: 0,
        }
    }

    #[cfg(feature = "ringbuf")]
    pub fn ringbuf() -> Self {
        Self {
            name: None,
            map_type: MapType::Ringbuf,
            key_layout: None,
            value_layout: None,
            len: 0,
        }
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Set key layout from a [`MapKey`] type.
    ///
    /// Compile-time: `FIELD_LENS` must be non-empty. Hash-only use is enforced in
    /// [`Self::validate`] / [`MapRt::create`].
    pub fn with_key<K: MapKey>(mut self) -> Self {
        const {
            assert!(
                !K::FIELD_LENS.is_empty(),
                "MapKey FIELD_LENS must be non-empty"
            )
        };
        self.key_layout = Some(layout_vec(K::FIELD_LENS));
        self
    }

    /// Set value layout from a [`MapValue`] type.
    pub fn with_value<V: MapValue>(mut self) -> Self {
        const {
            assert!(
                !V::FIELD_LENS.is_empty(),
                "MapValue FIELD_LENS must be non-empty"
            )
        };
        self.value_layout = Some(layout_vec(V::FIELD_LENS));
        self
    }

    /// Raw key layout bytes (codegen).
    pub fn with_key_layout(mut self, layout: &[u8]) -> Self {
        self.key_layout = Some(layout.to_vec());
        self
    }

    /// Raw value layout bytes (codegen).
    pub fn with_value_layout(mut self, layout: &[u8]) -> Self {
        self.value_layout = Some(layout.to_vec());
        self
    }

    pub fn with_capacity(mut self, n: usize) -> Self {
        self.len = n;
        self
    }

    /// Rebuild a create spec from a Compact ELF / XDP required-map row.
    pub(crate) fn from_meta(meta: &crate::map::MapMeta) -> Self {
        Self {
            name: Some(meta.name.clone()),
            map_type: meta.map_type,
            key_layout: meta.key_layout.clone(),
            value_layout: meta.value_layout.clone(),
            len: meta.len,
        }
    }

    /// Fail closed if name / capacity / type-specific layouts disagree.
    pub(crate) fn validate(&self) -> Result<(), crate::map::runtime::Error> {
        use crate::map::runtime::Error;
        let name = self.name.as_deref().unwrap_or("");
        // Compact ELF MapDescriptorV0 names: 1..31 ASCII bytes.
        if name.is_empty() || name.len() > 31 || !name.is_ascii() {
            return Err(Error::InvalidName);
        }
        if self.len == 0 {
            return Err(Error::TypeMismatch);
        }
        match self.map_type {
            MapType::Hash => {
                // Hash requires key + value; reject empty layouts at create time.
                if self
                    .key_layout
                    .as_ref()
                    .map(|l| l.is_empty())
                    .unwrap_or(true)
                {
                    return Err(Error::TypeMismatch);
                }
                if self
                    .value_layout
                    .as_ref()
                    .map(|l| l.is_empty())
                    .unwrap_or(true)
                {
                    return Err(Error::TypeMismatch);
                }
            }
            MapType::Array => {
                if self.key_layout.is_some() {
                    return Err(Error::TypeMismatch);
                }
                if self
                    .value_layout
                    .as_ref()
                    .map(|l| l.is_empty())
                    .unwrap_or(true)
                {
                    return Err(Error::TypeMismatch);
                }
            }
            #[cfg(feature = "ringbuf")]
            MapType::Ringbuf => {
                if self.key_layout.is_some() || self.value_layout.is_some() {
                    return Err(Error::TypeMismatch);
                }
            }
        }
        Ok(())
    }
}
