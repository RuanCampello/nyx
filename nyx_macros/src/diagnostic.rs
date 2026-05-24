use crate::fmt::parse_template;
use proc_macro2::{Span, TokenStream};
use quote::{format_ident, quote};
use syn::{
    Data, DeriveInput, Error, Fields, Ident, LitStr, Meta, Result, Token,
    parse::{Parse, ParseStream},
    punctuated::Punctuated,
};

#[derive(Default)]
struct DiagnosticAttr {
    message: Option<LitStr>,
    primary: Option<LitStr>,
    note: Option<LitStr>,
    help: Option<LitStr>,
    secondary: Option<SecondaryAttr>,
    transparent: bool,
}

struct SecondaryAttr {
    span_field: Ident,
    label: LitStr,
}

struct Secondary {
    span_field: Option<LitStr>,
    label: LitStr,
}

impl Parse for Secondary {
    fn parse(input: ParseStream) -> Result<Self> {
        let mut span_field = None;
        let mut label = None;

        let items = Punctuated::<Meta, Token![,]>::parse_terminated(input)?;
        for item in items {
            let Meta::NameValue(nv) = item else {
                return Err(Error::new_spanned(item, "expected key = \"value\""));
            };

            let key = nv.path.get_ident().map(ToString::to_string).unwrap_or_default();
            let syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Str(s), .. }) = &nv.value else {
                return Err(Error::new_spanned(&nv.value, "expected string literal"));
            };

            match key.as_str() {
                "span_field" => span_field = Some(s.clone()),
                "label" => label = Some(s.clone()),
                other => {
                    return Err(Error::new_spanned(&nv.path, format!("unknown key `{other}`")));
                },
            }
        }

        let label = label.ok_or_else(|| Error::new(input.span(), "missing `label`"))?;
        Ok(Self { span_field, label })
    }
}

fn parse_diagnostic_attr(meta: &Meta) -> Result<DiagnosticAttr> {
    let Meta::List(list) = meta else {
        return Err(Error::new_spanned(meta, "expected #[diagnostic(...)]"));
    };

    let mut attr = DiagnosticAttr::default();
    let metas = list.parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)?;

    for meta in metas {
        match &meta {
            Meta::Path(path) if path.is_ident("transparent") => attr.transparent = true,
            Meta::NameValue(nv) => {
                let key = nv.path.get_ident().map(ToString::to_string).unwrap_or_default();
                let syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Str(s), .. }) = &nv.value else {
                    return Err(Error::new_spanned(&nv.value, "expected string literal"));
                };

                match key.as_str() {
                    "message" => attr.message = Some(s.clone()),
                    "primary" => attr.primary = Some(s.clone()),
                    "note" => attr.note = Some(s.clone()),
                    "help" => attr.help = Some(s.clone()),
                    other => {
                        return Err(Error::new_spanned(&nv.path, format!("unknown key `{other}`")));
                    },
                }
            },
            Meta::List(list) if list.path.is_ident("secondary") => {
                let sec = list.parse_args_with(Secondary::parse)?;
                let span_field = sec
                    .span_field
                    .map(|lit| Ident::new(&lit.value(), lit.span()))
                    .unwrap_or_else(|| Ident::new("__span", Span::call_site()));
                attr.secondary = Some(SecondaryAttr { span_field, label: sec.label });
            },
            other => {
                return Err(Error::new_spanned(other, "unexpected item in #[diagnostic(...)]"));
            },
        }
    }

    Ok(attr)
}

fn extract_diagnostic_attr(attrs: &[syn::Attribute]) -> Result<Option<DiagnosticAttr>> {
    let mut diag_attr = None;
    for attr in attrs.iter().filter(|a| a.path().is_ident("diagnostic")) {
        if diag_attr.is_some() {
            return Err(Error::new_spanned(
                attr,
                "only one #[diagnostic(...)] allowed per variant",
            ));
        }
        diag_attr = Some(parse_diagnostic_attr(&attr.meta)?);
    }
    Ok(diag_attr)
}

