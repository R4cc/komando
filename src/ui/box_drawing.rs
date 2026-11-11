//! Box-drawing lines, block elements and Powerline separators, drawn from geometry instead
//! of the font.
//!
//! Font glyphs for these are designed for the font's own line height. Komando's rows are
//! taller than that, so borders drawn by tmux, vim splits, htop, mc or lazygit would break
//! into dashes between rows, and fallback fonts may not line up with the grid at all. Drawn
//! here, every piece fills its cell exactly and joins its neighbours. All edges are snapped
//! to device pixels so lines stay crisp at any display scale.

/// A cell on screen in logical pixels, and the display's scale factor.
#[derive(Debug, Clone, Copy)]
pub struct Cell {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub scale: f32,
}

/// One drawing primitive, in logical pixels, painted in the cell's text colour.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Prim {
    /// A filled rectangle at `alpha` times the text colour's opacity (shades use < 1).
    Rect { x: f32, y: f32, w: f32, h: f32, alpha: f32 },
    /// A filled triangle.
    Triangle([(f32, f32); 3]),
    /// An open stroke `a → b → c`.
    Chevron { points: [(f32, f32); 3], width: f32 },
    /// A rounded corner: a line from `from` to `start`, a quarter circle to `end` bending
    /// toward `corner`, and a line on to `to`.
    Arc { from: (f32, f32), start: (f32, f32), corner: (f32, f32), end: (f32, f32), to: (f32, f32), width: f32 },
}

/// Whether `c` is drawn here rather than from the font.
pub fn handles(c: char) -> bool {
    lines(c).is_some() || matches!(c, '\u{256d}'..='\u{2570}' | '\u{2580}'..='\u{259f}' | '\u{e0b0}'..='\u{e0b3}')
}

/// Append the primitives for `c` in `cell`. Returns false (and draws nothing) for
/// characters left to the font.
pub fn draw(c: char, cell: Cell, out: &mut Vec<Prim>) -> bool {
    let g = Grid::new(cell);
    if let Some(arms) = lines(c) {
        g.lines(arms, out);
        return true;
    }
    match c {
        '\u{256d}'..='\u{2570}' => g.rounded(c, out),
        '\u{2580}'..='\u{259f}' => g.block(c, out),
        '\u{e0b0}'..='\u{e0b3}' => g.powerline(c, out),
        _ => return false,
    }
    true
}

const NONE: u8 = 0;
const LIGHT: u8 = 1;
const HEAVY: u8 = 2;
const DOUBLE: u8 = 3;

