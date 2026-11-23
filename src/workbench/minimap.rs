//! Sticky scroll and the minimap: views painted over and beside an editor
//! from its layout.

use std::{cell::Cell, rc::Rc};

use gpui_kit::component::input::{FoldRange, Position};

use super::*;

/// editor.stickyScroll.maxLineCount
const STICKY_MAX: usize = 5;
pub(super) const MINIMAP_WIDTH: f32 = 100.;
/// VS Code's minimap scale at this pixel density: a character is this many
/// device pixels wide and twice that tall, on whole pixels so none are
/// squeezed or gapped (1 below 2x, else 2, for `editor.minimap.scale` 1).
fn minimap_scale(dpr: f32) -> usize {
    if dpr >= 2. { 2 } else { 1 }
}

/// Printable ASCII's shapes ('!' to '~'), each 2x4 cells of brightness, left
/// to right then down: Consolas drawn large and averaged down, brightest
/// cell 255, as VS Code's minimap prebakes its character data.
#[rustfmt::skip]
const GLYPHS: [[u8; 8]; 94] = [
    [51, 52, 70, 72, 51, 51, 0, 0], // '!'
    [101, 101, 28, 28, 0, 0, 0, 0], // '"'
    [54, 55, 182, 193, 174, 166, 0, 0], // '#'
    [86, 135, 184, 134, 177, 172, 37, 0], // '$'
    [166, 87, 155, 117, 119, 243, 0, 0], // '%'
    [108, 89, 222, 148, 182, 255, 0, 0], // '&'
    [51, 54, 14, 15, 0, 0, 0, 0], // "'"
    [21, 86, 141, 4, 138, 12, 11, 82], // '('
    [87, 24, 4, 145, 9, 136, 78, 11], // ')'
    [77, 82, 108, 113, 0, 0, 0, 0], // '*'
    [0, 0, 131, 135, 72, 76, 0, 0], // '+'
    [0, 0, 0, 0, 60, 75, 73, 21], // ','
    [0, 0, 56, 58, 14, 14, 0, 0], // '-'
    [0, 0, 0, 0, 65, 60, 0, 0], // '.'
    [0, 84, 36, 98, 134, 1, 35, 0], // '/'
    [87, 93, 202, 225, 174, 150, 0, 0], // '0'
    [86, 54, 59, 117, 114, 173, 0, 0], // '1'
    [88, 89, 4, 156, 188, 124, 0, 0], // '2'
    [79, 87, 50, 171, 88, 146, 0, 0], // '3'
    [17, 102, 140, 148, 128, 201, 0, 0], // '4'
    [104, 76, 142, 99, 85, 144, 0, 0], // '5'
    [61, 80, 194, 116, 152, 156, 0, 0], // '6'
    [105, 128, 5, 144, 114, 25, 0, 0], // '7'
    [90, 97, 185, 185, 171, 169, 0, 0], // '8'
    [91, 86, 179, 198, 84, 121, 0, 0], // '9'
    [0, 0, 52, 54, 52, 54, 0, 0], // ':'
    [0, 0, 52, 54, 53, 81, 70, 24], // ';'
    [0, 4, 103, 84, 71, 88, 0, 0], // '<'
    [0, 0, 91, 93, 91, 93, 0, 0], // '='
    [4, 0, 82, 105, 85, 72, 0, 0], // '>'
    [59, 89, 61, 129, 88, 14, 0, 0], // '?'
    [85, 113, 205, 231, 231, 203, 114, 52], // '@'
    [57, 61, 142, 151, 188, 194, 0, 0], // 'A'
    [115, 95, 192, 187, 177, 157, 0, 0], // 'B'
    [80, 103, 156, 0, 160, 102, 0, 0], // 'C'
    [126, 86, 144, 155, 189, 148, 0, 0], // 'D'
    [106, 84, 184, 80, 167, 84, 0, 0], // 'E'
    [105, 84, 184, 76, 131, 0, 0, 0], // 'F'
    [83, 106, 155, 119, 160, 181, 0, 0], // 'G'
    [66, 66, 205, 207, 128, 128, 0, 0], // 'H'
    [98, 101, 70, 75, 129, 133, 0, 0], // 'I'
    [81, 98, 0, 147, 89, 133, 0, 0], // 'J'
    [66, 76, 242, 62, 140, 144, 0, 0], // 'K'
    [67, 0, 147, 0, 162, 93, 0, 0], // 'L'
    [96, 99, 240, 237, 125, 128, 0, 0], // 'M'
    [106, 61, 235, 177, 119, 225, 0, 0], // 'N'
    [92, 101, 151, 153, 167, 159, 0, 0], // 'O'
    [114, 94, 183, 173, 138, 4, 0, 0], // 'P'
    [93, 102, 151, 153, 176, 165, 15, 135], // 'Q'
    [113, 89, 190, 164, 128, 143, 0, 0], // 'R'
    [94, 80, 151, 87, 103, 163, 0, 0], // 'S'
    [117, 119, 70, 75, 62, 67, 0, 0], // 'T'
    [66, 66, 144, 144, 161, 155, 0, 0], // 'U'
    [74, 69, 158, 146, 126, 117, 0, 0], // 'V'
    [62, 58, 191, 189, 203, 211, 0, 0], // 'W'
    [77, 75, 144, 142, 151, 155, 0, 0], // 'X'
    [79, 74, 150, 143, 62, 67, 0, 0], // 'Y'
    [102, 131, 48, 116, 204, 110, 0, 0], // 'Z'
    [106, 56, 134, 0, 134, 0, 94, 56], // '['
    [84, 0, 94, 40, 0, 134, 0, 35], // '\\'
    [55, 108, 0, 136, 0, 136, 55, 96], // ']'
    [47, 50, 85, 94, 0, 0, 0, 0], // '^'
    [0, 0, 0, 0, 0, 0, 117, 117], // '_'
    [66, 8, 0, 0, 0, 0, 0, 0], // '`'
    [0, 0, 108, 194, 183, 183, 0, 0], // 'a'
    [89, 0, 196, 170, 167, 147, 0, 0], // 'b'
    [0, 0, 145, 84, 148, 83, 0, 0], // 'c'
    [0, 89, 150, 190, 162, 177, 0, 0], // 'd'
    [0, 0, 181, 192, 172, 113, 0, 0], // 'e'
    [50, 117, 194, 108, 113, 14, 0, 0], // 'f'
    [0, 0, 156, 200, 224, 159, 134, 127], // 'g'
    [89, 0, 196, 165, 125, 125, 0, 0], // 'h'
    [42, 44, 105, 87, 110, 147, 0, 0], // 'i'
    [0, 86, 81, 152, 0, 143, 96, 83], // 'j'
    [89, 0, 185, 120, 148, 139, 0, 0], // 'k'
    [91, 60, 47, 96, 110, 147, 0, 0], // 'l'
    [0, 0, 227, 215, 170, 174, 0, 0], // 'm'
    [0, 0, 181, 165, 125, 125, 0, 0], // 'n'
    [0, 0, 157, 166, 160, 154, 0, 0], // 'o'
    [0, 0, 182, 170, 189, 147, 89, 0], // 'p'
    [0, 0, 151, 178, 163, 197, 0, 89], // 'q'
    [0, 0, 172, 145, 126, 0, 0, 0], // 'r'
    [0, 0, 172, 92, 95, 177, 0, 0], // 's'
    [63, 0, 210, 88, 115, 92, 0, 0], // 't'
    [0, 0, 129, 129, 159, 177, 0, 0], // 'u'
    [0, 0, 143, 137, 119, 115, 0, 0], // 'v'
    [0, 0, 160, 161, 198, 205, 0, 0], // 'w'
    [0, 0, 144, 140, 142, 146, 0, 0], // 'x'
    [0, 0, 143, 137, 127, 129, 138, 9], // 'y'
    [0, 0, 93, 176, 178, 96, 0, 0], // 'z'
    [43, 91, 151, 22, 103, 35, 34, 88], // '{'
    [64, 69, 64, 69, 64, 69, 40, 43], // '|'
    [89, 46, 20, 155, 28, 106, 85, 37], // '}'
    [0, 0, 123, 67, 25, 82, 0, 0], // '~'
];

