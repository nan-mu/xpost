//! Fail-closed XDP runtime errors.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum XdpError {
    AlreadyInitialized,
    NotInitialized,
    NoPrimarySlot,
    MultiplePrimarySlots,
    OverlappingSlots,
    TooManySlots,
    TooManyImages,
    EmptySlot,
    SlotAlignment,
    SlotOverflow,
    SlotMismatch,
    SlotOccupied,
    ImageTooLarge,
    ImageEmpty,
    TableFull,
    TableIdOverflow,
    UnknownImage,
    UnknownSlot,
    Leased,
    NotLeased,
    PrimaryEmpty,
    RequiredMapAbsent,
    MapLease,
    AbiVersion,
    Arch,
    HelperAddress,
    HelperAddressMismatch,
    SlotAddress,
    SlotAddressAlign,
    MapsRequired,
    MissingMap,
    /// Map present but type/layout/len/heap_size disagree with [`crate::xdp::RequiredMap`].
    TypeMismatch,
    ImageDigest,
    /// Reserved: image magic / header mismatch (layout TODO).
    ImageMagic,
    /// Reserved: image bytes disagree with [`XdpMeta`] beyond magic.
    ImageMismatch,
    LinkedAddress,
    Storage,
    ScratchTooSmall,
    MetaDisagree,
    HotSwitch,
    InvalidName,
    DuplicateName,
    NotImplemented,
    /// Deprecated alias kept for source compatibility; prefer registering builders first.
    BpfTomlNotLoaded,
}

impl XdpError {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AlreadyInitialized => "already initialized",
            Self::NotInitialized => "not initialized",
            Self::NoPrimarySlot => "no primary slot",
            Self::MultiplePrimarySlots => "multiple primary slots",
            Self::OverlappingSlots => "overlapping slots",
            Self::TooManySlots => "too many slots",
            Self::TooManyImages => "too many images",
            Self::EmptySlot => "empty slot",
            Self::SlotAlignment => "slot alignment",
            Self::SlotOverflow => "slot overflow",
            Self::SlotMismatch => "slot address mismatch",
            Self::SlotOccupied => "slot occupied",
            Self::ImageTooLarge => "image exceeds slot/table",
            Self::ImageEmpty => "image empty",
            Self::TableFull => "table full",
            Self::TableIdOverflow => "table id overflow",
            Self::UnknownImage => "unknown image",
            Self::UnknownSlot => "unknown slot",
            Self::Leased => "operation while leased",
            Self::NotLeased => "not leased",
            Self::PrimaryEmpty => "primary empty",
            Self::RequiredMapAbsent => "required map absent",
            Self::MapLease => "map lease failed",
            Self::AbiVersion => "abi version",
            Self::Arch => "arch mismatch",
            Self::HelperAddress => "helper address",
            Self::HelperAddressMismatch => "helper address mismatch",
            Self::SlotAddress => "slot address",
            Self::SlotAddressAlign => "slot address align",
            Self::MapsRequired => "maps required but none",
            Self::MissingMap => "missing map",
            Self::TypeMismatch => "map type/layout mismatch",
            Self::ImageDigest => "image digest mismatch",
            Self::ImageMagic => "image magic mismatch",
            Self::ImageMismatch => "image storage mismatch",
            Self::LinkedAddress => "linked address mismatch",
            Self::Storage => "storage error",
            Self::ScratchTooSmall => "scratch too small",
            Self::MetaDisagree => "slot/meta disagree",
            Self::HotSwitch => "hot_switch rejected",
            Self::InvalidName => "invalid name",
            Self::DuplicateName => "duplicate name",
            Self::NotImplemented => "not implemented",
            Self::BpfTomlNotLoaded => "bpf.toml boot not loaded",
        }
    }
}

impl core::fmt::Display for XdpError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}