/// The arms of a line character as (up, right, down, left) weights. Dashed lines and
/// diagonals are left to the font.
fn lines(c: char) -> Option<[u8; 4]> {
    const L: u8 = LIGHT;
    const H: u8 = HEAVY;
    const D: u8 = DOUBLE;
    const N: u8 = NONE;
    #[rustfmt::skip]
    const SOLID: [[u8; 4]; 0x4c] = [
        [N,L,N,L], [N,H,N,H], [L,N,L,N], [H,N,H,N], // ─ ━ │ ┃
        [N;4], [N;4], [N;4], [N;4], [N;4], [N;4], [N;4], [N;4], // dashed ┄┅┆┇┈┉┊┋
        [N,L,L,N], [N,H,L,N], [N,L,H,N], [N,H,H,N], // ┌ ┍ ┎ ┏
        [N,N,L,L], [N,N,L,H], [N,N,H,L], [N,N,H,H], // ┐ ┑ ┒ ┓
        [L,L,N,N], [L,H,N,N], [H,L,N,N], [H,H,N,N], // └ ┕ ┖ ┗
        [L,N,N,L], [L,N,N,H], [H,N,N,L], [H,N,N,H], // ┘ ┙ ┚ ┛
        [L,L,L,N], [L,H,L,N], [H,L,L,N], [L,L,H,N], // ├ ┝ ┞ ┟
        [H,L,H,N], [H,H,L,N], [L,H,H,N], [H,H,H,N], // ┠ ┡ ┢ ┣
        [L,N,L,L], [L,N,L,H], [H,N,L,L], [L,N,H,L], // ┤ ┥ ┦ ┧
        [H,N,H,L], [H,N,L,H], [L,N,H,H], [H,N,H,H], // ┨ ┩ ┪ ┫
        [N,L,L,L], [N,L,L,H], [N,H,L,L], [N,H,L,H], // ┬ ┭ ┮ ┯
        [N,L,H,L], [N,L,H,H], [N,H,H,L], [N,H,H,H], // ┰ ┱ ┲ ┳
        [L,L,N,L], [L,L,N,H], [L,H,N,L], [L,H,N,H], // ┴ ┵ ┶ ┷
        [H,L,N,L], [H,L,N,H], [H,H,N,L], [H,H,N,H], // ┸ ┹ ┺ ┻
        [L,L,L,L], [L,L,L,H], [L,H,L,L], [L,H,L,H], // ┼ ┽ ┾ ┿
        [H,L,L,L], [L,L,H,L], [H,L,H,L], [H,L,L,H], // ╀ ╁ ╂ ╃
        [H,H,L,L], [L,L,H,H], [L,H,H,L], [H,H,L,H], // ╄ ╅ ╆ ╇
        [L,H,H,H], [H,L,H,H], [H,H,H,L], [H,H,H,H], // ╈ ╉ ╊ ╋
    ];
    #[rustfmt::skip]
    const DOUBLES: [[u8; 4]; 0x1d] = [
        [N,D,N,D], [D,N,D,N],                       // ═ ║
        [N,D,L,N], [N,L,D,N], [N,D,D,N],            // ╒ ╓ ╔
        [N,N,L,D], [N,N,D,L], [N,N,D,D],            // ╕ ╖ ╗
        [L,D,N,N], [D,L,N,N], [D,D,N,N],            // ╘ ╙ ╚
        [L,N,N,D], [D,N,N,L], [D,N,N,D],            // ╛ ╜ ╝
        [L,D,L,N], [D,L,D,N], [D,D,D,N],            // ╞ ╟ ╠
        [L,N,L,D], [D,N,D,L], [D,N,D,D],            // ╡ ╢ ╣
        [N,D,L,D], [N,L,D,L], [N,D,D,D],            // ╤ ╥ ╦
        [L,D,N,D], [D,L,N,L], [D,D,N,D],            // ╧ ╨ ╩
        [L,D,L,D], [D,L,D,L], [D,D,D,D],            // ╪ ╫ ╬
    ];
    #[rustfmt::skip]
    const HALVES: [[u8; 4]; 12] = [
        [N,N,N,L], [L,N,N,N], [N,L,N,N], [N,N,L,N], // ╴ ╵ ╶ ╷
        [N,N,N,H], [H,N,N,N], [N,H,N,N], [N,N,H,N], // ╸ ╹ ╺ ╻
        [N,H,N,L], [L,N,H,N], [N,L,N,H], [H,N,L,N], // ╼ ╽ ╾ ╿
    ];
    let arms = match c as u32 {
        i @ 0x2500..=0x254b => SOLID[(i - 0x2500) as usize],
        i @ 0x2550..=0x256c => DOUBLES[(i - 0x2550) as usize],
        i @ 0x2574..=0x257f => HALVES[(i - 0x2574) as usize],
        _ => return None,
    };
    (arms != [NONE; 4]).then_some(arms)
}

/// A cell in device pixels, where all the geometry is worked out.
struct Grid {
    scale: f32,
    x0: i32,
    x1: i32,
    y0: i32,
    y1: i32,
    cx: i32,
    cy: i32,
    /// A light line's thickness.
    t: i32,
    /// Distance from the centre to each line of a double line.
    dd: i32,
}

