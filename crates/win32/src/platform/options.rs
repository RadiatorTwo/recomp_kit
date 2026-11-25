//! The port's own settings menu (`Game::options_key`, F10): pages of rows the game gives
//! (`Game::options`), drawn into the window's overlay over the game in the style of the
//! dialogs (`dialog::draw`). While it is open the game stands still: `window::pump` waits in
//! it and the guest's clock is put back to where it was when it opened. The game gets none of
//! the input but keys and buttons going up. Changes are used at once and saved.
//!
//! Keys: Up / Down choose a row, Left / Right change it, Enter / Space the next choice,
//! Page Up / Page Down (or Tab) the page, Escape or the menu's key close it. The mouse: a click
//! on a page's tab turns to it; on a value's left half back, else on. A binding row (`bind`)
//! waits after Enter or a click for the key (or pad control, `pad`) to bind; Escape gives up,
//! Delete or Backspace unbinds. A page longer than the panel scrolls, in smaller text.

use super::dialog::font::{FIRST, GLYPHS};
use super::dialog::{OV_H, OV_W};
use super::window::Input;
use super::{display, glide, stats};

/// One page of the menu (a tab).
pub struct Page {
    pub title: &'static str,
    pub rows: Vec<Row>,
}

/// One row: its name, a note shown while it is chosen, and what it changes.
pub struct Row {
    pub name: &'static str,
    pub note: &'static str,
    pub kind: Kind,
}

pub enum Kind {
    /// the choices' texts and the one in effect; use the one at an index (and save it)
    Choice(Box<dyn Fn() -> (Vec<String>, usize)>, Box<dyn Fn(usize)>),
    /// the window: borderless fullscreen or not (done by the window)
    Fullscreen,
    /// the window's size and the resolution drawn at (done by the window)
    WindowSize,
    /// a binding: its text, bind what was pressed, unbind
    Bind(Box<dyn Fn() -> String>, Box<dyn Fn(Bound)>, Box<dyn Fn()>),
}

/// What a binding row gets: a key (its DirectInput code) or a pad control (the game's
/// numbering, as `pad` gets it).
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Bound {
    Key(u8),
    Pad(u32),
}

impl Row {
    pub fn choice(
        name: &'static str,
        note: &'static str,
        get: impl Fn() -> (Vec<String>, usize) + 'static,
        set: impl Fn(usize) + 'static,
    ) -> Row {
        Row {
            name,
            note,
            kind: Kind::Choice(Box::new(get), Box::new(set)),
        }
    }

    /// A binding: `text` shows it, `bind` adds what was pressed, `clear` removes all.
    pub fn bind(
        name: &'static str,
        note: &'static str,
        text: impl Fn() -> String + 'static,
        bind: impl Fn(Bound) + 'static,
        clear: impl Fn() + 'static,
    ) -> Row {
        Row {
            name,
            note,
            kind: Kind::Bind(Box::new(text), Box::new(bind), Box::new(clear)),
        }
    }

    /// Off / On.
    pub fn on_off(
        name: &'static str,
        note: &'static str,
        get: impl Fn() -> bool + 'static,
        set: impl Fn(bool) + 'static,
    ) -> Row {
        Row::choice(
            name,
            note,
            move || (vec!["Off".into(), "On".into()], get() as usize),
            move |k| set(k == 1),
        )
    }
}

/// The framework's own rows, for a game's display page.
pub fn fullscreen() -> Row {
    Row {
        name: "Fullscreen",
        note: "Borderless over the whole display (Alt+Enter too)",
        kind: Kind::Fullscreen,
    }
}

pub fn window_size() -> Row {
    Row {
        name: "Resolution",
        note: "The window's size and what is drawn at; Auto: the window's",
        kind: Kind::WindowSize,
    }
}

