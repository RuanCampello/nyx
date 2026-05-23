use proc_macro2::{Span, TokenStream};
use quote::{format_ident, quote};
use syn::{
    Data, DeriveInput, Error, Fields, Ident, LitStr, Meta, Result, Token,
    parse::{Parse, ParseStream},
    punctuated::Punctuated,
};

use crate::fmt::parse_template;

struct DiagnosticAttr {
    message: Option<LitStr>,
    primary: Option<LitStr>,
    note: Option<LitStr>,
    help: Option<LitStr>,
    secondary: Option<SecondaryAttr>,
    custom: bool,
}

struct SecondaryAttr {
    span_field: Ident,
    label: LitStr,
}

impl Default for DiagnosticAttr {
    fn default() -> Self {
        Self {
            message: None,
            primary: None,
            note: None,
            help: None,
            secondary: None,
            custom: false,
        }
    }
}

fn parse_diagnostic_attr(meta: &Meta) -> Result<DiagnosticAttr> {
    let Meta::List(list) = meta else {
        return Err(Error::new_spanned(meta, "expected #[diagnostic(...)]"));
    };

    let mut attr = DiagnosticAttr::default();

    struct AttrItem {
        key: Ident,
        value: AttrValue,
    }

    enum AttrValue {
        Str(LitStr),
        Nested(TokenStream),
        Flag,
    }

    let mut tokens = list.tokens.clone().into_iter().peekable();
    let ts: TokenStream = list.tokens.clone();
    let metas = syn::parse::Parser::parse2(
        |input: ParseStream| Punctuated::<Meta, Token![,]>::parse_terminated(input),
        ts,
    )?;

    for meta in metas {
        match &meta {
            Meta::Path(path) if path.is_ident("custom") => {
                attr.custom = true;
            },
            Meta::NameValue(nv) => {
                let key = nv
                    .path
                    .get_ident()
                    .ok_or_else(|| Error::new_spanned(&nv.path, "expected simple key"))?
                    .to_string();

                let lit = match &nv.value {
                    syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Str(s), .. }) => s.clone(),
                    other => return Err(Error::new_spanned(other, "expected string literal")),
                };

                match key.as_str() {
                    "message" => attr.message = Some(lit),
                    "primary" => attr.primary = Some(lit),
                    "note" => attr.note = Some(lit),
                    "help" => attr.help = Some(lit),
                    other => {
                        return Err(Error::new_spanned(
                            &nv.path,
                            format!(
                                "unknown diagnostic key `{other}`, expected one of: message, primary, note, help"
                            ),
                        ));
                    },
                }
            },
            Meta::List(list) if list.path.is_ident("secondary") => {
                // secondary(span_field = "FIELD", label = "...")
                struct Secondary {
                    span_field: LitStr,
                    label: LitStr,
                }
                impl Parse for Secondary {
                    fn parse(input: ParseStream) -> Result<Self> {
                        let mut span_field = None;
                        let mut label = None;
                        let items = Punctuated::<Meta, Token![,]>::parse_terminated(input)?;
                        for item in items {
                            if let Meta::NameValue(nv) = item {
                                let key =
                                    nv.path.get_ident().map(|i| i.to_string()).unwrap_or_default();
                                let lit = match &nv.value {
                                    syn::Expr::Lit(syn::ExprLit {
                                        lit: syn::Lit::Str(s), ..
                                    }) => s.clone(),
                                    other => {
                                        return Err(Error::new_spanned(other, "expected string"));
                                    },
                                };
                                match key.as_str() {
                                    "span_field" => span_field = Some(lit),
                                    "label" => label = Some(lit),
                                    other => {
                                        return Err(Error::new_spanned(
                                            &nv.path,
                                            format!("unknown key `{other}`"),
                                        ));
                                    },
                                }
                            }
                        }
                        Ok(Secondary {
                            span_field: span_field.ok_or_else(|| {
                                syn::Error::new(
                                    proc_macro2::Span::call_site(),
                                    "missing `span_field`",
                                )
                            })?,
                            label: label.ok_or_else(|| {
                                syn::Error::new(proc_macro2::Span::call_site(), "missing `label`")
                            })?,
                        })
                    }
                }

                let sec = syn::parse::Parser::parse2(Secondary::parse, list.tokens.clone())?;
                let span_field_ident = Ident::new(&sec.span_field.value(), sec.span_field.span());
                attr.secondary =
                    Some(SecondaryAttr { span_field: span_field_ident, label: sec.label });
            },
            other => {
                return Err(Error::new_spanned(other, "unexpected item in #[diagnostic(...)]"));
            },
        }
    }

    Ok(attr)
}

fn extract_diagnostic_attr(attrs: &[syn::Attribute]) -> Result<Option<DiagnosticAttr>> {
    let diag_attrs: Vec<_> = attrs.iter().filter(|a| a.path().is_ident("diagnostic")).collect();

    match diag_attrs.len() {
        0 => Ok(None),
        1 => Ok(Some(parse_diagnostic_attr(&diag_attrs[0].meta)?)),
        _ => Err(Error::new_spanned(
            diag_attrs[1],
            "only one #[diagnostic(...)] attribute per variant is allowed",
        )),
    }
}

