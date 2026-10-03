#![no_std]

//! Board artifacts from `xpost-build` sync (`partitions.csv` + crate metadata).

pub mod map_crud;

pub mod mem_map {
    include!(concat!(env!("OUT_DIR"), "/mem_map.rs"));
}
