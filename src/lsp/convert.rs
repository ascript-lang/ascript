//! Coordinate conversion for the LSP: the analysis core speaks BYTE offsets,
//! LSP speaks UTF-16 line/character `Position`s. All conversion lives here.

use crate::check::ByteSpan;
use crate::lsp::line_index::LineIndex;
use tower_lsp::lsp_types::Range;

/// Convert a byte offset to a char offset (the char-based `LineIndex` then maps
/// char→`Position`). Clamps to the largest char boundary `<= byte` so a
/// mid-codepoint byte never panics.
pub fn byte_to_char(src: &str, byte: usize) -> usize {
    let mut b = byte.min(src.len());
    while b > 0 && !src.is_char_boundary(b) {
        b -= 1;
    }
    src[..b].chars().count()
}

/// Convert a byte-offset `ByteSpan` to an LSP `Range`.
pub fn byte_span_to_range(src: &str, index: &LineIndex, span: ByteSpan) -> Range {
    Range {
        start: index.position(byte_to_char(src, span.start)),
        end: index.position(byte_to_char(src, span.end)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tower_lsp::lsp_types::Position;

    #[test]
    fn byte_to_char_handles_multibyte() {
        let src = "héllo";
        assert_eq!(byte_to_char(src, 0), 0);
        assert_eq!(byte_to_char(src, 3), 2);
        assert_eq!(byte_to_char(src, 2), 1);
        assert_eq!(byte_to_char(src, 999), 5);
    }

    #[test]
    fn byte_span_to_range_maps_endpoints() {
        let src = "let x = 1\nprint(x)\n";
        let index = LineIndex::new(src);
        let r = byte_span_to_range(src, &index, ByteSpan { start: 10, end: 15 });
        assert_eq!(r.start, Position::new(1, 0));
        assert_eq!(r.end, Position::new(1, 5));
    }
}
