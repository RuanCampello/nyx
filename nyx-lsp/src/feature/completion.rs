//! Completion: read what the cursor is qualified by, then answer from the
//! index [crate::analysis] built while the HIR was alive

use crate::analysis::{Completion, Completions, SemanticAnalysis};
use nyx::lexer::token::Keyword;

/// What qualifies the position being completed
#[derive(Debug, PartialEq, Eq)]
pub enum Context<'s> {
    /// after `receiver.`, offering that value's fields and methods
    Member { receiver: &'s str },
    /// after `Qualifier::`, offering a type's associated items or a module's exports
    Path { qualifier: String },
    /// anywhere else, offering everything nameable
    Open,
}

pub fn context_at(text: &str, offset: usize) -> Context<'_> {
    let before = &text[..offset.min(text.len())];
    let before = before.trim_end_matches(is_name_char);

    if let Some(head) = before.strip_suffix('.') {
        // a chained call has no name to look up, only a plain receiver resolves
        let receiver = tail_name(head);
        return match receiver.is_empty() {
            true => Context::Open,
            false => Context::Member { receiver },
        };
    }

    // `use a::b::{c, d|` still completes against `a::b`, so step back over the
    // brace list the cursor sits in
    let before = before.trim_end_matches([' ', '\t', '\n', ',']);
    let before = before.strip_suffix('{').unwrap_or(before);

    match before.strip_suffix("::") {
        Some(head) => Context::Path { qualifier: path_before(head) },
        None => Context::Open,
    }
}

pub fn candidates<'a>(
    analysis: &'a SemanticAnalysis,
    context: &Context<'_>,
    scope: Option<&'a [Completion]>,
) -> Vec<&'a Completion> {
    let index = &analysis.completions;

    match context {
        Context::Member { receiver } => match receiver_type(index, scope, receiver) {
            Some(key) => {
                index.members.get(&key).map(Vec::as_slice).unwrap_or_default().iter().collect()
            },
            None => Vec::new(),
        },
        Context::Path { qualifier } => index
            .associated
            .get(qualifier)
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .collect(),
        Context::Open => scope.unwrap_or_default().iter().chain(index.globals.iter()).collect(),
    }
}

pub fn keywords() -> impl Iterator<Item = &'static str> {
    Keyword::ALL.iter().map(|keyword| keyword.as_str())
}

pub fn scope_at(analysis: &SemanticAnalysis, position: nyx::BytePos) -> Option<&[Completion]> {
    analysis
        .scopes
        .iter()
        .filter(|(body, _)| body.start <= position && position < body.end)
        .min_by_key(|(body, _)| body.end.0 - body.start.0)
        .map(|(_, locals)| locals.as_slice())
}

fn receiver_type(
    index: &Completions,
    scope: Option<&[Completion]>,
    receiver: &str,
) -> Option<String> {
    let named = |items: &[Completion]| {
        items
            .iter()
            .find(|item| item.label == receiver)
            .and_then(|item| item.type_key.clone())
    };

    named(scope.unwrap_or_default())
        .or_else(|| named(&index.globals))
        .or_else(|| index.members.contains_key(receiver).then(|| receiver.to_owned()))
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
            None => break,
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
        false => name,
    }
}

#[inline]
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

    #[test]
    fn a_dot_asks_for_the_receivers_members() {
        assert_eq!(context("    p.|"), Context::Member { receiver: "p" });
        assert_eq!(context("    p.ar|"), Context::Member { receiver: "p" });
        assert_eq!(context("self.x + self.|"), Context::Member { receiver: "self" });
    }

    #[test]
    fn a_qualifier_asks_for_its_associated_items() {
        assert_eq!(context("Point::|"), Context::Path { qualifier: "Point".into() });
        assert_eq!(context("Msg::Qu|"), Context::Path { qualifier: "Msg".into() });
        assert_eq!(context("use std::mem::|"), Context::Path { qualifier: "std::mem".into() });
        assert_eq!(
            context("use std::mem::{size_|"),
            Context::Path { qualifier: "std::mem".into() },
            "a brace list still completes against the module"
        );
    }

    #[test]
    fn anything_else_is_open() {
        assert_eq!(context("let x = |"), Context::Open);
        assert_eq!(context("fn go() { to|"), Context::Open);
        assert_eq!(context("|"), Context::Open);
    }

    #[test]
    fn a_number_is_never_a_receiver() {
        assert_eq!(context("1.|"), Context::Open, "a float literal is not a member access");
    }

    #[test]
    fn a_chained_call_has_no_plain_receiver() {
        assert_eq!(context("f().|"), Context::Open);
    }
}
