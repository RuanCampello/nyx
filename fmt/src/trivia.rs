//! Recovery of comments and blank lines from the gaps between token spans
//!
//! The lexer discards ordinary `//` comments and every run of whitespace, but
//! each token records its byte range, so whatever lies between two adjacent
//! spans is exactly the trivia a formatter must preserve

use frontend::lexer::Lexer;
use frontend::lexer::token::{BytePos, Span};

/// Comments and blank lines recovered from one source file
#[derive(Debug, Default)]
pub struct Trivia<'src> {
    /// ordered by position, never overlapping
    gaps: Vec<Gap<'src>>,
}

/// The trivia lying between two adjacent tokens
#[derive(Debug)]
struct Gap<'src> {
    span: Span,
    /// whether a token precedes this gap, which decides whether a comment on the first line can trail that token
    follows_token: bool,
    pieces: Vec<Piece<'src>>,
}

/// One run of trivia, in source order
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Piece<'src> {
    /// a run of whitespace, carrying the number of line breaks it contains
    Newlines(u32),
    /// a `//` comment, kept verbatim including its slashes
    LineComment(&'src str),
}

impl<'src> Trivia<'src> {
    pub fn scan(source: &'src str) -> Self {
        let mut gaps = Vec::new();
        let mut previous = BytePos(0);
        let mut follows_token = false;

        for token in Lexer::new(source) {
            let span = token.expect("source lexes without error before it is formatted").span;

            if span.start > previous {
                let gap = Span::new(previous, span.start);
                let pieces = scan_gap(&source[gap.start.offset()..gap.end.offset()]);

                if !pieces.is_empty() {
                    gaps.push(Gap { span: gap, follows_token, pieces });
                }
            }

            previous = span.end;
            follows_token = true;
        }

        Self { gaps }
    }

    #[inline(always)]
    pub fn trailing(&self, after: BytePos) -> Option<&'src str> {
        let gap = self.gap_from(after)?;

        if gap.follows_token {
            return match gap.pieces.first() {
                Some(Piece::LineComment(text)) => Some(text),
                _ => None,
            };
        };

        None
    }

    #[inline(always)]
    pub fn leading(&self, at: BytePos) -> &[Piece<'src>] {
        let Some(gap) = self.gap_to(at) else {
            return &[];
        };

        match (gap.follows_token, gap.pieces.first()) {
            (true, Some(Piece::LineComment(_))) => &gap.pieces[1..],
            _ => &gap.pieces,
        }
    }

    fn gap_to(&self, end: BytePos) -> Option<&Gap<'src>> {
        self.gaps
            .binary_search_by(|gap| gap.span.end.cmp(&end))
            .ok()
            .map(|at| &self.gaps[at])
    }

    #[inline(always)]
    fn gap_from(&self, start: BytePos) -> Option<&Gap<'src>> {
        self.gaps.get(self.gaps.partition_point(|gap| gap.span.start < start))
    }

    pub fn comment_count(&self) -> usize {
        self.gaps
            .iter()
            .flat_map(|gap| &gap.pieces)
            .filter(|piece| matches!(piece, Piece::LineComment(_)))
            .count()
    }
}

pub fn opens_with_blank_line(pieces: &[Piece<'_>]) -> bool {
    matches!(pieces.first(), Some(Piece::Newlines(count)) if *count >= 2)
}

fn scan_gap(text: &str) -> Vec<Piece<'_>> {
    let mut pieces = Vec::new();
    let mut rest = text;

    while !rest.is_empty() {
        let trimmed = rest.trim_start();
        let breaks = rest[..rest.len() - trimmed.len()].bytes().filter(|byte| *byte == b'\n');

        match u32::try_from(breaks.count()).unwrap_or(u32::MAX) {
            0 => {},
            count => pieces.push(Piece::Newlines(count)),
        }

        rest = trimmed;

        if !rest.starts_with("//") {
            break;
        }

        let end = rest.find('\n').unwrap_or(rest.len());
        let (comment, remaining) = rest.split_at(end);

        pieces.push(Piece::LineComment(comment.trim_end()));
        rest = remaining;
    }

    pieces
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pieces(source: &str) -> Vec<Piece<'_>> {
        scan_gap(source)
    }

    #[test]
    fn plain_whitespace_records_only_its_line_breaks() {
        assert_eq!(pieces("   "), []);
        assert_eq!(pieces("\n"), [Piece::Newlines(1)]);
        assert_eq!(pieces("  \n\n  "), [Piece::Newlines(2)]);
    }

    #[test]
    fn a_comment_keeps_its_slashes_and_drops_trailing_spaces() {
        assert_eq!(pieces(" // why   \n"), [Piece::LineComment("// why"), Piece::Newlines(1)]);
    }

    #[test]
    fn a_divider_comment_is_ordinary_trivia() {
        assert_eq!(
            pieces("//// section\n"),
            [Piece::LineComment("//// section"), Piece::Newlines(1)]
        );
    }

    #[test]
    fn a_trailing_comment_precedes_the_line_break_that_follows_it() {
        assert_eq!(
            pieces("  // trailing\n\n// leading\n"),
            [
                Piece::LineComment("// trailing"),
                Piece::Newlines(2),
                Piece::LineComment("// leading"),
                Piece::Newlines(1),
            ]
        );
    }

    #[test]
    fn a_comment_on_the_first_line_trails_the_previous_token() {
        let source = "let x = 1; // why\nlet y = 2;";
        let trivia = Trivia::scan(source);
        let after_semicolon = BytePos(10);

        assert_eq!(trivia.trailing(after_semicolon), Some("// why"));
    }

    #[test]
    fn a_leading_comment_is_not_reported_as_trailing() {
        let source = "let x = 1;\n// why\nlet y = 2;";
        let trivia = Trivia::scan(source);

        assert_eq!(trivia.trailing(BytePos(10)), None);
        assert_eq!(
            trivia.leading(BytePos(18)),
            [Piece::Newlines(1), Piece::LineComment("// why"), Piece::Newlines(1)]
        );
    }

    #[test]
    fn a_file_opening_comment_trails_nothing() {
        let source = "// header\nfn main() {}";
        let trivia = Trivia::scan(source);

        assert_eq!(trivia.trailing(BytePos(0)), None);
        assert_eq!(
            trivia.leading(BytePos(10)),
            [Piece::LineComment("// header"), Piece::Newlines(1)]
        );
    }

    #[test]
    fn a_blank_line_between_items_is_visible_to_the_printer() {
        let source = "fn a() {}\n\nfn b() {}";
        let trivia = Trivia::scan(source);
        let second = BytePos(11);

        assert!(opens_with_blank_line(trivia.leading(second)));
    }
}
