//! Inline images: the kitty graphics protocol (APC `G`) and sixel (DCS `q`).
//!
//! Only direct (in-band) transmission is accepted: file, temp-file and shared-memory
//! media are refused so a (possibly remote) program can't make the terminal read local
//! files. Memory is capped; the oldest images are evicted first.

use crate::util::base64_decode;
use std::collections::HashMap;
use std::sync::Arc;

pub const MAX_IMAGE_BYTES: usize = 64 << 20;
pub const MAX_TOTAL_BYTES: usize = 256 << 20;
pub const MAX_DIMENSION: u32 = 10_000;
const MAX_PLACEMENTS: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageFormat {
    Rgb,
    Rgba,
    Png,
}

#[derive(Clone, Debug)]
pub struct Image {
    pub id: u32,
    pub width: u32,
    pub height: u32,
    pub format: ImageFormat,
    pub data: Arc<Vec<u8>>,
    /// Changes whenever the image under this id is replaced (cache key for renderers).
    pub generation: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placement {
    pub image_id: u32,
    pub placement_id: u32,
    /// Stable line id of the top row (moves with scrolling text).
    pub line: u64,
    pub col: usize,
    pub cols: usize,
    pub rows: usize,
    pub z: i32,
    pub alt_screen: bool,
}

pub struct Graphics {
    pub images: HashMap<u32, Image>,
    /// Per-image and total memory limits (bytes of decoded transfer data).
    pub max_image_bytes: usize,
    pub max_total_bytes: usize,
    pub placements: Vec<Placement>,
    order: Vec<u32>,
    bytes: usize,
    generation: u64,
    next_auto_id: u32,
    /// Chunked kitty transfer in progress: (control of the first chunk, base64 so far).
    pending: Option<(Control, Vec<u8>)>,
}

impl Default for Graphics {
    fn default() -> Self {
        Self {
            images: HashMap::new(),
            max_image_bytes: MAX_IMAGE_BYTES,
            max_total_bytes: MAX_TOTAL_BYTES,
            placements: Vec::new(),
            order: Vec::new(),
            bytes: 0,
            generation: 0,
            next_auto_id: 0,
            pending: None,
        }
    }
}

/// Parsed kitty control keys.
#[derive(Clone, Debug, Default)]
pub struct Control {
    pub action: u8,
    pub format: u32,
    pub medium: u8,
    pub width: u32,
    pub height: u32,
    pub id: u32,
    pub number: u32,
    pub placement: u32,
    pub more: bool,
    pub quiet: u8,
    pub cols: u32,
    pub rows: u32,
    pub no_cursor_move: bool,
    pub delete: u8,
    pub z: i32,
    pub compression: u8,
}

impl Control {
    fn parse(s: &[u8]) -> Control {
        let mut c = Control { action: b't', format: 32, medium: b'd', ..Default::default() };
        for kv in s.split(|&b| b == b',') {
            let [k, b'=', v @ ..] = kv else { continue };
            let num = || std::str::from_utf8(v).ok().and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
            let unum = || num().clamp(0, u32::MAX as i64) as u32;
            let ch = || v.first().copied().unwrap_or(0);
            match k {
                b'a' => c.action = ch(),
                b'f' => c.format = unum(),
                b't' => c.medium = ch(),
                b's' => c.width = unum(),
                b'v' => c.height = unum(),
                b'i' => c.id = unum(),
                b'I' => c.number = unum(),
                b'p' => c.placement = unum(),
                b'm' => c.more = num() == 1,
                b'q' => c.quiet = unum().min(2) as u8,
                b'c' => c.cols = unum(),
                b'r' => c.rows = unum(),
                b'C' => c.no_cursor_move = num() == 1,
                b'd' => c.delete = ch(),
                b'z' => c.z = num().clamp(i32::MIN as i64, i32::MAX as i64) as i32,
                b'o' => c.compression = ch(),
                _ => {}
            }
        }
        c
    }
}

/// What the terminal must do after a kitty command.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    /// Reply to write back to the program.
    pub reply: Option<Vec<u8>>,
    /// Place image `id` at the cursor using this many cells.
    pub place: Option<(u32, u32, usize, usize, i32, bool)>,
    /// Delete placements on the visible screen (used by `a=d,d=a`).
    pub deleted: bool,
}

