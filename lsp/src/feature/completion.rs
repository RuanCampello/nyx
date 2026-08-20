//! Completion: read what the cursor is qualified by, then answer from the
//! index [crate::analysis] built while the HIR was alive

use crate::analysis::{Completion, Completions, GenericSlots};
use frontend::lexer::token::Keyword;
use std::borrow::Cow;

/// What qualifies the position being completed
#[derive(Debug, PartialEq, Eq)]
pub enum Context<'s> {
    /// after `receiver.`, offering that value's fields and methods
    Member { receiver: &'s str },
    /// after `Qualifier::`, offering a type's associated items or a module's exports
    Path { qualifier: String, import: bool },
    /// anywhere else, offering everything nameable
    Open,
    /// directly inside an interface implementation, offering its required items
    InterfaceImpl { interface: String },
    /// after a `.` whose receiver is an expression with no name to look up, where
    /// offering the open scope instead would read as a working completion
    Unresolved,
    /// inside a comment or a literal, where prose is not code and nothing is nameable
    Inert,
}

/// what a request resolved to: the candidates exactly as the index holds them,
/// plus the generic arguments the receiver was written with
pub struct Candidates<'a> {
    pub items: Vec<Completion<'a>>,
    substitution: Option<(&'a GenericSlots<'a>, Vec<&'a str>)>,
}

/// What the text before the cursor is still inside of
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Code,
    Comment,
    Str,
    Char,
}

pub fn context_at(text: &str, offset: usize) -> Context<'_> {
    let before = &text[..offset.min(text.len())];
    if mode_at(before) != Mode::Code {
        return Context::Inert;
    }

    let before = before.trim_end_matches(is_name_char);

    if let Some(head) = before.strip_suffix('.') {
        return match receiver_chain(head) {
            Some(receiver) => Context::Member { receiver },
            None if head.ends_with([')', ']']) => Context::Unresolved,
            _ => Context::Open,
        };
    }

    let before = before.trim_end_matches([' ', '\t', '\n', ',']);
    let implementation = interface_impl_at(before);

    let (path, in_list) = match brace_list_start(before) {
        Some(open) => (&before[..open], true),
        _ => (before, false),
    };

    match path.strip_suffix("::") {
        Some(head) => Context::Path {
            qualifier: qualifier_at(before, path_before(head)),
            import: !in_list && is_use_path(head),
        },
        _ => match implementation {
            Some(interface) => Context::InterfaceImpl { interface },
            _ => Context::Open,
        },
    }
}

pub fn candidates<'a>(
    index: &'a Completions<'a>,
    context: &Context<'_>,
    scope: Option<&'a [Completion<'a>]>,
) -> Candidates<'a> {
    let associated = |name: &str| Candidates {
        items: index.associated.get(name).cloned().unwrap_or_default(),
        substitution: None,
    };

    match context {
        Context::Member { receiver } => receiver_members(index, scope, receiver),
        Context::Path { qualifier, .. } => associated(qualifier),
        Context::InterfaceImpl { interface } => associated(interface),
        Context::Open => Candidates {
            items: scope.unwrap_or_default().iter().chain(index.globals.iter()).copied().collect(),
            substitution: None,
        },
        Context::Unresolved | Context::Inert => Candidates::none(),
    }
}

impl<'a> Candidates<'a> {
    #[inline]
    fn none() -> Self {
        Self { items: Vec::new(), substitution: None }
    }

