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

/// Byte offset of `position` within `text`
///
/// Independent of the [SourceMap], so it stays correct for a buffer that has
/// changed since the last analysis, which is always the case mid-keystroke
pub fn position_to_offset(text: &str, position: Position, encoding: Encoding) -> usize {
    let Some(line) = text.split_inclusive('\n').nth(position.line as usize) else {
        return text.len();
    };
    let line_start = line.as_ptr() as usize - text.as_ptr() as usize;

    let mut units = 0;
    for (offset, c) in line.char_indices() {
        if units >= position.character {
            return line_start + offset;
        }
        units += match encoding {
            Encoding::Utf16 => c.len_utf16() as u32,
            Encoding::Utf8 => c.len_utf8() as u32,
        };
    }

    line_start + line.len()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offsets_are_found_per_line_in_both_encodings() {
        let text = "fn go() {\n    let p = 1;\n}\n";
        let at = |line, character| {
            position_to_offset(text, Position { line, character }, Encoding::Utf8)
        };

        assert_eq!(&text[at(1, 4)..at(1, 7)], "let");
        assert_eq!(at(0, 0), 0);
        assert_eq!(&text[..at(0, 2)], "fn");
    }

    #[test]
    fn a_multibyte_line_counts_in_the_negotiated_units() {
        let text = "let s = \"héllo\";";
        let utf16 = position_to_offset(text, Position { line: 0, character: 11 }, Encoding::Utf16);
        let utf8 = position_to_offset(text, Position { line: 0, character: 12 }, Encoding::Utf8);

        assert_eq!(utf16, utf8, "the same cursor, counted differently");
        assert_eq!(&text[..utf16], "let s = \"hé", "both land just past the accent");
    }

    #[test]
    fn a_position_past_the_end_clamps() {
        let text = "fn go() {}";
        let offset = position_to_offset(text, Position { line: 9, character: 0 }, Encoding::Utf8);
        assert_eq!(offset, text.len());
    }
}