/// Width/height from a PNG header, without decoding.
pub fn png_size(data: &[u8]) -> Option<(u32, u32)> {
    if data.len() < 24 || &data[..8] != b"\x89PNG\r\n\x1a\n" || &data[12..16] != b"IHDR" {
        return None;
    }
    let w = u32::from_be_bytes(data[16..20].try_into().ok()?);
    let h = u32::from_be_bytes(data[20..24].try_into().ok()?);
    Some((w, h))
}

impl Graphics {
    pub fn total_bytes(&self) -> usize {
        self.bytes
    }

    /// Store an image, evicting the oldest ones past the memory cap. Returns its id.
    pub fn insert(&mut self, mut id: u32, width: u32, height: u32, format: ImageFormat, data: Vec<u8>) -> u32 {
        if id == 0 {
            // Auto ids live in the upper range so they never collide with client ids.
            self.next_auto_id = self.next_auto_id.wrapping_add(1);
            id = 0x8000_0000 | self.next_auto_id;
        }
        self.remove_image(id);
        self.generation += 1;
        self.bytes += data.len();
        self.images.insert(id, Image { id, width, height, format, data: Arc::new(data), generation: self.generation });
        self.order.push(id);
        while self.bytes > self.max_total_bytes && self.order.len() > 1 {
            let oldest = self.order[0];
            self.remove_image(oldest);
            self.placements.retain(|p| p.image_id != oldest);
        }
        id
    }

    fn remove_image(&mut self, id: u32) {
        if let Some(img) = self.images.remove(&id) {
            self.bytes -= img.data.len();
            self.order.retain(|&i| i != id);
        }
    }

    pub fn place(&mut self, p: Placement) {
        if p.placement_id != 0 {
            self.placements.retain(|q| !(q.image_id == p.image_id && q.placement_id == p.placement_id));
        }
        if self.placements.len() >= MAX_PLACEMENTS {
            self.placements.remove(0);
        }
        self.placements.push(p);
    }

    /// Drop placements whose lines no longer exist.
    pub fn prune(&mut self, first_line: u64) {
        self.placements.retain(|p| p.line + p.rows as u64 > first_line);
    }

    pub fn clear(&mut self) {
        self.images.clear();
        self.placements.clear();
        self.order.clear();
        self.bytes = 0;
        self.pending = None;
    }

    /// Handle one APC payload (without the leading `G`). `cell_px` sizes placements.
    pub fn kitty(&mut self, payload: &[u8], cell_px: (u16, u16), alt_screen: bool) -> Outcome {
        let (ctrl_s, data) = match payload.iter().position(|&b| b == b';') {
            Some(i) => (&payload[..i], &payload[i + 1..]),
            None => (payload, &[][..]),
        };
        let mut ctrl = Control::parse(ctrl_s);

        // Continuation chunk of a chunked transfer.
        if let Some((first, mut buf)) = self.pending.take() {
            if buf.len() + data.len() > self.max_image_bytes * 4 / 3 + 4 {
                return self.error(&first, "EFBIG:image too large");
            }
            buf.extend_from_slice(data);
            if ctrl.more {
                self.pending = Some((first, buf));
                return Outcome::default();
            }
            ctrl = first;
            return self.finish(ctrl, &buf, cell_px, alt_screen);
        }
        if ctrl.more && matches!(ctrl.action, b't' | b'T' | b'q') {
            if data.len() > self.max_image_bytes * 4 / 3 + 4 {
                return self.error(&ctrl, "EFBIG:image too large");
            }
            self.pending = Some((ctrl, data.to_vec()));
            return Outcome::default();
        }
        self.finish(ctrl, data, cell_px, alt_screen)
    }

    fn reply(&self, c: &Control, msg: &str) -> Option<Vec<u8>> {
        let is_err = msg != "OK";
        if (c.id == 0 && c.number == 0) || (c.quiet >= 1 && !is_err) || c.quiet >= 2 {
            return None;
        }
        let mut s = String::from("\x1b_G");
        if c.id != 0 {
            s.push_str(&format!("i={}", c.id));
        }
        if c.number != 0 {
            s.push_str(&format!("{}I={}", if c.id != 0 { "," } else { "" }, c.number));
        }
        if c.placement != 0 {
            s.push_str(&format!(",p={}", c.placement));
        }
        s.push_str(&format!(";{msg}\x1b\\"));
        Some(s.into_bytes())
    }

    fn error(&self, c: &Control, msg: &str) -> Outcome {
        Outcome { reply: self.reply(c, msg), ..Default::default() }
    }