impl Grid {
    fn new(cell: Cell) -> Self {
        let s = cell.scale.max(0.5);
        let px = |v: f32| (v * s).round() as i32;
        let (x0, x1, y0, y1) = (px(cell.x), px(cell.x + cell.w), px(cell.y), px(cell.y + cell.h));
        let t = ((cell.w * s / 8.).round() as i32).max(1);
        Self { scale: s, x0, x1, y0, y1, cx: px(cell.x + cell.w / 2.), cy: px(cell.y + cell.h / 2.), t, dd: t + (t + 1) / 2 }
    }

    fn logical(&self, v: i32) -> f32 {
        v as f32 / self.scale
    }

    fn rect(&self, x0: i32, y0: i32, x1: i32, y1: i32, alpha: f32, out: &mut Vec<Prim>) {
        if x1 > x0 && y1 > y0 {
            out.push(Prim::Rect {
                x: self.logical(x0),
                y: self.logical(y0),
                w: self.logical(x1 - x0),
                h: self.logical(y1 - y0),
                alpha,
            });
        }
    }

    fn thickness(&self, weight: u8) -> i32 {
        match weight {
            HEAVY => self.t * 2,
            NONE => 0,
            _ => self.t,
        }
    }

    /// The span `[start, end)` a stroke of `thickness` centred on `center` covers.
    fn span(center: i32, thickness: i32) -> (i32, i32) {
        let start = center - thickness / 2;
        (start, start + thickness)
    }

    fn lines(&self, [up, right, down, left]: [u8; 4], out: &mut Vec<Prim>) {
        let (cx, cy, t, dd) = (self.cx, self.cy, self.t, self.dd);
        let h_double = left == DOUBLE || right == DOUBLE;
        let v_double = up == DOUBLE || down == DOUBLE;
        // How far the horizontal strokes reach above and below the centre, and the vertical
        // ones left and right of it, so arms meeting there overlap instead of leaving notches.
        let (h_lo, h_hi) = if h_double {
            (cy - dd - t / 2, cy + dd - t / 2 + t)
        } else {
            Self::span(cy, self.thickness(left).max(self.thickness(right)))
        };
        let (v_lo, v_hi) = if v_double {
            (cx - dd - t / 2, cx + dd - t / 2 + t)
        } else {
            Self::span(cx, self.thickness(up).max(self.thickness(down)))
        };
        // Near edges of the two strokes of a double line: (first line's far edge, second
        // line's near edge) around the centre.
        let first_end = |c: i32| c - dd - t / 2 + t;
        let second_start = |c: i32| c + dd - t / 2;
        let outer_start = |c: i32| c - dd - t / 2;
        let inner_end = |c: i32| c + dd - t / 2 + t;
        // A single arm meeting a double line that runs straight through stops at that line.
        let h_through = left == DOUBLE && right == DOUBLE;
        let v_through = up == DOUBLE && down == DOUBLE;

        let vline = |x: i32, w: i32, y0: i32, y1: i32, out: &mut Vec<Prim>| {
            let (a, b) = Self::span(x, w);
            self.rect(a, y0, b, y1, 1., out);
        };
        let hline = |y: i32, h: i32, x0: i32, x1: i32, out: &mut Vec<Prim>| {
            let (a, b) = Self::span(y, h);
            self.rect(x0, a, x1, b, 1., out);
        };

        match up {
            NONE => {}
            DOUBLE => {
                let (l_end, r_end) = if h_double {
                    (
                        if left != NONE { first_end(cy) } else { inner_end(cy) },
                        if right != NONE { first_end(cy) } else { inner_end(cy) },
                    )
                } else {
                    (h_hi, h_hi)
                };
                vline(cx - dd, t, self.y0, l_end, out);
                vline(cx + dd, t, self.y0, r_end, out);
            }
            w => {
                let end = if h_through && down == NONE { first_end(cy) } else { h_hi };
                vline(cx, self.thickness(w), self.y0, end, out);
            }
        }
        match down {
            NONE => {}
            DOUBLE => {
                let (l_start, r_start) = if h_double {
                    (
                        if left != NONE { second_start(cy) } else { outer_start(cy) },
                        if right != NONE { second_start(cy) } else { outer_start(cy) },
                    )
                } else {
                    (h_lo, h_lo)
                };
                vline(cx - dd, t, l_start, self.y1, out);
                vline(cx + dd, t, r_start, self.y1, out);
            }
            w => {
                let start = if h_through && up == NONE { second_start(cy) } else { h_lo };
                vline(cx, self.thickness(w), start, self.y1, out);
            }
        }
        match left {
            NONE => {}
            DOUBLE => {
                let (t_end, b_end) = if v_double {
                    (
                        if up != NONE { first_end(cx) } else { inner_end(cx) },
                        if down != NONE { first_end(cx) } else { inner_end(cx) },
                    )
                } else {
                    (v_hi, v_hi)
                };
                hline(cy - dd, t, self.x0, t_end, out);
                hline(cy + dd, t, self.x0, b_end, out);
            }
            w => {
                let end = if v_through && right == NONE { first_end(cx) } else { v_hi };
                hline(cy, self.thickness(w), self.x0, end, out);
            }
        }
        match right {
            NONE => {}
            DOUBLE => {
                let (t_start, b_start) = if v_double {
                    (
                        if up != NONE { second_start(cx) } else { outer_start(cx) },
                        if down != NONE { second_start(cx) } else { outer_start(cx) },
                    )
                } else {
                    (v_lo, v_lo)
                };
                hline(cy - dd, t, t_start, self.x1, out);
                hline(cy + dd, t, b_start, self.x1, out);
            }
            w => {
                let start = if v_through && left == NONE { second_start(cx) } else { v_lo };
                hline(cy, self.thickness(w), start, self.x1, out);
            }
        }
    }

