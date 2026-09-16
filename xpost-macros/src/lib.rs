//! Derive macros for [`xpost::map::MapKey`] / [`MapValue`], runtime injection,
//! and Compact ELF fixed wire views (`flash_layout!`).

mod flash_layout;

use proc_macro::TokenStream;
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::{Data, DeriveInput, Fields, Ident, Token, parse_macro_input};

/// Fixed wire layout over an already-loaded SRAM slice (`from_bytes` + getters).
///
/// No Flash I/O — caller reads via `XdpTableStorage`, then passes a subslice.
#[proc_macro]
pub fn flash_layout(input: TokenStream) -> TokenStream {
    flash_layout::expand(input)
}

fn field_widths(input: &DeriveInput) -> syn::Result<Vec<usize>> {
    let Data::Struct(data) = &input.data else {
        return Err(syn::Error::new_spanned(
            input,
            "MapKey/MapValue only support structs",
        ));
    };
    let fields = match &data.fields {
        Fields::Named(f) => f.named.iter().collect::<Vec<_>>(),
        Fields::Unnamed(f) => f.unnamed.iter().collect::<Vec<_>>(),
        Fields::Unit => Vec::new(),
    };
    let mut widths = Vec::with_capacity(fields.len());
    for field in fields {
        let ty = &field.ty;
        let width = match ty {
            syn::Type::Path(p) => {
                let ident = p
                    .path
                    .segments
                    .last()
                    .map(|s| s.ident.to_string())
                    .unwrap_or_default();
                match ident.as_str() {
                    "u8" | "i8" | "bool" => 1usize,
                    "u16" | "i16" => 2,
                    "u32" | "i32" => 4,
                    "u64" | "i64" => 8,
                    "usize" | "isize" => {
                        return Err(syn::Error::new_spanned(
                            ty,
                            "usize/isize not allowed in MapKey/MapValue layouts",
                        ));
                    }
                    other => {
                        return Err(syn::Error::new_spanned(
                            ty,
                            format!("unsupported MapKey/MapValue field type `{other}`"),
                        ));
                    }
                }
            }
            syn::Type::Array(arr) => {
                let syn::Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Int(n),
                    ..
                }) = &arr.len
                else {
                    return Err(syn::Error::new_spanned(
                        &arr.len,
                        "array length must be an integer literal",
                    ));
                };
                let len: usize = n.base10_parse()?;
                let elem = match &*arr.elem {
                    syn::Type::Path(p) => p
                        .path
                        .segments
                        .last()
                        .map(|s| s.ident.to_string())
                        .unwrap_or_default(),
                    _ => String::new(),
                };
                if elem != "u8" {
                    return Err(syn::Error::new_spanned(
                        ty,
                        "only [u8; N] arrays are supported",
                    ));
                }
                len
            }
            _ => {
                return Err(syn::Error::new_spanned(
                    ty,
                    "unsupported MapKey/MapValue field type",
                ));
            }
        };
        widths.push(width);
    }
    Ok(widths)
}

fn derive_layout(input: TokenStream, trait_path: proc_macro2::TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let widths = match field_widths(&input) {
        Ok(w) => w,
        Err(e) => return e.to_compile_error().into(),
    };
    let name = &input.ident;
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();
    let width_tokens = widths.iter().map(|w| {
        let b = *w as u8;
        quote! { #b }
    });
    quote! {
        impl #impl_generics #trait_path for #name #ty_generics #where_clause {
            const FIELD_LENS: &'static [u8] = &[#(#width_tokens),*];
        }
    }
    .into()
}

#[proc_macro_derive(MapKey)]
pub fn derive_map_key(input: TokenStream) -> TokenStream {
    derive_layout(input, quote! { ::xpost::map::MapKey })
}

#[proc_macro_derive(MapValue)]
pub fn derive_map_value(input: TokenStream) -> TokenStream {
    derive_layout(input, quote! { ::xpost::map::MapValue })
}

struct DeclareRuntimeArgs {
    heap: Ident,
    slots: Ident,
    table: Ident,
}

impl Parse for DeclareRuntimeArgs {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let heap: Ident = input.parse()?;
        input.parse::<Token![,]>()?;
        let slots: Ident = input.parse()?;
        input.parse::<Token![,]>()?;
        let table: Ident = input.parse()?;
        Ok(Self { heap, slots, table })
    }
}

/// Inject `map_heap` / dual-layer `slots` / SRAM `XdpTable` (`from-bpf-toml`).
///
/// Firmware-first does not use this — [`::xpost::xdp::XdpRt::init`] takes FLASH.
/// Requires `xpost-build` sync to have written `OUT_DIR/xpost_runtime_gen.rs`.
#[proc_macro]
pub fn declare_runtime(input: TokenStream) -> TokenStream {
    let DeclareRuntimeArgs { heap, slots, table } = parse_macro_input!(input as DeclareRuntimeArgs);
    quote! {
        mod __xpost_runtime_gen {
            include!(concat!(env!("OUT_DIR"), "/xpost_runtime_gen.rs"));
        }

        #[unsafe(link_section = ".bss")]
        static mut #heap: [u8; __xpost_runtime_gen::MAP_HEAP_SIZE] =
            [0u8; __xpost_runtime_gen::MAP_HEAP_SIZE];

        /// Dual-layer slot storage: `[slot_index][byte]` padded to `XDP_SLOT_LEN_MAX`.
        #[unsafe(link_section = ".xdp.hotload_0")]
        static mut #slots: [[u8; __xpost_runtime_gen::XDP_SLOT_LEN_MAX]; __xpost_runtime_gen::XDP_SLOT_COUNT] =
            [[0u8; __xpost_runtime_gen::XDP_SLOT_LEN_MAX]; __xpost_runtime_gen::XDP_SLOT_COUNT];

        #[unsafe(link_section = ".bss")]
        static mut #table: ::xpost::xdp::XdpTable<{ __xpost_runtime_gen::XDP_TABLE_BYTES }> =
            ::xpost::xdp::XdpTable::new(__xpost_runtime_gen::XDP_TABLE_IMAGE_CAPACITY);

        /// Build [`::xpost::xdp::XdpSlot`] descriptors over the dual-layer `#slots` buffer.
        ///
        /// # Safety
        /// Call once at boot before [`::xpost::xdp::XdpRt::init_table`]; slots must not move.
        unsafe fn __xpost_xdp_slots() -> [::xpost::xdp::XdpSlot; __xpost_runtime_gen::XDP_SLOT_COUNT] {
            let mut out =
                [::xpost::xdp::XdpSlot::dangling_primary(); __xpost_runtime_gen::XDP_SLOT_COUNT];
            let base = (#slots).as_ptr() as usize;
            let stride = __xpost_runtime_gen::XDP_SLOT_LEN_MAX;
            let mut i = 0usize;
            while i < __xpost_runtime_gen::XDP_SLOT_COUNT {
                let len = __xpost_runtime_gen::XDP_SLOT_LENS[i];
                let primary = __xpost_runtime_gen::XDP_SLOT_PRIMARY[i];
                let addr = base + i * stride;
                out[i] = unsafe { ::xpost::xdp::XdpSlot::from_raw_parts(addr, len, primary) };
                i += 1;
            }
            out
        }
    }
    .into()
}