    fn finish(&mut self, c: Control, b64: &[u8], cell_px: (u16, u16), alt: bool) -> Outcome {
        match c.action {
            b't' | b'T' | b'q' => {
                if c.medium != b'd' {
                    return self.error(&c, "EBADF:only direct transmission is allowed");
                }
                if c.compression != 0 {
                    return self.error(&c, "ENOTSUP:compression not supported");
                }
                let Some(data) = base64_decode(b64, self.max_image_bytes) else {
                    return self.error(&c, "EINVAL:bad base64 data or image too large");
                };
                let (format, width, height) = match c.format {
                    24 | 32 => {
                        let bpp = if c.format == 24 { 3 } else { 4 };
                        if c.width == 0 || c.height == 0 || c.width > MAX_DIMENSION || c.height > MAX_DIMENSION {
                            return self.error(&c, "EINVAL:bad dimensions");
                        }
                        if data.len() != c.width as usize * c.height as usize * bpp {
                            return self.error(&c, "ENODATA:insufficient image data");
                        }
                        (if bpp == 3 { ImageFormat::Rgb } else { ImageFormat::Rgba }, c.width, c.height)
                    }
                    100 => match png_size(&data) {
                        Some((w, h)) if w > 0 && h > 0 && w <= MAX_DIMENSION && h <= MAX_DIMENSION => {
                            (ImageFormat::Png, w, h)
                        }
                        _ => return self.error(&c, "EBADPNG:invalid PNG"),
                    },
                    _ => return self.error(&c, "EINVAL:unsupported format"),
                };
                if c.action == b'q' {
                    return Outcome { reply: self.reply(&c, "OK"), ..Default::default() };
                }
                let id = self.insert(c.id, width, height, format, data);
                let mut out = Outcome { reply: self.reply(&c, "OK"), ..Default::default() };
                if c.action == b'T' {
                    out.place = Some(self.cells_for(id, &c, cell_px, alt));
                }
                out
            }
            b'p' => {
                let id = c.id;
                if id == 0 || !self.images.contains_key(&id) {
                    return self.error(&c, "ENOENT:no such image");
                }
                Outcome {
                    reply: self.reply(&c, "OK"),
                    place: Some(self.cells_for(id, &c, cell_px, alt)),
                    ..Default::default()
                }
            }
            b'd' => {
                match c.delete {
                    0 | b'a' => self.placements.clear(),
                    b'A' => self.clear(),
                    b'i' => self
                        .placements
                        .retain(|p| p.image_id != c.id || (c.placement != 0 && p.placement_id != c.placement)),
                    b'I' => {
                        self.placements.retain(|p| p.image_id != c.id);
                        self.remove_image(c.id);
                    }
                    _ => {}
                }
                Outcome { deleted: true, ..Default::default() }
            }
            _ => self.error(&c, "EINVAL:unsupported action"),
        }
    }

    fn cells_for(&self, id: u32, c: &Control, cell_px: (u16, u16), alt: bool) -> (u32, u32, usize, usize, i32, bool) {
        let img = &self.images[&id];
        let (cw, ch) = (cell_px.0.max(1) as u32, cell_px.1.max(1) as u32);
        let cols = if c.cols > 0 { c.cols } else { img.width.div_ceil(cw) }.clamp(1, 1000) as usize;
        let rows = if c.rows > 0 { c.rows } else { img.height.div_ceil(ch) }.clamp(1, 1000) as usize;
        let _ = alt;
        (id, c.placement, cols, rows, c.z, c.no_cursor_move)
    }
}

/// Sixel decoder (DCS P1;P2;P3 q ... ST) producing an RGBA bitmap.
pub struct Sixel {
    palette: Vec<[u8; 4]>,
    color: usize,
    x: usize,
    y: usize,
    /// Extent actually drawn (or declared by raster attributes).
    width: usize,
    height: usize,
    /// Allocated size; grows geometrically so wide images stay linear-time.
    stride: usize,
    alloc_h: usize,
    pixels: Vec<[u8; 4]>,
    transparent_bg: bool,
    state: SixelState,
    params: Vec<u32>,
    failed: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SixelState {
    Data,
    Repeat,
    Color,
    Raster,
}

/// Enough for a 4K-wide image; larger sixels are rejected instead of allocated.
const SIXEL_MAX_PIXELS: usize = 3840 * 2400;

impl Sixel {
    pub fn new(p2: u16) -> Self {
        // VT340 default palette (first 16), the rest black.
        const VT340: [[u8; 3]; 16] = [
            [0, 0, 0],
            [51, 51, 204],
            [204, 36, 36],
            [51, 204, 51],
            [204, 51, 204],
            [51, 204, 204],
            [204, 204, 51],
            [120, 120, 120],
            [69, 69, 69],
            [87, 87, 153],
            [153, 69, 69],
            [87, 153, 87],
            [153, 87, 153],
            [87, 153, 153],
            [153, 153, 87],
            [204, 204, 204],
        ];
        let mut palette = vec![[0, 0, 0, 255]; 256];
        for (i, c) in VT340.iter().enumerate() {
            palette[i] = [c[0], c[1], c[2], 255];
        }
        Sixel {
            palette,
            color: 0,
            x: 0,
            y: 0,
            width: 0,
            height: 0,
            stride: 0,
            alloc_h: 0,
            pixels: Vec::new(),
            transparent_bg: p2 == 1,
            state: SixelState::Data,
            params: Vec::new(),
            failed: false,
        }
    }

