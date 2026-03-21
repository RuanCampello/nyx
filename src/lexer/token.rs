//! Token types for the Nyx lexer.

use crate::lexer::cursor::Cursor;
use crate::lexer::error::LexError;
use std::fmt;

/// Trait implemented by every sub-tokenizer.
///
/// Each token type (identifier, number, string, …) is a small struct that
/// implements this trait.  The [`Lexer`](super::Lexer) dispatches to the
/// appropriate implementor after peeking at the first character.
///
pub trait Tokenize<'src> {
    /// Lex a single token starting at `start`, advancing `cursor` past it.
    fn lex(self, cursor: &mut Cursor<'src>, start: Position) -> Result<Token<'src>, LexError>;
}

/// A single token produced by the lexer.
#[derive(Debug, Clone, PartialEq)]
pub struct Token<'src> {
    pub kind: TokenKind<'src>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind<'src> {
    Integer(i64),
    Float(f64),
    String(&'src str),
    Bool(bool),
    Identifier(&'src str),
    Keyword(Keyword),
    Punct(Punct),
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
    While,
    For,
    Struct,
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
    And, // &&
    Or,  // ||

    OpenParen,    // (
    CloseParen,   // )
    OpenBrace,    // {
    CloseBrace,   // }
    OpenBracket,  // [
    CloseBracket, // ]

    // separators
    Colon,     // :
    Semicolon, // ;
    Comma,     // ,
    Dot,       // .
    Arrow,     // ->
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    pub offset: usize,
    pub line: u32,
    pub column: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: Position,
    pub end: Position,
}

impl Position {
    #[inline]
    pub const fn new(offset: usize, line: u32, column: u32) -> Self {
        Self {
            offset,
            line,
            column,
        }
    }
}

impl fmt::Display for Position {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.line, self.column)
    }
}

impl Span {
    #[inline]
    pub const fn new(start: Position, end: Position) -> Self {
        Self { start, end }
    }
}

impl<'src> Token<'src> {
    #[inline]
    pub(in crate::lexer) const fn new(kind: TokenKind<'src>, span: Span) -> Self {
        Self { kind, span }
    }
}

impl Keyword {
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "fn" => Some(Self::Fn),
            "let" => Some(Self::Let),
            "mut" => Some(Self::Mut),
            "if" => Some(Self::If),
            "else" => Some(Self::Else),
            "return" => Some(Self::Return),
            "while" => Some(Self::While),
            "for" => Some(Self::For),
            "struct" => Some(Self::Struct),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Fn => "fn",
            Self::Let => "let",
            Self::Mut => "mut",
            Self::If => "if",
            Self::Else => "else",
            Self::Return => "return",
            Self::While => "while",
            Self::For => "for",
            Self::Struct => "struct",
        }
    }
}

impl Punct {
    pub const fn as_str(self) -> &'static str {
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
            Self::OpenParen => "(",
            Self::CloseParen => ")",
            Self::OpenBrace => "{",
            Self::CloseBrace => "}",
            Self::OpenBracket => "[",
            Self::CloseBracket => "]",
            Self::Colon => ":",
            Self::Semicolon => ";",
            Self::Comma => ",",
            Self::Dot => ".",
            Self::Arrow => "->",
        }
    }
}

impl fmt::Display for TokenKind<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Integer(n) => write!(f, "{n}"),
            Self::Float(n) => write!(f, "{n}"),
            Self::String(s) => write!(f, "\"{s}\""),
            Self::Bool(b) => write!(f, "{b}"),
            Self::Identifier(id) => write!(f, "{id}"),
            Self::Keyword(kw) => write!(f, "{kw}"),
            Self::Punct(p) => write!(f, "{p}"),
            Self::Eof => write!(f, "EOF"),
        }
    }
}

impl fmt::Display for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}..{}", self.start, self.end)
    }
}

impl fmt::Display for Keyword {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl fmt::Display for Punct {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
