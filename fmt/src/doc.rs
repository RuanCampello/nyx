//! Layout-independent documents used by the Nyx formatter

use std::borrow::Cow;

/// A source fragment with flat and broken layout alternatives
#[derive(Debug, Clone, PartialEq, Eq)]
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