    pub fn detail(&self, item: &Completion<'a>) -> Cow<'a, str> {
        match &self.substitution {
            Some((slots, args)) => slots.substitute(item.detail, args),
            _ => Cow::Borrowed(item.detail),
        }
    }

    /// the key a member's own type continues the chain under
    fn key_after(&self, key: &'a str) -> &'a str {
        match &self.substitution {
            Some((slots, args)) => slots.argument(key, args).unwrap_or(key),
            _ => key,
        }
    }
}

#[inline(always)]
pub fn keywords<'k>() -> impl Iterator<Item = &'k str> {
    Keyword::ALL.iter().map(|keyword| keyword.as_str())
}

/// replay the cursor's own line to see what it is still inside of
fn mode_at(before: &str) -> Mode {
    let line = before.rsplit('\n').next().unwrap_or(before);
    let mut mode = Mode::Code;
    let mut chars = line.chars().peekable();

    while let Some(c) = chars.next() {
        mode = match (mode, c) {
            (Mode::Code, '/') if chars.peek() == Some(&'/') => Mode::Comment,
            (Mode::Code, '"') => Mode::Str,
            (Mode::Code, '\'') => Mode::Char,
            (Mode::Code, _) => Mode::Code,
            (Mode::Comment, _) => Mode::Comment,
            (Mode::Str | Mode::Char, '\\') => {
                chars.next();
                mode
            },
            (Mode::Str, '"') | (Mode::Char, '\'') => Mode::Code,
            (Mode::Str | Mode::Char, _) => mode,
        };
    }

    mode
}

/// walk the dotted chain one segment at a time, so a receiver reached through a
/// field resolves exactly as a directly-named one does
fn receiver_members<'a>(
    index: &'a Completions<'a>,
    scope: Option<&'a [Completion<'a>]>,
    receiver: &str,
) -> Candidates<'a> {
    let mut segments = receiver.split('.');
    let Some(head) = segments.next() else {
        return Candidates::none();
    };

    let named = |items: &[Completion<'a>]| {
        items.iter().find(|item| item.label == head).and_then(|item| item.type_key)
    };

    let Some(mut typ) = named(scope.unwrap_or_default())
        .or_else(|| named(&index.globals))
        .or_else(|| index.members.get_key_value(head).map(|(&key, _)| key))
    else {
        return Candidates::none();
    };

    loop {
        let resolved = members_of(index, typ);
        let Some(segment) = segments.next() else {
            return resolved;
        };

        let through = resolved.items.iter().find(|item| item.label == segment);
        match through.and_then(|item| item.type_key) {
            Some(next) => typ = resolved.key_after(next),
            _ => return Candidates::none(),
        }
    }
}

/// the members a value of the rendered type `typ` offers, carrying the arguments
/// it was written with for the parameters of the template that owns them
fn members_of<'a>(index: &'a Completions<'a>, typ: &'a str) -> Candidates<'a> {
    let (key, args) = split_type(typ);
    let items = index.members.get(key).cloned().unwrap_or_default();
    let substitution = match args.is_empty() {
        true => None,
        _ => index.generics.get(key).map(|slots| (slots, args)),
    };

    Candidates { items, substitution }
}

/// a rendered type split into the key its members are indexed under and the
/// generic arguments it was written with
fn split_type(typ: &str) -> (&str, Vec<&str>) {
    let (Some(open), Some(close)) = (typ.find('<'), typ.rfind('>')) else {
        return (typ, Vec::new());
    };

    let (mut args, mut depth, mut start) = (Vec::new(), 0usize, open + 1);
    for (at, c) in typ[open + 1..close].char_indices() {
        let at = open + 1 + at;
        match c {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                args.push(typ[start..at].trim());
                start = at + 1;
            },
            _ => {},
        }
    }
    args.push(typ[start..close].trim());

    (&typ[..open], args)
}

fn interface_impl_at(before: &str) -> Option<String> {
    let open = *open_braces(before).last()?;
    impl_header(before, open).and_then(|(_, interface)| interface)
}

fn qualifier_at(before: &str, qualifier: String) -> String {
    let Some(rest) = qualifier.strip_prefix("Self") else {
        return qualifier;
    };
    if !rest.is_empty() && !rest.starts_with("::") {
        return qualifier;
    }

    match impl_self_type(before) {
        Some(on) => format!("{on}{rest}"),
        None => qualifier,
    }
}

fn impl_self_type(before: &str) -> Option<String> {
    open_braces(before)
        .iter()
        .rev()
        .find_map(|&open| impl_header(before, open).map(|(on, _)| on))
}