    fn ensure(&mut self, w: usize, h: usize) -> bool {
        if w > self.stride || h > self.alloc_h {
            let need = (w.max(self.stride), h.max(self.alloc_h));
            if need.0.saturating_mul(need.1) > SIXEL_MAX_PIXELS {
                self.failed = true;
                return false;
            }
            // Double the growing dimension(s), falling back to the exact size at the cap.
            let mut nw = if w > self.stride { w.max(self.stride * 2).max(64) } else { self.stride };
            let mut nh = if h > self.alloc_h { h.max(self.alloc_h * 2).max(6) } else { self.alloc_h };
            if nw.saturating_mul(nh) > SIXEL_MAX_PIXELS {
                (nw, nh) = need;
            }
            let bg = if self.transparent_bg { [0, 0, 0, 0] } else { self.palette[0] };
            let mut px = vec![bg; nw * nh];
            for row in 0..self.alloc_h {
                px[row * nw..row * nw + self.stride]
                    .copy_from_slice(&self.pixels[row * self.stride..(row + 1) * self.stride]);
            }
            self.pixels = px;
            self.stride = nw;
            self.alloc_h = nh;
        }
        self.width = self.width.max(w);
        self.height = self.height.max(h);
        true
    }

    fn sixel(&mut self, b: u8, repeat: usize) {
        let bits = b - 0x3f;
        let repeat = repeat.clamp(1, 4096);
        if !self.ensure(self.x + repeat, self.y + 6) {
            return;
        }
        let color = self.palette[self.color];
        for bit in 0..6 {
            if bits & (1 << bit) != 0 {
                let row = (self.y + bit) * self.stride;
                for x in self.x..self.x + repeat {
                    self.pixels[row + x] = color;
                }
            }
        }
        self.x += repeat;
    }

    fn end_params(&mut self) {
        match self.state {
            SixelState::Color => {
                let p = &self.params;
                if let Some(&reg) = p.first() {
                    self.color = (reg as usize).min(255);
                    if p.len() >= 5 {
                        let (u, a, b, c) = (p[1], p[2].min(360), p[3].min(100), p[4].min(100));
                        let rgb = if u == 1 { hls(a, b, c) } else { [pct(a), pct(b), pct(c)] };
                        self.palette[self.color] = [rgb[0], rgb[1], rgb[2], 255];
                    }
                }
            }
            SixelState::Raster => {
                // "Pan;Pad;Ph;Pv: pre-size the image.
                if let (Some(&w), Some(&h)) = (self.params.get(2), self.params.get(3)) {
                    self.ensure(w as usize, h as usize);
                }
            }
            _ => {}
        }
        self.params.clear();
    }

    pub fn put(&mut self, b: u8) {
        if self.failed {
            return;
        }
        match (self.state, b) {
            (SixelState::Repeat | SixelState::Color | SixelState::Raster, b'0'..=b'9') => {
                if self.params.is_empty() {
                    self.params.push(0);
                }
                let last = self.params.last_mut().expect("non-empty");
                *last = last.saturating_mul(10).saturating_add((b - b'0') as u32);
            }
            (SixelState::Repeat | SixelState::Color | SixelState::Raster, b';') => self.params.push(0),
            (SixelState::Repeat, 0x3f..=0x7e) => {
                let n = self.params.first().copied().unwrap_or(1) as usize;
                self.params.clear();
                self.state = SixelState::Data;
                self.sixel(b, n);
            }
            _ => {
                if self.state != SixelState::Data {
                    self.end_params();
                    self.state = SixelState::Data;
                }
                match b {
                    0x3f..=0x7e => self.sixel(b, 1),
                    b'!' => self.state = SixelState::Repeat,
                    b'#' => self.state = SixelState::Color,
                    b'"' => self.state = SixelState::Raster,
                    b'$' => self.x = 0,
                    b'-' => {
                        self.x = 0;
                        self.y += 6;
                    }
                    _ => {}
                }
            }
        }
    }

