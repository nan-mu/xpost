//! `flash_layout!` — fixed wire view over an already-loaded SRAM slice.
//!
//! No Flash / storage I/O. Caller reads into a buffer, then
//! [`from_bytes`](struct@Self) on a subslice.

use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::{Error, Ident, LitInt, Result, Token, Visibility, braced, parse_macro_input};

enum FieldKind {
    U8,
    LeU16,
    LeU24,
    LeU32,
    LeU64,
    BeU16,
    BeU32,
    BeU64,
    Bytes(usize),
    Skip,
}

struct Field {
    name: Option<Ident>,
    kind: FieldKind,
    offset: usize,
}

struct LayoutInput {
    vis: Visibility,
    name: Ident,
    fields: Vec<Field>,
    wire_size: usize,
}

impl Parse for LayoutInput {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let vis: Visibility = input.parse()?;
        let name: Ident = input.parse()?;
        let content;
        braced!(content in input);

        let mut fields = Vec::new();
        let mut cursor = 0usize;
        let mut seen = std::collections::HashSet::<String>::new();

        while !content.is_empty() {
            if content.peek(Ident) {
                let fork = content.fork();
                let id: Ident = fork.parse()?;
                if id == "skip" {
                    content.parse::<Ident>()?;
                    let n: LitInt = content.parse()?;
                    content.parse::<Token![;]>()?;
                    let size: usize = n.base10_parse()?;
                    if size == 0 {
                        return Err(Error::new(n.span(), "skip 0 is not allowed"));
                    }
                    let offset = cursor;
                    cursor = cursor
                        .checked_add(size)
                        .ok_or_else(|| Error::new(n.span(), "layout size overflow"))?;
                    fields.push(Field {
                        name: None,
                        kind: FieldKind::Skip,
                        offset,
                    });
                    continue;
                }
            }

            let field_name: Ident = content.parse()?;
            if !seen.insert(field_name.to_string()) {
                return Err(Error::new(field_name.span(), "duplicate field name"));
            }
            content.parse::<Token![:]>()?;
            let (kind, size) = parse_field_type(&content)?;
            content.parse::<Token![;]>()?;
            let offset = cursor;
            cursor = cursor
                .checked_add(size)
                .ok_or_else(|| Error::new(field_name.span(), "layout size overflow"))?;
            fields.push(Field {
                name: Some(field_name),
                kind,
                offset,
            });
        }

        if fields.is_empty() {
            return Err(Error::new(name.span(), "empty flash_layout is not allowed"));
        }
        if !fields.iter().any(|f| !matches!(f.kind, FieldKind::Skip)) {
            return Err(Error::new(
                name.span(),
                "layout must contain at least one data field",
            ));
        }

        Ok(Self {
            vis,
            name,
            fields,
            wire_size: cursor,
        })
    }
}

fn parse_field_type(input: ParseStream<'_>) -> Result<(FieldKind, usize)> {
    let id: Ident = input.parse()?;
    match id.to_string().as_str() {
        "u8" => Ok((FieldKind::U8, 1)),
        "le" | "be" => {
            input.parse::<Token![<]>()?;
            let ty: Ident = input.parse()?;
            input.parse::<Token![>]>()?;
            let le = id == "le";
            match ty.to_string().as_str() {
                "u16" => Ok((
                    if le {
                        FieldKind::LeU16
                    } else {
                        FieldKind::BeU16
                    },
                    2,
                )),
                "u24" => {
                    if !le {
                        return Err(Error::new(ty.span(), "only le<u24> is supported"));
                    }
                    Ok((FieldKind::LeU24, 3))
                }
                "u32" => Ok((
                    if le {
                        FieldKind::LeU32
                    } else {
                        FieldKind::BeU32
                    },
                    4,
                )),
                "u64" => Ok((
                    if le {
                        FieldKind::LeU64
                    } else {
                        FieldKind::BeU64
                    },
                    8,
                )),
                _ => Err(Error::new(ty.span(), "supported: u16, u24, u32, u64")),
            }
        }
        "bytes" => {
            input.parse::<Token![<]>()?;
            let n: LitInt = input.parse()?;
            input.parse::<Token![>]>()?;
            let size: usize = n.base10_parse()?;
            if size == 0 {
                return Err(Error::new(n.span(), "bytes<0> is not allowed"));
            }
            Ok((FieldKind::Bytes(size), size))
        }
        other => Err(Error::new(
            id.span(),
            format!("unsupported field type `{other}`; use u8, le<T>, be<T>, bytes<N>, skip N"),
        )),
    }
}