/// A character outside ASCII: a middling smudge.
const OTHER: [u8; 8] = [90, 90, 128, 128, 128, 128, 0, 0];

/// The minimap's pixels: the background, characters blended over it.
struct Bitmap {
    width: usize,
    height: usize,
    dpr: f32,
    /// BGRA, gpui's image order.
    bytes: Vec<u8>,
    /// Each glyph at this scale: `cw` x `ch` device pixels of brightness.
    glyphs: Vec<Vec<u8>>,
    other: Vec<u8>,
    cw: usize,
    ch: usize,
    bg: Rgba,
}

impl Bitmap {
    fn new(area: Size<Pixels>, dpr: f32, scale_by: usize, bg: Hsla) -> Self {
        let width = (f32::from(area.width) * dpr).ceil().max(0.) as usize;
        let height = (f32::from(area.height) * dpr).ceil().max(0.) as usize;
        let bg = bg.to_rgb();
        let pixel = [bg.b, bg.g, bg.r, 1.].map(|c| (c * 255.).round() as u8);
        let (cw, ch) = (scale_by, 2 * scale_by);
        Self {
            width,
            height,
            dpr,
            bytes: pixel.repeat(width * height),
            glyphs: GLYPHS.iter().map(|g| scale(g, cw, ch)).collect(),
            other: scale(&OTHER, cw, ch),
            cw,
            ch,
            bg,
        }
    }

