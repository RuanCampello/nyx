//! Width-aware rendering for [crate::Doc].
//!
//! The renderer is inspired by the work-stack implementation in Philip Wadler's
//! [*A Prettier Printer*](https://homepages.inf.ed.ac.uk/wadler/papers/prettier/prettier.pdf)

use crate::doc::{Doc, Line};

/// A pending document together with the indentation and layout it inherits
#[derive(Clone, Copy)]
struct Command<'doc, 'src> {
    indent: usize,
    mode: Mode,
    doc: &'doc Doc<'src>,
}

/// The width constraint used when rendering a document
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderOptions {
    pub print_width: usize,
}

/// The layout selected for an enclosing document group
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Flat,
    Break,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self { print_width: 80 }
    }
}

pub fn render(doc: &Doc<'_>, options: RenderOptions) -> String {
    let mut output = String::new();
    let mut column = 0;
    let mut commands = vec![Command { indent: 0, mode: Mode::Break, doc }];

    while let Some(command) = commands.pop() {
        match command.doc {
            Doc::Empty => {},
            Doc::Text(text) => {
                output.push_str(text);
                column += text.chars().count();
            },
            Doc::Concat(parts) => push_parts(&mut commands, command.indent, command.mode, parts),
            Doc::Line(Line::Hard) => write_line(&mut output, &mut column, command.indent),
            Doc::Line(Line::Soft) => match command.mode {
                Mode::Flat => {
                    output.push(' ');
                    column += 1;
                },
                Mode::Break => write_line(&mut output, &mut column, command.indent),
            },
            Doc::Indent { width, content } => commands.push(Command {
                indent: command.indent + usize::from(*width),
                mode: command.mode,
                doc: content,
            }),
            Doc::Group(content) => {
                let mut flat = commands.clone();
                flat.push(Command { indent: command.indent, mode: Mode::Flat, doc: content });
                let mode = match fits(options.print_width.saturating_sub(column), flat) {
                    true => Mode::Flat,
                    false => Mode::Break,
                };

                commands.push(Command { indent: command.indent, mode, doc: content });
            },
            Doc::IfBreak { broken, flat } => match command.mode {
                Mode::Flat => commands.push(Command { doc: flat, ..command }),
                Mode::Break => commands.push(Command { doc: broken, ..command }),
            },
        }
    }

    output
}

fn fits<'doc, 'src>(mut remaining: usize, mut commands: Vec<Command<'doc, 'src>>) -> bool {
    while let Some(command) = commands.pop() {
        match command.doc {
            Doc::Empty => {},
            Doc::Text(text) => match remaining.checked_sub(text.chars().count()) {
                Some(width) => remaining = width,
                None => return false,
            },
            Doc::Concat(parts) => push_parts(&mut commands, command.indent, command.mode, parts),
            Doc::Line(Line::Hard) => return true,
            Doc::Line(Line::Soft) => match command.mode {
                Mode::Flat => match remaining.checked_sub(1) {
                    Some(width) => remaining = width,
                    None => return false,
                },
                Mode::Break => return true,
            },
            Doc::Indent { width, content } => commands.push(Command {
                indent: command.indent + usize::from(*width),
                mode: command.mode,
                doc: content,
            }),
            Doc::Group(content) => {
                commands.push(Command { indent: command.indent, mode: Mode::Flat, doc: content })
            },
            Doc::IfBreak { flat, .. } => commands.push(Command { doc: flat, ..command }),
        }
    }

    true
}

fn push_parts<'doc, 'src>(
    commands: &mut Vec<Command<'doc, 'src>>,
    indent: usize,
    mode: Mode,
    parts: &'doc [Doc<'src>],
) {
    commands.extend(parts.iter().rev().map(|doc| Command { indent, mode, doc }));
}

fn write_line(output: &mut String, column: &mut usize, indent: usize) {
    output.push('\n');
    output.extend(std::iter::repeat_n(' ', indent));
    *column = indent;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list() -> Doc<'static> {
        Doc::group(Doc::concat([
            Doc::text("call("),
            Doc::indent(
                2,
                Doc::concat([
                    Doc::soft_line(),
                    Doc::text("first,"),
                    Doc::soft_line(),
                    Doc::text("second"),
                ]),
            ),
            Doc::soft_line(),
            Doc::text(")"),
        ]))
    }

    #[test]
    fn renders_a_group_flat_when_it_fits() {
        assert_eq!(render(&list(), RenderOptions { print_width: 80 }), "call( first, second )");
    }

    #[test]
    fn renders_a_group_broken_when_it_does_not_fit() {
        assert_eq!(
            render(&list(), RenderOptions { print_width: 12 }),
            "call(\n  first,\n  second\n)"
        );
    }

    #[test]
    fn fits_accounts_for_following_documents() {
        let doc = Doc::concat([
            Doc::text("x"),
            Doc::group(Doc::concat([Doc::text("a"), Doc::soft_line(), Doc::text("b")])),
            Doc::text("tail"),
        ]);

        assert_eq!(render(&doc, RenderOptions { print_width: 7 }), "xa\nbtail");
    }

    #[test]
    fn hard_lines_always_break() {
        let doc =
            Doc::group(Doc::concat([Doc::text("left"), Doc::hard_line(), Doc::text("right")]));

        assert_eq!(render(&doc, RenderOptions::default()), "left\nright");
    }

    #[test]
    fn if_break_selects_the_group_layout() {
        let doc = Doc::group(Doc::concat([
            Doc::text("["),
            Doc::indent(
                2,
                Doc::concat([
                    Doc::soft_line(),
                    Doc::text("item"),
                    Doc::if_break(Doc::text(","), Doc::text("")),
                ]),
            ),
            Doc::soft_line(),
            Doc::text("]"),
        ]));

        assert_eq!(render(&doc, RenderOptions { print_width: 80 }), "[ item ]");
        assert_eq!(render(&doc, RenderOptions { print_width: 6 }), "[\n  item,\n]");
    }

    #[test]
    fn renders_deeply_nested_documents_without_recursion() {
        let mut doc = Doc::text("item");

        for _ in 0..1_000 {
            doc = Doc::indent(1, doc);
        }

        assert_eq!(render(&doc, RenderOptions::default()), "item");
    }
}
