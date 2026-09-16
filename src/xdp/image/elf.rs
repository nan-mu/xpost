//! Compact ELF V0：[`xpost_elf`] decode + cold table → [`XdpBuilder`]。

use alloc::string::String;

use xpost_elf::{ByteSource, ParsedContainer, ScanOutcome, scan_at};

use crate::{
    map::MapMeta,
    xdp::{
        XdpTableStorage,
        image::builder::{Incomplete, XdpBuilder, map_builder_from_descriptor},
        image::meta::CollectedMetas,
        state::MAX_IMAGES,
    },
};

struct TableSource<'a>(&'a mut dyn XdpTableStorage);

impl ByteSource for TableSource<'_> {
    fn len(&self) -> usize {
        self.0.capacity()
    }

    fn read(&mut self, offset: usize, out: &mut [u8]) -> bool {
        self.0.read(offset, out).is_ok()
    }
}

fn fill_from_parsed(b: &mut XdpBuilder, parsed: &ParsedContainer, at: usize) -> Option<()> {
    b.required_maps.clear();
    b.aot_binds.clear();
    for rec in &parsed.maps {
        let builder = map_builder_from_descriptor(
            rec.name.as_str(),
            rec.map_type,
            rec.len,
            rec.key.as_deref(),
            rec.value.as_deref(),
        )
        .ok()?;
        let meta_row = MapMeta::from_builder(builder).ok()?;
        b.required_maps.push(meta_row).ok()?;
        b.aot_binds
            .push((String::from(rec.name.as_str()), rec.vma as usize))
            .ok()?;
    }
    b.name = Some(String::from(parsed.name.as_str()));
    b.link_addr = parsed.slot_address;
    b.helper_address = parsed.helper_address;
    b.image_abs = at.saturating_add(parsed.image_offset);
    b.image_len = parsed.image_size;
    b.container_abs = at;
    b.container_len = parsed.file_len;
    b.image_digest = parsed.digest;
    Some(())
}

/// 从冷区起点线性扫描。HardFail 停；SoftSkip / `finish_meta` 失败都按 `file_len` 前进。
pub(crate) fn collect_metas(table: &mut dyn XdpTableStorage) -> CollectedMetas {
    let mut out = CollectedMetas::new();
    let mut cursor = 0usize;
    let mut src = TableSource(table);
    while cursor.saturating_add(xpost_elf::FIXED_WIRE_SIZE) <= src.len() {
        if out.len() >= MAX_IMAGES {
            break;
        }
        match scan_at(&mut src, cursor) {
            ScanOutcome::HardFail => break, // 无法对齐下一容器
            ScanOutcome::SoftSkip { file_len } => {
                cursor = cursor.saturating_add(file_len);
            }
            ScanOutcome::Ok(parsed) => {
                let file_len = parsed.file_len;
                let mut b = XdpBuilder::<Incomplete>::for_elf(out.len());
                match fill_from_parsed(&mut b, &parsed, cursor) {
                    Some(()) => match b.finish_meta() {
                        Some(meta) => {
                            cursor = cursor.saturating_add(meta.container_len);
                            if out.push(meta).is_err() {
                                break;
                            }
                        }
                        None => {
                            cursor = cursor.saturating_add(file_len);
                        }
                    },
                    None => {
                        cursor = cursor.saturating_add(file_len);
                    }
                }
            }
        }
    }
    out
}
