//! Token types for the Nyx lexer.

use crate::lexer::cursor::Cursor;
use crate::lexer::error::LexError;
use std::fmt;
use std::ops::Add;

/// A single token produced by the lexer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Token<'src> {
    pub kind: TokenKind<'src>,
    pub span: Span,
}

/// A byte range `[start, end)` in the global source address space
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct Span {
    pub start: BytePos,
    pub end: BytePos,
}

/// A byte offset into the global [`SourceMap`](crate::source_map::SourceMap)
/// address space. File identity and line/column are derived from it on demand
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Hash)]
pub struct BytePos(pub u32);

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TokenKind<'src> {
    Integer(u64),
    Float(f64),
    String(&'src str),
    Char(char),
    Bool(bool),
    Identifier(&'src str),
    Keyword(Keyword),
    Punct(Punct),
    /// A `///` line, the text is everything after the slashes up to the newline
    DocComment(&'src str),
    Eof,
}

/// Reserved words in the Nyx language.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Keyword {
    Fn,
    Let,
    Mut,
    If,
    Else,
    Return,
    Loop,
    Break,
    Continue,
    In,
    For,
    Struct,
    Enum,
    Impl,
    Inline,
    Const,
    Pub,
    Use,
    Interface,
    With,
    As,
    Where,
    Match,
}

/// Punctuators and operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Punct {
    Plus,  // +
    Minus, // -
    Star,  // *
    Slash, // /

    Eq,     // =
    EqEq,   // ==
    Bang,   // !
    BangEq, // !=
    Lt,     // <
    Gt,     // >
    LtEq,   // <=
    GtEq,   // >=

    // logical
    And,       // &&
    Or,        // ||
    Ampersand, // &
    Pipe,      // |
    Caret,     // ^
    Shl,       // <<
    Shr,       // >>

    OpenParen,    // (
    CloseParen,   // )
    OpenBrace,    // {
    CloseBrace,   // }
    OpenBracket,  // [
    CloseBracket, // ]

    // separators
    Colon,      // :
    ColonColon, // ::
    Semicolon,  // ;
    Comma,      // ,
    Dot,        // .
    Range,      // ..
    RangeEq,    // ..=
    Arrow,      // ->
}

/// Trait implemented by every sub-tokenizer
///
/// Each token type (identifier, number, string, …) is a small struct that
/// implements this trait. The [`Lexer`](super::Lexer) dispatches to the
/// appropriate implementor after peeking at the first character
pub trait Tokenize<'src> {
    /// Lex a single token starting at `start`, advancing `cursor` past it
    fn lex(self, cursor: &mut Cursor<'src>, start: BytePos) -> Result<Token<'src>, LexError<'src>>;
}

const _: () = assert!(size_of::<Span>() == 8, "span must stay 8 bytes");

impl<'src> Token<'src> {
    #[inline]
    pub(in crate::lexer) const fn new(kind: TokenKind<'src>, span: Span) -> Self {
        Self { kind, span }
    }

    #[inline]
    pub(crate) fn is_kind(&self, kind: impl Into<TokenKind<'src>>) -> bool {
        self.kind == kind.into()
    }

    #[inline(always)]
    pub(crate) fn is_fn_start(&self) -> bool {
        self.is_kind(Keyword::Fn)
            || self.is_kind(Keyword::Pub)
            || self.is_kind(Keyword::Inline)
            || self.is_kind(Keyword::Const)
    }
}

impl Span {
    #[inline]
    pub const fn new(start: BytePos, end: BytePos) -> Self {
        Self { start, end }
    }

    #[inline(always)]
    pub const fn lo(self) -> BytePos {
        self.start
    }

    #[inline(always)]
    pub const fn hi(self) -> BytePos {
        self.end
    }
}

impl Add<Span> for Span {
    type Output = Self;

    fn add(self, rhs: Span) -> Self::Output {
        Span::new(self.start, rhs.end)
    }
}

impl fmt::Display for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}..{}", self.start.0, self.end.0)
    }
}

impl BytePos {
    #[inline(always)]
    pub const fn offset(self) -> usize {
        self.0 as usize
    }
}

