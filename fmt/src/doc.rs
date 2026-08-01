//! Layout-independent documents used by the Nyx formatter

use std::{borrow::Cow, fmt};

/// A source fragment with flat and broken layout alternatives
#[derive(Clone, PartialEq, Eq)]
pub enum Doc<'src> {
    Empty,
    Text(Cow<'src, str>),
    Concat(Vec<Self>),
    Line(Line),
    /// Applies after a broken line
    Indent {
        width: u8,
        content: Box<Self>,
    },
    /// selects flat or broken layout
    Group(Box<Self>),
    IfBreak {
        broken: Box<Self>,
        flat: Box<Self>,
    },
}

/// A line boundary whose layout may be chosen by an enclosing [Doc::Group]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Line {
    /// Always a new line
    Hard,
    /// Space when a [group](Doc::Group) fits,
    /// newline when it breaks
    Soft,
}

impl<'src> Doc<'src> {
    pub fn text(text: impl Into<Cow<'src, str>>) -> Self {
        match text.into() {
            text if text.is_empty() => Self::Empty,
            text => Self::Text(text),
        }
    }

    pub const fn hard_line() -> Self {
        Self::Line(Line::Hard)
    }

    pub const fn soft_line() -> Self {
        Self::Line(Line::Soft)
    }

    pub fn concat(parts: impl IntoIterator<Item = Self>) -> Self {
        let mut flat = Vec::new();

        for part in parts {
            match part {
                Self::Empty => {},
                Self::Concat(parts) => flat.extend(parts),
                part => flat.push(part),
            }
        }

        match flat.len() {
            0 => Self::Empty,
            1 => flat.pop().expect("one document remains after normalisation"),
            _ => Self::Concat(flat),
        }
    }

    pub fn indent(width: u8, content: Self) -> Self {
        match width {
            0 => content,
            _ => Self::Indent { width, content: Box::new(content) },
        }
    }

    pub fn group(content: Self) -> Self {
        Self::Group(Box::new(content))
    }

    pub fn if_break(broken: Self, flat: Self) -> Self {
        Self::IfBreak { broken: Box::new(broken), flat: Box::new(flat) }
    }
}

impl fmt::Debug for Doc<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fn fmt_doc(doc: &Doc<'_>, f: &mut fmt::Formatter<'_>, depth: usize) -> fmt::Result {
            match doc {
                Doc::Empty => f.write_str("empty"),
                Doc::Text(text) => write!(f, "{text:?}"),
                Doc::Concat(parts) => {
                    f.write_str("concat([")?;

                    for part in parts {
                        f.write_str("\n")?;
                        write_indent(f, depth + 1)?;
                        fmt_doc(part, f, depth + 1)?;
                        f.write_str(",")?;
                    }

                    f.write_str("\n")?;
                    write_indent(f, depth)?;
                    f.write_str("])")
                },
                Doc::Line(Line::Hard) => f.write_str("hard_line()"),
                Doc::Line(Line::Soft) => f.write_str("soft_line()"),
                Doc::Indent { width, content } => {
                    write!(f, "indent({width},\n")?;
                    write_indent(f, depth + 1)?;
                    fmt_doc(content, f, depth + 1)?;
                    f.write_str("\n")?;
                    write_indent(f, depth)?;
                    f.write_str(")")
                },
                Doc::Group(content) => {
                    f.write_str("group(\n")?;
                    write_indent(f, depth + 1)?;
                    fmt_doc(content, f, depth + 1)?;
                    f.write_str("\n")?;
                    write_indent(f, depth)?;
                    f.write_str(")")
                },
                Doc::IfBreak { broken, flat } => {
                    f.write_str("if_break(\n")?;
                    write_indent(f, depth + 1)?;
                    f.write_str("broken: ")?;
                    fmt_doc(broken, f, depth + 1)?;
                    f.write_str(",\n")?;
                    write_indent(f, depth + 1)?;
                    f.write_str("flat: ")?;
                    fmt_doc(flat, f, depth + 1)?;
                    f.write_str(",\n")?;
                    write_indent(f, depth)?;
                    f.write_str(")")
                },
            }
        }

        fn write_indent(f: &mut fmt::Formatter<'_>, depth: usize) -> fmt::Result {
            for _ in 0..depth {
                f.write_str("  ")?;
            }

            Ok(())
        }

        fmt_doc(self, f, 0)
    }
}
