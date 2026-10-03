#![no_std]

pub mod mem_map {
    include!(concat!(env!("OUT_DIR"), "/mem_map.rs"));
}

#[allow(dead_code)]
mod hot_slots {
    include!(concat!(env!("OUT_DIR"), "/hot_slots.rs"));
}

/// Keep IRAM slot buffers in the link (`KEEP(.xdp.hotload_*)`).
pub fn slot_link_addrs() -> [usize; mem_map::HOT_SLOT_COUNT] {
    hot_slots::slot_link_addrs()
}