    /// ╭ ╮ ╯ ╰: light lines from two edges, joined by a quarter circle.
    fn rounded(&self, c: char, out: &mut Vec<Prim>) {
        let (cx, cy) = (self.cx, self.cy);
        // Centre of the stroke, which `span` puts half a pixel off for odd widths.
        let (sx0, sx1) = Self::span(cx, self.t);
        let (sy0, sy1) = Self::span(cy, self.t);
        let mid_x = (sx0 + sx1) as f32 / 2. / self.scale;
        let mid_y = (sy0 + sy1) as f32 / 2. / self.scale;
        let r = ((self.x1 - self.x0).min(self.y1 - self.y0) as f32 / 2. / self.scale).max(1.);
        let (dx, dy) = match c {
            '╭' => (1., 1.),
            '╮' => (-1., 1.),
            '╯' => (-1., -1.),
            _ => (1., -1.), // ╰
        };
        let edge_x = self.logical(if dx > 0. { self.x1 } else { self.x0 });
        let edge_y = self.logical(if dy > 0. { self.y1 } else { self.y0 });
        out.push(Prim::Arc {
            from: (mid_x, edge_y),
            start: (mid_x, mid_y + dy * r),
            corner: (mid_x, mid_y),
            end: (mid_x + dx * r, mid_y),
            to: (edge_x, mid_y),
            width: self.t as f32 / self.scale,
        });
    }

