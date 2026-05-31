//! Maps AScript char-offset spans onto LSP `Position { line, character }`.
//!
//! The pipeline's `Span` uses CHAR offsets into the (UTF-8) source. LSP's default
//! position encoding is UTF-16: `character` is the number of UTF-16 code units from
//! the start of the line. Most chars are 1 unit, but astral chars (e.g. an emoji)
//! are 2. `LineIndex` precomputes line-start char offsets and the per-char text so
//! the conversion (both directions) is correct for multibyte and astral input.

use tower_lsp::lsp_types::Position;

pub struct LineIndex {
    /// The source as a Vec of chars (so we can index by char offset directly).
    chars: Vec<char>,
    /// Char offset of the first char of each line. `line_starts[0] == 0`.
    line_starts: Vec<usize>,
}

impl LineIndex {
    pub fn new(text: &str) -> Self {
        let chars: Vec<char> = text.chars().collect();
        let mut line_starts = vec![0usize];
        for (i, &c) in chars.iter().enumerate() {
            if c == '\n' {
                line_starts.push(i + 1);
            }
        }
        LineIndex { chars, line_starts }
    }

    /// Convert a char offset into an LSP `Position` (UTF-16 column).
    pub fn position(&self, char_offset: usize) -> Position {
        // Clamp to the end so out-of-range spans don't panic.
        let offset = char_offset.min(self.chars.len());
        // Find the line: the greatest line start <= offset.
        let line = match self.line_starts.binary_search(&offset) {
            Ok(l) => l,
            Err(next) => next - 1,
        };
        let line_start = self.line_starts[line];
        // UTF-16 code units from the line start to the offset.
        let character: u32 = self.chars[line_start..offset]
            .iter()
            .map(|c| c.len_utf16() as u32)
            .sum();
        Position {
            line: line as u32,
            character,
        }
    }

    /// Convert an LSP `Position` (UTF-16 column) back to a char offset.
    pub fn offset(&self, position: Position) -> usize {
        let line = position.line as usize;
        if line >= self.line_starts.len() {
            return self.chars.len();
        }
        let line_start = self.line_starts[line];
        // The (exclusive) end of this line's chars.
        let line_end = self
            .line_starts
            .get(line + 1)
            .copied()
            .unwrap_or(self.chars.len());
        // Walk chars accumulating UTF-16 units until we reach the target column.
        let target = position.character;
        let mut utf16 = 0u32;
        let mut offset = line_start;
        while offset < line_end {
            if utf16 >= target {
                break;
            }
            utf16 += self.chars[offset].len_utf16() as u32;
            offset += 1;
        }
        offset
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pos(line: u32, character: u32) -> Position {
        Position { line, character }
    }

    #[test]
    fn single_line() {
        let idx = LineIndex::new("hello");
        assert_eq!(idx.position(0), pos(0, 0));
        assert_eq!(idx.position(5), pos(0, 5));
        // Round-trip.
        assert_eq!(idx.offset(pos(0, 0)), 0);
        assert_eq!(idx.offset(pos(0, 5)), 5);
    }

    #[test]
    fn multi_line() {
        // "ab\ncde\nf" — char offsets: a0 b1 \n2 c3 d4 e5 \n6 f7
        let idx = LineIndex::new("ab\ncde\nf");
        assert_eq!(idx.position(0), pos(0, 0)); // 'a'
        assert_eq!(idx.position(1), pos(0, 1)); // 'b'
        assert_eq!(idx.position(3), pos(1, 0)); // 'c'
        assert_eq!(idx.position(5), pos(1, 2)); // 'e'
        assert_eq!(idx.position(7), pos(2, 0)); // 'f'
                                                // Round-trip each.
        for off in [0usize, 1, 3, 5, 7] {
            assert_eq!(idx.offset(idx.position(off)), off);
        }
    }

    #[test]
    fn multibyte_char() {
        // 'é' is 1 char and 1 UTF-16 unit (but 2 bytes in UTF-8).
        // "aé b" — a0 é1 (space)2 b3
        let idx = LineIndex::new("aéb");
        assert_eq!(idx.position(0), pos(0, 0)); // 'a'
        assert_eq!(idx.position(1), pos(0, 1)); // 'é'
        assert_eq!(idx.position(2), pos(0, 2)); // 'b' — é counted as 1 utf16 unit
        assert_eq!(idx.position(3), pos(0, 3));
        // Round-trip.
        for off in [0usize, 1, 2, 3] {
            assert_eq!(idx.offset(idx.position(off)), off);
        }
    }

    #[test]
    fn astral_char() {
        // '😀' is 1 char but 2 UTF-16 code units.
        // "a😀b" — a0 😀1 b2
        let idx = LineIndex::new("a😀b");
        assert_eq!(idx.position(0), pos(0, 0)); // 'a'
        assert_eq!(idx.position(1), pos(0, 1)); // '😀' starts at utf16 col 1
        assert_eq!(idx.position(2), pos(0, 3)); // 'b' — emoji took 2 utf16 units
        assert_eq!(idx.position(3), pos(0, 4));
        // Round-trip: char offsets map back exactly.
        for off in [0usize, 1, 2, 3] {
            assert_eq!(idx.offset(idx.position(off)), off);
        }
        // A column landing inside the surrogate pair (col 2) rounds to the next char.
        assert_eq!(idx.offset(pos(0, 2)), 2);
    }

    #[test]
    fn astral_across_lines() {
        // "😀\nx" — 😀0 \n1 x2
        let idx = LineIndex::new("😀\nx");
        assert_eq!(idx.position(0), pos(0, 0));
        assert_eq!(idx.position(2), pos(1, 0)); // 'x' on line 1
        assert_eq!(idx.offset(idx.position(2)), 2);
    }
}
