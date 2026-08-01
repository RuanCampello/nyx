pub mod diagnostic;
pub mod error_codes;
pub mod hir;
pub mod lexer;
pub mod lints;
pub mod parser;
pub mod source_map;

pub use diagnostic::{Label, RichDiagnostic, Severity};
pub use error_codes::ErrorCode;
pub use lexer::token::{BytePos, Span};
pub use lexer::{HasSpan, is_keyword};
pub use parser::statement::is_primitive;
pub use source_map::{FileId, Loc, SourceMap, SpanData};
