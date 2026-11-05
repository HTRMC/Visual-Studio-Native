//! vsn: inlay hints and CodeLens, as a language server reports them.
//!
//! Both are display-only: an inlay hint takes horizontal space at a byte
//! offset but is not in the buffer, and a CodeLens is a row above a buffer line
//! that is neither text nor a display row the cursor can reach. The editor
//! keeps them in step with edits (see [`shift_offset`]) until the owner sends
//! fresh ones.
use std::ops::Range;

use gpui::SharedString;

/// vsn: one piece of an inlay hint's label. A part with an `id` is clickable:
/// clicking it emits [`super::InputEvent::InlayClicked`] with that id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InlayLabelPart {
    pub text: SharedString,
    pub id: Option<usize>,
}

/// vsn: a hint painted at a byte offset, e.g. a type after a binding or a
/// parameter name before an argument.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InlayHint {
    /// Byte offset in the buffer the hint is painted at.
    pub offset: usize,
    pub label: Vec<InlayLabelPart>,
    /// Leave a gap before the hint.
    pub padding_left: bool,
    /// Leave a gap after the hint.
    pub padding_right: bool,
}

impl InlayHint {
    /// Whether a caret at the hint's offset is drawn before the hint rather than
    /// after it. A hint that only pads on the left (a type hint, `x: i32`)
    /// belongs to the text before it; one that pads on the right (a parameter
    /// name, `f(name: x)`) to the text after it.
    pub(crate) fn attaches_left(&self) -> bool {
        self.padding_left && !self.padding_right
    }
}

/// vsn: the lenses of one buffer line, painted as a row above it. Several
/// items on the line are joined with `" | "`; clicking an item emits
/// [`super::InputEvent::CodeLensClicked`] with its id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeLens {
    /// Zero-based buffer line the lens sits above.
    pub line: usize,
    /// `(title, id)` for each lens on the line.
    pub items: Vec<(SharedString, usize)>,
}

/// vsn: where `offset` goes after `range` was replaced by `new_len` bytes, or
/// `None` when the replaced text swallowed it.
///
/// An offset before the edit stays, one after it shifts by the length change.
/// An insertion exactly at the offset pushes it along (typing at a hint keeps
/// the hint after what was typed). A non-empty replacement keeps an offset at
/// its start, shifts one at its end, and drops one strictly inside.
pub(crate) fn shift_offset(offset: usize, range: &Range<usize>, new_len: usize) -> Option<usize> {
    if range.is_empty() {
        if offset < range.start {
            Some(offset)
        } else {
            Some(offset + new_len)
        }
    } else if offset <= range.start {
        Some(offset)
    } else if offset >= range.end {
        Some(offset - range.end + range.start + new_len)
    } else {
        None
    }
}

/// vsn: shift every hint for an edit, dropping the ones inside replaced text.
/// Hints stay sorted: the shift is monotonic on the offsets it keeps.
pub(crate) fn adjust_hints_for_edit(hints: &mut Vec<InlayHint>, range: &Range<usize>, new_len: usize) {
    if hints.is_empty() {
        return;
    }
    hints.retain_mut(|hint| match shift_offset(hint.offset, range, new_len) {
        Some(offset) => {
            hint.offset = offset;
            true
        }
        None => false,
    });
}

/// vsn: shift lens anchors (line-start offsets) for an edit, dropping the
/// lenses whose anchor was swallowed. `lenses` and `anchors` are parallel.
pub(crate) fn adjust_anchors_for_edit(
    lenses: &mut Vec<CodeLens>,
    anchors: &mut Vec<usize>,
    range: &Range<usize>,
    new_len: usize,
) {
    if anchors.is_empty() {
        return;
    }
    let mut keep = 0;
    for ix in 0..anchors.len() {
        if let Some(anchor) = shift_offset(anchors[ix], range, new_len) {
            anchors[keep] = anchor;
            lenses.swap(keep, ix);
            keep += 1;
        }
    }
    anchors.truncate(keep);
    lenses.truncate(keep);
}