pub fn anti_aliasing() -> Row {
    Row::choice(
        "Anti-aliasing",
        "Smooth edges (multisampling)",
        || {
            let c = glide::MSAA_CHOICES;
            let text = |n: u32| {
                if n == 0 {
                    "Off".into()
                } else {
                    format!("{n}x")
                }
            };
            let cur = c.iter().position(|&n| n == glide::msaa()).unwrap_or(0);
            (c.iter().map(|&n| text(n)).collect(), cur)
        },
        |k| glide::set_msaa(glide::MSAA_CHOICES[k]),
    )
}

pub fn textures() -> Row {
    Row::choice(
        "Textures",
        "Filtering of the textures the game filters",
        || {
            let c = glide::FILTER_CHOICES;
            let text = |n: u32| match n {
                0 => "Original".to_string(),
                1 => "Trilinear".into(),
                n => format!("Anisotropic {n}x"),
            };
            let cur = c.iter().position(|&n| n == glide::filtering()).unwrap_or(0);
            (c.iter().map(|&n| text(n)).collect(), cur)
        },
        |k| glide::set_filtering(glide::FILTER_CHOICES[k]),
    )
}

pub fn frame_rate_display() -> Row {
    Row::on_off(
        "Frame rate display",
        "Pictures a second in the corner (its key too)",
        stats::on,
        |on| {
            if on != stats::on() {
                stats::toggle();
            }
        },
    )
}

/// Window sizes offered (those that fit the display): 4:3, then 16:9 (widescreen).
const SIZES: [(u32, u32); 11] = [
    (960, 720),
    (1280, 960),
    (1440, 1080),
    (1600, 1200),
    (1920, 1440),
    (1280, 720),
    (1600, 900),
    (1920, 1080),
    (2560, 1440),
    (3440, 1440),
    (3840, 2160),
];

/// What the window does for a change.
pub enum Action {
    Fullscreen(bool),
    /// the window's size (None: as it is, drawn at its size)
    Window(Option<(u32, u32)>),
}

pub struct Menu {
    pages: Vec<Page>,
    page: usize,
    sel: usize,
    /// the row under the mouse
    hot: Option<usize>,
    /// the mouse in overlay pixels
    mouse: (i32, i32),
    /// the display's size (larger window sizes are not offered)
    display: (u32, u32),
    /// the window is fullscreen
    fullscreen: bool,
    /// the first row shown (a long page scrolls)
    top: usize,
    /// the binding row waiting for a key or pad control
    capture: Option<usize>,
}

impl Menu {
    pub fn new(pages: Vec<Page>, display: (u32, u32), fullscreen: bool) -> Menu {
        Menu {
            pages,
            page: 0,
            sel: 0,
            hot: None,
            mouse: (-1, -1),
            display,
            fullscreen,
            top: 0,
            capture: None,
        }
    }

    /// The rows of the page shown, and a Close row after them (its index: `rows.len()`).
    fn rows(&self) -> &[Row] {
        &self.pages[self.page].rows
    }

    fn count(&self) -> usize {
        self.rows().len() + 1
    }

    fn sizes(&self) -> Vec<Option<(u32, u32)>> {
        let mut v = vec![None];
        v.extend(
            SIZES
                .iter()
                .filter(|&&(w, h)| w <= self.display.0 && h <= self.display.1)
                .map(|&z| Some(z)),
        );
        let cur = display::chosen();
        if !v.contains(&cur) {
            v.push(cur);
        }
        v
    }

    /// A row's choices and the one in effect.
    fn choices(&self, r: &Row) -> (Vec<String>, usize) {
        match &r.kind {
            Kind::Choice(get, _) => get(),
            Kind::Fullscreen => (vec!["Off".into(), "On".into()], self.fullscreen as usize),
            Kind::WindowSize => {
                let v = self.sizes();
                let cur = display::chosen();
                let k = v.iter().position(|&z| z == cur).unwrap_or(0);
                let text =
                    |z: &Option<(u32, u32)>| z.map_or("Auto".into(), |(w, h)| format!("{w}x{h}"));
                (v.iter().map(text).collect(), k)
            }
            Kind::Bind(..) => (Vec::new(), 0),
        }
    }

