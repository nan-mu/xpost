//! Compile-time map key / value layout markers.

use alloc::vec::Vec;

/// Firmware-side compile-time key type marker.
pub trait MapKey {
    /// `push` copies into [`super::MapMeta::key_layout`].
    const FIELD_LENS: &'static [u8];
}

/// Firmware-side compile-time value type marker.
pub trait MapValue {
    /// `push` copies into [`super::MapMeta::value_layout`].
    const FIELD_LENS: &'static [u8];
}

/// Copy `FIELD_LENS` into a heap `Vec` for [`super::MapMeta`].
pub(crate) fn layout_vec(lens: &'static [u8]) -> Vec<u8> {
    lens.to_vec()
}

/// Sum field widths for heap budget estimation.
pub(crate) fn layout_size(lens: &[u8]) -> usize {
    lens.iter().map(|w| *w as usize).sum()
}

/// Manual `MapKey` for bpf.toml codegen structs.
#[macro_export]
macro_rules! impl_map_key {
    ($ty:ty, $($width:expr),+ $(,)?) => {
        impl $crate::map::MapKey for $ty {
            const FIELD_LENS: &'static [u8] = &[$($width),+];
        }
    };
}

/// Manual `MapValue` for bpf.toml codegen structs.
#[macro_export]
macro_rules! impl_map_value {
    ($ty:ty, $($width:expr),+ $(,)?) => {
        impl $crate::map::MapValue for $ty {
            const FIELD_LENS: &'static [u8] = &[$($width),+];
        }
    };
}

#[allow(unused_imports)] // #[macro_export] macros; keep discoverable at `map::builder::traits`
pub use impl_map_key;
#[allow(unused_imports)]
pub use impl_map_value;
