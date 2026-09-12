//! Type-state [`MapBuilder`] and map K/V traits.

pub mod traits;

pub use traits::{MapKey, MapValue};

use alloc::string::String;
use alloc::vec::Vec;

use crate::map::runtime::MapRt;
use traits::layout_vec;

/// BPF map kind stored in [`super::MapMeta`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MapType {
    Array,
    Hash,
    Ringbuf,
}

// —— type-state markers ——

#[derive(Clone, Copy, Debug, Default)]
pub struct Incomplete;
pub struct HashView;
pub struct ArrayView;
#[cfg(feature = "ringbuf")]
pub struct RingbufView;

#[derive(Clone, Copy, Debug, Default)]
pub struct HasKey;
pub struct HasValue;
pub struct HasName;
pub struct HasCapacity;

/// Map instance spec builder (type-state).
pub struct MapBuilder<V, S = Incomplete> {
    pub(crate) name: Option<String>,
    pub(crate) map_type: MapType,
    pub(crate) key_layout: Option<Vec<u8>>,
    pub(crate) value_layout: Option<Vec<u8>>,
    pub(crate) len: usize,
    _view: core::marker::PhantomData<(V, S)>,
}

/// Consumable builder passed to [`MapRt::push`].
pub struct CompleteBuilder {
    pub(crate) name: String,
    pub(crate) map_type: MapType,
    pub(crate) key_layout: Option<Vec<u8>>,
    pub(crate) value_layout: Option<Vec<u8>>,
    pub(crate) len: usize,
}

impl CompleteBuilder {
    /// Build from a static bpf.toml boot descriptor.
    pub fn from_boot_desc(d: &crate::map::BootMapDesc) -> Self {
        Self {
            name: String::from(d.name),
            map_type: d.map_type,
            key_layout: d.key_layout.map(|s| s.to_vec()),
            value_layout: d.value_layout.map(|s| s.to_vec()),
            len: d.len,
        }
    }
}

impl MapBuilder<HashView, Incomplete> {
    pub fn hash() -> Self {
        Self {
            name: None,
            map_type: MapType::Hash,
            key_layout: None,
            value_layout: None,
            len: 0,
            _view: core::marker::PhantomData,
        }
    }
}

impl MapBuilder<ArrayView, Incomplete> {
    pub fn array() -> Self {
        Self {
            name: None,
            map_type: MapType::Array,
            key_layout: None,
            value_layout: None,
            len: 0,
            _view: core::marker::PhantomData,
        }
    }
}

#[cfg(feature = "ringbuf")]
impl MapBuilder<RingbufView, Incomplete> {
    pub fn ringbuf() -> Self {
        Self {
            name: None,
            map_type: MapType::Ringbuf,
            key_layout: None,
            value_layout: None,
            len: 0,
            _view: core::marker::PhantomData,
        }
    }
}

macro_rules! impl_with_name {
    ($view:ty, $state:ty) => {
        impl MapBuilder<$view, $state> {
            pub fn with_name(mut self, name: impl Into<String>) -> MapBuilder<$view, HasName> {
                self.name = Some(name.into());
                MapBuilder {
                    name: self.name,
                    map_type: self.map_type,
                    key_layout: self.key_layout,
                    value_layout: self.value_layout,
                    len: self.len,
                    _view: core::marker::PhantomData,
                }
            }
        }
    };
}

impl_with_name!(HashView, Incomplete);
impl_with_name!(HashView, HasKey);
impl_with_name!(HashView, HasValue);
impl_with_name!(ArrayView, Incomplete);
impl_with_name!(ArrayView, HasValue);
#[cfg(feature = "ringbuf")]
impl_with_name!(RingbufView, Incomplete);

impl MapBuilder<HashView, Incomplete> {
    pub fn with_key<K: MapKey>(mut self) -> MapBuilder<HashView, HasKey> {
        self.key_layout = Some(layout_vec(K::FIELD_LENS));
        MapBuilder {
            name: self.name,
            map_type: self.map_type,
            key_layout: self.key_layout,
            value_layout: self.value_layout,
            len: self.len,
            _view: core::marker::PhantomData,
        }
    }
}

