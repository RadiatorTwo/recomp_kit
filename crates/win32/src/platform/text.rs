//! Fonts for GDI text (CreateFontA, TextOutA, GetTextExtentPoint32A): TrueType files drawn
//! with ab_glyph. A font is looked up by its family name in the game folder's .ttf files (the
//! fonts a game installed into Windows), then in Windows' own fonts, else the first one found.
//! Advances are whole pixels, as GDI's; edges are smooth (coverage).

use ab_glyph::{Font, FontVec, GlyphId, PxScale, ScaleFont, point};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

/// A font as CreateFont describes it (LOGFONT).
#[derive(Clone, Debug)]
pub struct Spec {
    pub face: String,
    /// LOGFONT height: negative the character height (em), positive the cell height
    pub height: i32,
    pub weight: u32,
    pub italic: bool,
}

struct Fonts {
    specs: Vec<Spec>,
    /// loaded files, by path (None: could not be read)
    files: BTreeMap<PathBuf, Option<Arc<FontVec>>>,
    /// the game folder's fonts: family name (lower case), bold, path
    found: Option<Vec<(String, bool, PathBuf)>>,
}

static FONTS: Mutex<Fonts> = Mutex::new(Fonts { specs: Vec::new(), files: BTreeMap::new(), found: None });

fn fonts() -> MutexGuard<'static, Fonts> {
    FONTS.lock().unwrap_or_else(|e| e.into_inner())
}

/// A new font; its number for `Obj::Font`.
pub fn add(spec: Spec) -> u32 {
    let mut f = fonts();
    f.specs.push(spec);
    f.specs.len() as u32 - 1
}

fn load(f: &mut Fonts, path: &Path) -> Option<Arc<FontVec>> {
    f.files
        .entry(path.to_path_buf())
        .or_insert_with(|| std::fs::read(path).ok().and_then(|d| FontVec::try_from_vec(d).ok()).map(Arc::new))
        .clone()
}

/// The family name of a font file and whether it is bold (its subfamily).
fn family(data: &[u8]) -> Option<(String, bool)> {
    let face = ttf_parser::Face::parse(data, 0).ok()?;
    let name = |id: u16| {
        face.names().into_iter().filter(|n| n.name_id == id && n.is_unicode()).find_map(|n| n.to_string())
    };
    let fam = name(1)?;
    let sub = name(2).unwrap_or_default().to_ascii_lowercase();
    Some((fam.to_ascii_lowercase(), sub.contains("bold")))
}

/// Windows' file for a family, when it has one.
fn windows_font(face: &str, bold: bool) -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("WINDIR")?).join("Fonts");
    let file = match (face, bold) {
        ("arial", false) => "arial.ttf",
        ("arial", true) => "arialbd.ttf",
        ("times new roman", false) => "times.ttf",
        ("times new roman", true) => "timesbd.ttf",
        ("courier new", false) => "cour.ttf",
        ("courier new", true) => "courbd.ttf",
        ("verdana", false) => "verdana.ttf",
        ("verdana", true) => "verdanab.ttf",
        ("tahoma", _) => "tahoma.ttf",
        _ => return None,
    };
    Some(dir.join(file)).filter(|p| p.exists())
}

/// The file for a font: the game folder's of that family (bold if wanted and there), else
/// Windows', else any of the game folder's, else Windows' Arial.
fn file_for(f: &mut Fonts, spec: &Spec) -> Option<Arc<FontVec>> {
    if f.found.is_none() {
        let mut found = Vec::new();
        let dir = super::game_dir();
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("ttf")) {
                if let Some((fam, bold)) = std::fs::read(&p).ok().as_deref().and_then(family) {
                    found.push((fam, bold, p));
                }
            }
        }
        found.sort_by(|a, b| a.2.cmp(&b.2));
        f.found = Some(found);
    }
    let face = spec.face.to_ascii_lowercase();
    let bold = spec.weight >= 600;
    let found = f.found.clone().unwrap_or_default();
    let same: Vec<&(String, bool, PathBuf)> = found.iter().filter(|x| x.0 == face).collect();
    let pick = same.iter().find(|x| x.1 == bold).or(same.first()).map(|x| x.2.clone());
    let path = pick
        .or_else(|| windows_font(&face, bold))
        .or_else(|| found.first().map(|x| x.2.clone()))
        .or_else(|| windows_font("arial", bold))?;
    load(f, &path)
}