    fn block(&self, c: char, out: &mut Vec<Prim>) {
        let (x0, x1, y0, y1) = (self.x0, self.x1, self.y0, self.y1);
        let (w, h) = (x1 - x0, y1 - y0);
        let part = |total: i32, eighths: i32| (total as f32 * eighths as f32 / 8.).round() as i32;
        let (mx, my) = (x0 + part(w, 4), y0 + part(h, 4));
        let fill = |a: i32, b: i32, c: i32, d: i32, out: &mut Vec<Prim>| self.rect(a, b, c, d, 1., out);
        match c as u32 {
            0x2580 => fill(x0, y0, x1, my, out),
            // ▁ ▂ ▃ ▄ ▅ ▆ ▇ █: lower eighths.
            i @ 0x2581..=0x2588 => fill(x0, y1 - part(h, (i - 0x2580) as i32), x1, y1, out),
            // ▉ ▊ ▋ ▌ ▍ ▎ ▏: left eighths, 7/8 down to 1/8.
            i @ 0x2589..=0x258f => fill(x0, y0, x0 + part(w, 8 - (i - 0x2588) as i32), y1, out),
            0x2590 => fill(mx, y0, x1, y1, out),
            // ░ ▒ ▓: shades, as a translucent fill.
            i @ 0x2591..=0x2593 => self.rect(x0, y0, x1, y1, (i - 0x2590) as f32 * 0.25, out),
            0x2594 => fill(x0, y0, x1, y0 + part(h, 1), out),
            0x2595 => fill(x1 - part(w, 1), y0, x1, y1, out),
            i @ 0x2596..=0x259f => {
                // Quadrants as bits: upper left, upper right, lower left, lower right.
                const UL: u8 = 1;
                const UR: u8 = 2;
                const LL: u8 = 4;
                const LR: u8 = 8;
                const QUADS: [u8; 10] =
                    [LL, LR, UL, UL | LL | LR, UL | LR, UL | UR | LL, UL | UR | LR, UR, UR | LL, UR | LL | LR];
                let q = QUADS[(i - 0x2596) as usize];
                for (bit, (a, b, c, d)) in
                    [(UL, (x0, y0, mx, my)), (UR, (mx, y0, x1, my)), (LL, (x0, my, mx, y1)), (LR, (mx, my, x1, y1))]
                {
                    if q & bit != 0 {
                        fill(a, b, c, d, out);
                    }
                }
            }
            _ => {}
        }
    }

