use proc_macro2::{Span, TokenStream};
use quote::quote;

#[derive(Debug)]
enum Segment {
    Literal(String),
    Field { name: String, color: FieldColor },
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
    Ok(emit_segments(&segments))
}

pub fn parse_template_plain(template: &str, call_site: Span) -> syn::Result<TokenStream> {
    let segments = parse_segments(template, call_site)?;
    Ok(emit_segments_plain(&segments))
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

        let Some(&(_, next_ch)) = chars.peek() else {
            return Err(syn::Error::new(span, "unterminated interpolation `{`"));
        };

        if next_ch == '`' {
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
                }
                content.push(c);
            }
            if !closed {
                return Err(syn::Error::new(span, "unterminated code snippet `{`...`}`"));
            }
            segments.push(Segment::CodeSnippet(parse_snippet_parts(&content)));
            continue;
        }

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
                '!' | '~' | '^' => {
                    color = match c {
                        '!' => FieldColor::Hi,
                        '~' => FieldColor::Primary,
                        '^' => FieldColor::Secondary,
                        _ => unreachable!(),
                    };
                    match chars.next() {
                        Some((_, '}')) => {
                            closed = true;
                            break;
                        },
                        Some((j, other)) => {
                            return Err(syn::Error::new(
                                span,
                                format!(
                                    "expected `}}` after modifier, found `{other}` at offset {j}"
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

        if !is_valid_identifier(&name) {
            literal.push_str(&format!("{{{name}}}"));
        } else {
            segments.push(Segment::Field { name, color });
        }
    }

    if !literal.is_empty() {
        segments.push(Segment::Literal(literal));
    }

    Ok(segments)
}

fn parse_snippet_parts(input: &str) -> Vec<SnippetPart> {
    let mut parts = Vec::new();
    let mut chars = input.chars().peekable();
    let mut literal = String::new();

    while let Some(ch) = chars.next() {
        if ch != '{' {
            literal.push(ch);
            continue;
        }
        let mut name = String::new();
        let mut closed = false;
        for c in chars.by_ref() {
            if c == '}' {
                closed = true;
                break;
            }
            name.push(c);
        }
        if closed && !name.is_empty() && is_valid_identifier(&name) {
            if !literal.is_empty() {
                parts.push(SnippetPart::Literal(std::mem::take(&mut literal)));
            }
            parts.push(SnippetPart::Field(name));
        } else {
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

fn emit_segments_plain(segments: &[Segment]) -> TokenStream {
    if let [Segment::Literal(s)] = segments {
        return quote! { #s.to_string() };
    }

    let parts = segments.iter().map(|seg| match seg {
        Segment::Literal(s) => quote! { __buf.push_str(#s); },
        Segment::Field { name, .. } => {
            let expr: syn::Expr =
                syn::parse_str(name).unwrap_or_else(|_| panic!("invalid field expression: {name}"));
            quote! { __buf.push_str(&format!("{}", #expr)); }
        },
        Segment::CodeSnippet(parts) => emit_snippet_plain(parts),
    });

    quote! {
        {
            let mut __buf = String::new();
            #(#parts)*
            __buf
        }
    }
}

fn emit_snippet_plain(parts: &[SnippetPart]) -> TokenStream {
    if parts.iter().all(|p| matches!(p, SnippetPart::Literal(_))) {
        let s: String = parts
            .iter()
            .map(|p| match p {
                SnippetPart::Literal(l) => l.as_str(),
                _ => unreachable!(),
            })
            .collect();
        return quote! { __buf.push_str(#s); };
    }

    let mut fmt_str = String::new();
    let mut args = Vec::new();

    for part in parts {
        match part {
            SnippetPart::Literal(s) => fmt_str.push_str(&s.replace('{', "{{").replace('}', "}}")),
            SnippetPart::Field(name) => {
                fmt_str.push_str("{}");
                let expr: syn::Expr = syn::parse_str(name)
                    .unwrap_or_else(|_| panic!("invalid field expression: {name}"));
                args.push(quote! { &#expr });
            },
        }
    }

    quote! { __buf.push_str(&format!(#fmt_str, #(#args),*)); }
}

fn emit_segments(segments: &[Segment]) -> TokenStream {
    if let [Segment::Literal(s)] = segments {
        return quote! { #s.to_string() };
    }

    let parts = segments.iter().map(|seg| match seg {
        Segment::Literal(s) => quote! { __buf.push_str(#s); },
        Segment::Field { name, color } => {
            let expr: syn::Expr =
                syn::parse_str(name).unwrap_or_else(|_| panic!("invalid field expression: {name}"));
            match color {
                FieldColor::Plain => quote! { __buf.push_str(&format!("{}", #expr)); },
                FieldColor::Hi => quote! { __buf.push_str(&format!("{}", hi(&#expr))); },
                FieldColor::Primary => quote! {
                    use ariadne::Fmt as _;
                    __buf.push_str(&format!("{}", (&#expr).fg(PRIMARY)));
                },
                FieldColor::Secondary => quote! {
                    use ariadne::Fmt as _;
                    __buf.push_str(&format!("{}", (&#expr).fg(SECONDARY)));
                },
            }
        },
        Segment::CodeSnippet(parts) => emit_snippet(parts),
    });

    quote! {
        {
            let mut __buf = String::new();
            #(#parts)*
            __buf
        }
    }
}

fn emit_snippet(parts: &[SnippetPart]) -> TokenStream {
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

    let mut fmt_str = String::new();
    let mut args = Vec::new();

    for part in parts {
        match part {
            SnippetPart::Literal(s) => fmt_str.push_str(&s.replace('{', "{{").replace('}', "}}")),
            SnippetPart::Field(name) => {
                fmt_str.push_str("{}");
                let expr: syn::Expr = syn::parse_str(name)
                    .unwrap_or_else(|_| panic!("invalid field expression: {name}"));
                args.push(quote! { &#expr });
            },
        }
    }

    quote! {
        use ariadne::Fmt as _;
        __buf.push_str(&format!("{}", format!(#fmt_str, #(#args),*).fg(SECONDARY)));
    }
}

fn is_valid_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_alphabetic() || first == '_')
        && chars.all(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | '(' | ')'))
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
    fn ellipsis_literal_braces_not_treated_as_field() {
        let segs = parse("add {`fn {name}(…)`} before using it");
        let Segment::CodeSnippet(parts) = &segs[1] else {
            panic!("expected CodeSnippet")
        };
        assert!(parts.iter().any(|p| matches!(p, SnippetPart::Literal(s) if s.contains('…'))));
    }

    #[test]
    fn primary_and_secondary() {
        let segs = parse("{a~} vs {b^}");
        assert!(matches!(&segs[0], Segment::Field { color: FieldColor::Primary, .. }));
        assert!(matches!(&segs[2], Segment::Field { color: FieldColor::Secondary, .. }));
    }
}
