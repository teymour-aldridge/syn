use crate::operand::*;
use crate::{file, full, gen};
use inflections::Inflect;
use proc_macro2::{Span, TokenStream};
use quote::{quote, TokenStreamExt};
use syn::*;
use syn_codegen as types;

const VISIT_SRC: &str = "../src/gen/visit.rs";

#[derive(Default)]
struct State {
    visit_trait: TokenStream,
    visit_impl: TokenStream,
}

fn under_name(name: &str) -> Ident {
    Ident::new(&name.to_snake_case(), Span::call_site())
}

fn simple_visit(item: &str, name: &Operand) -> TokenStream {
    let ident = under_name(item);
    let method = Ident::new(&format!("visit_{}", ident), Span::call_site());
    let name = name.ref_tokens();
    quote! {
        _visitor.#method(#name)
    }
}

fn noop_visit(name: &Operand) -> TokenStream {
    let name = name.tokens();
    quote! {
        skip!(#name)
    }
}

fn visit(
    ty: &types::Type,
    features: &types::Features,
    defs: &types::Definitions,
    name: &Operand,
) -> Option<TokenStream> {
    match ty {
        types::Type::Box(t) => {
            let name = name.owned_tokens();
            visit(t, features, defs, &Owned(quote!(*#name)))
        }
        types::Type::Vec(t) => {
            let operand = Borrowed(quote!(it));
            let val = visit(t, features, defs, &operand)?;
            let name = name.ref_tokens();
            Some(quote! {
                for it in #name {
                    #val
                }
            })
        }
        types::Type::Punctuated(p) => {
            let operand = Borrowed(quote!(it));
            let val = visit(&p.element, features, defs, &operand)?;
            let name = name.ref_tokens();
            Some(quote! {
                for el in Punctuated::pairs(#name) {
                    let it = el.value();
                    #val
                }
            })
        }
        types::Type::Option(t) => {
            let it = Borrowed(quote!(it));
            let val = visit(t, features, defs, &it)?;
            let name = name.owned_tokens();
            Some(quote! {
                if let Some(ref it) = #name {
                    #val
                }
            })
        }
        types::Type::Tuple(t) => {
            let mut code = TokenStream::new();
            for (i, elem) in t.iter().enumerate() {
                let name = name.tokens();
                let i = Index::from(i);
                let it = Owned(quote!((#name).#i));
                let val = visit(elem, features, defs, &it).unwrap_or_else(|| noop_visit(&it));
                code.append_all(val);
                code.append_all(quote!(;));
            }
            Some(code)
        }
        types::Type::Token(t) => {
            let name = name.tokens();
            let repr = &defs.tokens[t];
            let is_keyword = repr.chars().next().unwrap().is_alphabetic();
            let spans = if is_keyword {
                quote!(span)
            } else {
                quote!(spans)
            };
            Some(quote! {
                tokens_helper(_visitor, &#name.#spans)
            })
        }
        types::Type::Group(_) => {
            let name = name.tokens();
            Some(quote! {
                tokens_helper(_visitor, &#name.span)
            })
        }
        types::Type::Syn(t) => {
            fn requires_full(features: &types::Features) -> bool {
                features.any.contains("full") && features.any.len() == 1
            }
            let mut res = simple_visit(t, name);
            let target = defs.types.iter().find(|ty| ty.ident == *t).unwrap();
            if requires_full(&target.features) && !requires_full(features) {
                res = quote!(full!(#res));
            }
            Some(res)
        }
        types::Type::Ext(t) if gen::TERMINAL_TYPES.contains(&&t[..]) => Some(simple_visit(t, name)),
        types::Type::Ext(_) | types::Type::Std(_) => None,
    }
}

fn visit_features(features: &types::Features) -> TokenStream {
    let features = &features.any;
    match features.len() {
        0 => quote!(),
        1 => quote!(#[cfg(feature = #(#features)*)]),
        _ => quote!(#[cfg(any(#(feature = #features),*))]),
    }
}

fn node(state: &mut State, s: &types::Node, defs: &types::Definitions) {
    let features = visit_features(&s.features);
    let under_name = under_name(&s.ident);
    let ty = Ident::new(&s.ident, Span::call_site());
    let visit_fn = Ident::new(&format!("visit_{}", under_name), Span::call_site());

    let mut visit_impl = TokenStream::new();

    match &s.data {
        types::Data::Enum(variants) => {
            let mut visit_variants = TokenStream::new();

            for (variant, fields) in variants {
                let variant_ident = Ident::new(variant, Span::call_site());

                if fields.is_empty() {
                    visit_variants.append_all(quote! {
                        #ty::#variant_ident => {}
                    });
                } else {
                    let mut bind_visit_fields = TokenStream::new();
                    let mut visit_fields = TokenStream::new();

                    for (idx, ty) in fields.iter().enumerate() {
                        let name = format!("_binding_{}", idx);
                        let binding = Ident::new(&name, Span::call_site());

                        bind_visit_fields.append_all(quote! {
                            ref #binding,
                        });

                        let borrowed_binding = Borrowed(quote!(#binding));

                        visit_fields.append_all(
                            visit(ty, &s.features, defs, &borrowed_binding)
                                .unwrap_or_else(|| noop_visit(&borrowed_binding)),
                        );

                        visit_fields.append_all(quote!(;));
                    }

                    visit_variants.append_all(quote! {
                        #ty::#variant_ident(#bind_visit_fields) => {
                            #visit_fields
                        }
                    });
                }
            }

            visit_impl.append_all(quote! {
                match *_i {
                    #visit_variants
                }
            });
        }
        types::Data::Struct(fields) => {
            for (field, ty) in fields {
                let id = Ident::new(&field, Span::call_site());
                let ref_toks = Owned(quote!(_i.#id));
                let visit_field = visit(&ty, &s.features, defs, &ref_toks)
                    .unwrap_or_else(|| noop_visit(&ref_toks));
                visit_impl.append_all(quote! {
                    #visit_field;
                });
            }
        }
        types::Data::Private => {}
    }

    state.visit_trait.append_all(quote! {
        #features
        fn #visit_fn(&mut self, i: &'ast #ty) {
            #visit_fn(self, i)
        }
    });

    state.visit_impl.append_all(quote! {
        #features
        pub fn #visit_fn<'ast, V: Visit<'ast> + ?Sized>(
            _visitor: &mut V, _i: &'ast #ty
        ) {
            #visit_impl
        }
    });
}

pub fn generate(defs: &types::Definitions) {
    let state = gen::traverse(defs, node);
    let full_macro = full::get_macro();
    let visit_trait = state.visit_trait;
    let visit_impl = state.visit_impl;
    file::write(
        VISIT_SRC,
        quote! {
            #![cfg_attr(feature = "cargo-clippy", allow(trivially_copy_pass_by_ref))]

            use *;
            #[cfg(any(feature = "full", feature = "derive"))]
            use punctuated::Punctuated;
            use proc_macro2::Span;
            #[cfg(any(feature = "full", feature = "derive"))]
            use gen::helper::visit::*;

            #full_macro

            #[cfg(any(feature = "full", feature = "derive"))]
            macro_rules! skip {
                ($($tt:tt)*) => {};
            }

            /// Syntax tree traversal to walk a shared borrow of a syntax tree.
            ///
            /// See the [module documentation] for details.
            ///
            /// [module documentation]: index.html
            ///
            /// *This trait is available if Syn is built with the `"visit"` feature.*
            pub trait Visit<'ast> {
                #visit_trait
            }

            #visit_impl
        },
    );
}