    /// Move row `k`'s choice by `d` (wrapping) and use it.
    fn change(&mut self, k: usize, d: i32) -> Option<Action> {
        let r = self.rows().get(k)?;
        let (c, i) = self.choices(r);
        if c.is_empty() {
            return None;
        }
        let i = (i as i32 + d).rem_euclid(c.len() as i32) as usize;
        match &r.kind {
            Kind::Choice(_, set) => {
                set(i);
                None
            }
            Kind::Fullscreen => {
                self.fullscreen = i == 1;
                Some(Action::Fullscreen(self.fullscreen))
            }
            Kind::WindowSize => Some(Action::Window(self.sizes()[i])),
            Kind::Bind(..) => None,
        }
    }

    /// Enter or a click on row `k`: Close closes, a binding waits for a key, the others go on.
    fn enter(&mut self, k: usize, d: i32, out: &mut Vec<Action>) -> Result<(), ()> {
        self.sel = k;
        match self.rows().get(k).map(|r| &r.kind) {
            None => return Err(()),
            Some(Kind::Bind(..)) => self.capture = Some(k),
            Some(_) => out.extend(self.change(k, d)),
        }
        Ok(())
    }

    fn unbind(&mut self) {
        if let Some(Kind::Bind(_, _, clear)) = self.rows().get(self.sel).map(|r| &r.kind) {
            clear();
        }
    }

    /// Choose row `k` and scroll it into view.
    fn select(&mut self, k: usize) {
        self.sel = k;
        let shown = self.shown();
        if k < self.top {
            self.top = k;
        } else if k >= self.top + shown {
            self.top = k + 1 - shown;
        }
    }

    /// A pad control pressed while open: bound when a binding waits for one, else the D-pad
    /// (`Up` `Down` `Left` `Right`), `A` (Enter), `B` (back), `X` (unbind) work the menu.
    pub fn pad(&mut self, control: u32, menu: PadMenu, out: &mut Vec<Action>) -> Result<(), ()> {
        if let Some(k) = self.capture.take() {
            if let Some(Kind::Bind(_, bind, _)) = self.rows().get(k).map(|r| &r.kind) {
                bind(Bound::Pad(control));
            }
            return Ok(());
        }
        let key = |vk| Input::Key {
            vk,
            scan: 0,
            down: true,
            repeat: false,
        };
        match menu {
            PadMenu::Up => self.input(&key(0x26), out),
            PadMenu::Down => self.input(&key(0x28), out),
            PadMenu::Left => self.input(&key(0x25), out),
            PadMenu::Right => self.input(&key(0x27), out),
            PadMenu::Enter => self.input(&key(0x0d), out),
            PadMenu::Back => Err(()),
            PadMenu::Unbind => {
                self.unbind();
                Ok(())
            }
            PadMenu::PageUp => self.input(&key(0x21), out),
            PadMenu::PageDown => self.input(&key(0x22), out),
            PadMenu::None => Ok(()),
        }
    }

    fn turn(&mut self, d: i32) {
        self.page = (self.page as i32 + d).rem_euclid(self.pages.len() as i32) as usize;
        self.sel = 0;
        self.top = 0;
        self.hot = None;
        self.capture = None;
    }

