use crate::lexer::{
    error::LexError,
    token::{Span, TokenKind},
};
use nyx_macros::Diagnostic;

#[derive(Debug, Clone, PartialEq)]
pub struct ParserError<'i> {
    pub(crate) kind: ParseErrorKind<'i>,
    pub(crate) span: Span,
}

#[derive(Debug, Clone, PartialEq, Diagnostic)]
pub enum ParseErrorKind<'i> {
    #[diagnostic(transparent)]
    Lexical(LexError<'i>),
    #[diagnostic(
        code = "E020",
        message = "Expected {expected^}, found {found!}",
        primary = "expected {expected^} here"
    )]
    Expected { expected: TokenKind<'i>, found: TokenKind<'i> },
    #[diagnostic(
        code = "E021",
        message = "Expected an identifier, found {found!}",
        primary = "identifier expected here"
    )]
    ExpectedIdentifier { found: TokenKind<'i> },
    #[diagnostic(
        code = "E022",
        message = "Invalid assignment target",
        primary = "cannot assign to this expression",
        note = "Only identifiers and field paths can be assigned to"
    )]
    InvalidAssignmentTarget,
    #[diagnostic(
        code = "E023",
        message = "{found!} is not a binary operator",
        primary = "not valid between operands"
    )]
    InvalidBinaryOperator { found: TokenKind<'i> },
    #[diagnostic(
        code = "E024",
        message = "{found!} is not a prefix operator",
        primary = "not valid before an operand"
    )]
    InvalidUnaryOperator { found: TokenKind<'i> },
    #[diagnostic(
        code = "E025",
        message = "Expected an expression, found {found!}",
        primary = "expression expected here"
    )]
    ExpectedExpression { found: TokenKind<'i> },
    #[diagnostic(
        code = "E026",
        message = "Expected a literal pattern, found {found!}",
        primary = "literal expected here",
        note = "Range pattern endpoints must be literals"
    )]
    ExpectedPatternLiteral { found: TokenKind<'i> },
    #[diagnostic(
        code = "E027",
        message = "Expected a type name, found {found!}",
        primary = "type expected here"
    )]
    ExpectedTypeIdentifier { found: String },
    #[diagnostic(
        code = "E028",
        message = "Unexpected end of file",
        primary = "the file ends here",
        help = "The source ends in the middle of a construct — check for unclosed braces or parentheses"
    )]
    UnexpectedEof,
}

impl<'i> ParserError<'i> {
    pub fn new(kind: ParseErrorKind<'i>, span: Span) -> Self {
        Self { kind, span }
    }
}

impl<'i> From<&LexError<'i>> for ParserError<'i> {
    fn from(value: &LexError<'i>) -> Self {
        Self::new(ParseErrorKind::Lexical(*value), value.span)
    }
}

impl<'i> From<LexError<'i>> for ParseErrorKind<'i> {
    fn from(value: LexError<'i>) -> Self {
        Self::Lexical(value)
    }
}
