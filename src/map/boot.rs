//! Boot-time map creation from bpf.toml codegen.

use super::builder::CompleteBuilder;
use super::runtime::{Error, MapRt};

/// Called by [`CompleteBuilder::from_bpf_toml`].
pub async fn boot_maps_from_codegen(rt: &mut MapRt) -> Result<(), Error> {
    super::init_firmware_maps_via_builder(rt).await
}

/// Legacy entry — opens runtime write lock then runs codegen boot path.
pub async fn init_firmware_maps() -> Result<(), Error> {
    let mut guard = super::runtime::MAP_RT.write().await;
    let rt = guard.as_mut().ok_or(Error::NotInitialized)?;
    super::init_firmware_maps_via_builder(rt).await
}
