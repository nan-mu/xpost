//! Hand-written map create path (not called from `main`; compiles for Stage3).

use xpost::map::{MapBuilder, MapRt, MapValue};

/// Demo ARRAY value layout for hand `create`.
#[derive(MapValue)]
#[repr(C)]
pub struct DemoCnt {
    pub v: u64,
}

/// Create a small ARRAY map by hand (no bpf.toml).
///
/// Kept off the default boot path; call instead of `lease(None)` when experimenting
/// with maps + `lease(Some(&mut maps))`.
#[allow(dead_code)]
pub fn create_demo_array(rt: &mut MapRt) -> Result<xpost::map::MapId, xpost::map::Error> {
    let builder = MapBuilder::array()
        .with_name("demo_cnt")
        .with_value::<DemoCnt>()
        .with_capacity(4);
    rt.create(builder.into_complete())
}
