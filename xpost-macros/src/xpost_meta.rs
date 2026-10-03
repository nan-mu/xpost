//! `xpost_meta!` — owning `.xpost.meta` buffer + fixed prefix getters + map cursor.

use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::{Error, Ident, LitInt, Result, Token, Visibility, braced, parse_macro_input};

enum FieldKind {
    U8,
    LeU16,
    LeU32,
}

struct Field {
    name: Ident,
    kind: FieldKind,
    offset: usize,
}

struct MetaInput {
    vis: Visibility,
    name: Ident,
    capacity: usize,
    max_maps: usize,
    fields: Vec<Field>,
    prefix_size: usize,
}

impl Parse for MetaInput {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let vis: Visibility = input.parse()?;
        let name: Ident = input.parse()?;
        let content;
        braced!(content in input);

        let mut capacity: Option<usize> = None;
        let mut max_maps: Option<usize> = None;
        let mut fields = Vec::new();
        let mut cursor = 0usize;
        let mut seen = std::collections::HashSet::<String>::new();

        while !content.is_empty() {
            let key: Ident = content.parse()?;
            content.parse::<Token![:]>()?;

            if key == "capacity" {
                let n: LitInt = content.parse()?;
                content.parse::<Token![;]>()?;
                capacity = Some(n.base10_parse()?);
                continue;
            }
            if key == "max_maps" {
                let n: LitInt = content.parse()?;
                content.parse::<Token![;]>()?;
                max_maps = Some(n.base10_parse()?);
                continue;
            }

            if !seen.insert(key.to_string()) {
                return Err(Error::new(key.span(), "duplicate field"));
            }

            let ty: Ident = content.parse()?;
            let (kind, size) = match ty.to_string().as_str() {
                "u8" => (FieldKind::U8, 1usize),
                "le" => {
                    content.parse::<Token![<]>()?;
                    let inner: Ident = content.parse()?;
                    content.parse::<Token![>]>()?;
                    match inner.to_string().as_str() {
                        "u16" => (FieldKind::LeU16, 2),
                        "u32" => (FieldKind::LeU32, 4),
                        _ => {
                            return Err(Error::new(inner.span(), "meta prefix: le<u16>|le<u32>"));
                        }
                    }
                }
                _ => {
                    return Err(Error::new(
                        ty.span(),
                        "meta prefix fields: u8, le<u16>, le<u32>",
                    ));
                }
            };
            content.parse::<Token![;]>()?;
            fields.push(Field {
                name: key,
                kind,
                offset: cursor,
            });
            cursor += size;
        }

        let capacity =
            capacity.ok_or_else(|| Error::new(name.span(), "missing `capacity: N;`"))?;
        let max_maps = max_maps.unwrap_or(8);
        if fields.is_empty() {
            return Err(Error::new(name.span(), "need at least one prefix field"));
        }

        Ok(Self {
            vis,
            name,
            capacity,
            max_maps,
            fields,
            prefix_size: cursor,
        })
    }
}