    /// Input while open (`Move` in overlay pixels); `Err(())` closes the menu. The actions are
    /// the window's to do.
    pub fn input(&mut self, i: &Input, out: &mut Vec<Action>) -> Result<(), ()> {
        let n = self.count();
        // a binding waiting: the key goes to it, Escape or a click gives up
        if let Some(k) = self.capture {
            match *i {
                Input::Key {
                    vk,
                    scan,
                    down: true,
                    ..
                } => {
                    self.capture = None;
                    if vk != 0x1b {
                        if let Some(Kind::Bind(_, bind, _)) = self.rows().get(k).map(|r| &r.kind) {
                            bind(Bound::Key(super::dinput::dik(vk, scan)));
                        }
                    }
                }
                Input::Button(_, true) => self.capture = None,
                _ => {}
            }
            return Ok(());
        }
        match *i {
            Input::Key { vk, down: true, .. } => match vk {
                0x1b => return Err(()),
                0x26 => self.select((self.sel + n - 1) % n),
                0x28 => self.select((self.sel + 1) % n),
                0x25 => out.extend(self.change(self.sel, -1)),
                0x27 => out.extend(self.change(self.sel, 1)),
                0x21 => self.turn(-1),
                0x22 | 0x09 => self.turn(1),
                0x0d | 0x20 => return self.enter(self.sel, 1, out),
                0x2e | 0x08 => self.unbind(),
                _ => {}
            },
            Input::Move(x, y) => {
                self.mouse = (x, y);
                self.hot = self.row_at(x, y).map(|(k, _)| k);
            }
            Input::Button(0, true) => {
                let (x, y) = self.mouse;
                if let Some(p) = self.tab_at(x, y) {
                    self.turn(p as i32 - self.page as i32);
                } else if let Some((k, left)) = self.row_at(x, y) {
                    return self.enter(k, if left { -1 } else { 1 }, out);
                }
            }
            // the wheel scrolls a long page, else changes the row's value
            Input::Wheel(w) if self.shown() < n => {
                let most = n - self.shown();
                self.top = (self.top as i32 - w.signum()).clamp(0, most as i32) as usize;
            }
            Input::Wheel(w) => out.extend(self.change(self.sel, w.signum())),
            _ => {}
        }
        Ok(())
    }

    /// The menu as ARGB pixels, `OV_W` x `OV_H`.
    pub fn render(&self, key: &str) -> Vec<u32> {
        let mut cv = Canvas {
            px: vec![DIM; (OV_W * OV_H) as usize],
        };
        let p = self.panel();
        cv.fill(p, PANEL);
        cv.frame(p, EDGE_HI);
        cv.frame(inset(p, 1), EDGE);
        for (k, page) in self.pages.iter().enumerate() {
            let t = self.tab(k);
            let on = k == self.page;
            cv.fill(t, if on { FACE_HOT } else { CAPTION });
            if on {
                cv.frame(t, EDGE_HI);
            }
            cv.label(t, page.title, if on { TEXT } else { TEXT_OFF }, 1, 2);
        }
        let rows = self.rows();
        let scale = self.scale();
        for k in 0..self.count() {
            let Some(rr) = self.row_rect(k) else {
                continue;
            };
            let sel = k == self.sel;
            if sel || self.hot == Some(k) {
                cv.fill(rr, if sel { FACE_HOT } else { FACE });
            }
            if sel {
                cv.frame(rr, EDGE_HI);
            }
            let Some(r) = rows.get(k) else {
                cv.label(rr, "Close", TEXT, 1, scale);
                continue;
            };
            cv.label([rr[0] + 12, rr[1], rr[2], rr[3]], r.name, TEXT, 0, scale);
            let v = value_rect(rr);
            if let Kind::Bind(text, ..) = &r.kind {
                if self.capture == Some(k) {
                    cv.label(v, "Press...", MARK, 1, scale);
                } else {
                    cv.label(v, &text(), TEXT, 1, scale);
                }
                continue;
            }
            let (c, i) = self.choices(r);
            cv.label(v, "<", MARK, 0, scale);
            cv.label(v, ">", MARK, 2, scale);
            if let Some(t) = c.get(i) {
                cv.label(v, t, TEXT, 1, scale);
            }
        }
        // more rows above or below
        if self.top > 0 {
            cv.label(self.list_edge(false), "^", MARK, 1, 1);
        }
        if self.top + self.shown() < self.count() {
            cv.label(self.list_edge(true), "v", MARK, 1, 1);
        }
        let note = match rows.get(self.sel) {
            Some(Row {
                kind: Kind::Bind(..),
                note,
                ..
            }) if note.is_empty() => "Enter: bind a key   Delete: none",
            Some(r) => r.note,
            None => "",
        };
        cv.label([p[0], p[3] - 46, p[2], p[3] - 30], note, TEXT_OFF, 1, 1);
        let help = format!("Arrows: choose and change   PgUp/PgDn: page   {key} or Esc: close");
        cv.label([p[0], p[3] - 26, p[2], p[3] - 8], &help, TEXT_OFF, 1, 1);
        cv.px
    }

