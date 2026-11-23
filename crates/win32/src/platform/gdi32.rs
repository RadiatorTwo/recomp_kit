//! GDI32: device capabilities, objects (pens, brushes, fonts, bitmaps), device contexts, and
//! drawing into DIB sections (PatBlt, text with TrueType fonts, `text.rs`). Drawing on a window's
//! DC is not shown.

use super::user32::{DESKTOP_H, DESKTOP_W};
use super::{Import, arg, log, mem, text};
use recomp::rt::Cpu;
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

/// The one device context: the screen.
pub const SCREEN_DC: u32 = 0xdc00;

fn get_device_caps(c: &mut Cpu) -> u32 {
    match arg(c, 1) {
        8 => DESKTOP_W,  // HORZRES
        10 => DESKTOP_H, // VERTRES
        12 => 16,        // BITSPIXEL
        14 => 1,         // PLANES
        88 | 90 => 96,   // LOGPIXELSX/Y
        116 => 60,       // VREFRESH
        _ => 0,
    }
}

/// GetTextMetricsA(dc, TEXTMETRICA*): the selected font's.
fn get_text_metrics(c: &mut Cpu) -> u32 {
    let (dc, p) = (arg(c, 0), arg(c, 1));
    let t = text::metrics(dc_font(dc));
    let m = mem();
    m.slice(p, 56).fill(0);
    // height, ascent, descent, internal and external leading, average and maximum width, weight
    for (k, v) in [
        t.height,
        t.ascent,
        t.descent,
        t.internal_leading,
        0,
        t.avg_width,
        t.max_width,
        400,
    ]
    .into_iter()
    .enumerate()
    {
        m.wr32(p + 4 * k as u32, v as u32);
    }
    // digitized aspect, first, last, default and break character, pitch and family (TrueType,
    // variable pitch, swiss)
    m.wr32(p + 36, 96);
    m.wr32(p + 40, 96);
    for (o, b) in [(44, 0x20u8), (45, 0xff), (46, 0x1f), (47, 0x20), (51, 0x27)] {
        m.wr8(p + o, b);
    }
    1
}

/// A GDI object behind a handle (what GetObjectType tells; drawing is not done yet).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Obj {
    Pen(u32),
    Brush(u32),
    /// a device context: what is selected into it and its text state
    Dc(Dc),
    Palette,
    /// a font: its number in `text`
    Font(u32),
    /// its BITMAPINFOHEADER in guest memory (a resource's data), 0 when it has none
    Bitmap(u32),
    /// a DIB section: its pixels in guest memory, rows from the top
    Dib(Dib),
    Region,
}

/// A device context's state. Objects 0: the defaults.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Dc {
    pub bitmap: u32,
    pub brush: u32,
    pub pen: u32,
    pub font: u32,
    /// COLORREFs, OPAQUE (2) or TRANSPARENT (1), TA_* flags
    pub text: u32,
    pub bk: u32,
    pub bk_mode: u32,
    pub align: u32,
}

