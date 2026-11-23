//! The characters VS Code's terminal draws itself instead of taking them from
//! the font (terminal.integrated.customGlyphs): block elements, shades, box
//! drawing and Powerline symbols. They fill their cells edge to edge on whole
//! device pixels, so neighbors join with no gaps.

use gpui_kit::*;

pub(super) fn is_custom(ch: char) -> bool {
    matches!(ch, '\u{2500}'..='\u{259F}' | '\u{E0B0}'..='\u{E0BF}')
}

/// Paints `ch` in `color` over `cell`, in window pixels.
pub(super) fn paint(ch: char, cell: Bounds<Pixels>, color: Hsla, window: &mut Window) {
    let dpr = window.scale_factor();
    let d = |v: Pixels| (f32::from(v) * dpr).round();
    let c = Cell {
        x0: d(cell.left()),
        y0: d(cell.top()),
        x1: d(cell.right()),
        y1: d(cell.bottom()),
        dpr,
        color,
        // A light line: a pixel per 100% of scaling.
        t: dpr.round().max(1.),
    };
    match ch {
        '\u{2580}'..='\u{2590}' | '\u{2594}'..='\u{259F}' => c.block(ch, window),
        '\u{2591}'..='\u{2593}' => c.shade(ch, window),
        '\u{256D}'..='\u{2570}' => c.arc(ch, window),
        '\u{2571}'..='\u{2573}' => c.diagonal(ch, window),
        '\u{E0B0}'..='\u{E0BF}' => c.powerline(ch, window),
        _ => {
            if let Some((n, horizontal, weight)) = dashes(ch) {
                c.dashes(n, horizontal, weight, window);
            } else if let Some(arms) = arms(ch) {
                c.lines(arms, window);
            }
        }
    }
}

/// A line's weight on one side of a box-drawing character.
const NONE: u8 = 0;
const LIGHT: u8 = 1;
const HEAVY: u8 = 2;
const DOUBLE: u8 = 3;

/// U+2500 to U+257F's lines out of the cell's center, up, right, down and
/// left, as their Unicode names give them; '-' marks those drawn otherwise.
const ARMS: [&str; 128] = [
    "0101", "0202", "1010", "2020", "-", "-", "-", "-", "-", "-", "-", "-", "0110", "0210", "0120", "0220",
    "0011", "0012", "0021", "0022", "1100", "1200", "2100", "2200", "1001", "1002", "2001", "2002", "1110", "1210", "2110", "1120",
    "2120", "2210", "1220", "2220", "1011", "1012", "2011", "1021", "2021", "2012", "1022", "2022", "0111", "0112", "0211", "0212",
    "0121", "0122", "0221", "0222", "1101", "1102", "1201", "1202", "2101", "2102", "2201", "2202", "1111", "1112", "1211", "1212",
    "2111", "1121", "2121", "2112", "2211", "1122", "1221", "2212", "1222", "2122", "2221", "2222", "-", "-", "-", "-",
    "0303", "3030", "0310", "0130", "0330", "0013", "0031", "0033", "1300", "3100", "3300", "1003", "3001", "3003", "1310", "3130",
    "3330", "1013", "3031", "3033", "0313", "0131", "0333", "1303", "3101", "3303", "1313", "3131", "3333", "-", "-", "-",
    "-", "-", "-", "-", "0001", "1000", "0100", "0010", "0002", "2000", "0200", "0020", "0201", "1020", "0102", "2010",
];

fn arms(ch: char) -> Option<[u8; 4]> {
    let code = ARMS.get((ch as u32).checked_sub(0x2500)? as usize)?.as_bytes();
    (code.len() == 4).then(|| [0, 1, 2, 3].map(|i| code[i] - b'0'))
}

/// A dashed line's dash count, direction and weight.
fn dashes(ch: char) -> Option<(usize, bool, u8)> {
    Some(match ch {
        '\u{2504}' => (3, true, LIGHT),
        '\u{2505}' => (3, true, HEAVY),
        '\u{2506}' => (3, false, LIGHT),
        '\u{2507}' => (3, false, HEAVY),
        '\u{2508}' => (4, true, LIGHT),
        '\u{2509}' => (4, true, HEAVY),
        '\u{250A}' => (4, false, LIGHT),
        '\u{250B}' => (4, false, HEAVY),
        '\u{254C}' => (2, true, LIGHT),
        '\u{254D}' => (2, true, HEAVY),
        '\u{254E}' => (2, false, LIGHT),
        '\u{254F}' => (2, false, HEAVY),
        _ => return None,
    })
}

/// A cell in device pixels.
struct Cell {
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    dpr: f32,
    color: Hsla,
    t: f32,
}

impl Cell {
    fn w(&self) -> f32 {
        self.x1 - self.x0
    }

    fn h(&self) -> f32 {
        self.y1 - self.y0
    }