    /// Finish and return (width, height, RGBA bytes), or `None` if empty or too large.
    pub fn finish(mut self) -> Option<(u32, u32, Vec<u8>)> {
        if self.state != SixelState::Data {
            self.end_params();
        }
        if self.failed || self.width == 0 || self.height == 0 {
            return None;
        }
        let mut bytes = Vec::with_capacity(self.width * self.height * 4);
        for row in 0..self.height {
            let start = row * self.stride;
            bytes.extend(self.pixels[start..start + self.width].iter().flatten());
        }
        Some((self.width as u32, self.height as u32, bytes))
    }
}

fn pct(v: u32) -> u8 {
    ((v.min(100) * 255 + 50) / 100) as u8
}

fn hls(h: u32, l: u32, s: u32) -> [u8; 3] {
    // Sixel HLS: hue 0 = blue, rotated 120° relative to the usual HSL.
    let (h, l, s) = (((h + 240) % 360) as f32, l as f32 / 100.0, s as f32 / 100.0);
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = l - c / 2.0;
    let (r, g, b) = match (h / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let f = |v: f32| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    [f(r), f(g), f(b)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::base64_encode;

    fn kitty(g: &mut Graphics, s: &str) -> Outcome {
        g.kitty(s.as_bytes(), (10, 20), false)
    }

    #[test]
    fn transmit_and_display_rgba() {
        let mut g = Graphics::default();
        let px = base64_encode(&[255u8; 2 * 2 * 4]);
        let out = kitty(&mut g, &format!("a=T,f=32,s=2,v=2,i=7;{px}"));
        assert_eq!(out.reply.as_deref(), Some(&b"\x1b_Gi=7;OK\x1b\\"[..]));
        assert_eq!(out.place, Some((7, 0, 1, 1, 0, false)));
        assert_eq!(g.images[&7].format, ImageFormat::Rgba);
    }

    #[test]
    fn chunked_transfer() {
        let mut g = Graphics::default();
        let px = base64_encode(&[1u8; 4 * 4 * 3]);
        let (a, b) = px.split_at(20);
        assert_eq!(kitty(&mut g, &format!("a=t,f=24,s=4,v=4,i=1,m=1;{a}")), Outcome::default());
        let out = kitty(&mut g, &format!("m=0;{b}"));
        assert!(out.reply.unwrap().ends_with(b";OK\x1b\\"));
        assert_eq!(g.images[&1].data.len(), 48);
    }

    #[test]
    fn query_does_not_store() {
        let mut g = Graphics::default();
        let px = base64_encode(&[0u8; 3]);
        let out = kitty(&mut g, &format!("a=q,f=24,s=1,v=1,i=31;{px}"));
        assert_eq!(out.reply.as_deref(), Some(&b"\x1b_Gi=31;OK\x1b\\"[..]));
        assert!(g.images.is_empty());
    }

    #[test]
    fn rejects_file_and_shm_media() {
        let mut g = Graphics::default();
        for t in ["f", "t", "s"] {
            let out = kitty(&mut g, &format!("a=T,t={t},f=100,i=2;{}", base64_encode(b"/etc/passwd")));
            assert!(String::from_utf8(out.reply.unwrap()).unwrap().contains("EBADF"));
        }
        assert!(g.images.is_empty());
    }

    #[test]
    fn validates_sizes_and_formats() {
        let mut g = Graphics::default();
        let short = base64_encode(&[0u8; 5]);
        let r = |o: Outcome| String::from_utf8(o.reply.unwrap()).unwrap();
        assert!(r(kitty(&mut g, &format!("a=t,f=32,s=2,v=2,i=1;{short}"))).contains("ENODATA"));
        assert!(r(kitty(&mut g, &format!("a=t,f=32,s=0,v=2,i=1;{short}"))).contains("EINVAL"));
        assert!(r(kitty(&mut g, &format!("a=t,f=100,i=1;{short}"))).contains("EBADPNG"));
        assert!(r(kitty(&mut g, "a=t,f=24,s=1,v=1,i=1;!!!!")).contains("EINVAL"));
        assert!(r(kitty(&mut g, &format!("a=t,f=32,s=99999,v=1,i=1;{short}"))).contains("EINVAL"));
    }

    #[test]
    fn quiet_modes() {
        let mut g = Graphics::default();
        let px = base64_encode(&[0u8; 3]);
        assert!(kitty(&mut g, &format!("a=t,f=24,s=1,v=1,i=1,q=1;{px}")).reply.is_none());
        assert!(kitty(&mut g, "a=t,f=24,s=1,v=1,i=1,q=1;AA").reply.is_some());
        assert!(kitty(&mut g, "a=t,f=24,s=1,v=1,i=1,q=2;AA").reply.is_none());
        // No id: never reply.
        assert!(kitty(&mut g, &format!("a=t,f=24,s=1,v=1;{px}")).reply.is_none());
    }

    #[test]
    fn chunked_transfer_is_capped() {
        let mut g = Graphics { max_image_bytes: 30, ..Default::default() };
        let chunk = "A".repeat(32);
        assert_eq!(kitty(&mut g, &format!("a=t,f=24,s=10,v=1,i=4,m=1;{chunk}")), Outcome::default());
        let out = kitty(&mut g, &format!("m=0;{chunk}"));
        assert!(String::from_utf8(out.reply.unwrap()).unwrap().contains("EFBIG"));
        assert!(g.images.is_empty());
    }

    #[test]
    fn png_header() {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        png.extend_from_slice(&640u32.to_be_bytes());
        png.extend_from_slice(&480u32.to_be_bytes());
        assert_eq!(png_size(&png), Some((640, 480)));
        assert_eq!(png_size(b"not a png at all, sorry!"), None);
        let mut g = Graphics::default();
        let out = kitty(&mut g, &format!("a=T,f=100,i=3;{}", base64_encode(&png)));
        assert_eq!(out.place, Some((3, 0, 64, 24, 0, false)));
    }

    #[test]
    fn delete_and_put() {
        let mut g = Graphics::default();
        let px = base64_encode(&[0u8; 3]);
        kitty(&mut g, &format!("a=t,f=24,s=1,v=1,i=5;{px}"));
        assert!(kitty(&mut g, "a=p,i=5,c=3,r=2").place == Some((5, 0, 3, 2, 0, false)));
        assert!(kitty(&mut g, "a=p,i=99").reply.unwrap().windows(6).any(|w| w == b"ENOENT"));
        kitty(&mut g, "a=d,d=I,i=5");
        assert!(g.images.is_empty());
    }

    #[test]
    fn memory_cap_evicts_oldest() {
        let mut g = Graphics { max_total_bytes: 1000, ..Default::default() };
        for id in 1..=6 {
            g.insert(id, 1, 1, ImageFormat::Rgba, vec![0u8; 300]);
        }
        assert!(g.total_bytes() <= 1000);
        assert!(!g.images.contains_key(&1) && g.images.contains_key(&6));
    }

    #[test]
    fn sixel_decodes_colors_and_repeats() {
        let mut s = Sixel::new(1);
        // Register 1 = pure red (RGB percent), two full columns, newline, one column of register 2.
        for &b in b"#1;2;100;0;0#1!2~-#2;2;0;100;0~" {
            s.put(b);
        }
        let (w, h, px) = s.finish().unwrap();
        assert_eq!((w, h), (2, 12));
        assert_eq!(&px[0..4], &[255, 0, 0, 255]);
        assert_eq!(&px[4..8], &[255, 0, 0, 255]);
        let row6 = 6 * 2 * 4;
        assert_eq!(&px[row6..row6 + 4], &[0, 255, 0, 255]);
        // Transparent background where nothing was drawn.
        assert_eq!(&px[row6 + 4..row6 + 8], &[0, 0, 0, 0]);
    }

    #[test]
    fn sixel_without_raster_attributes_grows_and_crops() {
        let mut s = Sixel::new(1);
        for _ in 0..1000 {
            s.put(b'~');
        }
        s.put(b'-');
        s.put(b'@');
        let (w, h, px) = s.finish().unwrap();
        assert_eq!((w, h), (1000, 12));
        assert_eq!(px.len(), 1000 * 12 * 4);
    }

    #[test]
    fn oversized_sixel_is_rejected_without_allocating() {
        let mut s = Sixel::new(1);
        for &b in b"\"1;1;99999;99999~" {
            s.put(b);
        }
        assert!(s.finish().is_none());
        // Repeat counts are clamped too.
        let mut s = Sixel::new(1);
        for &b in b"!4294967295~" {
            s.put(b);
        }
        assert_eq!(s.finish().map(|(w, h, _)| (w, h)), Some((4096, 6)));
    }
}