    // Layout (overlay pixels).

    fn panel(&self) -> [i32; 4] {
        let most = self
            .pages
            .iter()
            .map(|p| p.rows.len() + 1)
            .max()
            .unwrap_or(1) as i32;
        let h = (TOP + most * ROW_H + BOTTOM).min(MAX_H);
        let (x, y) = ((OV_W - PANEL_W) / 2, (OV_H - h) / 2);
        [x, y, x + PANEL_W, y + h]
    }

    /// The page's rows fit at full size.
    fn roomy(&self) -> bool {
        let p = self.panel();
        TOP + self.count() as i32 * ROW_H + BOTTOM <= p[3] - p[1]
    }

    fn row_h(&self) -> i32 {
        if self.roomy() { ROW_H } else { ROW_H_SMALL }
    }

    fn scale(&self) -> i32 {
        if self.roomy() { 2 } else { 1 }
    }

    /// How many rows the panel shows.
    fn shown(&self) -> usize {
        let p = self.panel();
        (((p[3] - p[1] - TOP - BOTTOM) / self.row_h()).max(1) as usize).min(self.count())
    }

    /// Where the marks for more rows go: above the first row shown or below the last.
    fn list_edge(&self, below: bool) -> [i32; 4] {
        let p = self.panel();
        let y = if below {
            p[1] + TOP + self.shown() as i32 * self.row_h()
        } else {
            p[1] + TOP - 9
        };
        [p[0], y, p[2], y + 8]
    }

    fn tab(&self, k: usize) -> [i32; 4] {
        let p = self.panel();
        let n = self.pages.len() as i32;
        let w = (p[2] - p[0] - 4) / n;
        let x = p[0] + 2 + k as i32 * w;
        [x, p[1] + 2, x + w, p[1] + 24]
    }

    /// Row `k`'s rectangle, None when it is scrolled out of view.
    fn row_rect(&self, k: usize) -> Option<[i32; 4]> {
        let at = k.checked_sub(self.top).filter(|&a| a < self.shown())?;
        let p = self.panel();
        let h = self.row_h();
        let y = p[1] + TOP + at as i32 * h;
        Some([p[0] + 10, y, p[2] - 10, y + h - 2])
    }

    fn tab_at(&self, x: i32, y: i32) -> Option<usize> {
        (0..self.pages.len()).find(|&k| inside(self.tab(k), x, y))
    }

    /// The row at (x, y), and whether it is on the left half of its value.
    fn row_at(&self, x: i32, y: i32) -> Option<(usize, bool)> {
        (0..self.count()).find_map(|k| {
            let r = self.row_rect(k)?;
            let v = value_rect(r);
            inside(r, x, y).then_some((k, x < (v[0] + v[2]) / 2))
        })
    }
}

const PANEL_W: i32 = 560;
const ROW_H: i32 = 32;
const ROW_H_SMALL: i32 = 18;
/// the rows' start below the panel's top (under the tabs), the room under them (notes)
const TOP: i32 = 38;
const BOTTOM: i32 = 60;
/// the tallest panel
const MAX_H: i32 = 460;

/// What a pad control does in the menu (the game's port says which control is which).
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum PadMenu {
    Up,
    Down,
    Left,
    Right,
    Enter,
    Back,
    Unbind,
    PageUp,
    PageDown,
    None,
}

fn value_rect(r: [i32; 4]) -> [i32; 4] {
    [r[0] + 270, r[1], r[2] - 10, r[3]]
}

fn inside(r: [i32; 4], x: i32, y: i32) -> bool {
    x >= r[0] && x < r[2] && y >= r[1] && y < r[3]
}

