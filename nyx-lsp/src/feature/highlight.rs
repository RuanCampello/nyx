//! Tolerant semantic highlighter for editor semantic tokens
//!
//! Unlike the compiler's lexer, this scanner never fails and preserves
//! comments, so the editor always gets highlighting even for source that does
//! not parse yet. Identifier roles are inferred syntactically from neighbouring
//! tokens (declarations, calls, types, fields, paths)

use nyx::{is_keyword, is_primitive};

/// A classified, single-line highlight span
///
/// columns and lengths are provided
/// in both encodings so the lsp can answer in whichever the client negotiated
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HighlightToken {
    pub line: u32,
    pub start_utf16: u32,
    pub len_utf16: u32,
    pub start_utf8: u32,
    pub len_utf8: u32,
    pub ty: TokenType,
    pub modifiers: TokenModifiers,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TokenModifiers {
    pub declaration: bool,
    pub readonly: bool,
    pub mutable: bool,
}

/// a raw lexical span before semantic classification
struct Raw<'s> {
    start: usize,
    end: usize,
    kind: RawKind,
    text: &'s str,
}

/// semantic role of a token, mapped by the lsp onto its legend
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenType {
    Namespace,
    Type,
    Function,
    Method,
    Parameter,
    Variable,
    Property,
    Keyword,
    Comment,
    String,
    Number,
    Boolean,
    Operator,
    EnumMember,
}

/// the bracket group immediately containing a token, and whether that group
/// is an `enum` body (whose words are variants)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Encloser {
    bracket: char,
    enum_body: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RawKind {
    Comment,
    Str,
    Number,
    Word,
    Punct,
}

/// produce position-sorted semantic tokens for `src`
pub fn highlight(src: &str) -> Vec<HighlightToken> {
    classify(src, &scan(src))
}

fn scan(src: &str) -> Vec<Raw<'_>> {
    let bytes = src.as_bytes();
    let len = bytes.len();
    let mut raws = Vec::new();
    let mut i = 0;

    while i < len {
        let b = bytes[i];
        match b {
            _ if b.is_ascii_whitespace() => i += 1,

            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                let start = i;
                i += 2;
                while i < len && bytes[i] != b'\n' {
                    i += 1;
                }
                raws.push(Raw { start, end: i, kind: RawKind::Comment, text: &src[start..i] });
            },

            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                let start = i;
                i += 2;
                let mut depth = 1;
                while i < len && depth > 0 {
                    match (bytes[i], bytes.get(i + 1)) {
                        (b'/', Some(&b'*')) => {
                            depth += 1;
                            i += 2;
                        },
                        (b'*', Some(&b'/')) => {
                            depth -= 1;
                            i += 2;
                        },
                        _ => i += 1,
                    }
                }
                raws.push(Raw { start, end: i, kind: RawKind::Comment, text: &src[start..i] });
            },

            b'"' | b'\'' => {
                let start = i;
                i += 1;
                while i < len && bytes[i] != b && bytes[i] != b'\n' {
                    i += match bytes[i] == b'\\' {
                        true => 2,
                        _ => 1,
                    };
                }
                if i < len && bytes[i] == b {
                    i += 1;
                }
                let end = i.min(len);
                raws.push(Raw { start, end, kind: RawKind::Str, text: &src[start..end] });
            },

            _ if b.is_ascii_digit() => {
                let start = i;
                i += 1;
                while i < len {
                    match bytes[i] {
                        b'.' if bytes.get(i + 1).is_some_and(u8::is_ascii_digit) => i += 1,
                        c if c.is_ascii_alphanumeric() || c == b'_' => i += 1,
                        _ => break,
                    }
                }
                raws.push(Raw { start, end: i, kind: RawKind::Number, text: &src[start..i] });
            },

            b'_' | b'a'..=b'z' | b'A'..=b'Z' => {
                let start = i;
                i += 1;
                while i < len && (bytes[i] == b'_' || bytes[i].is_ascii_alphanumeric()) {
                    i += 1;
                }
                raws.push(Raw { start, end: i, kind: RawKind::Word, text: &src[start..i] });
            },

            // stray non-ascii outside a literal, skip the whole char so slices stay valid
            _ if b >= 0x80 => i += utf8_len(b),

            _ => {
                let width = match src.get(i..i + 2) {
                    Some("==" | "!=" | "<=" | ">=" | "&&" | "||" | "<<" | ">>" | "->" | "::") => 2,
                    _ => 1,
                };
                let end = i + width;
                raws.push(Raw { start: i, end, kind: RawKind::Punct, text: &src[i..end] });
                i = end;
            },
        }
    }

    raws
}

