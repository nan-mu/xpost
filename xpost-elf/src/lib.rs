//! Compact ELF V0 decode/encode + firmware mem-map contract blob (`XPM1`).
//!
//! Compact ELF layout：EHDR | SHT×4 | `.xpost.meta` | image | names | pad | digest。
//!
//! 冷区扫描分两档失败：
//! - `HardFail`：连容器边界都算不出（非 ELF / 读失败 / 长度溢出）→ 停扫。
//! - `SoftSkip`：`file_len` 可信，但不是合法 V0 profile → 按长度跳过，继续找下一个。
//!
//! 固件 ELF 上的 [mem_map]（`.xpost.mem`）只含编解码；读写 ELF 文件由 xpost-build 负责。
//!
//! 不含 `XdpBuilder` / map runtime；调用方把 [`ParsedContainer`] 再填进固件侧。

#![no_std]

#[path = "encode.rs"]
mod compact;
mod mem_map;

pub use compact::{
    EncodeError, ImageMetadataV0, MAX_CONTAINER_SIZE, MAX_ENTRIES, MAX_META_SIZE, MapDescriptorV0,
    SECTION_NAMES, build_container,
};
pub use mem_map::{MAGIC, MemMapError, MemMapV0, SECTION_NAME, WIRE_SIZE, decode, encode};

use sha2::{Digest, Sha256};

/// Compact ELF V0：每张 image 最多 8 张 map（host `MAX_MAPS`）。
pub const MAX_MAPS: usize = 8;
/// 单条 key/value layout 最多 32 个 descriptor。
pub const MAX_LAYOUT: usize = 32;
/// 名字 1..=31 ASCII；heapless 容量多留 1。
pub const MAX_NAME: usize = 31;

/// 单次 SRAM 读上限；同时是 `.xpost.meta` 最大长度（host `MAX_META_SIZE`）。
const WIRE_BUF: usize = 1024;
const ELF_MAGIC: [u8; 4] = [0x7f, b'E', b'L', b'F'];
/// [`CompactElfFixed`] 已覆盖的 `.xpost.meta` 前缀：version / machine / map_count / helper。
const META_PREFIX: usize = 8;
/// Compact ELF V0 的 `.shstrtab`（host `SECTION_NAMES`）固定 36 字节。
const SHSTRTAB_LEN: usize = 36;
/// meta 最短：8 字节前缀 + 最短 4 字节对齐名字（`x\0\0\0`）。
const MIN_META_SIZE: usize = 12;
const XPOST_VERSION: u8 = 0;
/// V0 machine 码：1（当前实例是 ESP32-C3）。
const XPOST_MACHINE: u8 = 1;

/// 解析器只需要随机读和总长度；不包含 erase / write。
pub trait ByteSource {
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    fn read(&mut self, offset: usize, out: &mut [u8]) -> bool;
}

impl ByteSource for &[u8] {
    fn len(&self) -> usize {
        <[u8]>::len(self)
    }

    fn read(&mut self, offset: usize, out: &mut [u8]) -> bool {
        let Some(end) = offset.checked_add(out.len()) else {
            return false;
        };
        let Some(src) = self.get(offset..end) else {
            return false;
        };
        out.copy_from_slice(src);
        true
    }
}

// 从容器起点覆盖：EHDR + 4×shdr + `.xpost.meta` 的 8 字节前缀。
xpost_macros::flash_layout! {
    CompactElfFixed {
        skip 52;              // Elf32Header
        skip 40;              // shdr[0] NULL
        skip 16;              // shdr[1] name..addr
        meta_offset: le<u32>;
        meta_size: le<u32>;
        skip 16;
        skip 12;              // shdr[2] name,type,flags
        slot_address: le<u32>;
        image_offset: le<u32>;
        image_size: le<u32>;
        skip 16;
        skip 16;              // shdr[3] name..addr
        names_offset: le<u32>;
        skip 20;
        xpost_version: u8;
        machine: u8;
        map_count: le<u16>;
        helper_address: le<u32>;
    }
}

/// 固定头长度（EHDR + SHT×4 + meta 前缀）。冷区扫描步进用。
pub const FIXED_WIRE_SIZE: usize = CompactElfFixed::WIRE_SIZE;

// MapDescriptorV0 定长头；名字在其前，key/value layout 在其后。
xpost_macros::flash_layout! {
    MapDescFixed {
        map_type: u8;
        len: le<u24>;
        map_vma: le<u32>;
        key_layout_len: le<u32>;
        value_layout_len: le<u32>;
    }
}

// 容器末尾的 image SHA-256。digest 范围只有 raw image，不含 meta / SHT。
xpost_macros::flash_layout! {
    ImageDigest {
        sha256: bytes<32>;
    }
}

