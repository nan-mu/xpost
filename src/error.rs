//! Control-plane errors: one small enum per public entrypoint.
//!
//! BPF helper errno lives in [`crate::map::ops::MapOpError`], not here.

use core::fmt;

// ─── Map ───────────────────────────────────────────────────────────────

/// [`crate::map::MapRt::init`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MapInitError {
    AlreadyInitialized,
    HeapClaimFailed,
}

/// [`crate::map::MapRt::create`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MapCreateError {
    InvalidName,
    AlreadyExists,
    TypeMismatch,
    Oom,
    /// Datapath is leased or a flight still owns the maps.
    Busy,
}

/// [`crate::map::MapRt::check`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MapCheckError {
    NotFound,
    TypeMismatch,
    /// Datapath is leased or a flight still owns the maps.
    Busy,
}

/// [`crate::map::MapRt::find`] / [`crate::map::MapGuard`] accessors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MapUseError {
    NotFound,
    TypeMismatch,
    InvalidArg,
    Exists,
    NoMem,
    Busy,
}

// ─── Shared leaf ───────────────────────────────────────────────────────

/// Cold Flash I/O ([`crate::xdp::table`] storage).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlashError {
    Storage,
}

// ─── XDP ───────────────────────────────────────────────────────────────

/// [`crate::xdp::XdpRt::init`] (boot: slots, flash, required maps).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum XdpInitError {
    AlreadyInitialized,
    NoPrimarySlot,
    MultiplePrimarySlots,
    OverlappingSlots,
    TooManySlots,
    TooManyImages,
    EmptySlot,
    SlotAlignment,
    SlotOverflow,
    LinkedAddress,
    Storage,
    DuplicateName,
    InvalidName,
    ImageEmpty,
    ImageTooLarge,
    TableFull,
    TableIdOverflow,
    HelperAddress,
    HelperAddressMismatch,
    SlotAddress,
    TypeMismatch,
    RequiredMapAbsent,
    MapCreate(MapCreateError),
    MapCheck(MapCheckError),
}

/// [`crate::xdp::XdpRt::switch`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum XdpSwitchError {
    Leased,
    NoPrimarySlot,
    UnknownSlot,
    UnknownImage,
    PrimaryEmpty,
    SlotMismatch,
    SlotOccupied,
    ImageEmpty,
    ImageTooLarge,
    ScratchTooSmall,
    Storage,
    Arch,
    HelperAddress,
    HelperAddressMismatch,
    SlotAddress,
    SlotAddressAlign,
    MapsRequired,
    MissingMap,
    /// Leased image's aliases do not cover the incoming image's binds.
    MapBind,
    MapCheck(MapCheckError),
}

/// [`crate::xdp::XdpRt::lease`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum XdpLeaseError {
    Leased,
    NoPrimarySlot,
    PrimaryEmpty,
    UnknownSlot,
    UnknownImage,
    SlotMismatch,
    ImageEmpty,
    ImageTooLarge,
    Arch,
    HelperAddress,
    HelperAddressMismatch,
    SlotAddress,
    SlotAddressAlign,
    MapsRequired,
    MissingMap,
    AliasFull,
    Map(MapUseError),
    MapCheck(MapCheckError),
}

/// [`crate::xdp::XdpRt::release`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum XdpReleaseError {
    NotLeased,
}

/// [`crate::xdp::XdpRt::slot_for`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum XdpSlotError {
    UnknownSlot,
}

