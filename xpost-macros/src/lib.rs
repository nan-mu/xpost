//! Derive macros for [`xpost::map::MapKey`] / [`MapValue`] and Compact ELF
//! fixed wire views (`flash_layout!`).

mod flash_layout;

use proc_macro::TokenStream;
use quote::quote;
use syn::{Data, DeriveInput, Fields, parse_macro_input};

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