/// 冷区扫描一步的结果。`file_len` 只在还能定位容器尾时给出。
#[derive(Debug)]
#[allow(clippy::large_enum_variant)] // Ok carries ParsedContainer by value; Box would change the API.
pub enum ScanOutcome {
    /// 无法判断容器边界，调用方应终止扫描。
    HardFail,
    /// 容器长度可信，但内容不是合法 V0 profile；调用方按 `file_len` 跳过。
    SoftSkip {
        file_len: usize,
    },
    Ok(ParsedContainer),
}

/// 一张已通过 wire / digest 校验的 Compact ELF 容器（相对 `at` 的偏移）。
#[derive(Debug)]
pub struct ParsedContainer {
    pub name: heapless::String<{ MAX_NAME + 1 }>,
    pub slot_address: usize,
    pub helper_address: usize,
    pub maps: heapless::Vec<ParsedMap, MAX_MAPS>,
    pub image_offset: usize,
    pub image_size: usize,
    pub file_len: usize,
    pub digest: [u8; 32],
}

/// 一条 MapDescriptorV0；空 layout 为 `None`（ARRAY 的 implicit key）。
#[derive(Debug)]
pub struct ParsedMap {
    pub name: heapless::String<{ MAX_NAME + 1 }>,
    pub map_type: u8,
    pub len: u32,
    pub vma: u32,
    pub key: Option<heapless::Vec<u8, MAX_LAYOUT>>,
    pub value: Option<heapless::Vec<u8, MAX_LAYOUT>>,
}

/// header 校验通过后，后续 meta / digest 需要的偏移。
struct WireLayout {
    meta_size: usize,
    slot_address: usize,
    image_offset: usize,
    image_size: usize,
    map_count: u16,
    helper: usize,
    digest_off: usize,
}

/// 一条已切片的 MapDescriptorV0；layout 切片借自 meta buffer。
struct MapRecord<'a> {
    name: &'a str,
    map_type: u8,
    len: u32,
    vma: u32,
    key: Option<&'a [u8]>,
    value: Option<&'a [u8]>,
}

#[inline(always)]
pub(crate) const fn align4(x: usize) -> usize {
    (x + 3) & !3
}

fn src_read(src: &mut dyn ByteSource, abs: usize, out: &mut [u8]) -> bool {
    let end = match abs.checked_add(out.len()) {
        Some(v) => v,
        None => return false,
    };
    if end > src.len() {
        return false;
    }
    src.read(abs, out)
}

/// 按 4 字节对齐从 source 读，把逻辑区间交给 `visit`。
/// Flash 读通常要求字对齐；`base` 可以不是 4 的倍数。
fn visit_bytes(
    src: &mut dyn ByteSource,
    base: usize,
    len: usize,
    mut visit: impl FnMut(&[u8]),
) -> bool {
    #[repr(C, align(4))]
    struct Block([u8; 68]);

    if len == 0 {
        return true;
    }
    let end = match base.checked_add(len) {
        Some(v) => v,
        None => return false,
    };
    if ((end + 3) & !3) > src.len() {
        return false;
    }
    let mut block = Block([0; 68]);
    let mut current = base;
    while current < end {
        let n = (end - current).min(64);
        let prefix = current & 3;
        let physical_start = current & !3;
        let physical_len = (prefix + n + 3) & !3;
        if physical_start.saturating_add(physical_len) > src.len() {
            return false;
        }
        if !src.read(physical_start, &mut block.0[..physical_len]) {
            return false;
        }
        visit(&block.0[prefix..prefix + n]);
        current += n;
    }
    true
}

/// 1..=31 ASCII 字节，NUL 后填零到 4 字节对齐。返回 `(name, 编码长度)`。
fn take_cstring4(rest: &[u8]) -> Option<(&str, usize)> {
    let nul = rest.iter().position(|&c| c == 0)?;
    if nul == 0 || nul > MAX_NAME {
        return None;
    }
    let encoded = (nul + 1 + 3) & !3;
    if rest.len() < encoded {
        return None;
    }
    let raw = &rest[..nul];
    if !raw.is_ascii() || rest[nul + 1..encoded].iter().any(|&c| c != 0) {
        return None;
    }
    let name = core::str::from_utf8(raw).ok()?;
    Some((name, encoded))
}

fn take_cstr<'a>(rest: &mut &'a [u8]) -> Option<&'a str> {
    let (name, n) = take_cstring4(rest)?;
    *rest = &rest[n..];
    Some(name)
}

