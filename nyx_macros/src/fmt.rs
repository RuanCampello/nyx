use proc_macro2::{Span, TokenStream};
use quote::quote;
use syn::Ident;

#[derive(Debug)]
enum Segment {
    Literal(String),
    Field {
        name: String,
        color: FieldColor,
    },
    /// A backtick-delimited code snippet styled with SECONDARY colour.
    /// May itself contain plain `{field}` references.
    CodeSnippet(Vec<SnippetPart>),
}

#[derive(Debug)]
enum SnippetPart {
    Literal(String),
    Field(String),
}

#[derive(Debug, Clone, Copy)]
enum FieldColor {
    Plain,
    Hi,
    Primary,
    Secondary,
}

pub fn parse_template(template: &str, call_site: Span) -> syn::Result<TokenStream> {
    let segments = parse_segments(template, call_site)?;
    Ok(emit_segments(&segments, call_site))
}

fn parse_segments(input: &str, span: Span) -> syn::Result<Vec<Segment>> {
    let mut segments = Vec::new();
    let mut chars = input.char_indices().peekable();
    let mut literal = String::new();

    while let Some((i, ch)) = chars.next() {
        if ch != '{' {
            literal.push(ch);
            continue;
        }

        if !literal.is_empty() {
            segments.push(Segment::Literal(std::mem::take(&mut literal)));
        }

        match chars.peek() {
            Some((_, '`')) => {
                chars.next(); // consume '`'
                let mut content = String::new();
                let mut closed = false;
                while let Some((_, c)) = chars.next() {
                    if c == '`' {
                        match chars.next() {
                            Some((_, '}')) => {
                                closed = true;
                                break;
                            },
                            Some((j, other)) => {
                                return Err(syn::Error::new(
                                    span,
                                    format!(
                                        "expected `}}` after closing backtick, found `{other}` at offset {j}"
                                    ),
                                ));
                            },
                            None => break,
                        }
                    } else {
                        content.push(c);
                    }
                }
                if !closed {
                    return Err(syn::Error::new(span, "unterminated code snippet `{`...`}`"));
                }
                segments.push(Segment::CodeSnippet(parse_snippet_parts(&content)));
            },

            _ => {
                // Field interpolation: {name}, {name!}, {name~}, {name^}
                let mut name = String::new();
                let mut color = FieldColor::Plain;
                let mut closed = false;

                while let Some((_, c)) = chars.next() {
                    match c {
                        '}' => {
                            closed = true;
                            break;
                        },
                        '!' => {
                            color = FieldColor::Hi;
                            match chars.next() {
                                Some((_, '}')) => {
                                    closed = true;
                                    break;
                                },
                                Some((j, other)) => {
                                    return Err(syn::Error::new(
                                        span,
                                        format!(
                                            "expected `}}` after `!`, found `{other}` at offset {j}"
                                        ),
                                    ));
                                },
                                None => break,
                            }
                        },
                        '~' => {
                            color = FieldColor::Primary;
                            match chars.next() {
                                Some((_, '}')) => {
                                    closed = true;
                                    break;
                                },
                                Some((j, other)) => {
                                    return Err(syn::Error::new(
                                        span,
                                        format!(
                                            "expected `}}` after `~`, found `{other}` at offset {j}"
                                        ),
                                    ));
                                },
                                None => break,
                            }
                        },
                        '^' => {
                            color = FieldColor::Secondary;
                            match chars.next() {
                                Some((_, '}')) => {
                                    closed = true;
                                    break;
                                },
                                Some((j, other)) => {
                                    return Err(syn::Error::new(
                                        span,
                                        format!(
                                            "expected `}}` after `^`, found `{other}` at offset {j}"
                                        ),
                                    ));
                                },
                                None => break,
                            }
                        },
                        c => name.push(c),
                    }
                }

                if !closed {
                    return Err(syn::Error::new(
                        span,
                        format!("unterminated interpolation `{{{name}...` at offset {i}"),
                    ));
                }
                if name.is_empty() {
                    return Err(syn::Error::new(span, "empty interpolation `{}`"));
                }

                segments.push(Segment::Field { name, color });
            },
        }
    }

    if !literal.is_empty() {
        segments.push(Segment::Literal(literal));
    }

    Ok(segments)
}

/// Parse the interior of a backtick code snippet for plain `{field}` references.
fn parse_snippet_parts(input: &str) -> Vec<SnippetPart> {
    let mut parts = Vec::new();
    let mut chars = input.chars().peekable();
    let mut literal = String::new();

    while let Some(ch) = chars.next() {
        if ch != '{' {
            literal.push(ch);
            continue;
        }
        // peek: if next is '}' it's an empty brace, treat as literal
        let mut name = String::new();
        let mut closed = false;
        for c in chars.by_ref() {
            if c == '}' {
                closed = true;
                break;
            }
            name.push(c);
        }
        if closed && !name.is_empty() {
            if !literal.is_empty() {
                parts.push(SnippetPart::Literal(std::mem::take(&mut literal)));
            }
            parts.push(SnippetPart::Field(name));
        } else {
            // not a valid interpolation, treat as literal
            literal.push('{');
            literal.push_str(&name);
            if closed {
                literal.push('}');
            }
        }
    }

    if !literal.is_empty() {
        parts.push(SnippetPart::Literal(literal));
    }

    parts
}

