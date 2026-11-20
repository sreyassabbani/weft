//! Positions. Parsed spans are byte offsets within a line; the protocol speaks
//! in UTF-8 or UTF-16 code units, negotiated at startup.

use lsp_types::{Position, Range, TextDocumentContentChangeEvent};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    Utf8,
    Utf16,
}

/// A range on one line, in bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Span {
    pub line: u32,
    pub start: u32,
    pub end: u32,
}

impl Span {
    pub fn new(line: usize, start: usize, end: usize) -> Span {
        Span { line: line as u32, start: start as u32, end: end as u32 }
    }

    pub fn contains(&self, line: u32, byte: u32) -> bool {
        self.line == line && self.start <= byte && byte <= self.end
    }
}

/// Text split into lines, with conversions between bytes and protocol columns.
pub struct Lines<'a> {
    lines: Vec<&'a str>,
}

impl<'a> Lines<'a> {
    pub fn new(text: &'a str) -> Lines<'a> {
        Lines { lines: split_lines(text) }
    }

    pub fn line(&self, n: u32) -> &'a str {
        self.lines.get(n as usize).copied().unwrap_or("")
    }

    pub fn range(&self, span: Span, enc: Encoding) -> Range {
        let line = self.line(span.line);
        Range {
            start: Position::new(span.line, to_col(line, span.start as usize, enc)),
            end: Position::new(span.line, to_col(line, span.end as usize, enc)),
        }
    }

    /// The line and byte offset of a protocol position.
    pub fn byte_of(&self, pos: Position, enc: Encoding) -> (u32, u32) {
        let line = self.line(pos.line);
        (pos.line, from_col(line, pos.character, enc) as u32)
    }
}

/// Lines without their terminators. A trailing newline does not add a line.
pub fn split_lines(text: &str) -> Vec<&str> {
    text.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l)).collect()
}

pub fn to_col(line: &str, byte: usize, enc: Encoding) -> u32 {
    let byte = floor_boundary(line, byte.min(line.len()));
    match enc {
        Encoding::Utf8 => byte as u32,
        Encoding::Utf16 => line[..byte].encode_utf16().count() as u32,
    }
}

pub fn from_col(line: &str, col: u32, enc: Encoding) -> usize {
    match enc {
        Encoding::Utf8 => floor_boundary(line, (col as usize).min(line.len())),
        Encoding::Utf16 => {
            let mut units = 0;
            for (i, c) in line.char_indices() {
                if units >= col as usize {
                    return i;
                }
                units += c.len_utf16();
            }
            line.len()
        }
    }
}

fn floor_boundary(s: &str, mut i: usize) -> usize {
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// The byte offset in `text` of a protocol position.
pub fn offset(text: &str, pos: Position, enc: Encoding) -> usize {
    let mut start = 0;
    for _ in 0..pos.line {
        match text[start..].find('\n') {
            Some(i) => start += i + 1,
            None => return text.len(),
        }
    }
    let end = text[start..].find('\n').map_or(text.len(), |i| start + i);
    let line = text[start..end].strip_suffix('\r').unwrap_or(&text[start..end]);
    start + from_col(line, pos.character, enc)
}

pub fn apply_change(text: &mut String, change: TextDocumentContentChangeEvent, enc: Encoding) {
    match change.range {
        None => *text = change.text,
        Some(range) => {
            let a = offset(text, range.start, enc);
            let b = offset(text, range.end, enc).max(a);
            text.replace_range(a..b, &change.text);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns() {
        let line = "aé𝄞b";
        assert_eq!(to_col(line, 3, Encoding::Utf16), 2);
        assert_eq!(to_col(line, 7, Encoding::Utf16), 4);
        assert_eq!(from_col(line, 4, Encoding::Utf16), 7);
        assert_eq!(from_col(line, 2, Encoding::Utf8), 1);
    }

    #[test]
    fn incremental_change() {
        let mut text = String::from("one\ntwo é\nthree\n");
        let change = TextDocumentContentChangeEvent {
            range: Some(Range::new(Position::new(1, 4), Position::new(2, 1))),
            range_length: None,
            text: "X".into(),
        };
        apply_change(&mut text, change, Encoding::Utf16);
        assert_eq!(text, "one\ntwo Xhree\n");
    }
}