pub fn expand(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let layout = parse_macro_input!(input as LayoutInput);
    match expand_layout(layout) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

fn expand_layout(layout: LayoutInput) -> Result<TokenStream2> {
    let LayoutInput {
        vis,
        name,
        fields,
        wire_size,
    } = layout;
    let name_str = name.to_string();

    let mut getters = Vec::new();
    for field in &fields {
        let Some(fname) = &field.name else {
            continue;
        };
        let off = field.offset;
        let get = match &field.kind {
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
            FieldKind::BeU16 => quote! {
                #[inline(always)]
                pub(crate) fn #fname(&self) -> u16 {
                    u16::from_be_bytes([self.bytes[#off], self.bytes[#off + 1]])
                }
            },
            FieldKind::LeU24 => quote! {
                #[inline(always)]
                pub(crate) fn #fname(&self) -> u32 {
                    (self.bytes[#off] as u32)
                        | ((self.bytes[#off + 1] as u32) << 8)
                        | ((self.bytes[#off + 2] as u32) << 16)
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
            FieldKind::BeU32 => quote! {
                #[inline(always)]
                pub(crate) fn #fname(&self) -> u32 {
                    u32::from_be_bytes([
                        self.bytes[#off],
                        self.bytes[#off + 1],
                        self.bytes[#off + 2],
                        self.bytes[#off + 3],
                    ])
                }
            },
            FieldKind::LeU64 => quote! {
                #[inline(always)]
                pub(crate) fn #fname(&self) -> u64 {
                    u64::from_le_bytes([
                        self.bytes[#off],
                        self.bytes[#off + 1],
                        self.bytes[#off + 2],
                        self.bytes[#off + 3],
                        self.bytes[#off + 4],
                        self.bytes[#off + 5],
                        self.bytes[#off + 6],
                        self.bytes[#off + 7],
                    ])
                }
            },
            FieldKind::BeU64 => quote! {
                #[inline(always)]
                pub(crate) fn #fname(&self) -> u64 {
                    u64::from_be_bytes([
                        self.bytes[#off],
                        self.bytes[#off + 1],
                        self.bytes[#off + 2],
                        self.bytes[#off + 3],
                        self.bytes[#off + 4],
                        self.bytes[#off + 5],
                        self.bytes[#off + 6],
                        self.bytes[#off + 7],
                    ])
                }
            },
            FieldKind::Bytes(n) => {
                let n = *n;
                let expect_msg = format!("{name_str}.{fname}");
                quote! {
                    #[inline(always)]
                    pub(crate) fn #fname(&self) -> &[u8; #n] {
                        self.bytes[#off..#off + #n].try_into().expect(#expect_msg)
                    }
                }
            }
            FieldKind::Skip => unreachable!(),
        };
        getters.push(get);
    }

    Ok(quote! {
        #vis struct #name {
            bytes: [u8; Self::WIRE_SIZE],
        }

        impl #name {
            pub const WIRE_SIZE: usize = #wire_size;

            /// Parse from an already-loaded SRAM slice (no Flash I/O).
            #[inline]
            pub(crate) fn from_bytes(bytes: &[u8]) -> Option<Self> {
                if bytes.len() < Self::WIRE_SIZE {
                    return None;
                }
                let mut out = [0u8; Self::WIRE_SIZE];
                out.copy_from_slice(&bytes[..Self::WIRE_SIZE]);
                Some(Self { bytes: out })
            }

            #(#getters)*
        }
    })
}