impl Add<u32> for BytePos {
    type Output = Self;

    #[inline(always)]
    fn add(self, rhs: u32) -> Self::Output {
        Self(self.0 + rhs)
    }
}

impl fmt::Display for BytePos {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<Punct> for TokenKind<'_> {
    fn from(value: Punct) -> Self {
        Self::Punct(value)
    }
}

impl From<Keyword> for TokenKind<'_> {
    fn from(value: Keyword) -> Self {
        Self::Keyword(value)
    }
}

impl fmt::Display for TokenKind<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Integer(n) => write!(f, "{n}"),
            Self::Float(n) => write!(f, "{n}"),
            Self::String(s) => write!(f, "\"{s}\""),
            Self::Char(c) => write!(f, "'{c}'"),
            Self::Bool(b) => write!(f, "{b}"),
            Self::Identifier(id) => write!(f, "{id}"),
            Self::Keyword(kw) => write!(f, "{kw}"),
            Self::Punct(p) => write!(f, "{p}"),
            Self::DocComment(text) => write!(f, "///{text}"),
            Self::Eof => write!(f, "EOF"),
        }
    }
}

impl Keyword {
    pub const fn as_str<'s>(self) -> &'s str {
        match self {
            Self::Fn => "fn",
            Self::Let => "let",
            Self::Mut => "mut",
            Self::If => "if",
            Self::Else => "else",
            Self::Return => "return",
            Self::Loop => "loop",
            Self::Break => "break",
            Self::Continue => "continue",
            Self::In => "in",
            Self::For => "for",
            Self::Struct => "struct",
            Self::Enum => "enum",
            Self::Impl => "impl",
            Self::Inline => "inline",
            Self::Const => "const",
            Self::Use => "use",
            Self::Pub => "pub",
            Self::Interface => "interface",
            Self::With => "with",
            Self::As => "as",
            Self::Where => "where",
            Self::Match => "match",
        }
    }
}

impl std::str::FromStr for Keyword {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "fn" => Self::Fn,
            "let" => Self::Let,
            "mut" => Self::Mut,
            "if" => Self::If,
            "else" => Self::Else,
            "return" => Self::Return,
            "loop" => Self::Loop,
            "break" => Self::Break,
            "continue" => Self::Continue,
            "in" => Self::In,
            "for" => Self::For,
            "struct" => Self::Struct,
            "enum" => Self::Enum,
            "impl" => Self::Impl,
            "inline" => Self::Inline,
            "const" => Self::Const,
            "pub" => Self::Pub,
            "use" => Self::Use,
            "with" => Self::With,
            "interface" => Self::Interface,
            "as" => Self::As,
            "where" => Self::Where,
            "match" => Self::Match,
            _ => return Err(()),
        })
    }
}

impl fmt::Display for Keyword {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Punct {
    pub const fn as_str<'s>(self) -> &'s str {
        match self {
            Self::Plus => "+",
            Self::Minus => "-",
            Self::Star => "*",
            Self::Slash => "/",
            Self::Eq => "=",
            Self::EqEq => "==",
            Self::Bang => "!",
            Self::BangEq => "!=",
            Self::Lt => "<",
            Self::Gt => ">",
            Self::LtEq => "<=",
            Self::GtEq => ">=",
            Self::And => "&&",
            Self::Or => "||",
            Self::Ampersand => "&",
            Self::Pipe => "|",
            Self::Caret => "^",
            Self::Shl => "<<",
            Self::Shr => ">>",
            Self::OpenParen => "(",
            Self::CloseParen => ")",
            Self::OpenBrace => "{",
            Self::CloseBrace => "}",
            Self::OpenBracket => "[",
            Self::CloseBracket => "]",
            Self::Colon => ":",
            Self::ColonColon => "::",
            Self::Semicolon => ";",
            Self::Comma => ",",
            Self::Dot => ".",
            Self::Range => "..",
            Self::RangeEq => "..=",
            Self::Arrow => "->",
        }
    }
}

impl fmt::Display for Punct {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