impl Dc {
    fn new() -> Dc {
        Dc {
            bk: 0xffffff,
            bk_mode: 2,
            ..Dc::default()
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Dib {
    pub bits: u32,
    pub w: u32,
    pub h: u32,
    pub stride: u32,
    pub bpp: u32,
    /// 16 bits a pixel: 565 (else 555)
    pub g6: bool,
}

impl Obj {
    /// GetObjectType's OBJ_ number.
    fn kind(&self) -> u32 {
        match self {
            Obj::Pen(_) => 1,
            Obj::Brush(_) => 2,
            Obj::Dc(_) => 3,
            Obj::Palette => 5,
            Obj::Font(_) => 6,
            Obj::Bitmap(_) | Obj::Dib(_) => 7,
            Obj::Region => 8,
        }
    }
}

static OBJECTS: Mutex<BTreeMap<u32, Obj>> = Mutex::new(BTreeMap::new());
static NEXT: AtomicU32 = AtomicU32::new(0x6700);

/// A new GDI object's handle.
pub fn new_obj(o: Obj) -> u32 {
    let h = NEXT.fetch_add(4, Ordering::Relaxed);
    OBJECTS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(h, o);
    h
}

pub fn obj(h: u32) -> Option<Obj> {
    if h == SCREEN_DC {
        return Some(Obj::Dc(Dc::new()));
    }
    OBJECTS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&h)
        .copied()
}

fn create_solid_brush(c: &mut Cpu) -> u32 {
    new_obj(Obj::Brush(arg(c, 0)))
}

fn create_pen(c: &mut Cpu) -> u32 {
    new_obj(Obj::Pen(arg(c, 2)))
}

/// The font face name at `p` (up to 32 bytes).
fn face(p: u32) -> String {
    if p == 0 {
        return String::new();
    }
    let m = mem();
    (0..32)
        .map(|k| m.rd8(p + k))
        .take_while(|&b| b != 0)
        .map(|b| b as char)
        .collect()
}

/// CreateFontIndirectA(LOGFONTA*): height, width, escapement, orientation, weight, italic, ...,
/// face name at 28.
fn create_font_indirect(c: &mut Cpu) -> u32 {
    let p = arg(c, 0);
    let m = mem();
    let spec = text::Spec {
        face: face(p + 28),
        height: m.rd32(p) as i32,
        weight: m.rd32(p + 16),
        italic: m.rd8(p + 20) != 0,
    };
    new_obj(Obj::Font(text::add(spec)))
}

/// CreateFontA(height, width, escapement, orientation, weight, italic, ..., face): 14 arguments.
fn create_font(c: &mut Cpu) -> u32 {
    let spec = text::Spec {
        face: face(arg(c, 13)),
        height: arg(c, 0) as i32,
        weight: arg(c, 4),
        italic: arg(c, 5) != 0,
    };
    new_obj(Obj::Font(text::add(spec)))
}

fn create_compatible_dc(_: &mut Cpu) -> u32 {
    new_obj(Obj::Dc(Dc::new()))
}

/// A DC's state, changed by `f`; what `f` returns (0 for no DC).
fn with_dc(dc: u32, f: impl FnOnce(&mut Dc) -> u32) -> u32 {
    match OBJECTS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_mut(&dc)
    {
        Some(Obj::Dc(d)) => f(d),
        _ => 0,
    }
}

fn dc_state(dc: u32) -> Dc {
    match obj(dc) {
        Some(Obj::Dc(d)) => d,
        _ => Dc::new(),
    }
}

/// The font number selected into a DC (the default font: a 16-pixel Arial).
fn dc_font(dc: u32) -> u32 {
    static DEFAULT: AtomicU32 = AtomicU32::new(u32::MAX);
    match obj(dc_state(dc).font) {
        Some(Obj::Font(k)) => k,
        _ => {
            if DEFAULT.load(Ordering::Relaxed) == u32::MAX {
                DEFAULT.store(
                    text::add(text::Spec {
                        face: "Arial".into(),
                        height: 16,
                        weight: 400,
                        italic: false,
                    }),
                    Ordering::Relaxed,
                );
            }
            DEFAULT.load(Ordering::Relaxed)
        }
    }
}

fn set_text_color(c: &mut Cpu) -> u32 {
    let v = arg(c, 1);
    with_dc(arg(c, 0), |d| std::mem::replace(&mut d.text, v))
}

fn set_bk_color(c: &mut Cpu) -> u32 {
    let v = arg(c, 1);
    with_dc(arg(c, 0), |d| std::mem::replace(&mut d.bk, v))
}

fn set_bk_mode(c: &mut Cpu) -> u32 {
    let v = arg(c, 1);
    with_dc(arg(c, 0), |d| std::mem::replace(&mut d.bk_mode, v))
}

fn set_text_align(c: &mut Cpu) -> u32 {
    let v = arg(c, 1);
    with_dc(arg(c, 0), |d| std::mem::replace(&mut d.align, v))
}

fn get_text_color(c: &mut Cpu) -> u32 {
    dc_state(arg(c, 0)).text
}

fn get_bk_color(c: &mut Cpu) -> u32 {
    dc_state(arg(c, 0)).bk
}

fn get_bk_mode(c: &mut Cpu) -> u32 {
    dc_state(arg(c, 0)).bk_mode
}

fn get_text_align(c: &mut Cpu) -> u32 {
    dc_state(arg(c, 0)).align
}

/// The bytes of a text argument: `n` of them, or up to its NUL when `n` is -1.
fn text_arg(p: u32, n: u32) -> Vec<u8> {
    let m = mem();
    if n == u32::MAX {
        (0..)
            .map(|k| m.rd8(p + k))
            .take_while(|&b| b != 0)
            .collect()
    } else {
        (0..n.min(4096)).map(|k| m.rd8(p + k)).collect()
    }
}

/// GetTextExtentPoint32A / GetTextExtentPointA(dc, text, n, SIZE*).
fn get_text_extent_point(c: &mut Cpu) -> u32 {
    let (dc, s, n, size) = (arg(c, 0), arg(c, 1), arg(c, 2), arg(c, 3));
    let (w, h) = text::extent(dc_font(dc), &text_arg(s, n));
    mem().wr32(size, w as u32);
    mem().wr32(size + 4, h as u32);
    1
}

/// GetCharWidthA(dc, first, last, widths*).
fn get_char_width(c: &mut Cpu) -> u32 {
    let (dc, first, last, out) = (arg(c, 0), arg(c, 1), arg(c, 2), arg(c, 3));
    let bytes: Vec<u8> = (first..=last.min(255)).map(|b| b as u8).collect();
    for (k, a) in text::advances(dc_font(dc), &bytes).into_iter().enumerate() {
        mem().wr32(out + 4 * k as u32, a as u32);
    }
    1
}

/// Text into the DIB selected into `dc` (none: nothing drawn): the background cell when
/// OPAQUE, the glyphs' coverage blended in the text colour. `dx`: ExtTextOutA's advances.
fn draw_text(
    dc: u32,
    (x, y): (i32, i32),
    s: &[u8],
    dx: Option<Vec<i32>>,
    opaque: Option<[i32; 4]>,
    clip: Option<[i32; 4]>,
) {
    let Some(d) = dib_of(dc) else { return };
    let st = dc_state(dc);
    let k = dc_font(dc);
    let mt = text::metrics(k);
    let w: i32 = dx
        .as_ref()
        .map_or_else(|| text::extent(k, s).0, |v| v.iter().sum());
    // TA_RIGHT 2, TA_CENTER 6; TA_BOTTOM 8, TA_BASELINE 24
    let x = match st.align & 6 {
        6 => x - w / 2,
        2 => x - w,
        _ => x,
    };
    let y = match st.align & 24 {
        24 => y - mt.ascent,
        8 => y - mt.height,
        _ => y,
    };
    if let Some(r) = opaque {
        let bk = dib_pixel(&d, st.bk);
        dib_fill(&d, r, |_| bk);
    } else if st.bk_mode == 2 {
        let bk = dib_pixel(&d, st.bk);
        dib_fill(&d, [x, y, x + w, y + mt.height], |_| bk);
    }
    let [cx0, cy0, cx1, cy1] = clip.unwrap_or([0, 0, d.w as i32, d.h as i32]);
    let color = rgb_of(st.text);
    text::draw(k, s, (x, y), dx.as_deref(), |px, py, cov| {
        if px < cx0.max(0)
            || py < cy0.max(0)
            || px >= cx1.min(d.w as i32)
            || py >= cy1.min(d.h as i32)
        {
            return;
        }
        let old = dib_get(&d, px as u32, py as u32);
        let mix = |sh: u32| {
            let (a, b) = ((old >> sh & 0xff) as f32, (color >> sh & 0xff) as f32);
            ((a + (b - a) * cov).round() as u32) << sh
        };
        dib_put(&d, px as u32, py as u32, mix(16) | mix(8) | mix(0));
    });
}

/// 0x00BBGGRR as 0xRRGGBB.
fn rgb_of(c: u32) -> u32 {
    (c & 0xff) << 16 | (c & 0xff00) | (c >> 16 & 0xff)
}

/// A DIB pixel as 0xRRGGBB.
fn dib_get(d: &Dib, x: u32, y: u32) -> u32 {
    let m = mem();
    let a = d.bits + y * d.stride;
    let e5 = |v: u32| (v << 3) | (v >> 2);
    let e6 = |v: u32| (v << 2) | (v >> 4);
    match d.bpp {
        16 => {
            let v = m.rd16(a + 2 * x) as u32;
            if d.g6 {
                e5(v >> 11) << 16 | e6(v >> 5 & 63) << 8 | e5(v & 31)
            } else {
                e5(v >> 10 & 31) << 16 | e5(v >> 5 & 31) << 8 | e5(v & 31)
            }
        }
        32 => m.rd32(a + 4 * x) & 0xff_ffff,
        _ => 0,
    }
}

/// 0xRRGGBB into a DIB pixel.
fn dib_put(d: &Dib, x: u32, y: u32, c: u32) {
    let a = d.bits + y * d.stride;
    let v = dib_pixel(d, rgb_of(c));
    match d.bpp {
        16 => mem().wr16(a + 2 * x, v as u16),
        32 => mem().wr32(a + 4 * x, v),
        _ => {}
    }
}

/// TextOutA(dc, x, y, text, n)
fn text_out(c: &mut Cpu) -> u32 {
    let (dc, x, y, s, n) = (
        arg(c, 0),
        arg(c, 1) as i32,
        arg(c, 2) as i32,
        arg(c, 3),
        arg(c, 4),
    );
    draw_text(dc, (x, y), &text_arg(s, n), None, None, None);
    1
}

/// The lines DrawTextA lays out: `&` marks removed (DT_NOPREFIX 0x800 keeps them), split at
/// line ends unless DT_SINGLELINE (0x20), and with DT_WORDBREAK (0x10) wrapped at spaces to
/// `width`.
fn text_lines(k: u32, s: &[u8], flags: u32, width: i32) -> Vec<Vec<u8>> {
    let mut t = Vec::with_capacity(s.len());
    let mut it = s.iter().copied().peekable();
    while let Some(b) = it.next() {
        if b == b'&' && flags & 0x800 == 0 {
            if let Some(n) = it.next() {
                t.push(n);
            }
        } else {
            t.push(b);
        }
    }
    let raw: Vec<Vec<u8>> = if flags & 0x20 != 0 {
        vec![
            t.into_iter()
                .filter(|&b| b != b'\r' && b != b'\n')
                .collect(),
        ]
    } else {
        t.split(|&b| b == b'\n')
            .map(|l| l.iter().copied().filter(|&b| b != b'\r').collect())
            .collect()
    };
    if flags & 0x10 == 0 {
        return raw;
    }
    let mut out = Vec::new();
    for l in raw {
        let mut line: Vec<u8> = Vec::new();
        for word in l.split(|&b| b == b' ') {
            let mut try_ = line.clone();
            if !try_.is_empty() {
                try_.push(b' ');
            }
            try_.extend_from_slice(word);
            if !line.is_empty() && text::extent(k, &try_).0 > width {
                out.push(std::mem::take(&mut line));
                line.extend_from_slice(word);
            } else {
                line = try_;
            }
        }
        out.push(line);
    }
    out
}

/// DrawTextA(dc, text, n, rect, flags): lines in the rectangle, left, centred (DT_CENTER 1) or
/// right (DT_RIGHT 2); a single line also centred (DT_VCENTER 4) or at the bottom (DT_BOTTOM
/// 8); cut to the rectangle unless DT_NOCLIP (0x100). DT_CALCRECT (0x400) only measures: the
/// rectangle gets the text's size. Returns the text's height.
pub fn draw_text_a(c: &mut Cpu) -> u32 {
    let (dc, s, n, rect, flags) = (arg(c, 0), arg(c, 1), arg(c, 2), arg(c, 3), arg(c, 4));
    let m = mem();
    let [l, t, r, b] = [0, 4, 8, 12].map(|o| m.rd32(rect + o) as i32);
    let k = dc_font(dc);
    let lh = text::metrics(k).height;
    let lines = text_lines(k, &text_arg(s, n), flags, r - l);
    let h = lh * lines.len() as i32;
    if flags & 0x400 != 0 {
        let w = lines
            .iter()
            .map(|x| text::extent(k, x).0)
            .max()
            .unwrap_or(0);
        m.wr32(rect + 8, (l + w) as u32);
        m.wr32(rect + 12, (t + h) as u32);
        return h as u32;
    }
    let y0 = match (flags & 0x20 != 0, flags & 0xc) {
        (true, 4) | (true, 0xc) => t + (b - t - h) / 2,
        (true, 8) => b - h,
        _ => t,
    };
    let clip = (flags & 0x100 == 0).then_some([l, t, r, b]);
    for (i, line) in lines.iter().enumerate() {
        let w = text::extent(k, line).0;
        let x = match flags & 3 {
            1 => l + (r - l - w) / 2,
            2 => r - w,
            _ => l,
        };
        draw_text(dc, (x, y0 + lh * i as i32), line, None, None, clip);
    }
    h as u32
}

/// ExtTextOutA(dc, x, y, options, rect, text, n, dx): ETO_OPAQUE (2) fills the rectangle with
/// the background colour, ETO_CLIPPED (4) cuts the text to it.
fn ext_text_out(c: &mut Cpu) -> u32 {
    let (dc, x, y, opts, rect, s, n, dx) = (
        arg(c, 0),
        arg(c, 1) as i32,
        arg(c, 2) as i32,
        arg(c, 3),
        arg(c, 4),
        arg(c, 5),
        arg(c, 6),
        arg(c, 7),
    );
    let m = mem();
    let r = (rect != 0).then(|| [0, 4, 8, 12].map(|o| m.rd32(rect + o) as i32));
    let s = text_arg(s, n);
    let dx = (dx != 0).then(|| {
        (0..s.len() as u32)
            .map(|k| m.rd32(dx + 4 * k) as i32)
            .collect()
    });
    draw_text(
        dc,
        (x, y),
        &s,
        dx,
        r.filter(|_| opts & 2 != 0),
        r.filter(|_| opts & 4 != 0),
    );
    1
}

/// SelectObject(dc, object): into its slot by kind; the one there before (a stock object
/// when it was the default).
fn select_object(c: &mut Cpu) -> u32 {
    let (dc, h) = (arg(c, 0), arg(c, 1));
    let mut objs = OBJECTS.lock().unwrap_or_else(|e| e.into_inner());
    let kind = objs.get(&h).copied();
    let Some(Obj::Dc(d)) = objs.get_mut(&dc) else {
        return 0x6600;
    };
    let (slot, default) = match kind {
        Some(Obj::Bitmap(_) | Obj::Dib(_)) => (&mut d.bitmap, 0x6680),
        Some(Obj::Brush(_)) => (&mut d.brush, STOCK),
        Some(Obj::Pen(_)) => (&mut d.pen, STOCK + 4 * 7),
        Some(Obj::Font(_)) => (&mut d.font, STOCK + 4 * 13),
        _ => return 0x6600,
    };
    match std::mem::replace(slot, h) {
        0 => default,
        old => old,
    }
}

/// CreateDIBSection(dc, BITMAPINFO*, usage, &bits, section, offset): pixels from the guest
/// heap, rows from the top as the game sees them (a bottom-up DIB is kept top-down too: our
/// drawing knows no other).
fn create_dib_section(c: &mut Cpu) -> u32 {
    let (bmi, out) = (arg(c, 1), arg(c, 3));
    let m = mem();
    let (w, h, bpp, compression) = (
        m.rd32(bmi + 4),
        m.rd32(bmi + 8) as i32,
        m.rd16(bmi + 14) as u32,
        m.rd32(bmi + 16),
    );
    // BI_BITFIELDS: the green mask tells 565 from 555
    let g6 = bpp == 16 && compression == 3 && m.rd32(bmi + 44) == 0x07e0;
    let stride = (w * bpp).div_ceil(32) * 4;
    let bits = super::alloc(stride * h.unsigned_abs());
    if out != 0 {
        m.wr32(out, bits);
    }
    new_obj(Obj::Dib(Dib {
        bits,
        w,
        h: h.unsigned_abs(),
        stride,
        bpp,
        g6,
    }))
}

/// The DIB selected into a DC.
pub fn dib_of(dc: u32) -> Option<Dib> {
    match obj(dc)? {
        Obj::Dc(d) => match obj(d.bitmap)? {
            Obj::Dib(d) => Some(d),
            _ => None,
        },
        _ => None,
    }
}

/// A COLORREF (0x00BBGGRR) as a pixel of the DIB.
fn dib_pixel(d: &Dib, c: u32) -> u32 {
    let (r, g, b) = (c & 0xff, c >> 8 & 0xff, c >> 16 & 0xff);
    match d.bpp {
        16 if d.g6 => (r >> 3) << 11 | (g >> 2) << 5 | b >> 3,
        16 => (r >> 3) << 10 | (g >> 3) << 5 | b >> 3,
        _ => r << 16 | g << 8 | b,
    }
}

/// Fill a rectangle of a DIB with `f(old pixel)`.
fn dib_fill(d: &Dib, [x0, y0, x1, y1]: [i32; 4], f: impl Fn(u32) -> u32) {
    let m = mem();
    let (x0, x1) = (x0.max(0) as u32, (x1.max(0) as u32).min(d.w));
    let (y0, y1) = (y0.max(0) as u32, (y1.max(0) as u32).min(d.h));
    for y in y0..y1 {
        let row = d.bits + y * d.stride;
        for x in x0..x1 {
            match d.bpp {
                16 => m.wr16(row + 2 * x, f(m.rd16(row + 2 * x) as u32) as u16),
                32 => m.wr32(row + 4 * x, f(m.rd32(row + 4 * x))),
                _ => {}
            }
        }
    }
}

/// PatBlt(dc, x, y, w, h, rop): BLACKNESS, WHITENESS, PATCOPY (the brush), PATINVERT,
/// DSTINVERT on a DIB.
fn pat_blt(c: &mut Cpu) -> u32 {
    let (dc, x, y, w, h, rop) = (
        arg(c, 0),
        arg(c, 1) as i32,
        arg(c, 2) as i32,
        arg(c, 3) as i32,
        arg(c, 4) as i32,
        arg(c, 5),
    );
    let Some(d) = dib_of(dc) else { return 1 };
    let brush = match obj(dc) {
        Some(Obj::Dc(d)) => match obj(d.brush) {
            Some(Obj::Brush(c)) => c,
            _ => 0xffffff,
        },
        _ => 0xffffff,
    };
    let r = [x, y, x + w, y + h];
    let (black, white, pat) = (
        dib_pixel(&d, 0),
        dib_pixel(&d, 0xffffff),
        dib_pixel(&d, brush),
    );
    let mask = if d.bpp == 16 { 0xffff } else { 0xff_ffff };
    match rop {
        0x42 => dib_fill(&d, r, |_| black),
        0xff0062 => dib_fill(&d, r, |_| white),
        0xf00021 => dib_fill(&d, r, |_| pat),
        0x5a0049 => dib_fill(&d, r, |p| p ^ pat),
        0x550009 => dib_fill(&d, r, |p| !p & mask),
        _ => log!("warn: PatBlt rop {rop:#x} not done"),
    }
    1
}

fn delete_object(c: &mut Cpu) -> u32 {
    OBJECTS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&arg(c, 0))
        .is_some() as u32
}

fn get_object_type(c: &mut Cpu) -> u32 {
    obj(arg(c, 0)).map_or(0, |o| o.kind())
}

/// The stock objects (GetStockObject's index): fixed handles below the others.
const STOCK: u32 = 0x6600;

fn get_stock_object(c: &mut Cpu) -> u32 {
    let k = arg(c, 0);
    let o = match k {
        0..=5 => Obj::Brush([0xffffff, 0xc0c0c0, 0x808080, 0x404040, 0, 0][k as usize]),
        6..=8 => Obj::Pen([0xffffff, 0, 0][k as usize - 6]),
        15 => Obj::Palette,
        _ => Obj::Font(text::add(text::Spec {
            face: "Arial".into(),
            height: if k == 13 { -11 } else { 16 },
            weight: 400,
            italic: false,
        })),
    };
    let h = STOCK + 4 * k;
    OBJECTS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .entry(h)
        .or_insert(o);
    h
}

/// GetSysColor's colours (Windows 98's standard scheme), by COLOR_ index.
pub const SYS_COLORS: [u32; 31] = [
    0xc0c0c0, 0x808000, 0x800000, 0x808080, 0xc0c0c0, 0xffffff, 0x000000, 0x000000, 0x000000,
    0xffffff, 0xc0c0c0, 0xc0c0c0, 0x808080, 0x800000, 0xffffff, 0xc0c0c0, 0x808080, 0x808080,
    0x000000, 0xc0c0c0, 0xffffff, 0x000000, 0xdfdfdf, 0x000000, 0xe1ffff, 0x000000, 0x800000,
    0xd08410, 0x808080, 0x800000, 0xc0c0c0,
];

pub fn get_sys_color(c: &mut Cpu) -> u32 {
    SYS_COLORS.get(arg(c, 0) as usize).copied().unwrap_or(0)
}

/// GetSysColorBrush: one brush per colour index, made once.
pub fn get_sys_color_brush(c: &mut Cpu) -> u32 {
    static MADE: Mutex<BTreeMap<u32, u32>> = Mutex::new(BTreeMap::new());
    let k = arg(c, 0);
    let color = SYS_COLORS.get(k as usize).copied().unwrap_or(0);
    *MADE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .entry(k)
        .or_insert_with(|| new_obj(Obj::Brush(color)))
}

/// GetObjectA for a pen or brush: LOGPEN / LOGBRUSH; others answer 0.
fn get_object(c: &mut Cpu) -> u32 {
    let (h, n, p) = (arg(c, 0), arg(c, 1), arg(c, 2));
    let m = mem();
    match obj(h) {
        Some(Obj::Brush(color)) if n >= 12 => {
            m.wr32(p, 0); // BS_SOLID
            m.wr32(p + 4, color);
            m.wr32(p + 8, 0);
            12
        }
        Some(Obj::Pen(color)) if n >= 16 => {
            m.slice(p, 16).fill(0);
            m.wr32(p + 12, color);
            16
        }
        Some(Obj::Bitmap(info)) if n >= 24 && info != 0 => {
            // BITMAP: type, width, height, bytes a row (even), planes, bits a pixel, bits
            let (w, h, bpp) = (
                m.rd32(info + 4),
                m.rd32(info + 8) as i32,
                m.rd16(info + 14) as u32,
            );
            let colors = match m.rd32(info + 32) {
                0 if bpp <= 8 => 1 << bpp,
                k => k,
            };
            m.wr32(p, 0);
            m.wr32(p + 4, w);
            m.wr32(p + 8, h.unsigned_abs());
            m.wr32(p + 12, (w * bpp).div_ceil(16) * 2);
            m.wr16(p + 16, 1);
            m.wr16(p + 18, bpp as u16);
            m.wr32(p + 20, info + m.rd32(info) + 4 * colors);
            24
        }
        Some(Obj::Dib(d)) if n >= 24 => {
            // BITMAP with the pixels' address; DIBSECTION (84 bytes) adds the header (top-down,
            // as kept), the 16-bit masks, no section
            m.wr32(p, 0);
            m.wr32(p + 4, d.w);
            m.wr32(p + 8, d.h);
            m.wr32(p + 12, d.stride);
            m.wr16(p + 16, 1);
            m.wr16(p + 18, d.bpp as u16);
            m.wr32(p + 20, d.bits);
            if n < 84 {
                return 24;
            }
            m.slice(p + 24, 60).fill(0);
            m.wr32(p + 24, 40);
            m.wr32(p + 28, d.w);
            m.wr32(p + 32, (d.h as i32).wrapping_neg() as u32);
            m.wr16(p + 36, 1);
            m.wr16(p + 38, d.bpp as u16);
            if d.bpp == 16 {
                let masks = if d.g6 {
                    [0xf800, 0x07e0, 0x001f]
                } else {
                    [0x7c00, 0x03e0, 0x001f]
                };
                m.wr32(p + 40, 3); // BI_BITFIELDS
                for (k, v) in masks.into_iter().enumerate() {
                    m.wr32(p + 64 + 4 * k as u32, v);
                }
            }
            m.wr32(p + 44, d.stride * d.h);
            84
        }
        _ => 0,
    }
}

/// LoadBitmapA(module, name): a bitmap of the module's resources.
pub fn load_bitmap(c: &mut Cpu) -> u32 {
    let (h, name) = (arg(c, 0), arg(c, 1));
    match super::resource_in(
        h,
        super::ResId::Id(2),
        super::ResId::from_guest(name, super::cstr),
    ) {
        Some((at, _)) => new_obj(Obj::Bitmap(at)),
        None => 0,
    }
}

fn ret1(_: &mut Cpu) -> u32 {
    1
}

pub const IMPORTS: &[Import] = &[
    ("GetDeviceCaps", 2, get_device_caps),
    ("GetSystemPaletteEntries", 4, |_| 0), // no palette device: no entries
    ("GetTextMetricsA", 2, get_text_metrics),
    ("GetStockObject", 1, get_stock_object),
    ("CreateFontA", 14, create_font),
    ("CreateFontIndirectA", 1, create_font_indirect),
    ("CreateSolidBrush", 1, create_solid_brush),
    ("CreatePen", 3, create_pen),
    ("CreateCompatibleDC", 1, create_compatible_dc),
    ("DeleteDC", 1, delete_object),
    ("GetObjectType", 1, get_object_type),
    ("GetObjectA", 3, get_object),
    ("SelectObject", 2, select_object),
    ("CreateDIBSection", 6, create_dib_section),
    ("DeleteObject", 1, delete_object),
    ("TextOutA", 5, text_out),
    ("ExtTextOutA", 8, ext_text_out),
    ("GetTextExtentPoint32A", 4, get_text_extent_point),
    ("GetTextExtentPointA", 4, get_text_extent_point),
    ("GetCharWidthA", 4, get_char_width),
    ("GetTextColor", 1, get_text_color),
    ("GetBkColor", 1, get_bk_color),
    ("GetBkMode", 1, get_bk_mode),
    ("GetTextAlign", 1, get_text_align),
    ("SetTextAlign", 2, set_text_align),
    // drawing on a window's DC is not shown yet (the game's own screens are DirectDraw and
    // Glide)
    ("BitBlt", 9, ret1),
    ("StretchBlt", 11, ret1),
    ("PatBlt", 6, pat_blt),
    ("StretchDIBits", 13, ret1),
    ("SetBkColor", 2, set_bk_color),
    ("SetBkMode", 2, set_bk_mode),
    ("SetTextColor", 2, set_text_color),
];
