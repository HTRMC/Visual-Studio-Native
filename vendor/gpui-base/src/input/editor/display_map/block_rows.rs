//! vsn: BlockRows: rows painted above buffer lines that are not display rows.
//!
//! A CodeLens takes a row of its own above its buffer line. That row is not
//! text, so cursor movement, folding and soft wrap keep working in display
//! rows as before, and only geometry (which pixel row a display row is drawn
//! at, which display row a pixel row belongs to) goes through this layer.
//!
//! Each block is one row tall, sits above the first display row of its buffer
//! line, and a "visual row" counts both: visual row `v` is `v * line_height`
//! from the top of the content. With no blocks every mapping is the identity.

/// vsn: the visible block rows, keyed by the display row they sit above.
#[derive(Default, Debug, Clone)]
pub(crate) struct BlockRows {
    /// Buffer lines that want a block above them: sorted, unique.
    lines: Vec<usize>,
    /// The buffer lines of `lines` that are visible, in order.
    visible_lines: Vec<usize>,
    /// For each of `visible_lines`, the first display row of that line:
    /// sorted, unique.
    rows: Vec<usize>,
}

impl BlockRows {
    /// Replace the buffer lines that want a block. Takes effect on the next
    /// [`Self::rebuild`].
    pub(crate) fn set_lines(&mut self, mut lines: Vec<usize>) {
        lines.sort_unstable();
        lines.dedup();
        self.lines = lines;
    }

    #[inline]
    pub(crate) fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// Recompute which blocks are visible and where, given each line's first
    /// display row (`None` for a hidden or missing line).
    pub(crate) fn rebuild(&mut self, mut first_display_row: impl FnMut(usize) -> Option<usize>) {
        self.visible_lines.clear();
        self.rows.clear();
        for &line in &self.lines {
            if let Some(row) = first_display_row(line) {
                // Two lines can't share a first display row, but stay safe.
                if self.rows.last().is_some_and(|&last| last >= row) {
                    continue;
                }
                self.visible_lines.push(line);
                self.rows.push(row);
            }
        }
    }

    /// How many block rows are visible.
    #[inline]
    pub(crate) fn row_count(&self) -> usize {
        self.rows.len()
    }

    /// Whether a visible block sits above this buffer line.
    #[inline]
    pub(crate) fn has_block_above(&self, line: usize) -> bool {
        !self.visible_lines.is_empty() && self.visible_lines.binary_search(&line).is_ok()
    }

    /// The visual row a display row's text is drawn at.
    #[inline]
    pub(crate) fn display_row_to_visual_row(&self, display_row: usize) -> usize {
        display_row + self.rows.partition_point(|&b| b <= display_row)
    }

    /// The visual row of the top of a display row, counting a block above it.
    #[inline]
    pub(crate) fn display_row_block_top(&self, display_row: usize) -> usize {
        display_row + self.rows.partition_point(|&b| b < display_row)
    }

    /// The display row a visual row belongs to. A block row belongs to the
    /// display row below it.
    pub(crate) fn visual_row_to_display_row(&self, visual_row: usize) -> usize {
        // The block above `rows[i]` is at visual row `rows[i] + i`, which is
        // strictly increasing in `i`.
        let mut lo = 0;
        let mut hi = self.rows.len();
        while lo < hi {
            let mid = (lo + hi) / 2;
            if self.rows[mid] + mid < visual_row {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        visual_row - lo
    }

    /// Whether a visual row is a block row.
    #[cfg(test)]
    pub(crate) fn is_block_row(&self, visual_row: usize) -> bool {
        let d = self.visual_row_to_display_row(visual_row);
        self.rows.binary_search(&d).is_ok() && self.display_row_block_top(d) == visual_row
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blocks(lines: &[usize], first_rows: &[Option<usize>]) -> BlockRows {
        let mut blocks = BlockRows::default();
        blocks.set_lines(lines.to_vec());
        blocks.rebuild(|line| first_rows.get(line).copied().flatten());
        blocks
    }

    /// Lay the rows out one by one: for each display row, its block (if any)
    /// then its text. Returns, per visual row, (display row, is block).
    fn dense_visual_rows(display_rows: usize, block_rows: &[usize]) -> Vec<(usize, bool)> {
        let mut out = Vec::new();
        for d in 0..display_rows {
            if block_rows.contains(&d) {
                out.push((d, true));
            }
            out.push((d, false));
        }
        out
    }

    #[test]
    fn no_blocks_is_the_identity() {
        let blocks = BlockRows::default();
        assert!(blocks.is_empty());
        for row in 0..10 {
            assert_eq!(blocks.display_row_to_visual_row(row), row);
            assert_eq!(blocks.display_row_block_top(row), row);
            assert_eq!(blocks.visual_row_to_display_row(row), row);
            assert!(!blocks.is_block_row(row));
        }
        assert_eq!(blocks.row_count(), 0);
    }

    #[test]
    fn visual_rows_match_the_dense_layout() {
        let display_rows = 12;
        let cases: &[&[usize]] = &[&[0], &[3], &[0, 1, 2], &[2, 5, 11], &[11], &[1, 4, 6, 7]];
        for &block_rows in cases {
            // Buffer line == display row here: no wrap, no folds.
            let first: Vec<_> = (0..display_rows).map(Some).collect();
            let blocks = blocks(block_rows, &first);
            let dense = dense_visual_rows(display_rows, block_rows);
            assert_eq!(blocks.row_count(), block_rows.len());

            for (v, &(d, is_block)) in dense.iter().enumerate() {
                assert_eq!(blocks.visual_row_to_display_row(v), d, "{block_rows:?} v={v}");
                assert_eq!(blocks.is_block_row(v), is_block, "{block_rows:?} v={v}");
            }
            for d in 0..display_rows {
                let text = dense.iter().position(|&r| r == (d, false)).unwrap();
                let top = dense.iter().position(|&r| r.0 == d).unwrap();
                assert_eq!(blocks.display_row_to_visual_row(d), text, "{block_rows:?} d={d}");
                assert_eq!(blocks.display_row_block_top(d), top, "{block_rows:?} d={d}");
            }
        }
    }

    #[test]
    fn hidden_lines_have_no_block_and_wrapped_lines_get_one_above_their_first_row() {
        // Lines 0..6. Line 1 wraps to two rows; lines 3 and 4 are folded away
        // into line 2.
        let first = [Some(0), Some(1), Some(3), None, None, Some(4)];
        let blocks = blocks(&[1, 3, 5, 9], &first);
        assert_eq!(blocks.row_count(), 2);
        assert!(blocks.has_block_above(1));
        assert!(!blocks.has_block_above(3));
        assert!(blocks.has_block_above(5));
        assert!(!blocks.has_block_above(9));
        // Visual: 0 text(0), 1 block, 2 text(1), 3 text(2, wrap), 4 text(3), 5 block, 6 text(4).
        assert_eq!(blocks.display_row_block_top(1), 1);
        assert_eq!(blocks.display_row_to_visual_row(1), 2);
        assert_eq!(blocks.display_row_to_visual_row(2), 3);
        assert_eq!(blocks.display_row_to_visual_row(4), 6);
        assert_eq!(blocks.visual_row_to_display_row(5), 4);
        assert!(blocks.is_block_row(5));
    }
}