macro_rules! impl_error_display {
    ($ty:ty, $($variant:pat => $msg:expr),+ $(,)?) => {
        impl $ty {
            pub const fn as_str(self) -> &'static str {
                match self {
                    $($variant => $msg,)+
                }
            }
        }
        impl fmt::Display for $ty {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

impl_error_display! {
    MapInitError,
    Self::AlreadyInitialized => "already initialized",
    Self::HeapClaimFailed => "heap claim failed",
}

impl_error_display! {
    MapCreateError,
    Self::InvalidName => "invalid name",
    Self::AlreadyExists => "already exists",
    Self::TypeMismatch => "map type/layout mismatch",
    Self::Oom => "out of memory",
    Self::Busy => "busy",
}

impl_error_display! {
    MapCheckError,
    Self::NotFound => "not found",
    Self::TypeMismatch => "map type/layout mismatch",
    Self::Busy => "busy",
}

impl_error_display! {
    MapUseError,
    Self::NotFound => "not found",
    Self::TypeMismatch => "map type/layout mismatch",
    Self::InvalidArg => "invalid argument",
    Self::Exists => "already exists",
    Self::NoMem => "out of memory",
    Self::Busy => "busy",
}

impl_error_display! {
    FlashError,
    Self::Storage => "storage error",
}

impl XdpInitError {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AlreadyInitialized => "already initialized",
            Self::NoPrimarySlot => "no primary slot",
            Self::MultiplePrimarySlots => "multiple primary slots",
            Self::OverlappingSlots => "overlapping slots",
            Self::TooManySlots => "too many slots",
            Self::TooManyImages => "too many images",
            Self::EmptySlot => "empty slot",
            Self::SlotAlignment => "slot alignment",
            Self::SlotOverflow => "slot overflow",
            Self::LinkedAddress => "linked address mismatch",
            Self::Storage => "storage error",
            Self::DuplicateName => "duplicate name",
            Self::InvalidName => "invalid name",
            Self::ImageEmpty => "image empty",
            Self::ImageTooLarge => "image exceeds slot/table",
            Self::TableFull => "table full",
            Self::TableIdOverflow => "table id overflow",
            Self::HelperAddress => "helper address",
            Self::HelperAddressMismatch => "helper address mismatch",
            Self::SlotAddress => "slot address",
            Self::TypeMismatch => "map type/layout mismatch",
            Self::RequiredMapAbsent => "required map absent",
            Self::MapCreate(e) => e.as_str(),
            Self::MapCheck(e) => e.as_str(),
        }
    }
}

impl fmt::Display for XdpInitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl XdpSwitchError {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Leased => "operation while leased",
            Self::NoPrimarySlot => "no primary slot",
            Self::UnknownSlot => "unknown slot",
            Self::UnknownImage => "unknown image",
            Self::PrimaryEmpty => "primary empty",
            Self::SlotMismatch => "slot address mismatch",
            Self::SlotOccupied => "slot occupied",
            Self::ImageEmpty => "image empty",
            Self::ImageTooLarge => "image exceeds slot/table",
            Self::ScratchTooSmall => "scratch too small",
            Self::Storage => "storage error",
            Self::Arch => "arch mismatch",
            Self::HelperAddress => "helper address",
            Self::HelperAddressMismatch => "helper address mismatch",
            Self::SlotAddress => "slot address",
            Self::SlotAddressAlign => "slot address align",
            Self::MapsRequired => "maps required but none",
            Self::MissingMap => "missing map",
            Self::MapBind => "map bind mismatch",
            Self::MapCheck(e) => e.as_str(),
        }
    }
}

impl fmt::Display for XdpSwitchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl XdpLeaseError {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Leased => "operation while leased",
            Self::NoPrimarySlot => "no primary slot",
            Self::PrimaryEmpty => "primary empty",
            Self::UnknownSlot => "unknown slot",
            Self::UnknownImage => "unknown image",
            Self::SlotMismatch => "slot address mismatch",
            Self::ImageEmpty => "image empty",
            Self::ImageTooLarge => "image exceeds slot/table",
            Self::Arch => "arch mismatch",
            Self::HelperAddress => "helper address",
            Self::HelperAddressMismatch => "helper address mismatch",
            Self::SlotAddress => "slot address",
            Self::SlotAddressAlign => "slot address align",
            Self::MapsRequired => "maps required but none",
            Self::MissingMap => "missing map",
            Self::AliasFull => "map alias table full",
            Self::Map(e) => e.as_str(),
            Self::MapCheck(e) => e.as_str(),
        }
    }
}