    /// Character `c` with its top left at device pixel (`x0`, `y0`).
    fn char(&mut self, x0: isize, y0: isize, c: char, color: Hsla) {
        let glyph = match c as u32 {
            33..=126 => &self.glyphs[c as usize - 33],
            _ => &self.other,
        };
        let fg = color.to_rgb();
        for gy in 0..self.ch {
            for gx in 0..self.cw {
                let (px, py) = (x0 + gx as isize, y0 + gy as isize);
                let alpha = glyph[gy * self.cw + gx] as f32 / 255. * fg.a;
                if alpha == 0. || px < 0 || py < 0 || px as usize >= self.width || py as usize >= self.height {
                    continue;
                }
                let at = (py as usize * self.width + px as usize) * 4;
                let mix = |f: f32, b: f32| ((b + (f - b) * alpha) * 255.).round() as u8;
                self.bytes[at] = mix(fg.b, self.bg.b);
                self.bytes[at + 1] = mix(fg.g, self.bg.g);
                self.bytes[at + 2] = mix(fg.r, self.bg.r);
            }
        }
    }

    /// Paints it over `bounds`, reusing the image `owner` last painted when
    /// nothing changed: each new one is a new texture.
    fn paint(self, owner: EntityId, bounds: Bounds<Pixels>, window: &mut Window) {
        type Last = std::collections::HashMap<EntityId, (u64, std::sync::Arc<RenderImage>)>;
        thread_local! {
            static LAST: std::cell::RefCell<Last> = Default::default();
        }
        if self.width == 0 || self.height == 0 {
            return;
        }
        let key = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            (self.width, self.height, &self.bytes).hash(&mut h);
            h.finish()
        };
        let (width, height, dpr) = (self.width, self.height, self.dpr);
        let image = LAST.with_borrow_mut(|last| {
            if let Some((k, image)) = last.get(&owner)
                && *k == key
            {
                return Some(image.clone());
            }
            let buffer = image::RgbaImage::from_raw(width as u32, height as u32, self.bytes)?;
            let image = std::sync::Arc::new(RenderImage::new(vec![image::Frame::new(buffer)]));
            if let Some((_, old)) = last.insert(owner, (key, image.clone())) {
                window.drop_image(old).ok();
            }
            Some(image)
        });
        let Some(image) = image else { return };
        let area = Bounds::new(bounds.origin, size(px(width as f32 / dpr), px(height as f32 / dpr)));
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            window.paint_image(area, area, Corners::default(), image, 0, false).ok();
        });
    }
}

/// A 2x4 glyph resampled to `cw` x `ch`, each pixel its source cells' mean.
fn scale(glyph: &[u8; 8], cw: usize, ch: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(cw * ch);
    for ty in 0..ch {
        for tx in 0..cw {
            let (x0, y0) = (tx * 2 / cw, ty * 4 / ch);
            let (x1, y1) = (((tx + 1) * 2 / cw).max(x0 + 1), ((ty + 1) * 4 / ch).max(y0 + 1));
            let (mut sum, mut n) = (0u32, 0u32);
            for y in y0..y1 {
                for x in x0..x1 {
                    sum += glyph[y * 2 + x] as u32;
                    n += 1;
                }
            }
            // Softened to 12/15, as VS Code does its prebaked data.
            out.push((sum / n * 12 / 15) as u8);
        }
    }
    out
}

