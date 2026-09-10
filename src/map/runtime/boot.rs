//! Boot-time map creation from bpf.toml codegen.

use super::{Error, MAP_RT, MapRt};

/// Called by [`MapRt::from_bpf_toml`].
pub async fn boot_maps_from_codegen(rt: &mut MapRt) -> Result<(), Error> {
    crate::map::init_firmware_maps_via_builder(rt).await
}

/// Opens the global runtime lock then runs codegen boot path.
pub async fn init_firmware_maps() -> Result<(), Error> {
    let lock = MAP_RT.get().await;
    let mut rt = lock.lock().await;
    crate::map::init_firmware_maps_via_builder(&mut rt).await
}