    /// The center, on a whole pixel.
    fn center(&self) -> (f32, f32) {
        (self.x0 + (self.w() / 2.).floor(), self.y0 + (self.h() / 2.).floor())
    }

    fn at(&self, x: f32, y: f32) -> Point<Pixels> {
        point(px(x / self.dpr), px(y / self.dpr))
    }

    /// Fills a rectangle of device pixels, its edges rounded to whole ones.
    fn rect(&self, l: f32, t: f32, r: f32, b: f32, window: &mut Window) {
        let (l, t, r, b) = (l.round(), t.round(), r.round(), b.round());
        if r > l && b > t {
            window.paint_quad(fill(Bounds::from_corners(self.at(l, t), self.at(r, b)), self.color));
        }
    }

    /// Fills a part of the cell given in fractions of it.
    fn part(&self, l: f32, t: f32, r: f32, b: f32, window: &mut Window) {
        let (w, h) = (self.w(), self.h());
        self.rect(self.x0 + l * w, self.y0 + t * h, self.x0 + r * w, self.y0 + b * h, window);
    }

    /// U+2580 to U+2590 and U+2594 to U+259F.
    fn block(&self, ch: char, window: &mut Window) {
        let q = |w: &mut Window, l, t| self.part(l, t, l + 0.5, t + 0.5, w);
        let (ul, ur, ll, lr) = ((0., 0.), (0.5, 0.), (0., 0.5), (0.5, 0.5));
        let quads: &[(f32, f32)] = match ch {
            '\u{2580}' => return self.part(0., 0., 1., 0.5, window),
            '\u{2581}'..='\u{2588}' => {
                let n = (ch as u32 - 0x2580) as f32 / 8.;
                return self.part(0., 1. - n, 1., 1., window);
            }
            '\u{2589}'..='\u{258F}' => {
                let n = (0x2590 - ch as u32) as f32 / 8.;
                return self.part(0., 0., n, 1., window);
            }
            '\u{2590}' => return self.part(0.5, 0., 1., 1., window),
            '\u{2594}' => return self.part(0., 0., 1., 1. / 8., window),
            '\u{2595}' => return self.part(7. / 8., 0., 1., 1., window),
            '\u{2596}' => &[ll],
            '\u{2597}' => &[lr],
            '\u{2598}' => &[ul],
            '\u{2599}' => &[ul, ll, lr],
            '\u{259A}' => &[ul, lr],
            '\u{259B}' => &[ul, ur, ll],
            '\u{259C}' => &[ul, ur, lr],
            '\u{259D}' => &[ur],
            '\u{259E}' => &[ur, ll],
            _ => &[ur, ll, lr],
        };
        for &(l, t) in quads {
            q(window, l, t);
        }
    }

    /// ░ ▒ ▓: VS Code's dot patterns, tiled on the screen's pixels so the
    /// shading runs on unbroken from cell to cell.
    fn shade(&self, ch: char, window: &mut Window) {
        const LIGHT: [[u8; 4]; 4] = [[1, 0, 0, 0], [0, 0, 0, 0], [0, 0, 1, 0], [0, 0, 0, 0]];
        let on = |x: i64, y: i64| {
            let light = LIGHT[y.rem_euclid(4) as usize][x.rem_euclid(4) as usize] == 1;
            match ch {
                '\u{2591}' => light,
                '\u{2592}' => (x + y).rem_euclid(2) == 0,
                _ => !light,
            }
        };
        for y in self.y0 as i64..self.y1 as i64 {
            // Runs of lit pixels, one rectangle each.
            let mut x = self.x0 as i64;
            while x < self.x1 as i64 {
                if !on(x, y) {
                    x += 1;
                    continue;
                }
                let start = x;
                while x < self.x1 as i64 && on(x, y) {
                    x += 1;
                }
                self.rect(start as f32, y as f32, x as f32, y as f32 + 1., window);
            }
        }
    }

    /// A line's thickness at a weight; a double line's two are each light.
    fn thickness(&self, weight: u8) -> f32 {
        match weight {
            HEAVY => 2. * self.t,
            _ => self.t,
        }
    }

    /// How far a line through the center at `weight` reaches to the
    /// negative and positive side of it.
    fn reach(&self, weight: u8) -> (f32, f32) {
        let t = self.thickness(weight);
        let gap = if weight == DOUBLE { self.t } else { 0. };
        match weight {
            NONE => (0., 0.),
            _ => (gap + (t / 2.).floor(), gap + t - (t / 2.).floor()),
        }
    }

