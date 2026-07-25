use crate::fmt::{parse_template, parse_template_plain};
use proc_macro2::{Span, TokenStream};
use quote::{format_ident, quote};
use syn::{
    Data, DeriveInput, Error, Fields, Ident, LitStr, Meta, Result, Token,
    parse::{Parse, ParseStream},
    punctuated::Punctuated,
};

#[derive(Default)]
struct DiagnosticAttr {
    code: Option<LitStr>,
    lint: Option<LitStr>,
    message: Option<LitStr>,
    primary: Option<LitStr>,
    note: Option<LitStr>,
    help: Option<LitStr>,
    secondaries: Vec<SecondaryAttr>,
    transparent: bool,
}

struct SecondaryAttr {
    span_field: Ident,
    label: LitStr,
    optional: bool,
}

struct Secondary {
    span_field: Option<LitStr>,
    label: LitStr,
    optional: bool,
}

impl Parse for Secondary {
    fn parse(input: ParseStream) -> Result<Self> {
        let mut span_field = None;
        let mut label = None;
        let mut optional = false;

        let items = Punctuated::<Meta, Token![,]>::parse_terminated(input)?;
        for item in items {
            match item {
                Meta::Path(path) if path.is_ident("optional") => optional = true,
                Meta::NameValue(nv) => {
                    let key = nv.path.get_ident().map(ToString::to_string).unwrap_or_default();
                    let syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Str(s), .. }) = &nv.value
                    else {
                        return Err(Error::new_spanned(&nv.value, "expected string literal"));
                    };

                    match key.as_str() {
                        "span_field" => span_field = Some(s.clone()),
                        "label" => label = Some(s.clone()),
                        other => {
                            return Err(Error::new_spanned(
                                &nv.path,
                                format!("unknown key `{other}`"),
                            ));
                        },
                    }
                },
                other => return Err(Error::new_spanned(other, "expected key = \"value\"")),
            }
        }

        let label = label.ok_or_else(|| Error::new(input.span(), "missing `label`"))?;
        Ok(Self { span_field, label, optional })
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
                    "code" => attr.code = Some(s.clone()),
                    "message" => attr.message = Some(s.clone()),
                    "primary" => attr.primary = Some(s.clone()),
                    "note" => attr.note = Some(s.clone()),
                    "help" => attr.help = Some(s.clone()),
                    "lint" => attr.lint = Some(s.clone()),
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

                let sec = SecondaryAttr { span_field, label: sec.label, optional: sec.optional };
                attr.secondaries.push(sec);
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

fn code_ident(code: &LitStr) -> Result<Ident> {
    let value = code.value();
    match syn::parse_str::<Ident>(&value) {
        Ok(ident) => Ok(ident),
        Err(_) => Err(Error::new(code.span(), format!("`{value}` is not a valid error code"))),
    }
}

