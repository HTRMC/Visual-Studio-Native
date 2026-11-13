//! Sticky scroll and the minimap: views painted over and beside an editor
//! from its layout.

use std::{cell::Cell, rc::Rc};

use gpui_kit::component::input::{FoldRange, Position};

use super::*;

/// editor.stickyScroll.maxLineCount
const STICKY_MAX: usize = 5;
pub(super) const MINIMAP_WIDTH: f32 = 100.;
/// A line's height and a character's width in the minimap.
const ROW: f32 = 2.;
const COL: f32 = 1.;

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
            let map = e.display_map();
            let count = map.visual_row_count();
            let (top, view) = rows(e, lh);
            // How far the minimap scrolls per editor row, as VS Code
            // scrolls it: in step with the editor, end to end.
            let content = count as f32 * ROW;
            let height = bounds.size.height / px(1.);
            let max_top = (count as f32 - 1.).max(1.);
            let shift = (content - height).max(0.) / max_top;
            let offset = top.clamp(0., max_top) * shift;
            let style = e.editor_style();
            let fg = style.foreground;

            window.paint_quad(fill(bounds, style.background));
            let first = (offset / ROW) as usize;
            let last = (first + (height / ROW) as usize + 1).min(count);
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
                let max_cols = (bounds.size.width / px(COL)) as usize;
                window.with_content_mask(Some(ContentMask { bounds }), |window| {
                    for &(row, line) in &lines {
                        let y = bounds.top() + px(row as f32 * ROW - offset);
                        let start = text.line_start_offset(line);
                        let mut col = 0;
                        let mut run: Option<(usize, usize, Hsla)> = None; // start col, len, color
                        let flush = |run: &mut Option<(usize, usize, Hsla)>, window: &mut Window| {
                            if let Some((c0, n, color)) = run.take() {
                                let at = point(bounds.left() + px(c0 as f32 * COL), y);
                                window.paint_quad(fill(Bounds::new(at, size(px(n as f32 * COL), px(ROW))), color.opacity(0.6)));
                            }
                        };
                        for (i, ch) in text.slice_line(line).to_string().char_indices() {
                            if col >= max_cols {
                                break;
                            }
                            let at = start + i;
                            while styles.peek().is_some_and(|(r, _)| r.end <= at) {
                                styles.next();
                            }
                            if ch.is_whitespace() {
                                flush(&mut run, window);
                                col += if ch == '\t' { 4 - col % 4 } else { 1 };
                                continue;
                            }
                            let color = (styles.peek())
                                .filter(|(r, _)| r.start <= at)
                                .and_then(|(_, s)| s.color)
                                .unwrap_or(fg);
                            match &mut run {
                                Some((_, n, c)) if *c == color => *n += 1,
                                _ => {
                                    flush(&mut run, window);
                                    run = Some((col, 1, color));
                                }
                            }
                            col += 1;
                        }
                        flush(&mut run, window);
                    }
                });
            }

            // minimapSlider.background, always shown.
            let slider = Bounds::new(
                point(bounds.left(), bounds.top() + px(top * ROW - offset)),
                size(bounds.size.width, px(view * ROW)),
            );
            window.paint_quad(fill(slider, ca(0x79797933)));

            // Editor rows a pixel of slider movement scrolls.
            let per_px = 1. / (ROW - shift).max(0.1);
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
                    let row = (ev.position.y - bounds.top()) / px(1.) / ROW + offset / ROW;
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