impl MapBuilder<HashView, HasKey> {
    pub fn with_value<V: MapValue>(mut self) -> MapBuilder<HashView, HasValue> {
        self.value_layout = Some(layout_vec(V::FIELD_LENS));
        MapBuilder {
            name: self.name,
            map_type: self.map_type,
            key_layout: self.key_layout,
            value_layout: self.value_layout,
            len: self.len,
            _view: core::marker::PhantomData,
        }
    }
}

impl MapBuilder<ArrayView, Incomplete> {
    pub fn with_value<V: MapValue>(mut self) -> MapBuilder<ArrayView, HasValue> {
        self.value_layout = Some(layout_vec(V::FIELD_LENS));
        MapBuilder {
            name: self.name,
            map_type: self.map_type,
            key_layout: self.key_layout,
            value_layout: self.value_layout,
            len: self.len,
            _view: core::marker::PhantomData,
        }
    }
}

impl MapBuilder<HashView, HasName> {
    pub fn with_key<K: MapKey>(mut self) -> MapBuilder<HashView, HasKey> {
        self.key_layout = Some(layout_vec(K::FIELD_LENS));
        MapBuilder {
            name: self.name,
            map_type: self.map_type,
            key_layout: self.key_layout,
            value_layout: self.value_layout,
            len: self.len,
            _view: core::marker::PhantomData,
        }
    }
}

impl MapBuilder<ArrayView, HasName> {
    pub fn with_value<V: MapValue>(mut self) -> MapBuilder<ArrayView, HasValue> {
        self.value_layout = Some(layout_vec(V::FIELD_LENS));
        MapBuilder {
            name: self.name,
            map_type: self.map_type,
            key_layout: self.key_layout,
            value_layout: self.value_layout,
            len: self.len,
            _view: core::marker::PhantomData,
        }
    }
}

macro_rules! impl_with_capacity {
    ($view:ty, $state:ty) => {
        impl MapBuilder<$view, $state> {
            pub fn with_capacity(mut self, n: usize) -> MapBuilder<$view, HasCapacity> {
                self.len = n;
                MapBuilder {
                    name: self.name,
                    map_type: self.map_type,
                    key_layout: self.key_layout,
                    value_layout: self.value_layout,
                    len: self.len,
                    _view: core::marker::PhantomData,
                }
            }
        }
    };
}

impl_with_capacity!(HashView, HasValue);
impl_with_capacity!(ArrayView, HasValue);
#[cfg(feature = "ringbuf")]
impl_with_capacity!(RingbufView, Incomplete);
#[cfg(feature = "ringbuf")]
impl_with_capacity!(RingbufView, HasName);

macro_rules! impl_into_complete {
    ($view:ty) => {
        impl From<MapBuilder<$view, HasCapacity>> for CompleteBuilder {
            fn from(b: MapBuilder<$view, HasCapacity>) -> Self {
                CompleteBuilder {
                    name: b.name.expect("with_name required"),
                    map_type: b.map_type,
                    key_layout: b.key_layout,
                    value_layout: b.value_layout,
                    len: b.len,
                }
            }
        }
    };
}

impl_into_complete!(HashView);
impl_into_complete!(ArrayView);
#[cfg(feature = "ringbuf")]
impl_into_complete!(RingbufView);

impl MapBuilder<HashView, HasCapacity> {
    pub fn into_complete(self) -> CompleteBuilder {
        self.into()
    }
}

impl MapBuilder<ArrayView, HasCapacity> {
    pub fn into_complete(self) -> CompleteBuilder {
        self.into()
    }
}

#[cfg(feature = "ringbuf")]
impl MapBuilder<RingbufView, HasCapacity> {
    pub fn into_complete(self) -> CompleteBuilder {
        self.into()
    }
}

impl MapRt {
    pub fn create_builder<B>(
        &mut self,
        builder: B,
    ) -> Result<crate::map::runtime::MapId, crate::map::runtime::Error>
    where
        B: Into<CompleteBuilder>,
    {
        self.create(builder.into())
    }
}