fn generate_variant_arm(
    enum_name: &Ident,
    variant: &syn::Variant,
    attr: &DiagnosticAttr,
) -> Result<TokenStream> {
    let variant_name = &variant.ident;
    let field_bindings = field_bindings_pattern(&variant.fields);

    if attr.transparent {
        let first_field = match &variant.fields {
            Fields::Named(n) => {
                n.named.first().and_then(|f| f.ident.as_ref()).map(|id| quote!(#id))
            },
            Fields::Unnamed(_) => Some(quote!(field_0)),
            Fields::Unit => None,
        }
        .ok_or_else(|| {
            Error::new_spanned(variant, "#[diagnostic(transparent)] requires at least one field")
        })?;

        return Ok(quote! {
            #enum_name::#variant_name #field_bindings => {
                crate::diagnostic::AsDiagnostic::as_diagnostic(#first_field, __span)
            }
        });
    }

    let msg = attr
        .message
        .as_ref()
        .ok_or_else(|| Error::new_spanned(variant, "missing `message`"))?;
    let prim = attr
        .primary
        .as_ref()
        .ok_or_else(|| Error::new_spanned(variant, "missing `primary`"))?;

    let msg_ts = parse_template(&msg.value(), msg.span())?;
    let prim_ts = parse_template(&prim.value(), prim.span())?;

    let note_chain = attr
        .note
        .as_ref()
        .map(|n| parse_template(&n.value(), n.span()))
        .transpose()?
        .map(|ts| quote!(.note(#ts)));

    let help_chain = attr
        .help
        .as_ref()
        .map(|h| parse_template(&h.value(), h.span()))
        .transpose()?
        .map(|ts| quote!(.help(#ts)));

    let sec_chain = attr
        .secondary
        .as_ref()
        .map(|sec| {
            let sf = &sec.span_field;
            let lbl = parse_template(&sec.label.value(), sec.label.span())?;
            Ok::<_, Error>(quote!(.secondary(#sf, #lbl)))
        })
        .transpose()?;

    Ok(quote! {
        #enum_name::#variant_name #field_bindings => {
            crate::diagnostic::Builder::new(#msg_ts)
                .primary(__span, #prim_ts)
                #sec_chain
                #note_chain
                #help_chain
                .build()
        }
    })
}

fn field_bindings_pattern(fields: &Fields) -> TokenStream {
    match fields {
        Fields::Named(named) => {
            let names = named.named.iter().filter_map(|f| f.ident.as_ref());
            quote! { { #(#names),* } }
        },
        Fields::Unnamed(unnamed) => {
            let names = (0..unnamed.unnamed.len()).map(|i| format_ident!("field_{i}"));
            quote! { ( #(#names),* ) }
        },
        Fields::Unit => quote! {},
    }
}

pub fn derive_diagnostic(input: DeriveInput) -> Result<TokenStream> {
    let enum_name = &input.ident;
    let Data::Enum(data) = &input.data else {
        return Err(Error::new_spanned(&input, "#[derive(Diagnostic)] only works on enums"));
    };

    let arms = data
        .variants
        .iter()
        .map(|variant| {
            let attr = extract_diagnostic_attr(&variant.attrs)?.ok_or_else(|| {
                Error::new_spanned(
                    variant,
                    format!("variant `{}` missing #[diagnostic(...)]", variant.ident),
                )
            })?;
            generate_variant_arm(enum_name, variant, &attr)
        })
        .collect::<Result<Vec<_>>>()?;

    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

    Ok(quote! {
        impl #impl_generics crate::diagnostic::AsDiagnostic for #enum_name #ty_generics #where_clause {
            #[allow(unused_variables, unused_assignments)]
            fn as_diagnostic(self, __span: crate::lexer::token::Span) -> crate::diagnostic::Diagnostic {
                use crate::diagnostic::{Builder, hi, PRIMARY, SECONDARY, HIGHLIGHT};
                use ariadne::Fmt as _;

                match self {
                    #(#arms)*
                }
            }
        }
    })
}