fn classify(src: &str, raws: &[Raw<'_>]) -> Vec<HighlightToken> {
    let n = raws.len();
    let (mut prev_code, mut next_code) = (vec![None; n], vec![None; n]);

    let mut last = None;
    for i in 0..n {
        prev_code[i] = last;
        if raws[i].kind != RawKind::Comment {
            last = Some(i);
        }
    }
    let mut next = None;
    for i in (0..n).rev() {
        next_code[i] = next;
        if raws[i].kind != RawKind::Comment {
            next = Some(i);
        }
    }

    let enclosers = enclosers(raws);
    let parameters = parameter_uses(raws, &next_code);
    let starts = line_starts(src);
    let mut out = Vec::with_capacity(n);

    for (i, raw) in raws.iter().enumerate() {
        let (ty, modifiers) = match raw.kind {
            RawKind::Comment => (TokenType::Comment, TokenModifiers::default()),
            RawKind::Str => (TokenType::String, TokenModifiers::default()),
            RawKind::Number => (TokenType::Number, TokenModifiers::default()),
            RawKind::Punct => match is_operator(raw.text) {
                true => (TokenType::Operator, TokenModifiers::default()),
                false => continue,
            },
            RawKind::Word => match raw.text {
                "true" | "false" => (TokenType::Boolean, TokenModifiers::default()),
                word if is_keyword(word) => (TokenType::Keyword, TokenModifiers::default()),
                _ if parameters[i] => (TokenType::Parameter, TokenModifiers::default()),
                _ => classify_word(raws, &prev_code, &next_code, &enclosers, i),
            },
        };

        emit(&mut out, src, &starts, raw.start, raw.end, ty, modifiers);
    }

    out
}

/// parameter names highlighted at their *uses* inside the function body,
/// like rust-analyzer does: collect the `name:` words of each `fn`'s
/// signature, then mark matching words up to the body's closing brace
fn parameter_uses(raws: &[Raw<'_>], next_code: &[Option<usize>]) -> Vec<bool> {
    let mut marks = vec![false; raws.len()];
    let mut i = 0;

    while i < raws.len() {
        if !(raws[i].kind == RawKind::Word && raws[i].text == "fn") {
            i += 1;
            continue;
        }

        let Some((params, body)) = signature_of(raws, next_code, i) else {
            i += 1;
            continue;
        };

        let mut depth = 0;
        let mut at = body;
        while at < raws.len() {
            match (raws[at].kind, raws[at].text) {
                (RawKind::Punct, "{") => depth += 1,
                (RawKind::Punct, "}") => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                },
                (RawKind::Word, word) if params.contains(&word) => {
                    // not a field access, and not a fresh `name:` binding
                    let named = next_code[at].is_some_and(|n| raws[n].text == ":");
                    let field = at.checked_sub(1).is_some_and(|p| raws[p].text == ".");
                    if !named && !field {
                        marks[at] = true;
                    }
                },
                _ => {},
            }
            at += 1;
        }

        i = body + 1;
    }

    marks
}

fn signature_of<'s>(
    raws: &[Raw<'s>],
    next_code: &[Option<usize>],
    at: usize,
) -> Option<(Vec<&'s str>, usize)> {
    let mut i = at + 1;
    while i < raws.len() && raws[i].text != "(" {
        match raws[i].text {
            "{" | "}" | ";" => return None,
            _ => i += 1,
        }
    }

    let mut params = Vec::new();
    let mut depth = 0;
    while i < raws.len() {
        match (raws[i].kind, raws[i].text) {
            (RawKind::Punct, "(") => depth += 1,
            (RawKind::Punct, ")") => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            },
            (RawKind::Word, word)
                if depth == 1 && next_code[i].is_some_and(|n| raws[n].text == ":") =>
            {
                params.push(word);
            },
            _ => {},
        }
        i += 1;
    }

    while i < raws.len() && raws[i].text != "{" {
        match raws[i].text {
            ";" | "}" => return None,
            _ => i += 1,
        }
    }

    (i < raws.len() && !params.is_empty()).then_some((params, i))
}