fn emit_format(lit: &LitStr) -> Result<TokenStream> {
    let template = lit.value();
    parse_template(&template, lit.span())
}

fn generate_variant_arm(
    enum_name: &Ident,
    variant: &syn::Variant,
    attr: &DiagnosticAttr,
) -> Result<TokenStream> {
    if attr.custom {
        // Route to user-provided method
        let field_bindings = field_bindings_pattern(&variant.fields);
        let field_names = field_names_vec(&variant.fields);
        let variant_name = &variant.ident;

        return Ok(quote! {
            #enum_name::#variant_name #field_bindings => {
                #enum_name::#variant_name { #(#field_names,)* }.into_diagnostic_custom(__span)
            }
        });
    }

    let message_lit = attr.message.as_ref().ok_or_else(|| {
        Error::new_spanned(&variant.ident, "#[diagnostic] requires `message = \"...\"`")
    })?;
    let primary_lit = attr.primary.as_ref().ok_or_else(|| {
        Error::new_spanned(&variant.ident, "#[diagnostic] requires `primary = \"...\"`")
    })?;

    let message_ts = emit_format(message_lit)?;
    let primary_ts = emit_format(primary_lit)?;

    let note_ts = attr.note.as_ref().map(emit_format).transpose()?;
    let help_ts = attr.help.as_ref().map(emit_format).transpose()?;
    let secondary_ts = attr
        .secondary
        .as_ref()
        .map(|sec| -> Result<TokenStream> {
            let span_field = &sec.span_field;
            let label_ts = emit_format(&sec.label)?;
            Ok(quote! { .secondary(#span_field, #label_ts) })
        })
        .transpose()?;

    let note_chain = note_ts.map(|n| quote! { .note(#n) }).unwrap_or_default();
    let help_chain = help_ts.map(|h| quote! { .help(#h) }).unwrap_or_default();
    let secondary_chain = secondary_ts.unwrap_or_default();

    let field_bindings = field_bindings_pattern(&variant.fields);
    let variant_name = &variant.ident;

    Ok(quote! {
        #enum_name::#variant_name #field_bindings => {
            Builder::new(#message_ts)
                .primary(__span, #primary_ts)
                #secondary_chain
                #note_chain
                #help_chain
                .build()
        }
    })
}

fn field_bindings_pattern(fields: &Fields) -> TokenStream {
    match fields {
        Fields::Named(named) => {
            let names: Vec<_> = named.named.iter().filter_map(|f| f.ident.as_ref()).collect();
            quote! { { #(#names),* } }
        },
        Fields::Unnamed(unnamed) => {
            let names: Vec<_> =
                (0..unnamed.unnamed.len()).map(|i| format_ident!("field_{i}")).collect();
            quote! { ( #(#names),* ) }
        },
        Fields::Unit => quote! {},
    }
}

fn field_names_vec(fields: &Fields) -> Vec<Ident> {
    match fields {
        Fields::Named(named) => named.named.iter().filter_map(|f| f.ident.clone()).collect(),
        Fields::Unnamed(unnamed) => {
            (0..unnamed.unnamed.len()).map(|i| format_ident!("field_{i}")).collect()
        },
        Fields::Unit => vec![],
    }
}

pub fn derive_diagnostic(input: DeriveInput) -> Result<TokenStream> {
    let enum_name = &input.ident;

    let Data::Enum(data) = &input.data else {
        return Err(Error::new_spanned(&input, "#[derive(Diagnostic)] only works on enums"));
    };

    let mut arms = Vec::new();
    let mut has_custom = false;

    for variant in &data.variants {
        let attr = extract_diagnostic_attr(&variant.attrs)?;

        match attr {
            None => {
                return Err(Error::new_spanned(
                    &variant.ident,
                    format!(
                        "variant `{}` is missing #[diagnostic(...)] or #[diagnostic(custom)]",
                        variant.ident
                    ),
                ));
            },
            Some(ref a) => {
                if a.custom {
                    has_custom = true;
                }
                arms.push(generate_variant_arm(enum_name, variant, a)?);
            },
        }
    }

    let custom_bound = if has_custom {
        quote! {}
    } else {
        quote! {}
    };

    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

    Ok(quote! {
        #custom_bound

        impl #impl_generics crate::diagnostic::IntoDiagnostic for #enum_name #ty_generics #where_clause {
            fn into_diagnostic(self, __span: crate::lexer::token::Span) -> crate::diagnostic::Diagnostic {
                use crate::diagnostic::{Builder, hi, PRIMARY, SECONDARY, HIGHLIGHT};
                use ariadne::Fmt as _;

                match self {
                    #(#arms)*
                }
            }
        }
    })
}
