//! A visual row whose offsets remain in source UTF-8 bytes.
use gpui::{
    App, Bounds, Hsla, Pixels, Point, ShapedLine, SharedString, TextAlign, Window, point, px, size,
};
use std::ops::Range;

pub(crate) struct InlineFragment {
    pub(crate) range: Range<usize>,
    pub(crate) x: Pixels,
    pub(crate) width: Pixels,
    pub(crate) text: Option<ShapedLine>,
    /// vsn: an inlay hint, painted in this fragment's empty range.
    pub(crate) hint: Option<Box<HintFragment>>,
}

/// vsn: an inlay hint laid out as an inline fragment: `range` is empty, at
/// the offset the hint is painted at, so the hint takes space but no text.
pub(crate) struct HintFragment {
    /// The whole label, shaped once.
    pub(crate) label: ShapedLine,
    /// Where the label starts, from the fragment's left edge.
    pub(crate) label_x: Pixels,
    /// The rounded background, from the fragment's left edge.
    pub(crate) chip: Range<Pixels>,
    pub(crate) background: Hsla,
    /// Each label part's x range, from the label's left edge, and its id.
    pub(crate) parts: Vec<(Range<Pixels>, Option<usize>)>,
    /// A caret at the hint's offset is drawn before the hint, not after it.
    pub(crate) attach_left: bool,
}

pub(crate) struct InputLine {
    pub(crate) len: usize,
    pub(crate) width: Pixels,
    pub(crate) text: SharedString,
    content: Content,
}
// Keep the ordinary shaped row inline: boxing it would add an allocation to
// every existing plain-text row. Only token rows allocate fragment storage.
#[allow(clippy::large_enum_variant)]
enum Content {
    Text(ShapedLine),
    Inline(Vec<InlineFragment>),
}
impl From<ShapedLine> for InputLine {
    fn from(line: ShapedLine) -> Self {
        Self {
            len: line.len,
            width: line.width,
            text: line.text.clone(),
            content: Content::Text(line),
        }
    }
}
impl InputLine {
    pub(crate) fn inline(text: SharedString, fragments: Vec<InlineFragment>) -> Self {
        let width = fragments.last().map_or(px(0.), |f| f.x + f.width);
        Self {
            len: text.len(),
            text,
            width,
            content: Content::Inline(fragments),
        }
    }
    /// vsn: the inlay hints in this row, with their fragments.
    pub(crate) fn hint_fragments(&self) -> impl Iterator<Item = (&InlineFragment, &HintFragment)> {
        let fragments = match &self.content {
            Content::Inline(fragments) => fragments.as_slice(),
            Content::Text(_) => &[],
        };
        fragments
            .iter()
            .filter_map(|f| f.hint.as_deref().map(|hint| (f, hint)))
    }

    pub(crate) fn x_for_index(&self, ix: usize) -> Pixels {
        match &self.content {
            Content::Text(line) => line.x_for_index(ix),
            Content::Inline(fragments) => {
                for f in fragments {
                    // vsn: a caret at a hint that belongs to the text before
                    // it is drawn before the hint.
                    if f.hint.as_ref().is_some_and(|hint| hint.attach_left) && ix == f.range.start {
                        return f.x;
                    }
                    if ix < f.range.end {
                        return f.x
                            + f.text.as_ref().map_or(
                                if ix <= f.range.start { px(0.) } else { f.width },
                                |line| line.x_for_index(ix.saturating_sub(f.range.start)),
                            );
                    }
                }
                self.width
            }
        }
    }
    pub(crate) fn closest_index_for_x(&self, x: Pixels) -> usize {
        match &self.content {
            Content::Text(line) => line.closest_index_for_x(x),
            Content::Inline(fragments) => {
                for f in fragments {
                    if x <= f.x + f.width {
                        return f.text.as_ref().map_or(
                            if x - f.x < f.width / 2. {
                                f.range.start
                            } else {
                                f.range.end
                            },
                            |line| f.range.start + line.closest_index_for_x(x - f.x),
                        );
                    }
                }
                self.len
            }
        }
    }
    pub(crate) fn index_for_x(&self, x: Pixels) -> Option<usize> {
        match &self.content {
            Content::Text(line) => line.index_for_x(x),
            Content::Inline(_) => {
                (x >= px(0.) && x <= self.width).then(|| self.closest_index_for_x(x))
            }
        }
    }
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn paint(
        &self,
        pos: Point<Pixels>,
        height: Pixels,
        align: TextAlign,
        width: Option<Pixels>,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.paint_pass(pos, height, align, width, false, window, cx);
    }
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn paint_background(
        &self,
        pos: Point<Pixels>,
        height: Pixels,
        align: TextAlign,
        width: Option<Pixels>,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.paint_pass(pos, height, align, width, true, window, cx);
    }
    #[allow(clippy::too_many_arguments)]
    fn paint_pass(
        &self,
        pos: Point<Pixels>,
        height: Pixels,
        align: TextAlign,
        width: Option<Pixels>,
        background: bool,
        window: &mut Window,
        cx: &mut App,
    ) {
        let paint = |line: &ShapedLine, pos, align, width, window: &mut Window, cx: &mut App| {
            if background {
                let _ = line.paint_background(pos, height, align, width, window, cx);
            } else {
                let _ = line.paint(pos, height, align, width, window, cx);
            }
        };
        match &self.content {
            Content::Text(line) => paint(line, pos, align, width, window, cx),
            Content::Inline(fragments) => {
                let remaining = (width.unwrap_or(self.width) - self.width).max(px(0.));
                let offset = match align {
                    TextAlign::Right => remaining,
                    TextAlign::Center => remaining / 2.,
                    _ => px(0.),
                };
                for f in fragments {
                    // vsn: an inlay hint: a faint rounded chip, then its label.
                    if let Some(hint) = &f.hint {
                        if !background {
                            let inset = (height / 10.).round();
                            let chip = Bounds::new(
                                pos + point(offset + f.x + hint.chip.start, inset),
                                size(hint.chip.end - hint.chip.start, height - inset * 2.),
                            );
                            window.paint_quad(gpui::fill(chip, hint.background).corner_radii(px(3.)));
                            let _ = hint.label.paint(
                                pos + point(offset + f.x + hint.label_x, px(0.)),
                                height,
                                TextAlign::Left,
                                None,
                                window,
                                cx,
                            );
                        }
                        continue;
                    }
                    if let Some(line) = &f.text {
                        paint(
                            line,
                            pos + point(offset + f.x, px(0.)),
                            TextAlign::Left,
                            None,
                            window,
                            cx,
                        );
                    }
                }
            }
        }
    }
}