fn emit_segments(segments: &[Segment], call_site: Span) -> TokenStream {
    // Single plain literal — no format! needed
    if let [Segment::Literal(s)] = segments {
        return quote! { #s.to_string() };
    }

    let parts: Vec<TokenStream> = segments
        .iter()
        .map(|seg| match seg {
            Segment::Literal(s) => quote! { __buf.push_str(#s); },

            Segment::Field { name, color } => {
                let ident = Ident::new(name, call_site);
                match color {
                    FieldColor::Plain => quote! { __buf.push_str(&format!("{}", #ident)); },
                    FieldColor::Hi => quote! { __buf.push_str(&format!("{}", hi(&#ident))); },
                    FieldColor::Primary => quote! {
                        use ariadne::Fmt as _;
                        __buf.push_str(&format!("{}", (#ident).fg(PRIMARY)));
                    },
                    FieldColor::Secondary => quote! {
                        use ariadne::Fmt as _;
                        __buf.push_str(&format!("{}", (#ident).fg(SECONDARY)));
                    },
                }
            },

            Segment::CodeSnippet(parts) => emit_snippet(parts, call_site),
        })
        .collect();

    quote! {
        {
            let mut __buf = String::new();
            #(#parts)*
            __buf
        }
    }
}

/// Emit a code snippet: build the inner string (with field substitutions), then apply `.fg(SECONDARY)`.
fn emit_snippet(parts: &[SnippetPart], call_site: Span) -> TokenStream {
    // Fast path: no field references, pure literal
    if parts.iter().all(|p| matches!(p, SnippetPart::Literal(_))) {
        let s: String = parts
            .iter()
            .map(|p| match p {
                SnippetPart::Literal(l) => l.as_str(),
                _ => unreachable!(),
            })
            .collect();
        return quote! {
            use ariadne::Fmt as _;
            __buf.push_str(&format!("{}", #s.fg(SECONDARY)));
        };
    }

    // Build a format string + argument list for the interpolated parts
    let mut fmt_str = String::new();
    let mut args: Vec<TokenStream> = Vec::new();

    for part in parts {
        match part {
            SnippetPart::Literal(s) => {
                // Escape any `{` or `}` in the literal for format!
                fmt_str.push_str(&s.replace('{', "{{").replace('}', "}}"));
            },
            SnippetPart::Field(name) => {
                fmt_str.push_str("{}");
                let ident = Ident::new(name, call_site);
                args.push(quote! { #ident });
            },
        }
    }

    quote! {
        use ariadne::Fmt as _;
        __buf.push_str(&format!("{}", format!(#fmt_str, #(#args),*).fg(SECONDARY)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Vec<Segment> {
        parse_segments(s, Span::call_site()).expect("parse failed")
    }

    #[test]
    fn literal_only() {
        let segs = parse("hello world");
        assert!(matches!(&segs[0], Segment::Literal(s) if s == "hello world"));
    }

    #[test]
    fn plain_field() {
        let segs = parse("call to {name}");
        assert!(matches!(&segs[0], Segment::Literal(s) if s == "call to "));
        assert!(
            matches!(&segs[1], Segment::Field { name, color: FieldColor::Plain } if name == "name")
        );
    }

    #[test]
    fn hi_field() {
        let segs = parse("{name!} is unknown");
        assert!(
            matches!(&segs[0], Segment::Field { name, color: FieldColor::Hi } if name == "name")
        );
    }

    #[test]
    fn code_snippet_no_interpolation() {
        let segs = parse("try {`fn foo()`}");
        assert!(matches!(&segs[1], Segment::CodeSnippet(parts)
            if matches!(parts.as_slice(), [SnippetPart::Literal(s)] if s == "fn foo()")));
    }

    #[test]
    fn code_snippet_with_interpolation() {
        let segs = parse("declare {`fn {name}(…)`}");
        let Segment::CodeSnippet(parts) = &segs[1] else {
            panic!("expected CodeSnippet")
        };
        assert!(matches!(&parts[0], SnippetPart::Literal(s) if s == "fn "));
        assert!(matches!(&parts[1], SnippetPart::Field(s) if s == "name"));
        assert!(matches!(&parts[2], SnippetPart::Literal(s) if s == "(…)"));
    }

    #[test]
    fn primary_and_secondary() {
        let segs = parse("{a~} vs {b^}");
        assert!(matches!(&segs[0], Segment::Field { color: FieldColor::Primary, .. }));
        assert!(matches!(&segs[2], Segment::Field { color: FieldColor::Secondary, .. }));
    }
}