    /// Box drawing: lines from the center to the edges, joined as VS Code
    /// joins them, double lines' strokes meeting in corners.
    fn lines(&self, arms: [u8; 4], window: &mut Window) {
        let (cx, cy) = self.center();
        let [up, right, down, left] = arms;
        // Each arm: its weight, whether it runs along x, whether it heads to
        // larger coordinates, and the arms across it on the smaller and
        // larger side.
        let all = [
            (up, false, false, left, right),
            (right, true, true, up, down),
            (down, false, true, left, right),
            (left, true, false, up, down),
        ];
        for (weight, along_x, forward, neg, pos) in all {
            if weight == NONE {
                continue;
            }
            let (c, cp, edge) = match (along_x, forward) {
                (true, true) => (cx, cy, self.x1),
                (true, false) => (cx, cy, self.x0),
                (false, true) => (cy, cx, self.y1),
                (false, false) => (cy, cx, self.y0),
            };
            // Where a stroke starts: just covering the lines across it.
            let start = |reach_back: f32| if forward { c - reach_back } else { c + reach_back };
            let across = |reach: (f32, f32)| if forward { reach.0 } else { reach.1 };
            let mut strokes: Vec<(f32, f32, f32)> = Vec::new(); // perp center, thickness, start
            if weight == DOUBLE {
                let t = self.t;
                for (side, same, other) in [(-1., neg, pos), (1., pos, neg)] {
                    let from = if same != NONE {
                        // Stops at the near line of the arm on its side.
                        let offset = if same == DOUBLE { self.t } else { 0. };
                        let st = self.thickness(same);
                        if forward { c + offset - (st / 2.).floor() } else { c - offset + st - (st / 2.).floor() }
                    } else if other != NONE {
                        // Runs on to the far side of the arm across.
                        start(across(self.reach(other)))
                    } else {
                        c
                    };
                    strokes.push((cp + side * self.t, t, from));
                }
            } else {
                let back = across(self.reach(neg)).max(across(self.reach(pos)));
                strokes.push((cp, self.thickness(weight), start(back)));
            }
            for (center, t, from) in strokes {
                let (p0, p1) = (center - (t / 2.).floor(), center - (t / 2.).floor() + t);
                let (a0, a1) = if from < edge { (from, edge) } else { (edge, from) };
                if along_x {
                    self.rect(a0, p0, a1, p1, window);
                } else {
                    self.rect(p0, a0, p1, a1, window);
                }
            }
        }
    }

    /// ┄ ┆ ┈ ┊ ╌ ╎ and their heavy forms.
    fn dashes(&self, n: usize, horizontal: bool, weight: u8, window: &mut Window) {
        let (cx, cy) = self.center();
        let t = self.thickness(weight);
        let (p0, p1) = if horizontal { (cy, cy) } else { (cx, cx) };
        let (p0, p1) = (p0 - (t / 2.).floor(), p1 - (t / 2.).floor() + t);
        let (from, len) = if horizontal { (self.x0, self.w()) } else { (self.y0, self.h()) };
        let step = len / n as f32;
        let gap = (step * 0.4).max(1.);
        for i in 0..n {
            let a = from + i as f32 * step + gap / 2.;
            let b = from + (i + 1) as f32 * step - gap / 2.;
            if horizontal {
                self.rect(a, p0, b, p1, window);
            } else {
                self.rect(p0, a, p1, b, window);
            }
        }
    }

    /// The center of a light line through the cell's center.
    fn line_center(&self) -> (f32, f32) {
        let (cx, cy) = self.center();
        let half = self.t / 2. - (self.t / 2.).floor();
        (cx + half, cy + half)
    }

    fn stroke(&self, build: impl FnOnce(&mut PathBuilder), window: &mut Window) {
        let mut path = PathBuilder::stroke(px(self.t / self.dpr));
        build(&mut path);
        if let Ok(path) = path.build() {
            window.paint_path(path, self.color);
        }
    }

    fn filled(&self, points: &[(f32, f32)], window: &mut Window) {
        let mut path = PathBuilder::fill();
        let (w, h) = (self.w(), self.h());
        for (i, &(x, y)) in points.iter().enumerate() {
            let p = self.at(self.x0 + x * w, self.y0 + y * h);
            if i == 0 { path.move_to(p) } else { path.line_to(p) }
        }
        path.close();
        if let Ok(path) = path.build() {
            window.paint_path(path, self.color);
        }
    }

    /// ╭ ╮ ╯ ╰: a light line rounding the corner.
    fn arc(&self, ch: char, window: &mut Window) {
        let (cx, cy) = self.line_center();
        let r = (self.w() / 2.).min(self.h() / 2.);
        let (sx, sy, sweep) = match ch {
            '\u{256D}' => (1., 1., false),
            '\u{256E}' => (-1., 1., true),
            '\u{256F}' => (-1., -1., false),
            _ => (1., -1., true),
        };
        let edge_x = if sx > 0. { self.x1 } else { self.x0 };
        let edge_y = if sy > 0. { self.y1 } else { self.y0 };
        let radii = point(px(r / self.dpr), px(r / self.dpr));
        self.stroke(
            |p| {
                p.move_to(self.at(edge_x, cy));
                p.line_to(self.at(cx + sx * r, cy));
                p.arc_to(radii, px(0.), false, sweep, self.at(cx, cy + sy * r));
                p.line_to(self.at(cx, edge_y));
            },
            window,
        );
    }

