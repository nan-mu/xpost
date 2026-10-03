//! Fixed wire layouts via [`xpost_macros::flash_layout`] (SRAM slice views).
//!
//! Normative: `libbpf-mcu/docs/xpost_compact_elf.proto` (XpostCompactElfV0).

xpost_macros::flash_layout! {
    pub(crate) Elf32Header {
        ident: bytes<16>;
        elf_type: le<u16>;
        machine: le<u16>;
        elf_version: le<u32>;
        entry: le<u32>;
        phoff: le<u32>;
        shoff: le<u32>;
        flags: le<u32>;
        ehsize: le<u16>;
        phentsize: le<u16>;
        phnum: le<u16>;
        shentsize: le<u16>;
        shnum: le<u16>;
        shstrndx: le<u16>;
    }
}

xpost_macros::flash_layout! {
    pub(crate) ElfSectionHeader {
        name: le<u32>;
        kind: le<u32>;
        flags: le<u32>;
        addr: le<u32>;
        offset: le<u32>;
        size: le<u32>;
        link: le<u32>;
        info: le<u32>;
        addralign: le<u32>;
        entsize: le<u32>;
    }
}

xpost_macros::flash_layout! {
    pub(crate) ElfDigest {
        sha256: bytes<32>;
    }
}

xpost_macros::flash_layout! {
    pub(crate) XpostMetaPrefix {
        xpost_version: u8;
        machine: u8;
        map_count: le<u16>;
        helper_address: le<u32>;
    }
}