fn inset(r: [i32; 4], d: i32) -> [i32; 4] {
    [r[0] + d, r[1] + d, r[2] - d, r[3] - d]
}

const DIM: u32 = 0x8000_0000;
const PANEL: u32 = 0xf00a_1440;
const CAPTION: u32 = 0xff1c_3c9c;
const EDGE: u32 = 0xff3c_64d2;
const EDGE_HI: u32 = 0xff6a_9cff;
const FACE: u32 = 0xff0c_1e5a;
const FACE_HOT: u32 = 0xff1a_3a9a;
const TEXT: u32 = 0xffe8_f0ff;
const TEXT_OFF: u32 = 0xff8a_9aca;
const MARK: u32 = 0xff7f_d0ff;

struct Canvas {
    px: Vec<u32>,
}

impl Canvas {
    fn put(&mut self, x: i32, y: i32, c: u32) {
        if x >= 0 && x < OV_W && y >= 0 && y < OV_H {
            let p = &mut self.px[(y * OV_W + x) as usize];
            *p = over(*p, c);
        }
    }

    fn fill(&mut self, r: [i32; 4], c: u32) {
        for y in r[1]..r[3] {
            for x in r[0]..r[2] {
                self.put(x, y, c);
            }
        }
    }

    fn frame(&mut self, r: [i32; 4], c: u32) {
        self.fill([r[0], r[1], r[2], r[1] + 1], c);
        self.fill([r[0], r[3] - 1, r[2], r[3]], c);
        self.fill([r[0], r[1] + 1, r[0] + 1, r[3] - 1], c);
        self.fill([r[2] - 1, r[1] + 1, r[2], r[3] - 1], c);
    }

    /// Text at `scale` with its top left at (x, y).
    fn text(&mut self, mut x: i32, y: i32, s: &str, c: u32, scale: i32) {
        for ch in s.chars() {
            let Some((g, l, w)) = glyph(ch) else {
                x += 3 * scale;
                continue;
            };
            for (row, bits) in g.iter().enumerate() {
                for col in l..l + w {
                    if bits >> col & 1 != 0 {
                        let (px, py) = (x + (col - l) * scale, y + row as i32 * scale);
                        self.fill([px, py, px + scale, py + scale], c);
                    }
                }
            }
            x += (w + 1) * scale;
        }
    }

    /// Text in a rectangle: `align` 0 left, 1 centred, 2 right; vertically centred.
    fn label(&mut self, r: [i32; 4], s: &str, c: u32, align: u8, scale: i32) {
        let w = text_width(s) * scale;
        let x = match align {
            1 => (r[0] + r[2] - w) / 2,
            2 => r[2] - w,
            _ => r[0],
        };
        self.text(x, (r[1] + r[3] - 8 * scale) / 2 + scale, s, c, scale);
    }
}

/// `c` (ARGB) over `p` (ARGB).
fn over(p: u32, c: u32) -> u32 {
    let a = c >> 24;
    if a == 255 || p >> 24 == 0 {
        return c;
    }
    let mix = |sh: u32| (((c >> sh) & 0xff) * a + ((p >> sh) & 0xff) * (255 - a)) / 255 << sh;
    (a.max(p >> 24) << 24) | mix(16) | mix(8) | mix(0)
}

/// A character's glyph, its first column and width (None: blank, e.g. a space).
fn glyph(ch: char) -> Option<(&'static [u8; 8], i32, i32)> {
    let ch = if ch.is_ascii() { ch } else { '?' };
    let g = GLYPHS.get((ch as u8).checked_sub(FIRST)? as usize)?;
    let cols = g.iter().fold(0u8, |a, &b| a | b);
    if cols == 0 {
        return None;
    }
    let l = cols.trailing_zeros() as i32;
    let r = 7 - cols.leading_zeros() as i32;
    Some((g, l, r - l + 1))
}

fn text_width(s: &str) -> i32 {
    let w: i32 = s.chars().map(|ch| glyph(ch).map_or(3, |g| g.2 + 1)).sum();
    (w - 1).max(0)
}
