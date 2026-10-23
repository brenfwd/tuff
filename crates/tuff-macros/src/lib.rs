//! Derive macros for the `tuff` crate. Generated code refers to items by
//! `crate::` paths, so the derives are only usable inside that crate.
//! `watchdog` refers to `::tuff_core`, which `tuff-core` aliases to itself.

use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::{format_ident, quote};
use syn::spanned::Spanned;
use syn::{Data, DeriveInput, Expr, Fields, Ident, Index, Path, parse_macro_input};

/// Derives `Packet` (or only `Encode` with `encode_only`) for a struct whose
/// fields are written in declaration order.
///
/// Container: `#[packet(id = Clientbound::Name)]`, optionally `encode_only`.
///
/// Field (at most one):
/// - none: `WriteField` / `ReadField`
/// - `max = EXPR`: reads with `ReadBounded::read_bounded(r, EXPR)`
/// - `codec`: shorthand for `with = crate::protocol::field::codec`
/// - `with = path`: calls `path::write(w, &value)` and `path::read(r)`
#[proc_macro_derive(Packet, attributes(packet))]
pub fn derive_packet(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    expand(input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

struct Container {
    id: Path,
    encode_only: bool,
}

enum Codec {
    Default,
    Bounded(Expr),
    With(Path),
}

fn expand(input: DeriveInput) -> syn::Result<TokenStream2> {
    let container = container_attrs(&input)?;
    let Data::Struct(data) = &input.data else {
        return Err(syn::Error::new(
            input.ident.span(),
            "Packet can only be derived for structs",
        ));
    };

    let mut writes = Vec::new();
    let mut reads = Vec::new();
    for (i, field) in data.fields.iter().enumerate() {
        let access = match &field.ident {
            Some(ident) => quote!(#ident),
            None => {
                let index = Index::from(i);
                quote!(#index)
            }
        };
        let (write, read) = match field_codec(field)? {
            Codec::Default => (
                quote!(crate::protocol::field::WriteField::write(&self.#access, w);),
                quote!(crate::protocol::field::ReadField::read(r)?),
            ),
            Codec::Bounded(max) => (
                quote!(crate::protocol::field::WriteField::write(&self.#access, w);),
                quote!(crate::protocol::field::ReadBounded::read_bounded(r, #max)?),
            ),
            Codec::With(path) => (
                quote!(#path::write(w, &self.#access);),
                quote!(#path::read(r)?),
            ),
        };
        writes.push(write);
        reads.push(read);
    }

    let empty = data.fields.is_empty();
    let w = param("w", empty);
    let r = param("r", empty);
    let name = &input.ident;
    let id = &container.id;
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();
    let encode = quote! {
        fn encode(&self, #w: &mut ::tuff_wire::PacketWriter) {
            #(#writes)*
        }
    };

    if container.encode_only {
        return Ok(quote! {
            impl #impl_generics crate::protocol::Encode for #name #ty_generics #where_clause {
                const ID: i32 = #id.id();
                #encode
            }
        });
    }

    let construct = match &data.fields {
        Fields::Named(named) => {
            let names = named.named.iter().map(|f| &f.ident);
            quote!(Self { #(#names: #reads),* })
        }
        Fields::Unnamed(_) => quote!(Self(#(#reads),*)),
        Fields::Unit => quote!(Self),
    };
    Ok(quote! {
        impl #impl_generics crate::protocol::Packet for #name #ty_generics #where_clause {
            const ID: i32 = #id.id();
            #encode
            fn decode(
                #r: &mut ::tuff_wire::PacketReader,
            ) -> ::tuff_wire::Result<Self> {
                Ok(#construct)
            }
        }
    })
}

fn param(name: &str, unused: bool) -> Ident {
    if unused {
        format_ident!("_{name}")
    } else {
        Ident::new(name, Span::call_site())
    }
}

fn container_attrs(input: &DeriveInput) -> syn::Result<Container> {
    let mut id = None;
    let mut encode_only = false;
    for attr in input.attrs.iter().filter(|a| a.path().is_ident("packet")) {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("id") {
                id = Some(meta.value()?.parse::<Path>()?);
            } else if meta.path.is_ident("encode_only") {
                encode_only = true;
            } else {
                return Err(meta.error("expected `id = ...` or `encode_only`"));
            }
            Ok(())
        })?;
    }
    let id = id.ok_or_else(|| {
        syn::Error::new(
            input.ident.span(),
            "missing `#[packet(id = ...)]` attribute",
        )
    })?;
    Ok(Container { id, encode_only })
}

fn field_codec(field: &syn::Field) -> syn::Result<Codec> {
    let mut codec = Codec::Default;
    for attr in field.attrs.iter().filter(|a| a.path().is_ident("packet")) {
        attr.parse_nested_meta(|meta| {
            if !matches!(codec, Codec::Default) {
                return Err(meta.error("a field takes at most one packet codec"));
            }
            codec = if meta.path.is_ident("max") {
                Codec::Bounded(meta.value()?.parse()?)
            } else if meta.path.is_ident("with") {
                Codec::With(meta.value()?.parse()?)
            } else if let Some(ident) = meta.path.get_ident() {
                Codec::With(
                    syn::parse_quote_spanned!(ident.span()=> crate::protocol::field::#ident),
                )
            } else {
                return Err(syn::Error::new(meta.path.span(), "expected a codec name"));
            };
            Ok(())
        })?;
    }
    Ok(codec)
}

/// Arms `tuff_core::watchdog` for the whole test: put it above
/// `#[test]`/`#[tokio::test]` so a hung test aborts the run naming itself.
#[proc_macro_attribute]
pub fn watchdog(args: TokenStream, item: TokenStream) -> TokenStream {
    if !args.is_empty() {
        return syn::Error::new(Span::call_site(), "watchdog takes no arguments")
            .into_compile_error()
            .into();
    }
    let mut function = parse_macro_input!(item as syn::ItemFn);
    let guard: syn::Stmt = syn::parse_quote! {
        let _watchdog = ::tuff_core::watchdog::arm(::tuff_core::watchdog::DEFAULT);
    };
    function.block.stmts.insert(0, guard);
    // tuff-item's own tests link a second copy of the crate through
    // tuff-core, whose fixture guard marks the other copy's thread.
    if std::env::var("CARGO_CRATE_NAME").is_ok_and(|name| name == "tuff_item") {
        let fixtures: syn::Stmt = syn::parse_quote! {
            let _fixtures = crate::item::conservation::fixture_thread();
        };
        function.block.stmts.insert(1, fixtures);
    }
    quote!(#function).into()
}
