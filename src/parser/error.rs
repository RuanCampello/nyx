use crate::lexer::{error::LexError, token::Span};
use std::error::Error;

#[derive(Debug, Clone, PartialEq)]
pub struct ParserError {
    pub kind: ParseErrorKind,
    pub span: Span,
    pub help: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ParseErrorKind {
    Lexical(LexError),
    UnexpectedToken {
        expected: &'static str,
        found: String,
    },
    ExpectedIdentifier {
        found: String,
    },
    InvalidAssignmentTarget,
    UnterminatedBlock,
}

impl Error for ParserError {}

impl std::fmt::Display for ParserError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let msg = match &self.kind {
            ParseErrorKind::Lexical(lex) => lex.to_string(),
            ParseErrorKind::UnexpectedToken { expected, found } => {
                format!("expected {expected}, found {found}")
            }
            ParseErrorKind::ExpectedIdentifier { found } => {
                format!("expected identifier, found {found}")
            }
            ParseErrorKind::InvalidAssignmentTarget => {
                "invalid assignment target, expected an identifier on the left-hand side".into()
            }
            ParseErrorKind::UnterminatedBlock => "unterminated block: missing `}`".into(),
        };

        write!(
            f,
            "error: {msg}\n --> {}:{}\n",
            self.span.start.line, self.span.start.column
        )?;

        if let Some(help) = &self.help {
            write!(f, " help: {help}\n")?;
        }

        Ok(())
    }
}

impl From<LexError> for ParserError {
    fn from(value: LexError) -> Self {
        Self {
            kind: ParseErrorKind::Lexical(value.to_owned()),
            span: value.span(),
            help: value.help().map(ToOwned::to_owned),
        }
    }
}
