//! Byte-offset source spans for AST nodes and parse errors.

/// A half-open byte range `[start, end)` into the original formula string.
///
/// Spans are byte offsets (not char offsets) so they index directly into the
/// source `&str`; every AST node and every [`crate::ParseError`] carries one
/// for precise diagnostics.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Span {
    /// Inclusive start byte offset.
    pub start: usize,
    /// Exclusive end byte offset.
    pub end: usize,
}

impl Span {
    /// Construct a span from a start and end byte offset.
    #[must_use]
    pub const fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    /// A zero-length span at `pos` (used for "expected token" errors that
    /// point at a position rather than a range).
    #[must_use]
    pub const fn point(pos: usize) -> Self {
        Self {
            start: pos,
            end: pos,
        }
    }

    /// The smallest span covering both `self` and `other`.
    #[must_use]
    pub const fn to(self, other: Span) -> Span {
        Span {
            start: self.start,
            end: other.end,
        }
    }
}