/// `n == 0` 返回空切片且不前进（给 pad=0 用）。layout 为空要另编成 `None`。
fn take_slice<'a>(rest: &mut &'a [u8], n: usize) -> Option<&'a [u8]> {
    if n == 0 {
        return Some(&[]);
    }
    if rest.len() < n {
        return None;
    }
    let (head, tail) = rest.split_at(n);
    *rest = tail;
    Some(head)
}

fn nonzero_align4(addr: usize) -> bool {
    addr != 0 && addr & 3 == 0
}

fn owned_name(s: &str) -> Option<heapless::String<{ MAX_NAME + 1 }>> {
    heapless::String::try_from(s).ok()
}

fn owned_layout(bytes: Option<&[u8]>) -> Option<Option<heapless::Vec<u8, MAX_LAYOUT>>> {
    match bytes {
        None => Some(None),
        Some(b) => {
            let mut v = heapless::Vec::new();
            v.extend_from_slice(b).ok()?;
            Some(Some(v))
        }
    }
}

/// 从偏移 `at` 尝试解析一个 Compact ELF 容器。
pub fn scan_at(src: &mut dyn ByteSource, at: usize) -> ScanOutcome {
    let mut buf = [0u8; WIRE_BUF];
    // 读固定头 / 推 file_len 失败 → 后面的字节也无法当容器对齐，停扫。
    let Some(fixed) = read_fixed(src, at, &mut buf) else {
        return ScanOutcome::HardFail;
    };
    let Some((digest_off, file_len)) = container_span(&fixed) else {
        return ScanOutcome::HardFail;
    };
    if at.saturating_add(file_len) > src.len() {
        return ScanOutcome::HardFail;
    }

    // file_len 已可信：profile / meta / digest 任一不过只跳过这一格。
    let Some(layout) = decode_layout(&fixed, digest_off) else {
        return ScanOutcome::SoftSkip { file_len };
    };
    let Some((name, maps)) = parse_meta(src, at, &layout, &mut buf) else {
        return ScanOutcome::SoftSkip { file_len };
    };
    let Some(digest) = verify_digest(src, at, &layout, &mut buf) else {
        return ScanOutcome::SoftSkip { file_len };
    };

    ScanOutcome::Ok(ParsedContainer {
        name,
        slot_address: layout.slot_address,
        helper_address: layout.helper,
        maps,
        image_offset: layout.image_offset,
        image_size: layout.image_size,
        file_len,
        digest,
    })
}

/// 读固定头并核对 ELF magic。失败 → HardFail。
fn read_fixed(
    src: &mut dyn ByteSource,
    at: usize,
    buf: &mut [u8; WIRE_BUF],
) -> Option<CompactElfFixed> {
    if !src_read(src, at, &mut buf[..CompactElfFixed::WIRE_SIZE]) {
        return None;
    }
    if buf[..ELF_MAGIC.len()] != ELF_MAGIC {
        return None;
    }
    CompactElfFixed::from_bytes(buf)
}

/// 用 `.shstrtab` 尾对齐后的 digest 位置推容器长度。
/// 故意不校验 `names_offset` 是否合法：即使 profile 错了，也要能 SoftSkip。
fn container_span(f: &CompactElfFixed) -> Option<(usize, usize)> {
    let names_offset = f.names_offset() as usize;
    let digest_off = align4(names_offset.saturating_add(SHSTRTAB_LEN));
    let file_len = digest_off.checked_add(ImageDigest::WIRE_SIZE)?;
    Some((digest_off, file_len))
}

/// Compact ELF V0 几何：meta 紧贴固定头，image / names 顺序拼接。
///
/// [`CompactElfFixed`] 把 `.xpost.meta` 的 8 字节前缀叠进了 `WIRE_SIZE`，
/// 所以 `meta_offset` 必须等于 `WIRE_SIZE - META_PREFIX`。
fn decode_layout(f: &CompactElfFixed, digest_off: usize) -> Option<WireLayout> {
    let meta_offset = f.meta_offset() as usize;
    let meta_size = f.meta_size() as usize;
    let slot_address = f.slot_address() as usize;
    let image_offset = f.image_offset() as usize;
    let image_size = f.image_size() as usize;
    let names_offset = f.names_offset() as usize;
    let helper = f.helper_address() as usize;
    let map_count = f.map_count();
    // 固定头已含 meta 前缀，剩余 meta 从 WIRE_SIZE 起读。
    let meta_abs = CompactElfFixed::WIRE_SIZE - META_PREFIX;

    if meta_offset != meta_abs
        || !(MIN_META_SIZE..=WIRE_BUF).contains(&meta_size)
        || !meta_size.is_multiple_of(4)
    {
        return None;
    }
    // slot 字对齐；image 紧跟 meta；names 紧跟 image。
    if !nonzero_align4(slot_address)
        || image_size == 0
        || image_offset != meta_abs + meta_size
        || slot_address.checked_add(image_size).is_none()
        || names_offset != image_offset + image_size
    {
        return None;
    }
    if f.xpost_version() != XPOST_VERSION || f.machine() != XPOST_MACHINE {
        return None;
    }
    if map_count as usize > MAX_MAPS || !nonzero_align4(helper) {
        return None;
    }
    Some(WireLayout {
        meta_size,
        slot_address,
        image_offset,
        image_size,
        map_count,
        helper,
        digest_off,
    })
}

