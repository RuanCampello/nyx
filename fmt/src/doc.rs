use std::borrow::Cow;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Doc<'src> {
    Empty,
    Text(Cow<'src, str>),
    Concat(Vec<Self>),
    Line(Line),
    /// Applies after a broken line
    Ident {
        width: u8,
        context: Box<Self>,
    },
    /// selects flat or broken layout
    Group(Box<Self>),
    IfBreak {
        broken: Box<Self>,
        flat: Box<Self>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Line {
    /// Always a new line
    Hard,
    /// Space when a [group](Doc::Group) fits,
    /// newline when it breaks
    Soft,
}