    /// ╱ ╲ ╳, corner to corner.
    fn diagonal(&self, ch: char, window: &mut Window) {
        window.with_content_mask(Some(self.mask()), |window| {
            if ch != '\u{2572}' {
                self.stroke(|p| {
                    p.move_to(self.at(self.x1, self.y0));
                    p.line_to(self.at(self.x0, self.y1));
                }, window);
            }
            if ch != '\u{2571}' {
                self.stroke(|p| {
                    p.move_to(self.at(self.x0, self.y0));
                    p.line_to(self.at(self.x1, self.y1));
                }, window);
            }
        });
    }

    fn mask(&self) -> ContentMask<Pixels> {
        ContentMask { bounds: Bounds::from_corners(self.at(self.x0, self.y0), self.at(self.x1, self.y1)) }
    }

    /// Powerline's arrows, semicircles and triangles, U+E0B0 to U+E0BF.
    fn powerline(&self, ch: char, window: &mut Window) {
        let (x0, y0, x1, y1) = (self.x0, self.y0, self.x1, self.y1);
        let cy = (y0 + y1) / 2.;
        let line = |w: &mut Window, pts: &[(f32, f32)]| {
            self.stroke(
                |p| {
                    for (i, &(x, y)) in pts.iter().enumerate() {
                        let at = self.at(x, y);
                        if i == 0 { p.move_to(at) } else { p.line_to(at) }
                    }
                },
                w,
            )
        };
        window.with_content_mask(Some(self.mask()), |window| match ch {
            '\u{E0B0}' => self.filled(&[(0., 0.), (1., 0.5), (0., 1.)], window),
            '\u{E0B1}' => line(window, &[(x0, y0), (x1, cy), (x0, y1)]),
            '\u{E0B2}' => self.filled(&[(1., 0.), (0., 0.5), (1., 1.)], window),
            '\u{E0B3}' => line(window, &[(x1, y0), (x0, cy), (x1, y1)]),
            '\u{E0B4}'..='\u{E0B7}' => {
                let right = ch <= '\u{E0B5}';
                let x = if right { x0 } else { x1 };
                let radii = point(px(self.w() / self.dpr), px((y1 - y0) / 2. / self.dpr));
                let shape = |p: &mut PathBuilder| {
                    p.move_to(self.at(x, y0));
                    p.arc_to(radii, px(0.), false, right, self.at(x, y1));
                };
                if matches!(ch, '\u{E0B4}' | '\u{E0B6}') {
                    let mut p = PathBuilder::fill();
                    shape(&mut p);
                    p.close();
                    if let Ok(p) = p.build() {
                        window.paint_path(p, self.color);
                    }
                } else {
                    self.stroke(shape, window);
                }
            }
            '\u{E0B8}' => self.filled(&[(0., 0.), (0., 1.), (1., 1.)], window),
            '\u{E0B9}' => line(window, &[(x0, y0), (x1, y1)]),
            '\u{E0BA}' => self.filled(&[(1., 0.), (1., 1.), (0., 1.)], window),
            '\u{E0BB}' => line(window, &[(x1, y0), (x0, y1)]),
            '\u{E0BC}' => self.filled(&[(0., 0.), (1., 0.), (0., 1.)], window),
            '\u{E0BD}' => line(window, &[(x1, y0), (x0, y1)]),
            '\u{E0BE}' => self.filled(&[(0., 0.), (1., 0.), (1., 1.)], window),
            _ => line(window, &[(x0, y0), (x1, y1)]),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::{DOUBLE, HEAVY, LIGHT, arms, dashes, is_custom};

    #[test]
    fn box_drawing_table_matches_unicode_names() {
        assert_eq!(arms('─'), Some([0, LIGHT, 0, LIGHT]));
        assert_eq!(arms('┌'), Some([0, LIGHT, LIGHT, 0]));
        assert_eq!(arms('╋'), Some([HEAVY; 4]));
        assert_eq!(arms('┧'), Some([LIGHT, 0, HEAVY, LIGHT]));
        assert_eq!(arms('╔'), Some([0, DOUBLE, DOUBLE, 0]));
        assert_eq!(arms('╬'), Some([DOUBLE; 4]));
        assert_eq!(arms('╿'), Some([HEAVY, 0, LIGHT, 0]));
        assert_eq!(arms('┄'), None);
        assert_eq!(dashes('┄'), Some((3, true, LIGHT)));
        assert!(is_custom('\u{E0B0}') && is_custom('▓') && !is_custom('a'));
    }
}