/// Sticky scroll: the lines opening the scopes around the viewport's top,
/// outermost first. `line_at(i)` is the buffer line on the viewport's row i.
// ponytail: the last header doesn't slide up as its scope ends.
pub(super) fn sticky_lines(folds: &[FoldRange], line_at: impl Fn(usize) -> usize) -> Vec<usize> {
    let mut out: Vec<usize> = Vec::new();
    while out.len() < STICKY_MAX {
        let line = line_at(out.len());
        let next = (folds.iter())
            .filter(|r| r.start_line < line && line < r.end_line)
            .filter(|r| out.last().is_none_or(|&l| r.start_line > l))
            .map(|r| r.start_line)
            .min();
        let Some(start) = next else { break };
        out.push(start);
    }
    out
}

/// The text runs of `line` in the editor's syntax colors.
fn line_runs(e: &EditorState, line: usize, font: &Font, fg: Hsla) -> (SharedString, Vec<TextRun>) {
    let text = e.text();
    let start = text.line_start_offset(line);
    let s = text.slice_line(line).to_string();
    let s = s.trim_end_matches(['\r', '\n']).to_string();
    let run = |len, color, font: Font| TextRun {
        len,
        font,
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let mut runs = Vec::new();
    let mut at = 0;
    for (range, style) in e.syntax_styles(&(start..start + s.len())) {
        let (a, b) = (range.start.saturating_sub(start).max(at), (range.end - start).min(s.len()));
        if a >= b {
            continue;
        }
        if a > at {
            runs.push(run(a - at, fg, font.clone()));
        }
        let mut f = font.clone();
        f.weight = style.font_weight.unwrap_or(f.weight);
        f.style = style.font_style.unwrap_or(f.style);
        runs.push(run(b - a, style.color.unwrap_or(fg), f));
        at = b;
    }
    if at < s.len() {
        runs.push(run(s.len() - at, fg, font.clone()));
    }
    (s.into(), runs)
}

/// The editor's top row and how many rows it shows.
fn rows(e: &EditorState, lh: Pixels) -> (f32, f32) {
    (-e.scroll_offset().y / lh, e.input_bounds().size.height / lh)
}

/// Scrolls the editor so row `top` is at its top.
fn scroll_to(e: &mut EditorState, top: f32, lh: Pixels, cx: &mut Context<EditorState>) {
    let x = e.scroll_offset().x;
    e.set_scroll_offset(point(x, -lh * top.max(0.)), cx);
}

/// Sticky scroll over `editor`: the headers of the scopes it is scrolled
/// into; a click goes to one.
pub(super) fn sticky_scroll(editor: Entity<EditorState>, cx: &App) -> Div {
    let theme = gpui_kit::component::theme::ActiveTheme::theme(cx);
    let paint_editor = editor.clone();
    div()
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .font_family(theme.mono_font_family.clone())
        .text_size(theme.mono_font_size)
        .child(
            canvas(
                move |bounds, window, cx| {
                    let e = editor.read(cx);
                    let lh = e.line_height()?;
                    let map = e.display_map();
                    let b = e.input_bounds();
                    // The row at the top of the view, above the editor's padding.
                    let top = (rows(e, lh).0 + (bounds.top() - b.top()) / lh).max(0.) as usize;
                    // ponytail: scopes, not comment blocks, as VS Code's outline
                    // model has it; a comment is guessed from its first characters.
                    let text = e.text();
                    let folds: Vec<FoldRange> = (map.fold_candidates().iter())
                        .filter(|r| {
                            let line = text.slice_line(r.start_line).to_string();
                            !["//", "/*", "#", "<!--", "--"].iter().any(|c| line.trim_start().starts_with(c))
                        })
                        .copied()
                        .collect();
                    let lines = sticky_lines(&folds, |i| {
                        map.display_row_to_buffer_line(map.visual_row_to_display_row(top + i))
                    });
                    if lines.is_empty() {
                        return None;
                    }
                    // Short of the vertical scrollbar.
                    let area = Bounds::new(point(b.left(), bounds.top()), size(b.size.width - px(14.), lh * lines.len() as f32));
                    Some((lines, area, window.insert_hitbox(area, HitboxBehavior::BlockMouse)))
                },
                move |_, state, window, cx| {
                    let Some((lines, area, hitbox)) = state else { return };
                    let editor = paint_editor;
                    let e = editor.read(cx);
                    let (Some(lh), Some(gutter)) = (e.line_height(), e.gutter_width()) else {
                        return;
                    };
                    let style = e.editor_style();
                    let (fg, muted, bg) = (style.foreground, style.muted_foreground, style.background);
                    let text_style = window.text_style();
                    let font = text_style.font();
                    let font_size = text_style.font_size.to_pixels(window.rem_size());
                    let digits = (e.text().lines_len().max(1).ilog10() as usize + 1).clamp(3, 7);
                    let text_x = area.left() + gutter + e.scroll_offset().x;
                    let shaped: Vec<_> = (lines.iter())
                        .map(|&line| {
                            let num: SharedString = format!("{:>digits$}", line + 1).into();
                            let num_run = vec![TextRun {
                                len: num.len(),
                                font: font.clone(),
                                color: muted,
                                background_color: None,
                                underline: None,
                                strikethrough: None,
                            }];
                            let (text, runs) = line_runs(e, line, &font, fg);
                            let ts = window.text_system();
                            (ts.shape_line(num, font_size, &num_run, None), ts.shape_line(text, font_size, &runs, None))
                        })
                        .collect();
                    window.paint_quad(fill(area, bg));
                    let text_area = Bounds::from_corners(point(area.left() + gutter, area.top()), area.bottom_right());
                    for (i, (num, text)) in shaped.iter().enumerate() {
                        let y = area.top() + lh * i as f32;
                        num.paint(point(area.left(), y), lh, TextAlign::Left, None, window, cx).ok();
                        window.with_content_mask(Some(ContentMask { bounds: text_area }), |window| {
                            text.paint(point(text_x, y), lh, TextAlign::Left, None, window, cx).ok();
                        });
                    }
                    // editorStickyScroll.shadow
                    let shadow = Bounds::new(area.bottom_left(), size(area.size.width, px(3.)));
                    window.paint_quad(fill(
                        shadow,
                        linear_gradient(
                            180.,
                            linear_color_stop(black().opacity(0.36), 0.),
                            linear_color_stop(black().opacity(0.), 1.),
                        ),
                    ));
                    window.set_cursor_style(CursorStyle::PointingHand, &hitbox);
                    window.on_mouse_event(move |ev: &MouseDownEvent, phase, window, cx| {
                        if !phase.bubble() || ev.button != MouseButton::Left || !hitbox.is_hovered(window) {
                            return;
                        }
                        let i = ((ev.position.y - area.top()) / lh) as usize;
                        let Some(&line) = lines.get(i) else { return };
                        cx.stop_propagation();
                        // The line goes under the headers above it.
                        editor.update(cx, |e, cx| {
                            e.set_cursor_position(Position { line: line as u32, character: 0 }, window, cx);
                            let row = e.display_map().buffer_line_to_display_row(line);
                            let row = e.display_map().display_row_to_visual_row(row);
                            scroll_to(e, row as f32 - i as f32, lh, cx);
                            e.focus(window, cx);
                        });
                    });
                },
            )
            .size_full(),
        )
}

/// The minimap beside `editor`: its lines in miniature with a slider over
/// the part in view. A click centers there; dragging moves the slider.
pub(super) fn minimap(editor: Entity<EditorState>, drag: Rc<Cell<Option<(Pixels, f32)>>>) -> impl IntoElement {
    canvas(
        |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
        move |bounds, hitbox, window, cx| {
            let e = editor.read(cx);
            let Some(lh) = e.line_height() else { return };
            // A line's height and a character's width, in pixels.
            let dpr = window.scale_factor();
            let s = minimap_scale(dpr);
            let (row_h, col_w) = (2. * s as f32 / dpr, s as f32 / dpr);
            let map = e.display_map();
            let count = map.visual_row_count();
            let (top, view) = rows(e, lh);
            // How far the minimap scrolls per editor row, as VS Code
            // scrolls it: in step with the editor, end to end.
            let content = count as f32 * row_h;
            let height = bounds.size.height / px(1.);
            let max_top = (count as f32 - 1.).max(1.);
            let shift = (content - height).max(0.) / max_top;
            let offset = top.clamp(0., max_top) * shift;
            let style = e.editor_style();
            let fg = style.foreground;

            window.paint_quad(fill(bounds, style.background));
            let first = (offset / row_h) as usize;
            let last = (first + (height / row_h) as usize + 1).min(count);
            let text = e.text();
            let lines: Vec<(usize, usize)> = (first..last)
                .map(|row| (row, map.visual_row_to_display_row(row)))
                // Not a CodeLens row; a wrapped line shows on its first row.
                .filter(|&(row, d)| map.display_row_to_visual_row(d) == row)
                .map(|(row, d)| (row, d, map.display_row_to_buffer_line(d)))
                .filter(|&(_, d, line)| map.buffer_line_to_display_row(line) == d)
                .map(|(row, _, line)| (row, line))
                .collect();
            if let (Some(&(_, a)), Some(&(_, b))) = (lines.first(), lines.last()) {
                let range = text.line_start_offset(a)..text.line_end_offset(b);
                let styles = e.syntax_styles(&range);
                let mut styles = styles.iter().peekable();
                let max_cols = (bounds.size.width / px(col_w)) as usize;
                // Character shapes in one bitmap of device pixels, as VS
                // Code's minimap renders characters.
                let mut map = Bitmap::new(bounds.size, dpr, s, style.background);
                let shift_px = (offset * dpr).round() as isize;
                for &(row, line) in &lines {
                    let y = (row * 2 * s) as isize - shift_px;
                    let start = text.line_start_offset(line);
                    let mut col = 0;
                    for (i, ch) in text.slice_line(line).to_string().char_indices() {
                        if col >= max_cols {
                            break;
                        }
                        let at = start + i;
                        while styles.peek().is_some_and(|(r, _)| r.end <= at) {
                            styles.next();
                        }
                        if ch.is_whitespace() {
                            col += if ch == '\t' { 4 - col % 4 } else { 1 };
                            continue;
                        }
                        let color = (styles.peek())
                            .filter(|(r, _)| r.start <= at)
                            .and_then(|(_, s)| s.color)
                            .unwrap_or(fg);
                        map.char((col * s) as isize, y, ch, color);
                        col += 1;
                    }
                }
                map.paint(editor.entity_id(), bounds, window);
            }

            // minimapSlider.background, always shown.
            let slider = Bounds::new(
                point(bounds.left(), bounds.top() + px(top * row_h - offset)),
                size(bounds.size.width, px(view * row_h)),
            );
            window.paint_quad(fill(slider, ca(0x79797933)));

            // Editor rows a pixel of slider movement scrolls.
            let per_px = 1. / (row_h - shift).max(0.1);
            let down = (editor.clone(), drag.clone(), hitbox.clone());
            window.on_mouse_event(move |ev: &MouseDownEvent, phase, window, cx| {
                let (editor, drag, hitbox) = &down;
                if !phase.bubble() || ev.button != MouseButton::Left || !hitbox.is_hovered(window) {
                    return;
                }
                cx.stop_propagation();
                let mut top = top;
                // Outside the slider, the clicked line comes to the middle.
                if !slider.contains(&ev.position) {
                    let row = (ev.position.y - bounds.top()) / px(1.) / row_h + offset / row_h;
                    top = row - view / 2.;
                    editor.update(cx, |e, cx| scroll_to(e, top, lh, cx));
                }
                drag.set(Some((ev.position.y, top)));
            });
            let moved = (editor.clone(), drag.clone());
            window.on_mouse_event(move |ev: &MouseMoveEvent, phase, _, cx| {
                let (editor, drag) = &moved;
                let Some((y, top)) = drag.get() else { return };
                if !phase.bubble() {
                    return;
                }
                if ev.pressed_button != Some(MouseButton::Left) {
                    return drag.set(None);
                }
                let top = top + (ev.position.y - y) / px(1.) * per_px;
                editor.update(cx, |e, cx| scroll_to(e, top, lh, cx));
            });
            window.on_mouse_event(move |_: &MouseUpEvent, _, _, _| drag.set(None));
            window.on_mouse_event(move |ev: &ScrollWheelEvent, phase, window, cx| {
                if !phase.bubble() || !hitbox.is_hovered(window) {
                    return;
                }
                let dy = ev.delta.pixel_delta(lh).y;
                editor.update(cx, |e, cx| scroll_to(e, top - dy / lh, lh, cx));
            });
        },
    )
    .size_full()
}

#[cfg(test)]
mod tests {
    use super::{FoldRange, sticky_lines};

    #[test]
    fn sticky() {
        // 0 impl {  1 fn a() {  2..4 body  5 }  6 fn b() {  7 body  8 }  9 }
        let folds = [FoldRange::new(0, 9), FoldRange::new(1, 5), FoldRange::new(6, 8)];
        assert_eq!(sticky_lines(&folds, |i| i), Vec::<usize>::new());
        assert_eq!(sticky_lines(&folds, |i| 2 + i), vec![0, 1]);
        assert_eq!(sticky_lines(&folds, |i| 5 + i), vec![0]);
        assert_eq!(sticky_lines(&folds, |i| 6 + i), vec![0, 6]);
        assert_eq!(sticky_lines(&folds, |i| 9 + i), Vec::<usize>::new());
    }
}
