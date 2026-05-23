/// Parse the interpolation DSL used in `#[diagnostic(...)]` string attributes
use proc_macro2::{Span, TokenStream};
use quote::quote;
use syn::Ident;

#[derive(Debug)]
enum Segment {
    Literal(String),
    Field { name: String, color: FieldColor },
    CodeSnippet(String),
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
    let mut current_literal = String::new();

    while let Some((i, ch)) = chars.next() {
        if ch != '{' {
            current_literal.push(ch);
            continue;
        }

        // Flush accumulated literal
        if !current_literal.is_empty() {
            segments.push(Segment::Literal(std::mem::take(&mut current_literal)));
        }

        // Peek: backtick = code snippet
        match chars.peek() {
            Some((_, '`')) => {
                chars.next(); // consume backtick
                let mut content = String::new();
                let mut closed = false;
                while let Some((_, c)) = chars.next() {
                    if c == '`' {
                        // expect closing brace
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
                    return Err(syn::Error::new(
                        span,
                        "unterminated code snippet interpolation `{{`...`}}`",
                    ));
                }
                segments.push(Segment::CodeSnippet(content));
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
                            // expect closing brace next
                            match chars.next() {
                                Some((_, '}')) => {
                                    closed = true;
                                    break;
                                },
                                Some((j, other)) => {
                                    return Err(syn::Error::new(
                                        span,
                                        format!(
                                            "expected `}}` after `!` in interpolation, found `{other}` at offset {j}"
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
                                            "expected `}}` after `~` in interpolation, found `{other}` at offset {j}"
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
                                            "expected `}}` after `^` in interpolation, found `{other}` at offset {j}"
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
                        format!("unterminated interpolation `{{{name}...` starting at offset {i}"),
                    ));
                }
                if name.is_empty() {
                    return Err(syn::Error::new(span, "empty interpolation `{}`"));
                }

                segments.push(Segment::Field { name, color });
            },
        }
    }

    if !current_literal.is_empty() {
        segments.push(Segment::Literal(current_literal));
    }

    Ok(segments)
}

fn emit_segments(segments: &[Segment], call_site: Span) -> TokenStream {
    // Optimisation: single plain literal → just a string literal, no format! call
    if segments.len() == 1 {
        if let Segment::Literal(s) = &segments[0] {
            return quote! { #s.to_string() };
        }
    }

    // Build a series of push operations onto a String
    let mut parts = Vec::new();

    for seg in segments {
        let ts = match seg {
            Segment::Literal(s) => {
                quote! { __buf.push_str(#s); }
            },
            Segment::Field { name, color } => {
                let ident = Ident::new(name, call_site);
                match color {
                    FieldColor::Plain => quote! {
                        __buf.push_str(&format!("{}", #ident));
                    },
                    FieldColor::Hi => quote! {
                        __buf.push_str(&format!("{}", hi(&#ident)));
                    },
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
            Segment::CodeSnippet(s) => {
                // Code snippets can themselves contain {field} interpolation
                // We evaluate them as a plain format string substituting field idents.
                // For now: treat as a literal that gets .fg(SECONDARY)
                // A future improvement could recurse the parser here.
                quote! {
                    use ariadne::Fmt as _;
                    __buf.push_str(&format!("{}", #s.fg(SECONDARY)));
                }
            },
        };
        parts.push(ts);
    }

    quote! {
        {
            let mut __buf = String::new();
            #(#parts)*
            __buf
        }
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
    fn code_snippet() {
        let segs = parse("try {`fn foo()`}");
        assert!(matches!(&segs[1], Segment::CodeSnippet(s) if s == "fn foo()"));
    }

    #[test]
    fn primary_and_secondary() {
        let segs = parse("{a~} vs {b^}");
        assert!(matches!(&segs[0], Segment::Field { color: FieldColor::Primary, .. }));
        assert!(matches!(&segs[2], Segment::Field { color: FieldColor::Secondary, .. }));
    }
}
