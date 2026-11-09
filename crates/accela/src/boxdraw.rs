//! Box-drawing and block characters drawn as rectangles, so lines connect
//! pixel-perfectly regardless of font metrics or line height (like kitty/ghostty).

/// A rectangle relative to the cell origin, with coverage `alpha`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub alpha: f32,
}

fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
    Rect { x, y, w, h, alpha: 1.0 }
}

// Generated from Unicode character names by a script; (up, right, down, left).
// 0 = none, 1 = light, 2 = heavy, 3 = double.
const BOX_ARMS: [(u32, [u8; 4]); 125] = [
    (0x2500, [0, 1, 0, 1]), // ─ light horizontal
    (0x2501, [0, 2, 0, 2]), // ━ heavy horizontal
    (0x2502, [1, 0, 1, 0]), // │ light vertical
    (0x2503, [2, 0, 2, 0]), // ┃ heavy vertical
    (0x2504, [0, 1, 0, 1]), // ┄ light triple dash horizontal
    (0x2505, [0, 2, 0, 2]), // ┅ heavy triple dash horizontal
    (0x2506, [1, 0, 1, 0]), // ┆ light triple dash vertical
    (0x2507, [2, 0, 2, 0]), // ┇ heavy triple dash vertical
    (0x2508, [0, 1, 0, 1]), // ┈ light quadruple dash horizontal
    (0x2509, [0, 2, 0, 2]), // ┉ heavy quadruple dash horizontal
    (0x250A, [1, 0, 1, 0]), // ┊ light quadruple dash vertical
    (0x250B, [2, 0, 2, 0]), // ┋ heavy quadruple dash vertical
    (0x250C, [0, 1, 1, 0]), // ┌ light down and right
    (0x250D, [0, 2, 1, 0]), // ┍ down light and right heavy
    (0x250E, [0, 1, 2, 0]), // ┎ down heavy and right light
    (0x250F, [0, 2, 2, 0]), // ┏ heavy down and right
    (0x2510, [0, 0, 1, 1]), // ┐ light down and left
    (0x2511, [0, 0, 1, 2]), // ┑ down light and left heavy
    (0x2512, [0, 0, 2, 1]), // ┒ down heavy and left light
    (0x2513, [0, 0, 2, 2]), // ┓ heavy down and left
    (0x2514, [1, 1, 0, 0]), // └ light up and right
    (0x2515, [1, 2, 0, 0]), // ┕ up light and right heavy
    (0x2516, [2, 1, 0, 0]), // ┖ up heavy and right light
    (0x2517, [2, 2, 0, 0]), // ┗ heavy up and right
    (0x2518, [1, 0, 0, 1]), // ┘ light up and left
    (0x2519, [1, 0, 0, 2]), // ┙ up light and left heavy
    (0x251A, [2, 0, 0, 1]), // ┚ up heavy and left light
    (0x251B, [2, 0, 0, 2]), // ┛ heavy up and left
    (0x251C, [1, 1, 1, 0]), // ├ light vertical and right
    (0x251D, [1, 2, 1, 0]), // ┝ vertical light and right heavy
    (0x251E, [2, 1, 1, 0]), // ┞ up heavy and right down light
    (0x251F, [1, 1, 2, 0]), // ┟ down heavy and right up light
    (0x2520, [2, 1, 2, 0]), // ┠ vertical heavy and right light
    (0x2521, [2, 2, 1, 0]), // ┡ down light and right up heavy
    (0x2522, [1, 2, 2, 0]), // ┢ up light and right down heavy
    (0x2523, [2, 2, 2, 0]), // ┣ heavy vertical and right
    (0x2524, [1, 0, 1, 1]), // ┤ light vertical and left
    (0x2525, [1, 0, 1, 2]), // ┥ vertical light and left heavy
    (0x2526, [2, 0, 1, 1]), // ┦ up heavy and left down light
    (0x2527, [1, 0, 2, 1]), // ┧ down heavy and left up light
    (0x2528, [2, 0, 2, 1]), // ┨ vertical heavy and left light
    (0x2529, [2, 0, 1, 2]), // ┩ down light and left up heavy
    (0x252A, [1, 0, 2, 2]), // ┪ up light and left down heavy
    (0x252B, [2, 0, 2, 2]), // ┫ heavy vertical and left
    (0x252C, [0, 1, 1, 1]), // ┬ light down and horizontal
    (0x252D, [0, 1, 1, 2]), // ┭ left heavy and right down light
    (0x252E, [0, 2, 1, 1]), // ┮ right heavy and left down light
    (0x252F, [0, 2, 1, 2]), // ┯ down light and horizontal heavy
    (0x2530, [0, 1, 2, 1]), // ┰ down heavy and horizontal light
    (0x2531, [0, 1, 2, 2]), // ┱ right light and left down heavy
    (0x2532, [0, 2, 2, 1]), // ┲ left light and right down heavy
    (0x2533, [0, 2, 2, 2]), // ┳ heavy down and horizontal
    (0x2534, [1, 1, 0, 1]), // ┴ light up and horizontal
    (0x2535, [1, 1, 0, 2]), // ┵ left heavy and right up light
    (0x2536, [1, 2, 0, 1]), // ┶ right heavy and left up light
    (0x2537, [1, 2, 0, 2]), // ┷ up light and horizontal heavy
    (0x2538, [2, 1, 0, 1]), // ┸ up heavy and horizontal light
    (0x2539, [2, 1, 0, 2]), // ┹ right light and left up heavy
    (0x253A, [2, 2, 0, 1]), // ┺ left light and right up heavy
    (0x253B, [2, 2, 0, 2]), // ┻ heavy up and horizontal
    (0x253C, [1, 1, 1, 1]), // ┼ light vertical and horizontal
    (0x253D, [1, 1, 1, 2]), // ┽ left heavy and right vertical light
    (0x253E, [1, 2, 1, 1]), // ┾ right heavy and left vertical light
    (0x253F, [1, 2, 1, 2]), // ┿ vertical light and horizontal heavy
    (0x2540, [2, 1, 1, 1]), // ╀ up heavy and down horizontal light
    (0x2541, [1, 1, 2, 1]), // ╁ down heavy and up horizontal light
    (0x2542, [2, 1, 2, 1]), // ╂ vertical heavy and horizontal light
    (0x2543, [2, 1, 1, 2]), // ╃ left up heavy and right down light
    (0x2544, [2, 2, 1, 1]), // ╄ right up heavy and left down light
    (0x2545, [1, 1, 2, 2]), // ╅ left down heavy and right up light
    (0x2546, [1, 2, 2, 1]), // ╆ right down heavy and left up light
    (0x2547, [2, 2, 1, 2]), // ╇ down light and up horizontal heavy
    (0x2548, [1, 2, 2, 2]), // ╈ up light and down horizontal heavy
    (0x2549, [2, 1, 2, 2]), // ╉ right light and left vertical heavy
    (0x254A, [2, 2, 2, 1]), // ╊ left light and right vertical heavy
    (0x254B, [2, 2, 2, 2]), // ╋ heavy vertical and horizontal
    (0x254C, [0, 1, 0, 1]), // ╌ light double dash horizontal
    (0x254D, [0, 2, 0, 2]), // ╍ heavy double dash horizontal
    (0x254E, [1, 0, 1, 0]), // ╎ light double dash vertical
    (0x254F, [2, 0, 2, 0]), // ╏ heavy double dash vertical
    (0x2550, [0, 3, 0, 3]), // ═ double horizontal
    (0x2551, [3, 0, 3, 0]), // ║ double vertical
    (0x2552, [0, 3, 1, 0]), // ╒ down single and right double
    (0x2553, [0, 1, 3, 0]), // ╓ down double and right single
    (0x2554, [0, 3, 3, 0]), // ╔ double down and right
    (0x2555, [0, 0, 1, 3]), // ╕ down single and left double
    (0x2556, [0, 0, 3, 1]), // ╖ down double and left single
    (0x2557, [0, 0, 3, 3]), // ╗ double down and left
    (0x2558, [1, 3, 0, 0]), // ╘ up single and right double
    (0x2559, [3, 1, 0, 0]), // ╙ up double and right single
    (0x255A, [3, 3, 0, 0]), // ╚ double up and right
    (0x255B, [1, 0, 0, 3]), // ╛ up single and left double
    (0x255C, [3, 0, 0, 1]), // ╜ up double and left single
    (0x255D, [3, 0, 0, 3]), // ╝ double up and left
    (0x255E, [1, 3, 1, 0]), // ╞ vertical single and right double
    (0x255F, [3, 1, 3, 0]), // ╟ vertical double and right single
    (0x2560, [3, 3, 3, 0]), // ╠ double vertical and right
    (0x2561, [1, 0, 1, 3]), // ╡ vertical single and left double
    (0x2562, [3, 0, 3, 1]), // ╢ vertical double and left single
    (0x2563, [3, 0, 3, 3]), // ╣ double vertical and left
    (0x2564, [0, 3, 1, 3]), // ╤ down single and horizontal double
    (0x2565, [0, 1, 3, 1]), // ╥ down double and horizontal single
    (0x2566, [0, 3, 3, 3]), // ╦ double down and horizontal
    (0x2567, [1, 3, 0, 3]), // ╧ up single and horizontal double
    (0x2568, [3, 1, 0, 1]), // ╨ up double and horizontal single
    (0x2569, [3, 3, 0, 3]), // ╩ double up and horizontal
    (0x256A, [1, 3, 1, 3]), // ╪ vertical single and horizontal double
    (0x256B, [3, 1, 3, 1]), // ╫ vertical double and horizontal single
    (0x256C, [3, 3, 3, 3]), // ╬ double vertical and horizontal
    (0x256D, [0, 1, 1, 0]), // ╭ light arc down and right
    (0x256E, [0, 0, 1, 1]), // ╮ light arc down and left
    (0x256F, [1, 0, 0, 1]), // ╯ light arc up and left
    (0x2570, [1, 1, 0, 0]), // ╰ light arc up and right
    (0x2574, [0, 0, 0, 1]), // ╴ light left
    (0x2575, [1, 0, 0, 0]), // ╵ light up
    (0x2576, [0, 1, 0, 0]), // ╶ light right
    (0x2577, [0, 0, 1, 0]), // ╷ light down
    (0x2578, [0, 0, 0, 2]), // ╸ heavy left
    (0x2579, [2, 0, 0, 0]), // ╹ heavy up
    (0x257A, [0, 2, 0, 0]), // ╺ heavy right
    (0x257B, [0, 0, 2, 0]), // ╻ heavy down
    (0x257C, [0, 2, 0, 1]), // ╼ light left and heavy right
    (0x257D, [1, 0, 2, 0]), // ╽ light up and heavy down
    (0x257E, [0, 1, 0, 2]), // ╾ heavy left and light right
    (0x257F, [2, 0, 1, 0]), // ╿ heavy up and light down
];