/// `(implementing type, interface)` when the block opened at `open` is an `impl`
fn impl_header(before: &str, open: usize) -> Option<(String, Option<String>)> {
    let header = &before[..open];
    let header = header.rfind(['{', '}', ';']).map_or(header, |boundary| &header[boundary + 1..]);
    let mut words = header.split(|c: char| !is_name_char(c)).filter(|word| !word.is_empty());

    if words.next()? != "impl" {
        return None;
    }

    let on = words.next()?.to_owned();
    let interface = match words.any(|word| word == "with") {
        true => words.next().map(str::to_owned),
        false => None,
    };

    Some((on, interface))
}

/// the offsets of every brace still open at the end of `before`, outermost first
fn open_braces(before: &str) -> Vec<usize> {
    let (mut braces, mut mode) = (Vec::new(), Mode::Code);
    let bytes = before.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        mode = match (mode, bytes[i]) {
            (Mode::Code, b'/') if bytes.get(i + 1) == Some(&b'/') => {
                i += 1;
                Mode::Comment
            },
            (Mode::Code, b'"') => Mode::Str,
            (Mode::Code, b'\'') => Mode::Char,
            (Mode::Code, b'{') => {
                braces.push(i);
                Mode::Code
            },
            (Mode::Code, b'}') => {
                braces.pop();
                Mode::Code
            },
            (Mode::Comment, b'\n') => Mode::Code,
            (Mode::Str | Mode::Char, b'\\') => {
                i += 1;
                mode
            },
            (Mode::Str, b'"') | (Mode::Char, b'\'') => Mode::Code,
            _ => mode,
        };
        i += 1;
    }

    braces
}

/// where the innermost open brace starts, when everything after it is a plain
/// list of names
fn brace_list_start(before: &str) -> Option<usize> {
    let open = *open_braces(before).last()?;

    before[open + 1..]
        .chars()
        .all(|c| is_name_char(c) || c == ',' || c.is_whitespace())
        .then_some(open)
}

#[inline]
fn is_use_path(path: &str) -> bool {
    let statement = path.rsplit([';', '{', '}', '\n']).next().unwrap_or(path);
    statement.trim_start().starts_with("use ")
}

fn receiver_chain(text: &str) -> Option<&str> {
    let (mut rest, mut start) = (text, None);

    loop {
        let name = tail_name(rest);
        if name.is_empty() {
            break;
        }

        rest = &rest[..rest.len() - name.len()];
        start = Some(rest.len());

        match rest.strip_suffix('.') {
            Some(head) => rest = head,
            _ => break,
        }
    }

    start.map(|start| &text[start..])
}

/// The `::`-separated path ending at the end of `text`
fn path_before(text: &str) -> String {
    let mut segments = Vec::new();
    let mut rest = text;

    loop {
        let name = tail_name(rest);
        if name.is_empty() {
            break;
        }
        segments.push(name);
        rest = &rest[..rest.len() - name.len()];

        match rest.strip_suffix("::") {
            Some(head) => rest = head,
            _ => break,
        }
    }

    segments.reverse();
    segments.join("::")
}

#[inline]
fn tail_name(text: &str) -> &str {
    let start = text.trim_end_matches(is_name_char).len();
    let name = &text[start..];

    match name.starts_with(|c: char| c.is_ascii_digit()) {
        true => "",
        _ => name,
    }
}