impl fmt::Display for XdpLeaseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl_error_display! {
    XdpReleaseError,
    Self::NotLeased => "not leased",
}

impl_error_display! {
    XdpSlotError,
    Self::UnknownSlot => "unknown slot",
}

impl From<MapCreateError> for XdpInitError {
    fn from(e: MapCreateError) -> Self {
        Self::MapCreate(e)
    }
}

impl From<MapCheckError> for XdpInitError {
    fn from(e: MapCheckError) -> Self {
        Self::MapCheck(e)
    }
}

impl From<MapCheckError> for XdpSwitchError {
    fn from(e: MapCheckError) -> Self {
        Self::MapCheck(e)
    }
}

impl From<MapCheckError> for XdpLeaseError {
    fn from(e: MapCheckError) -> Self {
        Self::MapCheck(e)
    }
}

impl From<MapUseError> for XdpLeaseError {
    fn from(e: MapUseError) -> Self {
        Self::Map(e)
    }
}

impl From<FlashError> for XdpInitError {
    fn from(_: FlashError) -> Self {
        Self::Storage
    }
}

impl From<FlashError> for XdpSwitchError {
    fn from(_: FlashError) -> Self {
        Self::Storage
    }
}

impl From<allocator_api2::alloc::AllocError> for MapCreateError {
    fn from(_: allocator_api2::alloc::AllocError) -> Self {
        Self::Oom
    }
}

/// [`crate::xdp::XdpRt::stage`] and [`crate::xdp::XdpRt::activate`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum XdpSwapError {
    NotLeased,
    ActiveSlot,
    SlotOccupied,
    UnknownSlot,
    UnknownImage,
    BadGeneration,
    NoStaged,
    ImageEmpty,
    ImageTooLarge,
    ScratchTooSmall,
    Arch,
    HelperAddress,
    HelperAddressMismatch,
    SlotAddress,
    SlotAddressAlign,
    Digest,
    MissingMap,
    MapsRequired,
    AliasFull,
    Busy,
    Storage,
    /// Alias restore failed. The datapath stays in the quiesce drop policy.
    RollbackFailed,
    Map(MapUseError),
    MapCheck(MapCheckError),
}

impl XdpSwapError {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotLeased => "not leased",
            Self::ActiveSlot => "active slot",
            Self::SlotOccupied => "slot occupied",
            Self::UnknownSlot => "unknown slot",
            Self::UnknownImage => "unknown image",
            Self::BadGeneration => "bad generation",
            Self::NoStaged => "nothing staged",
            Self::ImageEmpty => "image empty",
            Self::ImageTooLarge => "image exceeds slot",
            Self::ScratchTooSmall => "scratch too small",
            Self::Arch => "arch mismatch",
            Self::HelperAddress => "helper address",
            Self::HelperAddressMismatch => "helper address mismatch",
            Self::SlotAddress => "slot address",
            Self::SlotAddressAlign => "slot address align",
            Self::Digest => "digest mismatch",
            Self::MissingMap => "missing map",
            Self::MapsRequired => "maps required but none",
            Self::AliasFull => "alias table full",
            Self::Busy => "busy",
            Self::Storage => "storage error",
            Self::RollbackFailed => "rollback failed",
            Self::Map(e) => e.as_str(),
            Self::MapCheck(e) => e.as_str(),
        }
    }

    pub const fn code(self) -> u8 {
        match self {
            Self::NotLeased => 11,
            Self::ActiveSlot => 1,
            Self::SlotOccupied => 3,
            Self::BadGeneration => 4,
            Self::SlotAddress | Self::SlotAddressAlign => 5,
            Self::Digest => 6,
            Self::MissingMap | Self::MapsRequired | Self::MapCheck(_) | Self::Map(_) => 7,
            Self::NoStaged => 8,
            Self::RollbackFailed => 10,
            _ => 12,
        }
    }
}

impl fmt::Display for XdpSwapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<FlashError> for XdpSwapError {
    fn from(_: FlashError) -> Self {
        Self::Storage
    }
}