pub fn expand(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let meta = parse_macro_input!(input as MetaInput);
    match expand_meta(meta) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

fn expand_meta(meta: MetaInput) -> Result<TokenStream2> {
    let MetaInput {
        vis,
        name,
        capacity,
        max_maps,
        fields,
        prefix_size,
    } = meta;

    let mut getters = Vec::new();
    for f in &fields {
        let fname = &f.name;
        let off = f.offset;
        let get = match f.kind {
            FieldKind::U8 => quote! {
                #[inline(always)]
                pub(crate) fn #fname(&self) -> u8 {
                    self.bytes[#off]
                }
            },
            FieldKind::LeU16 => quote! {
                #[inline(always)]
                pub(crate) fn #fname(&self) -> u16 {
                    u16::from_le_bytes([self.bytes[#off], self.bytes[#off + 1]])
                }
            },
            FieldKind::LeU32 => quote! {
                #[inline(always)]
                pub(crate) fn #fname(&self) -> u32 {
                    u32::from_le_bytes([
                        self.bytes[#off],
                        self.bytes[#off + 1],
                        self.bytes[#off + 2],
                        self.bytes[#off + 3],
                    ])
                }
            },
        };
        getters.push(get);
    }

    Ok(quote! {
        #vis struct #name {
            bytes: ::heapless::Vec<u8, #capacity>,
        }

        #[derive(Clone, Copy, Debug)]
        pub(crate) struct MapDescriptor<'a> {
            pub name: &'a str,
            pub map_type: u8,
            pub len: u32,
            pub map_vma: u32,
            pub key_layout: Option<&'a [u8]>,
            pub value_layout: Option<&'a [u8]>,
        }

        struct MetaCursor<'a> {
            rest: &'a [u8],
        }

        impl<'a> MetaCursor<'a> {
            #[inline(always)]
            fn new(bytes: &'a [u8]) -> Self {
                Self { rest: bytes }
            }
            #[inline(always)]
            fn remaining(&self) -> usize {
                self.rest.len()
            }
            fn take_bytes(&mut self, len: usize) -> Result<&'a [u8], ElfWireError> {
                if self.rest.len() < len {
                    return Err(ElfWireError::Truncated);
                }
                let (value, rest) = self.rest.split_at(len);
                self.rest = rest;
                Ok(value)
            }
            fn take_u8(&mut self) -> Result<u8, ElfWireError> {
                Ok(self.take_bytes(1)?[0])
            }
            fn take_u24_le(&mut self) -> Result<u32, ElfWireError> {
                let b = self.take_bytes(3)?;
                Ok((b[0] as u32) | ((b[1] as u32) << 8) | ((b[2] as u32) << 16))
            }
            fn take_u32_le(&mut self) -> Result<u32, ElfWireError> {
                let b = self.take_bytes(4)?;
                Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            }
            fn take_cstring4(&mut self) -> Result<&'a str, ElfWireError> {
                const MAX_NAME: usize = 31;
                let nul = self
                    .rest
                    .iter()
                    .position(|&c| c == 0)
                    .ok_or(ElfWireError::Truncated)?;
                if nul == 0 || nul > MAX_NAME {
                    return Err(ElfWireError::Truncated);
                }
                let encoded = (nul + 1 + 3) & !3;
                if self.rest.len() < encoded {
                    return Err(ElfWireError::Truncated);
                }
                let raw = &self.rest[..nul];
                if !raw.is_ascii() || self.rest[nul + 1..encoded].iter().any(|&c| c != 0) {
                    return Err(ElfWireError::Truncated);
                }
                let name = ::core::str::from_utf8(raw).map_err(|_| ElfWireError::Truncated)?;
                self.rest = &self.rest[encoded..];
                Ok(name)
            }
            fn align4_consumed(&mut self, consumed: usize) -> Result<(), ElfWireError> {
                let pad = ((consumed + 3) & !3) - consumed;
                if pad == 0 {
                    return Ok(());
                }
                let bytes = self.take_bytes(pad)?;
                if bytes.iter().any(|&c| c != 0) {
                    return Err(ElfWireError::Truncated);
                }
                Ok(())
            }
        }

        pub(crate) struct MapIter<'a> {
            cursor: MetaCursor<'a>,
            remaining: u16,
        }

        impl<'a> MapIter<'a> {
            pub(crate) fn finish(self) -> Result<(), ElfWireError> {
                if self.remaining != 0 || self.cursor.remaining() != 0 {
                    return Err(ElfWireError::Truncated);
                }
                Ok(())
            }
        }

        impl<'a> ::core::iter::Iterator for MapIter<'a> {
            type Item = Result<MapDescriptor<'a>, ElfWireError>;
            fn next(&mut self) -> Option<Self::Item> {
                if self.remaining == 0 {
                    return None;
                }
                self.remaining -= 1;
                Some(parse_map_descriptor(&mut self.cursor))
            }
        }

        fn validate_layout_intervals(layout: &[u8], max_bytes: usize) -> Result<(), ElfWireError> {
            const MAX_LAYOUT: usize = 32;
            if layout.is_empty() || layout.len() > MAX_LAYOUT {
                return Err(ElfWireError::Truncated);
            }
            let mut bits: usize = 0;
            for &b in layout {
                if b == 0 {
                    return Err(ElfWireError::Truncated);
                }
                bits = bits.checked_add(b as usize).ok_or(ElfWireError::Truncated)?;
            }
            if bits % 8 != 0 || bits / 8 > max_bytes {
                return Err(ElfWireError::Truncated);
            }
            Ok(())
        }

        fn parse_map_descriptor<'a>(
            cursor: &mut MetaCursor<'a>,
        ) -> Result<MapDescriptor<'a>, ElfWireError> {
            const MAX_LAYOUT: usize = 32;
            const MAX_ENTRIES: u32 = 64;
            let mark = cursor.remaining();
            let map_name = cursor.take_cstring4()?;
            let map_type = cursor.take_u8()?;
            let len = cursor.take_u24_le()?;
            let map_vma = cursor.take_u32_le()?;
            let key_len = cursor.take_u32_le()? as usize;
            let value_len = cursor.take_u32_le()? as usize;
            if key_len > MAX_LAYOUT || value_len > MAX_LAYOUT {
                return Err(ElfWireError::Truncated);
            }
            let key_layout = if key_len == 0 {
                None
            } else {
                let k = cursor.take_bytes(key_len)?;
                validate_layout_intervals(k, 64)?;
                Some(k)
            };
            let value_layout = if value_len == 0 {
                None
            } else {
                let v = cursor.take_bytes(value_len)?;
                validate_layout_intervals(v, 256)?;
                Some(v)
            };
            let consumed = mark - cursor.remaining();
            cursor.align4_consumed(consumed)?;
            if !(1..=MAX_ENTRIES).contains(&len) || map_vma == 0 || (map_vma & 3) != 0 {
                return Err(ElfWireError::Truncated);
            }
            match map_type {
                1 if key_layout.is_none() || value_layout.is_none() => {
                    return Err(ElfWireError::Truncated);
                }
                2 if key_layout.is_some() || value_layout.is_none() => {
                    return Err(ElfWireError::Truncated);
                }
                1 | 2 => {}
                _ => return Err(ElfWireError::Truncated),
            }
            Ok(MapDescriptor {
                name: map_name,
                map_type,
                len,
                map_vma,
                key_layout,
                value_layout,
            })
        }

        impl #name {
            pub const MAX_SIZE: usize = #capacity;
            pub const PREFIX_SIZE: usize = #prefix_size;
            pub const MAX_MAPS: usize = #max_maps;

            /// One bulk read of `.xpost.meta`; subsequent access is SRAM-only.
            pub(crate) fn read(
                table: &mut dyn crate::xdp::XdpTableStorage,
                section_offset: usize,
                section_size: usize,
            ) -> Result<Self, ElfWireError> {
                if section_size > Self::MAX_SIZE {
                    return Err(ElfWireError::MetadataTooLarge);
                }
                if section_size < Self::PREFIX_SIZE || section_size % 4 != 0 {
                    return Err(ElfWireError::Truncated);
                }
                let buf = FlashBuffer::<#capacity>::read(table, section_offset, section_size)?;
                Ok(Self {
                    bytes: buf.into_inner(),
                })
            }

            #(#getters)*

            /// Parse `xdp_name` then iterate `map_count` descriptors; call [`MapIter::finish`].
            pub(crate) fn xdp_name_and_maps(&self) -> Result<(&str, MapIter<'_>), ElfWireError> {
                if self.bytes.len() < Self::PREFIX_SIZE {
                    return Err(ElfWireError::Truncated);
                }
                let count = self.map_count();
                if count as usize > Self::MAX_MAPS {
                    return Err(ElfWireError::Truncated);
                }
                let mut cursor = MetaCursor::new(&self.bytes[Self::PREFIX_SIZE..]);
                let xdp_name = cursor.take_cstring4()?;
                Ok((
                    xdp_name,
                    MapIter {
                        cursor,
                        remaining: count,
                    },
                ))
            }
        }
    })
}