/// vsn: sort lenses by line and merge the ones that share a line, so there is
/// one row per line.
pub(crate) fn normalize_lenses(mut lenses: Vec<CodeLens>) -> Vec<CodeLens> {
    lenses.sort_by_key(|lens| lens.line);
    let mut out: Vec<CodeLens> = Vec::with_capacity(lenses.len());
    for lens in lenses {
        if lens.items.is_empty() {
            continue;
        }
        match out.last_mut() {
            Some(last) if last.line == lens.line => last.items.extend(lens.items),
            _ => out.push(lens),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hint(offset: usize) -> InlayHint {
        InlayHint {
            offset,
            label: vec![InlayLabelPart {
                text: ": i32".into(),
                id: None,
            }],
            padding_left: true,
            padding_right: false,
        }
    }

    fn lens(line: usize, id: usize) -> CodeLens {
        CodeLens {
            line,
            items: vec![(format!("lens {id}").into(), id)],
        }
    }

    #[test]
    fn offsets_before_an_edit_stay_and_after_it_shift() {
        // Replace 10..14 with 2 bytes: -2.
        assert_eq!(shift_offset(3, &(10..14), 2), Some(3));
        assert_eq!(shift_offset(10, &(10..14), 2), Some(10));
        assert_eq!(shift_offset(14, &(10..14), 2), Some(12));
        assert_eq!(shift_offset(20, &(10..14), 2), Some(18));
        // Insert 5 bytes at 10: +5, and an offset right at the insertion moves.
        assert_eq!(shift_offset(9, &(10..10), 5), Some(9));
        assert_eq!(shift_offset(10, &(10..10), 5), Some(15));
        assert_eq!(shift_offset(30, &(10..10), 5), Some(35));
        // Delete 10..14.
        assert_eq!(shift_offset(14, &(10..14), 0), Some(10));
    }

    #[test]
    fn offsets_inside_a_replaced_range_are_dropped() {
        assert_eq!(shift_offset(11, &(10..14), 2), None);
        assert_eq!(shift_offset(13, &(10..14), 0), None);
    }

    #[test]
    fn hints_shift_and_drop_in_order() {
        let mut hints = vec![hint(2), hint(11), hint(14), hint(40)];
        adjust_hints_for_edit(&mut hints, &(10..14), 7);
        let offsets: Vec<_> = hints.iter().map(|h| h.offset).collect();
        assert_eq!(offsets, vec![2, 17, 43]);

        let mut empty = Vec::new();
        adjust_hints_for_edit(&mut empty, &(0..5), 0);
        assert!(empty.is_empty());
    }

    #[test]
    fn lens_anchors_follow_their_line_and_drop_with_it() {
        // Lines start at 0, 10, 20, 30.
        let mut lenses = vec![lens(0, 0), lens(1, 1), lens(2, 2), lens(3, 3)];
        let mut anchors = vec![0, 10, 20, 30];
        // Delete 5..25: swallows the starts of lines 1 and 2.
        adjust_anchors_for_edit(&mut lenses, &mut anchors, &(5..25), 0);
        assert_eq!(anchors, vec![0, 10]);
        assert_eq!(lenses.iter().map(|l| l.items[0].1).collect::<Vec<_>>(), vec![0, 3]);

        // An insertion at a line start pushes its lens along with the line.
        adjust_anchors_for_edit(&mut lenses, &mut anchors, &(10..10), 4);
        assert_eq!(anchors, vec![0, 14]);
    }

    #[test]
    fn lenses_on_one_line_merge() {
        let merged = normalize_lenses(vec![
            lens(5, 2),
            lens(1, 0),
            lens(5, 3),
            CodeLens {
                line: 7,
                items: vec![],
            },
        ]);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].line, 1);
        assert_eq!(merged[1].line, 5);
        assert_eq!(
            merged[1].items.iter().map(|i| i.1).collect::<Vec<_>>(),
            vec![2, 3]
        );
    }
}