/// A font ready to measure and draw: the file and its scale.
struct Sized {
    font: Arc<FontVec>,
    scale: PxScale,
}

fn sized(k: u32) -> Option<Sized> {
    let mut f = fonts();
    let spec = f.specs.get(k as usize)?.clone();
    let font = file_for(&mut f, &spec)?;
    // ab_glyph's scale is the cell height (ascent - descent); a negative LOGFONT height is the
    // em height
    let px = match spec.height {
        0 => 16.0,
        h if h > 0 => h as f32,
        h => -h as f32 * font.height_unscaled() / font.units_per_em().unwrap_or(1000.0),
    };
    Some(Sized { font, scale: PxScale::from(px) })
}

/// TEXTMETRIC's numbers: cell height, ascent, descent, average and widest advance.
pub struct Metrics {
    pub height: i32,
    pub ascent: i32,
    pub descent: i32,
    pub internal_leading: i32,
    pub avg_width: i32,
    pub max_width: i32,
}

pub fn metrics(k: u32) -> Metrics {
    let Some(s) = sized(k) else {
        return Metrics { height: 16, ascent: 13, descent: 3, internal_leading: 0, avg_width: 8, max_width: 16 };
    };
    let f = s.font.as_scaled(s.scale);
    let (ascent, descent) = (f.ascent().round() as i32, (-f.descent()).round() as i32);
    let adv = |c: char| f.h_advance(f.glyph_id(c)).round() as i32;
    let em = s.scale.y * s.font.units_per_em().unwrap_or(1000.0) / s.font.height_unscaled();
    let max_width = (0x20u8..0x7f).map(|b| adv(b as char)).max().unwrap_or(0);
    Metrics { height: ascent + descent, ascent, descent, internal_leading: (ascent + descent - em.round() as i32).max(0), avg_width: adv('x'), max_width }
}

/// A byte of text (Windows-1252 read as Latin-1) as a character.
fn ch(b: u8) -> char {
    b as char
}

/// Whole-pixel advance of each byte.
pub fn advances(k: u32, text: &[u8]) -> Vec<i32> {
    let Some(s) = sized(k) else { return text.iter().map(|_| 8).collect() };
    let f = s.font.as_scaled(s.scale);
    text.iter().map(|&b| f.h_advance(f.glyph_id(ch(b))).round() as i32).collect()
}

/// The size of a text: the sum of its advances, the cell height.
pub fn extent(k: u32, text: &[u8]) -> (i32, i32) {
    (advances(k, text).iter().sum(), metrics(k).height)
}

/// Draw a text with the top of its cell at `y` and its first character at `x`; `dx` the
/// advances to use (ExtTextOutA's), else the font's. `put(x, y, coverage)` for each pixel.
pub fn draw(k: u32, text: &[u8], (x, y): (i32, i32), dx: Option<&[i32]>, mut put: impl FnMut(i32, i32, f32)) {
    let Some(s) = sized(k) else { return };
    let f = s.font.as_scaled(s.scale);
    let base = y as f32 + f.ascent().round();
    let own = advances(k, text);
    let mut pen = x;
    for (i, &b) in text.iter().enumerate() {
        let id: GlyphId = f.glyph_id(ch(b));
        let glyph = id.with_scale_and_position(s.scale, point(pen as f32, base));
        if let Some(o) = s.font.outline_glyph(glyph) {
            let r = o.px_bounds();
            o.draw(|gx, gy, c| {
                if c > 0.0 {
                    put(r.min.x as i32 + gx as i32, r.min.y as i32 + gy as i32, c.min(1.0));
                }
            });
        }
        pen += dx.and_then(|d| d.get(i).copied()).unwrap_or(own[i]);
    }
}