fn transparent_field(variant: &syn::Variant) -> Result<TokenStream> {
    match &variant.fields {
        Fields::Named(n) => n.named.first().and_then(|f| f.ident.as_ref()).map(|id| quote!(#id)),
        Fields::Unnamed(_) => Some(quote!(field_0)),
        Fields::Unit => None,
    }
    .ok_or_else(|| {
        Error::new_spanned(variant, "#[diagnostic(transparent)] requires at least one field")
    })
}

fn kind_tokens(
    variant: &syn::Variant,
    attr: &DiagnosticAttr,
) -> Result<(TokenStream, TokenStream, TokenStream)> {
    match (&attr.code, &attr.lint) {
        (Some(_), Some(lint)) => {
            Err(Error::new_spanned(lint, "a variant has a `code` or a `lint`"))
        },
        (None, None) => Err(Error::new_spanned(variant, "missing `code`")),
        (Some(code), None) => {
            let code = code_ident(code)?;
            Ok((
                quote!(crate::diagnostic::Severity::Error),
                quote!(::core::option::Option::Some(crate::error_codes::ErrorCode::#code)),
                quote!(::core::option::Option::None),
            ))
        },
        (None, Some(lint)) => {
            let lint = lint_ident(lint)?;
            Ok((
                quote!(crate::lints::Lint::#lint.default_level().severity()),
                quote!(::core::option::Option::None),
                quote!(::core::option::Option::Some(crate::lints::Lint::#lint)),
            ))
        },
    }
}

fn lint_ident(lint: &LitStr) -> Result<Ident> {
    let mut camel = String::new();
    for word in lint.value().split('_') {
        let mut chars = word.chars();
        match chars.next() {
            Some(first) => {
                camel.extend(first.to_uppercase());
                camel.push_str(chars.as_str());
            },
            None => return Err(Error::new_spanned(lint, "empty segment in lint name")),
        }
    }

    Ok(Ident::new(&camel, lint.span()))
}

fn required_parts<'a>(
    variant: &syn::Variant,
    attr: &'a DiagnosticAttr,
) -> Result<(&'a LitStr, &'a LitStr)> {
    let msg = attr
        .message
        .as_ref()
        .ok_or_else(|| Error::new_spanned(variant, "missing `message`"))?;
    let prim = attr
        .primary
        .as_ref()
        .ok_or_else(|| Error::new_spanned(variant, "missing `primary`"))?;
    Ok((msg, prim))
}

fn generate_variant_arm(
    enum_name: &Ident,
    variant: &syn::Variant,
    attr: &DiagnosticAttr,
) -> Result<TokenStream> {
    let variant_name = &variant.ident;
    let field_bindings = field_bindings_pattern(&variant.fields);

    if attr.transparent {
        let first_field = transparent_field(variant)?;
        return Ok(quote! {
            #enum_name::#variant_name #field_bindings => {
                crate::diagnostic::AsDiagnostic::into_diagnostic(#first_field, __span)
            }
        });
    }

    let (msg, prim) = required_parts(variant, attr)?;
    let kind_stmt = match (&attr.code, &attr.lint) {
        (_, Some(lint)) => {
            let lint = lint_ident(lint)?;
            quote!(__builder = __builder.lint(crate::lints::Lint::#lint);)
        },
        (Some(code), None) => {
            let code = code_ident(code)?;
            quote!(__builder = __builder.code(crate::error_codes::ErrorCode::#code);)
        },
        (None, None) => return Err(Error::new_spanned(variant, "missing `code`")),
    };
    let msg_ts = parse_template(&msg.value(), msg.span())?;
    let prim_ts = parse_template(&prim.value(), prim.span())?;

    let sec_stmts = attr
        .secondaries
        .iter()
        .map(|sec| {
            let sf = &sec.span_field;
            let lbl = parse_template(&sec.label.value(), sec.label.span())?;
            Ok::<_, Error>(match sec.optional {
                true => quote! {
                    if let ::core::option::Option::Some(__sec_span) = #sf {
                        __builder = __builder.secondary(__sec_span, #lbl);
                    }
                },
                false => quote! { __builder = __builder.secondary(#sf, #lbl); },
            })
        })
        .collect::<Result<Vec<_>>>()?;

    let note_stmt = attr
        .note
        .as_ref()
        .map(|n| parse_template(&n.value(), n.span()))
        .transpose()?
        .map(|ts| quote!(__builder = __builder.note(#ts);));

    let help_stmt = attr
        .help
        .as_ref()
        .map(|h| parse_template(&h.value(), h.span()))
        .transpose()?
        .map(|ts| quote!(__builder = __builder.help(#ts);));

    Ok(quote! {
        #enum_name::#variant_name #field_bindings => {
            let mut __builder = crate::diagnostic::Builder::new(#msg_ts)
                .primary(__span, #prim_ts);
            #kind_stmt
            #(#sec_stmts)*
            #note_stmt
            #help_stmt
            __builder.build()
        }
    })
}

fn generate_rich_arm(
    enum_name: &Ident,
    variant: &syn::Variant,
    attr: &DiagnosticAttr,
) -> Result<TokenStream> {
    let variant_name = &variant.ident;
    let field_bindings = field_bindings_pattern(&variant.fields);

    if attr.transparent {
        let first_field = transparent_field(variant)?;
        return Ok(quote! {
            #enum_name::#variant_name #field_bindings => {
                crate::diagnostic::AsDiagnostic::rich(#first_field, __span)
            }
        });
    }

    let (msg, prim) = required_parts(variant, attr)?;
    let (severity, code, lint) = kind_tokens(variant, attr)?;
    let msg_ts = parse_template_plain(&msg.value(), msg.span())?;
    let prim_ts = parse_template_plain(&prim.value(), prim.span())?;

    let note_ts = match &attr.note {
        Some(n) => {
            let t = parse_template_plain(&n.value(), n.span())?;
            quote!(::core::option::Option::Some(#t))
        },
        None => quote!(::core::option::Option::None),
    };
    let help_ts = match &attr.help {
        Some(h) => {
            let t = parse_template_plain(&h.value(), h.span())?;
            quote!(::core::option::Option::Some(#t))
        },
        None => quote!(::core::option::Option::None),
    };

    let sec_stmts = attr
        .secondaries
        .iter()
        .map(|sec| {
            let sf = &sec.span_field;
            let lbl = parse_template_plain(&sec.label.value(), sec.label.span())?;
            Ok::<_, Error>(match sec.optional {
                true => quote! {
                    if let ::core::option::Option::Some(__sec_span) = #sf {
                        __secondary.push(crate::diagnostic::Label {
                            span: __sec_span,
                            message: #lbl,
                        });
                    }
                },
                false => quote! {
                    __secondary.push(crate::diagnostic::Label { span: #sf, message: #lbl });
                },
            })
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(quote! {
        #enum_name::#variant_name #field_bindings => {
            let mut __secondary = ::std::vec::Vec::new();
            #(#sec_stmts)*
            crate::diagnostic::RichDiagnostic {
                severity: #severity,
                code: #code,
                lint: #lint,
                message: #msg_ts,
                primary: ::core::option::Option::Some(crate::diagnostic::Label {
                    span: __span,
                    message: #prim_ts,
                }),
                secondary: __secondary,
                note: #note_ts,
                help: #help_ts,
                rendered: ::core::option::Option::None,
            }
        }
    })
}

fn generate_message_arm(
    enum_name: &Ident,
    variant: &syn::Variant,
    attr: &DiagnosticAttr,
) -> Result<TokenStream> {
    let variant_name = &variant.ident;
    let field_bindings = field_bindings_pattern(&variant.fields);

    if attr.transparent {
        let first_field = transparent_field(variant)?;
        return Ok(quote! {
            #enum_name::#variant_name #field_bindings => {
                crate::diagnostic::AsDiagnostic::message(#first_field)
            }
        });
    }

    let msg = attr
        .message
        .as_ref()
        .ok_or_else(|| Error::new_spanned(variant, "missing `message`"))?;

    let msg_plain_ts = parse_template_plain(&msg.value(), msg.span())?;

    Ok(quote! {
        #enum_name::#variant_name #field_bindings => { #msg_plain_ts }
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

    let triples = data
        .variants
        .iter()
        .map(|variant| {
            let attr = extract_diagnostic_attr(&variant.attrs)?.ok_or_else(|| {
                Error::new_spanned(
                    variant,
                    format!("variant `{}` missing #[diagnostic(...)]", variant.ident),
                )
            })?;
            Ok((
                generate_variant_arm(enum_name, variant, &attr)?,
                generate_rich_arm(enum_name, variant, &attr)?,
                generate_message_arm(enum_name, variant, &attr)?,
            ))
        })
        .collect::<Result<Vec<_>>>()?;

    let into_arms = triples.iter().map(|(into, _, _)| into);
    let rich_arms = triples.iter().map(|(_, rich, _)| rich);
    let message_arms = triples.iter().map(|(_, _, message)| message);
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

    Ok(quote! {
        impl #impl_generics crate::diagnostic::AsDiagnostic for #enum_name #ty_generics #where_clause {
            #[allow(unused_variables, unused_assignments)]
            fn into_diagnostic(self, __span: crate::lexer::token::Span) -> crate::diagnostic::Diagnostic {
                use crate::diagnostic::{Builder, ERROR, CONTEXT, BOUND, SUGGEST};

                match self {
                    #(#into_arms)*
                }
            }

            #[allow(unused_variables, unused_assignments)]
            fn rich(self, __span: crate::lexer::token::Span) -> crate::diagnostic::RichDiagnostic {
                match self {
                    #(#rich_arms)*
                }
            }

            #[allow(unused_variables)]
            fn message(self) -> String {
                match self {
                    #(#message_arms)*
                }
            }
        }
    })
}