fn arms(c: char) -> Option<[u8; 4]> {
    let cp = c as u32;
    BOX_ARMS.binary_search_by_key(&cp, |(k, _)| *k).ok().map(|i| BOX_ARMS[i].1)
}

/// Whether `c` is drawn by [`rects`] instead of the font.
pub fn is_builtin(c: char) -> bool {
    matches!(c as u32, 0x2580..=0x259F) || arms(c).is_some()
}

/// Rectangles for `c` in a `cw`×`ch` cell, or `None` if the font should draw it.
pub fn rects(c: char, cw: f32, ch: f32) -> Option<Vec<Rect>> {
    let cp = c as u32;
    if let Some(a) = arms(c) {
        return Some(lines(a, cw, ch));
    }
    let (w8, h8) = (cw / 8.0, ch / 8.0);
    let q = |ul: bool, ur: bool, ll: bool, lr: bool| {
        let (hw, hh) = (cw / 2.0, ch / 2.0);
        let mut v = Vec::new();
        if ul {
            v.push(rect(0.0, 0.0, hw, hh));
        }
        if ur {
            v.push(rect(hw, 0.0, cw - hw, hh));
        }
        if ll {
            v.push(rect(0.0, hh, hw, ch - hh));
        }
        if lr {
            v.push(rect(hw, hh, cw - hw, ch - hh));
        }
        v
    };
    let shade = |alpha| vec![Rect { alpha, ..rect(0.0, 0.0, cw, ch) }];
    Some(match cp {
        0x2580 => vec![rect(0.0, 0.0, cw, ch / 2.0)],
        0x2581..=0x2588 => {
            let h = h8 * (cp - 0x2580) as f32;
            vec![rect(0.0, ch - h, cw, h)]
        }
        0x2589..=0x258F => vec![rect(0.0, 0.0, w8 * (8 - (cp - 0x2588)) as f32, ch)],
        0x2590 => vec![rect(cw / 2.0, 0.0, cw - cw / 2.0, ch)],
        0x2591 => shade(0.25),
        0x2592 => shade(0.5),
        0x2593 => shade(0.75),
        0x2594 => vec![rect(0.0, 0.0, cw, h8)],
        0x2595 => vec![rect(cw - w8, 0.0, w8, ch)],
        0x2596 => q(false, false, true, false),
        0x2597 => q(false, false, false, true),
        0x2598 => q(true, false, false, false),
        0x2599 => q(true, false, true, true),
        0x259A => q(true, false, false, true),
        0x259B => q(true, true, true, false),
        0x259C => q(true, true, false, true),
        0x259D => q(false, true, false, false),
        0x259E => q(false, true, true, false),
        0x259F => q(false, true, true, true),
        _ => return None,
    })
}

