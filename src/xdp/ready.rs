//! Shared readiness failures for [`crate::xdp::XdpRt::pre_lease`].
//!
//! Converted into [`XdpSwitchError`] / [`XdpLeaseError`] at the public boundary.

use crate::error::{MapCheckError, XdpLeaseError, XdpSwitchError};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReadyError {
    NoPrimarySlot,
    UnknownSlot,
    UnknownImage,
    PrimaryEmpty,
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
    MapCheck(MapCheckError),
}

impl From<ReadyError> for XdpSwitchError {
    fn from(e: ReadyError) -> Self {
        match e {
            ReadyError::NoPrimarySlot => Self::NoPrimarySlot,
            ReadyError::UnknownSlot => Self::UnknownSlot,
            ReadyError::UnknownImage => Self::UnknownImage,
            ReadyError::PrimaryEmpty => Self::PrimaryEmpty,
            ReadyError::SlotMismatch => Self::SlotMismatch,
            ReadyError::ImageEmpty => Self::ImageEmpty,
            ReadyError::ImageTooLarge => Self::ImageTooLarge,
            ReadyError::Arch => Self::Arch,
            ReadyError::HelperAddress => Self::HelperAddress,
            ReadyError::HelperAddressMismatch => Self::HelperAddressMismatch,
            ReadyError::SlotAddress => Self::SlotAddress,
            ReadyError::SlotAddressAlign => Self::SlotAddressAlign,
            ReadyError::MapsRequired => Self::MapsRequired,
            ReadyError::MissingMap => Self::MissingMap,
            ReadyError::MapCheck(e) => Self::MapCheck(e),
        }
    }
}

impl From<ReadyError> for XdpLeaseError {
    fn from(e: ReadyError) -> Self {
        match e {
            ReadyError::NoPrimarySlot => Self::NoPrimarySlot,
            ReadyError::UnknownSlot => Self::UnknownSlot,
            ReadyError::UnknownImage => Self::UnknownImage,
            ReadyError::PrimaryEmpty => Self::PrimaryEmpty,
            ReadyError::SlotMismatch => Self::SlotMismatch,
            ReadyError::ImageEmpty => Self::ImageEmpty,
            ReadyError::ImageTooLarge => Self::ImageTooLarge,
            ReadyError::Arch => Self::Arch,
            ReadyError::HelperAddress => Self::HelperAddress,
            ReadyError::HelperAddressMismatch => Self::HelperAddressMismatch,
            ReadyError::SlotAddress => Self::SlotAddress,
            ReadyError::SlotAddressAlign => Self::SlotAddressAlign,
            ReadyError::MapsRequired => Self::MapsRequired,
            ReadyError::MissingMap => Self::MissingMap,
            ReadyError::MapCheck(e) => Self::MapCheck(e),
        }
    }
}