/// 解析 `.xpost.meta` 剩余部分：XDP 名 + `map_count` 条 MapDescriptorV0。
///
/// 名字 / `map_vma` 必须唯一。`buf` 里的 `&str` 只在本函数返回前有效。
fn parse_meta(
    src: &mut dyn ByteSource,
    at: usize,
    layout: &WireLayout,
    buf: &mut [u8; WIRE_BUF],
) -> Option<(
    heapless::String<{ MAX_NAME + 1 }>,
    heapless::Vec<ParsedMap, MAX_MAPS>,
)> {
    let tail = layout.meta_size - META_PREFIX;
    if !src_read(src, at + CompactElfFixed::WIRE_SIZE, &mut buf[..tail]) {
        return None;
    }
    let mut rest = &buf[..tail];
    let xdp_name = take_cstr(&mut rest)?;

    let mut maps = heapless::Vec::<ParsedMap, MAX_MAPS>::new();
    let mut seen_names = heapless::Vec::<&str, MAX_MAPS>::new();
    let mut seen_vmas = heapless::Vec::<u32, MAX_MAPS>::new();

    for _ in 0..layout.map_count {
        let rec = take_map_record(&mut rest)?;
        // name / map_vma 都是 AOT 绑定键，重复即非规范。
        if seen_names.contains(&rec.name) || seen_vmas.contains(&rec.vma) {
            return None;
        }
        let _ = seen_names.push(rec.name);
        let _ = seen_vmas.push(rec.vma);
        maps.push(ParsedMap {
            name: owned_name(rec.name)?,
            map_type: rec.map_type,
            len: rec.len,
            vma: rec.vma,
            key: owned_layout(rec.key)?,
            value: owned_layout(rec.value)?,
        })
        .ok()?;
    }
    // meta 必须恰好消耗完，不允许尾部垃圾。
    if !rest.is_empty() {
        return None;
    }
    Some((owned_name(xdp_name)?, maps))
}

/// 一条 MapDescriptorV0：对齐 C 字符串 + 定长头 + key/value layout + 填零。
/// 空 layout 编成 `None`（ARRAY 的 implicit key），不是空切片。
fn take_map_record<'a>(rest: &mut &'a [u8]) -> Option<MapRecord<'a>> {
    let mark = rest.len();
    let name = take_cstr(rest)?;
    let fixed = MapDescFixed::from_bytes(rest)?;
    *rest = &rest[MapDescFixed::WIRE_SIZE..];

    let key_len = fixed.key_layout_len() as usize;
    let value_len = fixed.value_layout_len() as usize;
    if rest.len() < key_len + value_len {
        return None;
    }
    let key = match take_slice(rest, key_len)? {
        [] => None,
        bytes => Some(bytes),
    };
    let value = match take_slice(rest, value_len)? {
        [] => None,
        bytes => Some(bytes),
    };

    let consumed = mark - rest.len();
    let pad = align4(consumed) - consumed;
    let padding = take_slice(rest, pad)?;
    // 对齐填充必须是零；非零视为非规范容器。
    if padding.iter().any(|&c| c != 0) {
        return None;
    }

    Some(MapRecord {
        name,
        map_type: fixed.map_type(),
        len: fixed.len(),
        vma: fixed.map_vma(),
        key,
        value,
    })
}

/// digest 只覆盖 raw image 字节。对不上 → SoftSkip（可能是半写入或损坏槽）。
fn verify_digest(
    src: &mut dyn ByteSource,
    at: usize,
    layout: &WireLayout,
    buf: &mut [u8; WIRE_BUF],
) -> Option<[u8; 32]> {
    if !src_read(
        src,
        at + layout.digest_off,
        &mut buf[..ImageDigest::WIRE_SIZE],
    ) {
        return None;
    }
    let digest = ImageDigest::from_bytes(buf)?;
    let mut hasher = Sha256::new();
    if !visit_bytes(src, at + layout.image_offset, layout.image_size, |chunk| {
        hasher.update(chunk);
    }) {
        return None;
    }
    let computed: [u8; 32] = hasher.finalize().into();
    (computed == *digest.sha256()).then_some(computed)
}