fn lines([up, right, down, left]: [u8; 4], cw: f32, ch: f32) -> Vec<Rect> {
    let light = (cw.min(ch) / 9.0).round().max(1.0);
    let thick = |w: u8| match w {
        1 => light,
        2 => light * 2.0,
        3 => light * 3.0, // double: two light strokes with a light-sized gap
        _ => 0.0,
    };
    let cx = (cw / 2.0).floor();
    let cy = (ch / 2.0).floor();
    // How far horizontal arms reach into the center so corners close (and vice versa).
    let vmax = thick(up).max(thick(down));
    let hmax = thick(left).max(thick(right));
    let mut out = Vec::with_capacity(8);

    // A stroke of weight `w` centred on `c` across the given span: one rect, or two for double.
    let mut stroke = |w: u8, horizontal: bool, from: f32, to: f32| {
        let t = thick(w);
        let (center, len) = if horizontal { (cy, to - from) } else { (cx, to - from) };
        let parts: &[(f32, f32)] = if w == 3 {
            &[(center - t / 2.0, light), (center + t / 2.0 - light, light)]
        } else {
            &[(center - (t / 2.0).floor(), t)]
        };
        for &(start, size) in parts {
            out.push(if horizontal { rect(from, start, len, size) } else { rect(start, from, size, len) });
        }
    };
    if left > 0 {
        stroke(left, true, 0.0, cx + (vmax / 2.0).ceil().max(thick(left) / 2.0));
    }
    if right > 0 {
        stroke(right, true, cx - (vmax / 2.0).floor().max(thick(right) / 2.0), cw);
    }
    if up > 0 {
        stroke(up, false, 0.0, cy + (hmax / 2.0).ceil().max(thick(up) / 2.0));
    }
    if down > 0 {
        stroke(down, false, cy - (hmax / 2.0).floor().max(thick(down) / 2.0), ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const CW: f32 = 9.0;
    const CH: f32 = 19.0;

    fn covers(rs: &[Rect], x: f32, y: f32) -> bool {
        rs.iter().any(|r| x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h)
    }

    #[test]
    fn table_is_sorted_and_complete() {
        assert!(BOX_ARMS.windows(2).all(|w| w[0].0 < w[1].0));
        // Everything in U+2500..U+257F except the three diagonals.
        assert_eq!(BOX_ARMS.len(), 128 - 3);
        assert!(!is_builtin('╱') && !is_builtin('a') && is_builtin('█') && is_builtin('╭'));
    }

    #[test]
    fn horizontal_line_spans_cell_width_only() {
        let r = rects('─', CW, CH).unwrap();
        assert!(covers(&r, 0.0, (CH / 2.0).floor()) && covers(&r, CW - 0.5, (CH / 2.0).floor()));
        assert!(!covers(&r, 4.0, 0.0) && !covers(&r, 4.0, CH - 1.0));
    }

    #[test]
    fn cross_reaches_all_four_edges() {
        let r = rects('┼', CW, CH).unwrap();
        let (cx, cy) = ((CW / 2.0).floor(), (CH / 2.0).floor());
        for (x, y) in [(0.0, cy), (CW - 0.5, cy), (cx, 0.0), (cx, CH - 0.5)] {
            assert!(covers(&r, x, y), "({x},{y})");
        }
    }

    #[test]
    fn corners_close_without_gaps() {
        let r = rects('┌', CW, CH).unwrap();
        let (cx, cy) = ((CW / 2.0).floor(), (CH / 2.0).floor());
        assert!(covers(&r, cx, cy));
        assert!(!covers(&r, 0.0, cy) && !covers(&r, cx, 0.0));
    }

    #[test]
    fn heavy_is_thicker_and_double_has_two_strokes() {
        let light = rects('─', CW, CH).unwrap()[0].h;
        let heavy = rects('━', CW, CH).unwrap()[0].h;
        assert!(heavy > light);
        // Double: each arm is two parallel strokes, i.e. two distinct rows of pixels.
        let mut ys: Vec<i32> = rects('═', CW, CH).unwrap().iter().map(|r| r.y as i32).collect();
        ys.dedup();
        ys.sort();
        ys.dedup();
        assert_eq!(ys.len(), 2);
    }

    #[test]
    fn blocks_and_shades() {
        assert_eq!(rects('█', CW, CH).unwrap(), [rect(0.0, 0.0, CW, CH)]);
        assert_eq!(rects('▄', CW, CH).unwrap(), [rect(0.0, CH / 2.0, CW, CH / 2.0)]);
        assert_eq!(rects('▌', CW, CH).unwrap(), [rect(0.0, 0.0, CW / 2.0, CH)]);
        assert_eq!(rects('▒', CW, CH).unwrap()[0].alpha, 0.5);
        assert_eq!(rects('▚', CW, CH).unwrap().len(), 2);
        assert!(rects('a', CW, CH).is_none());
    }
}