    /// Powerline separators:   (solid) and   (thin).
    fn powerline(&self, c: char, out: &mut Vec<Prim>) {
        let (l, r, t, b) = (self.logical(self.x0), self.logical(self.x1), self.logical(self.y0), self.logical(self.y1));
        let m = (t + b) / 2.;
        match c {
            '\u{e0b0}' => out.push(Prim::Triangle([(l, t), (r, m), (l, b)])),
            '\u{e0b2}' => out.push(Prim::Triangle([(r, t), (l, m), (r, b)])),
            '\u{e0b1}' => out.push(Prim::Chevron { points: [(l, t), (r, m), (l, b)], width: self.t as f32 / self.scale }),
            _ => out.push(Prim::Chevron { points: [(r, t), (l, m), (r, b)], width: self.t as f32 / self.scale }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 10×20 cell at the origin, scale 1: light lines are 1px, centre (5, 10).
    const CELL: Cell = Cell { x: 0., y: 0., w: 10., h: 20., scale: 1. };

    fn rects(c: char) -> Vec<(f32, f32, f32, f32)> {
        let mut out = vec![];
        assert!(draw(c, CELL, &mut out), "{c} should be drawn");
        out.into_iter()
            .map(|p| match p {
                Prim::Rect { x, y, w, h, .. } => (x, y, x + w, y + h),
                other => panic!("unexpected {other:?}"),
            })
            .collect()
    }

    /// Which pixels of the cell are painted, as rows of '#' and '.'.
    fn bitmap(c: char) -> Vec<String> {
        let mut rows = vec![vec!['.'; 10]; 20];
        for (x0, y0, x1, y1) in rects(c) {
            for row in rows.iter_mut().take(y1 as usize).skip(y0 as usize) {
                for px in row.iter_mut().take(x1 as usize).skip(x0 as usize) {
                    *px = '#';
                }
            }
        }
        rows.into_iter().map(|r| r.into_iter().collect()).collect()
    }

    #[test]
    fn straight_lines_span_the_whole_cell() {
        assert_eq!(rects('─'), vec![(0., 10., 5., 11.), (5., 10., 10., 11.)]);
        assert_eq!(rects('│'), vec![(5., 0., 6., 10.), (5., 10., 6., 20.)]);
        // Heavy is twice as thick.
        assert_eq!(rects('━'), vec![(0., 9., 5., 11.), (5., 9., 10., 11.)]);
    }

    #[test]
    fn corners_and_crossings_join_without_gaps() {
        let corner = bitmap('┌');
        assert_eq!(&corner[9], "..........");
        assert_eq!(&corner[10], ".....#####");
        assert_eq!(&corner[19], ".....#....");
        let cross = bitmap('┼');
        assert_eq!(&cross[10], "##########");
        assert!(cross.iter().all(|row| row.as_bytes()[5] == b'#'));
    }

    #[test]
    fn double_lines_have_clean_inner_and_outer_corners() {
        // ╔: two strokes 2px either side of the centre; the outer one turns at (3, 8), the
        // inner one at (7, 12).
        let b = bitmap('╔');
        assert_eq!(&b[7], "..........");
        assert_eq!(&b[8], "...#######");
        assert_eq!(&b[9], "...#......");
        assert_eq!(&b[12], "...#...###");
        assert_eq!(&b[19], "...#...#..");
        // ╬: four separate corners, nothing in the middle.
        let x = bitmap('╬');
        assert_eq!(&x[7], "...#...#..");
        assert_eq!(&x[8], "####...###");
        assert_eq!(&x[10], "..........");
        assert_eq!(&x[12], "####...###");
        // ╤: the single stem starts at the lower line instead of poking between the two.
        let t = bitmap('╤');
        assert_eq!(&t[10], "..........");
        assert_eq!(&t[13], ".....#....");
    }

    #[test]
    fn mixed_single_and_double_lines_meet() {
        // ╟: the single arm starts at the right-hand stroke.
        let b = bitmap('╟');
        assert_eq!(&b[10], "...#...###");
        // ╓: no line above, so the single arm reaches the left-hand stroke.
        let b = bitmap('╓');
        assert_eq!(&b[10], "...#######");
        assert_eq!(&b[9], "..........");
    }

    #[test]
    fn blocks_fill_their_share_of_the_cell() {
        assert_eq!(rects('█'), vec![(0., 0., 10., 20.)]);
        assert_eq!(rects('▄'), vec![(0., 10., 10., 20.)]);
        assert_eq!(rects('▁'), vec![(0., 17., 10., 20.)]);
        assert_eq!(rects('▏'), vec![(0., 0., 1., 20.)]);
        assert_eq!(rects('▐'), vec![(5., 0., 10., 20.)]);
        assert_eq!(rects('▚'), vec![(0., 0., 5., 10.), (5., 10., 10., 20.)]);
        let mut shade = vec![];
        draw('▒', CELL, &mut shade);
        assert_eq!(shade, vec![Prim::Rect { x: 0., y: 0., w: 10., h: 20., alpha: 0.5 }]);
    }

    #[test]
    fn edges_snap_to_device_pixels() {
        // At 150% a cell 8.4 logical pixels wide is 12.6 device pixels; lines land on whole
        // device pixels and are 2 device pixels thick.
        let cell = Cell { x: 8.4, y: 0., w: 8.4, h: 18., scale: 1.5 };
        let mut out = vec![];
        draw('│', cell, &mut out);
        for p in out {
            let Prim::Rect { x, w, .. } = p else { panic!() };
            assert!((x * 1.5 - (x * 1.5).round()).abs() < 1e-4);
            assert!((w * 1.5 - 2.).abs() < 1e-4);
        }
    }

    #[test]
    fn fonts_keep_dashes_diagonals_and_text() {
        for c in ['┄', '╌', '╱', 'a', '•'] {
            assert!(!handles(c), "{c}");
            assert!(!draw(c, CELL, &mut vec![]), "{c}");
        }
        for c in ['─', '╬', '╭', '▀', '▟', '\u{e0b0}'] {
            assert!(handles(c), "{c}");
        }
    }
}