/// the bracket group immediately containing each token, so a `name:` can be
/// read as a parameter inside `(…)` or a field in `{…}`, and a word inside an
/// `enum` body as a variant
fn enclosers(raws: &[Raw<'_>]) -> Vec<Option<Encloser>> {
    let mut out = vec![None; raws.len()];
    let mut stack: Vec<Encloser> = Vec::new();
    let mut pending_enum = false;

    for (i, raw) in raws.iter().enumerate() {
        if raw.kind == RawKind::Word && raw.text == "enum" {
            pending_enum = true;
        }

        let bracket =
            (raw.kind == RawKind::Punct).then_some(raw.text).and_then(|t| t.chars().next());
        match bracket {
            Some(open @ ('(' | '{' | '[')) => {
                out[i] = stack.last().copied();
                stack.push(Encloser {
                    bracket: open,
                    enum_body: open == '{' && std::mem::take(&mut pending_enum),
                });
            },
            Some(')' | '}' | ']') => {
                stack.pop();
                out[i] = stack.last().copied();
            },
            Some(';') => {
                pending_enum = false;
                out[i] = stack.last().copied();
            },
            _ => out[i] = stack.last().copied(),
        }
    }

    out
}

fn classify_word(
    raws: &[Raw<'_>],
    prev_code: &[Option<usize>],
    next_code: &[Option<usize>],
    enclosers: &[Option<Encloser>],
    i: usize,
) -> (TokenType, TokenModifiers) {
    let text = raws[i].text;
    // the receiver reads as a keyword
    if text == "self" {
        return (TokenType::Keyword, TokenModifiers::default());
    }

    // a word directly inside an `enum { … }` body is a variant declaration
    // (payload types sit one group deeper, inside their `(…)`)
    if enclosers[i].is_some_and(|e| e.enum_body) {
        return (TokenType::EnumMember, TokenModifiers::DECLARATION);
    }

    let prev = prev_code[i].map(|j| raws[j].text);
    let next = next_code[i].map(|j| raws[j].text);
    let capitalised = text.chars().next().is_some_and(char::is_uppercase);
    let none = TokenModifiers::default();

    let by_keyword = match prev {
        Some("fn") => Some((TokenType::Function, TokenModifiers::DECLARATION)),
        Some("struct" | "enum" | "interface") => {
            Some((TokenType::Type, TokenModifiers::DECLARATION))
        },
        Some("impl" | "with" | "as") => Some((TokenType::Type, none)),
        Some("let") => Some((TokenType::Variable, TokenModifiers::DECLARATION)),
        Some("const") => Some((TokenType::Variable, TokenModifiers::READONLY_DECL)),
        Some("use") => Some((TokenType::Namespace, none)),
        Some("mut") => {
            Some(match prev_code[i].and_then(|p| prev_code[p]).map(|pp| raws[pp].text) {
                Some("let") => (TokenType::Variable, TokenModifiers::MUTABLE_DECL),
                _ => (TokenType::Type, none),
            })
        },
        // `:` introduces a type in annotations, but a value in struct literals
        // (`Point { x: x }`), primitives and capitalised names read as types
        Some(":") if capitalised || is_primitive(text) => Some((TokenType::Type, none)),
        Some(":") => Some((TokenType::Variable, none)),
        Some(".") => Some(match next {
            Some("(") => (TokenType::Method, none),
            _ => (TokenType::Property, none),
        }),
        _ => None,
    };
    if let Some(role) = by_keyword {
        return role;
    }

    // a 'name:' binding: a parameter inside '(…)', a struct field inside '{…}'
    // unless it is a 'where T: Bound' type parameter
    if next == Some(":") {
        if prev == Some("where") || is_type_param(text) {
            return (TokenType::Type, none);
        }
        match enclosers[i].map(|e| e.bracket) {
            Some('(') => return (TokenType::Parameter, TokenModifiers::DECLARATION),
            Some('{') => return (TokenType::Property, none),
            _ => {},
        }
    }

    // a capitalised constructor, a qualified variant,
    // or a bare variant in a match arm
    let variant = capitalised
        && !is_screaming_case(text)
        && (next == Some("(") || next == Some("->") || prev == Some("::"));
    if variant {
        return (TokenType::EnumMember, none);
    }

    match next {
        Some("(") => (TokenType::Function, none),
        Some("::") => (TokenType::Namespace, none),
        _ if is_screaming_case(text) => (TokenType::Variable, TokenModifiers::READONLY),
        _ if is_primitive(text) => (TokenType::Type, none),
        _ if capitalised => (TokenType::Type, none),
        // the tail of a `use` path (`use std::panic;`) is still a module
        _ if prev == Some("::") => (TokenType::Namespace, none),
        _ => (TokenType::Variable, none),
    }
}

/// a single uppercase letter is a type parameter (`T`, `S`) by convention
#[inline]
fn is_type_param(text: &str) -> bool {
    text.len() == 1 && text.chars().next().is_some_and(char::is_uppercase)
}

/// `SCREAMING_CASE` names are constants by convention
fn is_screaming_case(text: &str) -> bool {
    text.len() > 1
        && text.chars().any(|c| c.is_ascii_uppercase())
        && text.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// append `start..end` as one token per line it covers
fn emit(
    out: &mut Vec<HighlightToken>,
    src: &str,
    starts: &[usize],
    start: usize,
    end: usize,
    ty: TokenType,
    modifiers: TokenModifiers,
) {
    let mut line = line_of(starts, start);
    while line < starts.len() {
        let line_start = starts[line];
        let content_end = starts.get(line + 1).map_or(src.len(), |&s| s - 1);
        let seg_start = start.max(line_start);
        let seg_end = end.min(content_end);

        if seg_end > seg_start {
            out.push(HighlightToken {
                line: line as u32,
                start_utf8: (seg_start - line_start) as u32,
                len_utf8: (seg_end - seg_start) as u32,
                start_utf16: utf16_len(&src[line_start..seg_start]),
                len_utf16: utf16_len(&src[seg_start..seg_end]),
                ty,
                modifiers,
            });
        }

        if end <= content_end {
            break;
        }

        line += 1;
    }
}

fn line_starts(src: &str) -> Vec<usize> {
    let mut starts = vec![0];
    for (idx, byte) in src.bytes().enumerate() {
        if byte == b'\n' {
            starts.push(idx + 1);
        }
    }
    starts
}

#[inline]
fn line_of(starts: &[usize], offset: usize) -> usize {
    starts.partition_point(|&s| s <= offset).saturating_sub(1)
}

#[inline]
fn utf16_len(s: &str) -> u32 {
    s.chars().map(|c| c.len_utf16() as u32).sum()
}

#[inline]
const fn utf8_len(lead: u8) -> usize {
    match lead {
        b if b >= 0xF0 => 4,
        b if b >= 0xE0 => 3,
        b if b >= 0xC0 => 2,
        _ => 1,
    }
}

#[inline]
fn is_operator(text: &str) -> bool {
    matches!(
        text,
        "+" | "-"
            | "*"
            | "/"
            | "="
            | "=="
            | "!="
            | "<"
            | ">"
            | "<="
            | ">="
            | "&&"
            | "||"
            | "&"
            | "|"
            | "^"
            | "<<"
            | ">>"
            | "!"
            | "->"
    )
}

impl TokenModifiers {
    const DECLARATION: Self = Self { declaration: true, readonly: false, mutable: false };
    const READONLY: Self = Self { declaration: false, readonly: true, mutable: false };
    const READONLY_DECL: Self = Self { declaration: true, readonly: true, mutable: false };
    const MUTABLE_DECL: Self = Self { declaration: true, readonly: false, mutable: true };
}

#[cfg(test)]
mod tests {
    use super::*;
    use expect_test::{Expect, expect};

    fn check(src: &str, expect: Expect) {
        expect.assert_eq(&render(src));
    }

    fn render(src: &str) -> String {
        let starts = line_starts(src);

        highlight(src)
            .iter()
            .map(|token| {
                let line_start = starts[token.line as usize];
                let start = line_start + token.start_utf8 as usize;
                let text = &src[start..start + token.len_utf8 as usize];

                let mut kind = kind_name(token.ty).to_owned();
                for (flag, name) in [
                    (token.modifiers.declaration, "declaration"),
                    (token.modifiers.readonly, "readonly"),
                    (token.modifiers.mutable, "mutable"),
                ] {
                    if flag {
                        kind.push('.');
                        kind.push_str(name);
                    }
                }

                format!("{kind} {text}")
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    const fn kind_name<'s>(ty: TokenType) -> &'s str {
        match ty {
            TokenType::Namespace => "namespace",
            TokenType::Type => "type",
            TokenType::Function => "function",
            TokenType::Method => "method",
            TokenType::Parameter => "parameter",
            TokenType::Variable => "variable",
            TokenType::Property => "property",
            TokenType::Keyword => "keyword",
            TokenType::Comment => "comment",
            TokenType::String => "string",
            TokenType::Number => "number",
            TokenType::Boolean => "boolean",
            TokenType::Operator => "operator",
            TokenType::EnumMember => "enumMember",
        }
    }

    #[test]
    fn function_with_parameters_and_body() {
        check(
            "fn add(a: i32, b: i32): i32 { a + b }",
            expect![[r#"
            keyword fn
            function.declaration add
            parameter.declaration a
            type i32
            parameter.declaration b
            type i32
            type i32
            parameter a
            operator +
            parameter b"#]],
        );
    }

    #[test]
    fn bindings_literals_and_booleans() {
        check(
            "let x = 232; let mut y: bool = true;",
            expect![[r#"
            keyword let
            variable.declaration x
            operator =
            number 232
            keyword let
            keyword mut
            variable.declaration.mutable y
            type bool
            operator =
            boolean true"#]],
        );
    }

    #[test]
    fn const_binding_is_readonly() {
        check(
            "const MAX: i32 = 10;",
            expect![[r#"
            keyword const
            variable.declaration.readonly MAX
            type i32
            operator =
            number 10"#]],
        );
    }

    #[test]
    fn struct_definition_literal_and_fields() {
        check(
            "struct Point { x: i32 } fn make() { let p = Point { x: 1 }; }",
            expect![[r#"
            keyword struct
            type.declaration Point
            property x
            type i32
            keyword fn
            function.declaration make
            keyword let
            variable.declaration p
            operator =
            type Point
            property x
            number 1"#]],
        );
    }

    #[test]
    fn field_access_and_method_call() {
        check(
            "c.value = c.get();",
            expect![[r#"
            variable c
            property value
            operator =
            variable c
            method get"#]],
        );
    }

    #[test]
    fn use_path_tail_is_a_namespace() {
        check(
            "use std::panic;",
            expect![[r#"
            keyword use
            namespace std
            namespace panic"#]],
        );
    }

    #[test]
    fn variant_constructors_match_bare_variants() {
        check(
            "match self { Some(value) -> value, None -> false, }",
            expect![[r#"
            keyword match
            keyword self
            enumMember Some
            variable value
            operator ->
            variable value
            enumMember None
            operator ->
            boolean false"#]],
        );
    }

    #[test]
    fn enum_declaration_variants_are_members() {
        check(
            "enum Status { Ready, Done(T) } as u16",
            expect![[r#"
            keyword enum
            type.declaration Status
            enumMember.declaration Ready
            enumMember.declaration Done
            type T
            keyword as
            type u16"#]],
        );
    }

    #[test]
    fn receivers_parameters_and_primitives_in_signatures() {
        check(
            "fn expect(self, msg: &str): S { panic::panic(msg) }",
            expect![[r#"
            keyword fn
            function.declaration expect
            keyword self
            parameter.declaration msg
            operator &
            type str
            type S
            namespace panic
            function panic
            parameter msg"#]],
        );
    }

    #[test]
    fn screaming_case_uses_are_readonly() {
        check(
            "syscall(SYS_EXIT, i32::MAX);",
            expect![[r#"
            function syscall
            variable.readonly SYS_EXIT
            namespace i32
            variable.readonly MAX"#]],
        );
    }

    #[test]
    fn where_bounds_are_types() {
        check(
            "fn or_default(self): T where T: Default {",
            expect![[r#"
            keyword fn
            function.declaration or_default
            keyword self
            type T
            keyword where
            type T
            type Default"#]],
        );
    }

    #[test]
    fn comments_are_tolerant_and_string_safe() {
        check(
            r#"let s = "// not a comment"; // real"#,
            expect![[r#"
            keyword let
            variable.declaration s
            operator =
            string "// not a comment"
            comment // real"#]],
        );
    }

    #[test]
    fn block_comment_splits_per_line() {
        check(
            "/* a\nb */",
            expect![[r#"
            comment /* a
            comment b */"#]],
        );
    }
}