#[inline(always)]
fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context(source: &str) -> Context<'_> {
        let offset = source.find('|').expect("mark the cursor with `|`");
        context_at(source, offset)
    }

    fn path<'c>(qualifier: &str) -> Context<'c> {
        Context::Path { qualifier: qualifier.into(), import: false }
    }

    fn import<'i>(qualifier: &str) -> Context<'i> {
        Context::Path { qualifier: qualifier.into(), import: true }
    }

    #[test]
    fn a_dot_asks_for_the_receivers_members() {
        assert_eq!(context("    p.|"), Context::Member { receiver: "p" });
        assert_eq!(context("    p.ar|"), Context::Member { receiver: "p" });
        assert_eq!(context("self.x + self.|"), Context::Member { receiver: "self" });
    }

    #[test]
    fn a_dot_carries_the_whole_receiver_chain() {
        assert_eq!(context("self.inner.|"), Context::Member { receiver: "self.inner" });
        assert_eq!(context("a.b.c.d|"), Context::Member { receiver: "a.b.c" });
        assert_eq!(
            context("f().r.|"),
            Context::Member { receiver: "r" },
            "a chain resumes at the last segment that has a name"
        );
    }

    #[test]
    fn a_qualifier_asks_for_its_associated_items() {
        assert_eq!(context("Point::|"), path("Point"));
        assert_eq!(context("Msg::Qu|"), path("Msg"));
        assert_eq!(context("use std::mem::|"), import("std::mem"));
        assert_eq!(
            context("use std::mem::{size_|"),
            path("std::mem"),
            "a brace list still completes against the module"
        );
        assert_eq!(
            context("use std::mem::{size_of, al|"),
            path("std::mem"),
            "and keeps doing so once the list has items in it"
        );
        assert_eq!(context("use std::mem::{size_of, |"), path("std::mem"));
        assert_eq!(
            context("use std::mem::{\n    size_of,\n    al|"),
            path("std::mem"),
            "however the list is spread over lines"
        );
    }

    #[test]
    fn self_resolves_to_the_type_its_impl_is_on() {
        assert_eq!(context("impl Msg {\n    fn kind() { Self::|"), path("Msg"));
        assert_eq!(
            context("impl Msg {\n    fn kind() { match m {\n        Self::|"),
            path("Msg"),
            "however deeply nested inside the impl body"
        );
        assert_eq!(
            context("impl Wrapper<T> with Clone {\n    Self::|"),
            path("Wrapper"),
            "a generic impl names the type, not its parameters"
        );
        assert_eq!(
            context("fn free() { Self::|"),
            path("Self"),
            "outside an impl it names nothing"
        );
    }

    #[test]
    fn only_a_use_path_still_owes_a_brace_list() {
        assert_eq!(context("use std::|"), import("std"));
        assert_eq!(context("let x = std::io::|"), path("std::io"));
        assert_eq!(
            context("use std::{io, me|"),
            path("std"),
            "inside a list each name is imported on its own"
        );
    }

    #[test]
    fn anything_else_is_open() {
        assert_eq!(context("let x = |"), Context::Open);
        assert_eq!(context("fn go() { to|"), Context::Open);
        assert_eq!(context("|"), Context::Open);
    }

    #[test]
    fn an_interface_impl_offers_its_requirements() {
        assert_eq!(
            context("impl Packet with Encoded {\n    |"),
            Context::InterfaceImpl { interface: "Encoded".into() }
        );
        assert_eq!(
            context("impl Packet with Encoded { fn encode(&self) { |"),
            Context::Open,
            "a method body is ordinary expression scope"
        );
        assert_eq!(context("fn simple_without() { |"), Context::Open);
    }

    #[test]
    fn a_number_is_never_a_receiver() {
        assert_eq!(context("1.|"), Context::Open, "a float literal is not a member access");
    }

    #[test]
    fn a_chained_call_has_no_plain_receiver() {
        assert_eq!(
            context("f().|"),
            Context::Unresolved,
            "still a member access, just not one we can name: offering the open \
             scope here would look like a working completion"
        );
        assert_eq!(context("xs[0].|"), Context::Unresolved);
    }

    #[test]
    fn prose_is_never_completed() {
        assert_eq!(context("/// Creates a new col|"), Context::Inert);
        assert_eq!(context("let x = 1; // to|"), Context::Inert);
        assert_eq!(context("println(\"self.|"), Context::Inert);
        assert_eq!(context("let c = '\\'|"), Context::Inert);
    }

    #[test]
    fn prose_ends_with_its_line() {
        assert_eq!(context("// a note\nlet x = |"), Context::Open);
        assert_eq!(context("/// docs\nPoint::|"), path("Point"));
        assert_eq!(context("println(\"hi\"); p.|"), Context::Member { receiver: "p" });
        assert_eq!(
            context("let s = \"unterminated\nlet p = Point::|"),
            path("Point"),
            "an unterminated literal is confined to its line"
        );
    }
}
