//! Conversions between Nyx's global byte spans and LSP positions
//!
//! All line/column work goes through the [`SourceMap`] so positions are correct
//! for multi-byte text and in whichever encoding the client negotiated

use nyx::{BytePos, FileId, SourceMap, Span};
use tower_lsp::lsp_types::{Position, Range, Url};

/// The position encoding negotiated with the client
/// LSP defaults to UTF-16
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    Utf8,
    Utf16,
}

impl Encoding {
    #[inline]
    const fn is_utf16(self) -> bool {
        matches!(self, Self::Utf16)
    }
}

pub fn span_to_range(map: &SourceMap, span: Span, encoding: Encoding) -> Range {
    let start = map.loc(span.start);
    let end = map.loc(span.end);
    Range {
        start: to_position(start, encoding),
        end: to_position(end, encoding),
    }
}

pub fn position_to_pos(
    map: &SourceMap,
    file: FileId,
    position: Position,
    encoding: Encoding,
) -> BytePos {
    map.position(file, position.line, position.character, encoding.is_utf16())
}

pub fn url_for_file(map: &SourceMap, file: FileId) -> Option<Url> {
    Url::from_file_path(map.path(file)).ok()
}

#[inline]
fn to_position(loc: nyx::Loc, encoding: Encoding) -> Position {
    Position {
        line: loc.line,
        character: match encoding {
            Encoding::Utf16 => loc.col_utf16,
            Encoding::Utf8 => loc.col_utf8,
        },
    }
}
