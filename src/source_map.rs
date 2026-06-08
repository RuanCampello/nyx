//! Maps the global byte-offset address space back to concrete files
//!
//! Every source file occupies a contiguous, non-overlapping range of the
//! global address space. A [`Span`] therefore needs to store only its byte
//! offsets ([`BytePos`]), the owning [`FileId`], line and column are recovered
//! on demand from the [`SourceMap`]

use crate::lexer::token::{BytePos, Span};
use std::path::{Path, PathBuf};

/// Owns every source file and the global address space they share
#[derive(Debug, Default)]
pub struct SourceMap {
    files: Vec<SourceFile>,
}

/// One registered file and its precomputed line table
#[derive(Debug)]
pub struct SourceFile {
    pub id: FileId,
    pub name: PathBuf,
    pub start_pos: BytePos,
    pub end_pos: BytePos,
    pub src: String,
    /// global offset of the start of each line
    line_starts: Vec<u32>,
}

/// A [`Span`] decoded against its owning file: the byte offsets are still
/// global, but the file is now resolved
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpanData {
    pub file: FileId,
    pub lo: BytePos,
    pub hi: BytePos,
}

/// A resolved source location
/// Two column flavours are provided so the LSP can answer in whichever position encoding the client negotiated
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Loc {
    pub file: FileId,
    /// zero-based line index
    pub line: u32,
    /// zero-based column in utf-8 code units
    pub col_utf8: u32,
    /// zero-based column in utf-16 code units
    pub col_utf16: u32,
}

/// Index of a registered file within a [`SourceMap`]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FileId(pub u32);

impl SourceMap {
    /// Register a file, assigning it the next contiguous global range, and
    /// return its id together with the base offset its spans must be relative to.
    pub fn add_file(
        &mut self,
        name: impl Into<PathBuf>,
        src: impl Into<String>,
    ) -> (FileId, BytePos) {
        let src = src.into();

        let id = FileId(self.files.len() as u32);
        let start = self.files.last().map(|f| f.end_pos.0 + 1).unwrap_or_default();
        let end = start + src.len() as u32;
        let line_starts = line_starts(&src, start);

        self.files.push(SourceFile {
            id,
            name: name.into(),
            start_pos: BytePos(start),
            end_pos: BytePos(end),
            src,
            line_starts,
        });

        (id, BytePos(start))
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    pub fn file(&self, id: FileId) -> &SourceFile {
        self.files.get(id.0 as usize).expect("FileId must refer to a registered file")
    }

    /// The file containing `pos`, found by its global range
    pub fn lookup_file(&self, pos: BytePos) -> &SourceFile {
        let idx = self
            .files
            .partition_point(|f| f.end_pos.0 < pos.0)
            .min(self.files.len().saturating_sub(1));

        &self.files[idx]
    }

    pub fn span_data(&self, span: Span) -> SpanData {
        let file = self.lookup_file(span.start);

        SpanData { file: file.id, lo: span.start, hi: span.end }
    }

    /// Resolve a global position to a file-relative line and column
    pub fn loc(&self, pos: BytePos) -> Loc {
        let file = self.lookup_file(pos);
        file.loc(pos)
    }

    /// The byte range of `span` relative to the start of its own file, for
    /// rendering against that file's source text
    pub fn local_range(&self, span: Span) -> (FileId, std::ops::Range<usize>) {
        let file = self.lookup_file(span.start);
        let base = file.start_pos.0;
        let lo = span.start.0.saturating_sub(base) as usize;
        let hi = span.end.0.saturating_sub(base) as usize;

        (file.id, lo..hi)
    }

    pub fn path(&self, id: FileId) -> &Path {
        &self.file(id).name
    }

    pub fn source(&self, id: FileId) -> &str {
        &self.file(id).src
    }

    pub fn file_by_path(&self, path: &Path) -> Option<FileId> {
        self.files.iter().find(|f| f.name == path).map(|f| f.id)
    }

    /// Inverse of [`SourceMap::loc`]: the global position of a zero-based
    /// `line`/`col`, where `col` counts UTF-16 units when `utf16` else bytes
    pub fn position(&self, file: FileId, line: u32, col: u32, utf16: bool) -> BytePos {
        self.file(file).position(line, col, utf16)
    }

    /// The owning file's source text from `pos` to its end
    pub fn source_after(&self, pos: BytePos) -> &str {
        let file = self.lookup_file(pos);
        let local = ((pos.0 - file.start_pos.0) as usize).min(file.src.len());
        &file.src[local..]
    }
}

impl SourceFile {
    fn position(&self, line: u32, col: u32, utf16: bool) -> BytePos {
        let Some(&line_start) = self.line_starts.get(line as usize) else {
            return self.end_pos;
        };
        let line_start_local = (line_start - self.start_pos.0) as usize;
        let mut units = 0;
        let mut byte = line_start_local;

        for ch in self.src[line_start_local..].chars() {
            if units >= col || ch == '\n' {
                break;
            }
            units += match utf16 {
                true => ch.len_utf16() as u32,
                _ => ch.len_utf8() as u32,
            };
            byte += ch.len_utf8();
        }

        BytePos(self.start_pos.0 + byte as u32).min(self.end_pos)
    }

    fn loc(&self, pos: BytePos) -> Loc {
        let line = self.line_starts.partition_point(|&start| start <= pos.0).saturating_sub(1);
        let line_start = self.line_starts[line];
        let local = (pos.0 - self.start_pos.0) as usize;
        let line_start_local = (line_start - self.start_pos.0) as usize;
        let prefix = &self.src[line_start_local..local];

        Loc {
            file: self.id,
            line: line as u32,
            col_utf8: prefix.len() as u32,
            col_utf16: prefix.chars().map(|c| c.len_utf16() as u32).sum(),
        }
    }
}

/// Global offsets of every line start, beginning with the file's `base`
fn line_starts(src: &str, base: u32) -> Vec<u32> {
    let mut starts = Vec::with_capacity(src.len() / 32 + 1);
    starts.push(base);
    for (idx, byte) in src.bytes().enumerate() {
        if byte == b'\n' {
            starts.push(base + idx as u32 + 1);
        }
    }
    starts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_get_contiguous_non_overlapping_ranges() {
        let mut map = SourceMap::default();
        let (a, base_a) = map.add_file("a.nyx", "ab");
        let (b, base_b) = map.add_file("b.nyx", "cd");

        assert_eq!(base_a, BytePos(0));
        assert_eq!(map.file(a).end_pos, BytePos(2));
        assert_eq!(base_b, BytePos(3));
        assert_eq!(map.lookup_file(BytePos(0)).id, a);
        assert_eq!(map.lookup_file(BytePos(4)).id, b);
    }

    #[test]
    fn loc_reports_zero_based_line_and_utf16_columns() {
        let mut map = SourceMap::default();
        // 'é' is two UTF-8 bytes but one UTF-16 unit
        map.add_file("u.nyx", "let é = 1\nnext");
        let after_e = BytePos(("let é".len()) as u32);
        let loc = map.loc(after_e);
        assert_eq!(loc.line, 0);
        assert_eq!(loc.col_utf8, "let é".len() as u32);
        assert_eq!(loc.col_utf16, 5);

        let second_line = BytePos(("let é = 1\n".len()) as u32);
        assert_eq!(map.loc(second_line).line, 1);
        assert_eq!(map.loc(second_line).col_utf8, 0);
    }
}
