//! Menus and dialogs by gamepad. Many games' menus know only the mouse: here the D-pad or the
//! left stick moves the mouse from item to item (the nearest in that direction, its centre),
//! A is the left button there; a gold frame in the overlay shows the item and the window
//! system's cursor is hidden until the real mouse moves. While a video plays, A is Space
//! (skips it). Input goes in as the window's own (`window::Input`), so the game cannot tell it
//! from the mouse. The game says where its menu's items are (`Game::menu`); with a dialog open
//! the pad's input goes to the dialog (`dialog_events`).

use super::dialog::{OV_H, OV_W};
use super::window::Input;
use super::{dialog, log};
use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};


/// our pad buttons (`dinput`)
const A: u32 = 1 << 3;
const B: u32 = 1 << 2;
const UP: u32 = 1 << 11;
const DOWN: u32 = 1 << 12;
const LEFT: u32 = 1 << 13;
const RIGHT: u32 = 1 << 14;

/// A held direction moves again after this, then every `REPEAT`.
const FIRST_REPEAT: Duration = Duration::from_millis(400);
const REPEAT: Duration = Duration::from_millis(160);
/// how far the stick must go for a direction
const STICK: f32 = 0.5;

fn trace() -> bool {
    static T: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *T.get_or_init(|| recomp::config::env("PAD_LOG").is_some_and(|v| v != "0"))
}

struct State {
    buttons: u32,
    /// the direction held (dx, dy) and when it moves next
    dir: Option<((i32, i32), Instant)>,
    /// A went down as the left button (released as one too)
    clicked: bool,
    /// A went down as Space (a video)
    space: bool,
    /// the item chosen by pad (None once the real mouse moves)
    focus: Option<Rect>,
    /// the item the overlay's frame is around
    drawn: Option<Rect>,
    /// the pad was used after the mouse
    pad_last: bool,
    /// the item the pad came from to `focus`, and the direction
    came: Option<(Rect, (i32, i32))>,
}

static STATE: Mutex<State> = Mutex::new(State {
    buttons: 0,
    dir: None,
    clicked: false,
    space: false,
    focus: None,
    drawn: None,
    pad_last: false,
    came: None,
});

/// An item's rectangle (x, y, w, h).
pub type Rect = (i32, i32, i32, i32);

fn centre(r: &Rect) -> (i32, i32) {
    (r.0 + r.2 / 2, r.1 + r.3 / 2)
}

/// The way the pad came to the item it is on: from which one, in which direction (going the
/// opposite way leads back there).
pub type Came = Option<(usize, (i32, i32))>;

/// The rectangle to go to from `cur` (or, with none, from the point `from`) in direction `d`.
/// First the ones in the same row (column) ahead, the nearest; else the nearest wholly ahead,
/// sideways distance counting double. The opposite of the
/// way the pad came leads back. From outside every rectangle: the one nearest the point.
/// (Dialogs use it too.)
pub fn nearest(rects: &[Rect], cur: Option<usize>, came: Came, from: (i32, i32), d: (i32, i32)) -> Option<usize> {
    let inside = |r: &Rect| from.0 >= r.0 && from.0 < r.0 + r.2 && from.1 >= r.1 && from.1 < r.1 + r.3;
    let Some(cur) = cur.or_else(|| rects.iter().position(inside)) else {
        return (0..rects.len()).min_by_key(|&k| {
            let c = centre(&rects[k]);
            (c.0 - from.0).pow(2) + (c.1 - from.1).pow(2)
        });
    };
    if let Some((back, way)) = came {
        if way == (-d.0, -d.1) && back != cur && back < rects.len() {
            return Some(back);
        }
    }
    let r = rects[cur];
    let rc = centre(&r);
    // a rectangle's extent along an axis (0 x, 1 y)
    let span = |q: &Rect, axis: usize| if axis == 0 { (q.0, q.0 + q.2) } else { (q.1, q.1 + q.3) };
    let (along, across) = if d.0 != 0 { (0, 1) } else { (1, 0) };
    let sign = d.0 + d.1;
    (0..rects.len())
        .filter(|&k| k != cur)
        .filter_map(|k| {
            let q = rects[k];
            let c = centre(&q);
            let (dx, dy) = (c.0 - rc.0, c.1 - rc.1);
            let ahead = dx * d.0 + dy * d.1;
            if ahead <= 0 {
                return None;
            }
            let side = (dx * d.1 - dy * d.0).abs();
            // the gap between them in the direction (< 0: they overlap), and sideways
            let (a, b) = (span(&r, along), span(&q, along));
            let gap = if sign > 0 { b.0 - a.1 } else { a.0 - b.1 };
            let (a, b) = (span(&r, across), span(&q, across));
            let apart = (b.0 - a.1).max(a.0 - b.1).max(0);
            // one in another row (column) must lie wholly ahead
            if apart > 0 && gap < 0 {
                return None;
            }
            Some(((apart > 0, gap.max(0) + 2 * apart, side), k))
        })
        .min_by_key(|e| e.0)
        .map(|e| e.1)
}

fn direction(buttons: u32, stick: [f32; 2]) -> Option<(i32, i32)> {
    let d = if buttons & UP != 0 {
        (0, -1)
    } else if buttons & DOWN != 0 {
        (0, 1)
    } else if buttons & LEFT != 0 {
        (-1, 0)
    } else if buttons & RIGHT != 0 {
        (1, 0)
    } else if stick[0].abs().max(stick[1].abs()) < STICK {
        return None;
    } else if stick[0].abs() > stick[1].abs() {
        (stick[0].signum() as i32, 0)
    } else {
        (0, stick[1].signum() as i32)
    };
    Some(d)
}

/// The real mouse moved: the cursor shows where it is, the focus frame goes.
pub fn mouse_moved() {
    let mut s = STATE.lock().unwrap();
    s.focus = None;
    s.came = None;
    s.pad_last = false;
}

/// Whether the pad was used after the mouse (a dialog opened then starts with the pad's frame).
pub fn pad_last() -> bool {
    STATE.lock().unwrap().pad_last
}

/// What the window shows after a `step`.
pub struct Step {
    /// the focus frame is shown (the window system's cursor is hidden then: it would stay
    /// elsewhere)
    pub focus: bool,
    /// a new overlay (ARGB, OV_W x OV_H; None: none), if it changed
    pub overlay: Option<Option<Vec<u32>>>,
}

/// After the pads were read (main thread, every pump): the menu's input from them, and the
/// focus frame drawn around the item chosen.
pub fn step(input: &mut VecDeque<Input>) -> Step {
    let (buttons, stick) = super::dinput::menu_state();
    let mut s = STATE.lock().unwrap();
    let pressed = buttons & !s.buttons;
    let released = s.buttons & !buttons;
    s.buttons = buttons;
    if pressed != 0 || direction(buttons, stick).is_some() {
        s.pad_last = true;
    }
    // a release ends what its press began, wherever the game is now
    if released & A != 0 {
        if std::mem::take(&mut s.clicked) {
            input.push_back(Input::Button(0, false));
        }
        if std::mem::take(&mut s.space) {
            input.push_back(space(false));
        }
    }
    let video = crate::game::game().video_playing();
    if video && pressed & A != 0 {
        s.space = true;
        input.push_back(space(true));
    }
    let now = Instant::now();
    if dialog::any_open() {
        // the overlay is the dialogs' now (cleared when the last one closes); they take the
        // pad's input in their modal loop
        s.drawn = None;
        s.focus = None;
        let mut ev = DIALOG.lock().unwrap();
        if let Some(d) = repeat(&mut s, direction(buttons, stick), now) {
            ev.push_back(PadEvent::Dir(d));
        }
        if pressed & A != 0 {
            ev.push_back(PadEvent::A(true));
        }
        if released & A != 0 {
            ev.push_back(PadEvent::A(false));
        }
        if pressed & B != 0 {
            ev.push_back(PadEvent::B);
        }
        return Step {
            focus: dialog::pad_shown(),
            overlay: None,
        };
    }
    let menu = if video { None } else { crate::game::game().menu() };
    let items = menu.as_ref().map_or(Vec::new(), |m| m.items.clone());
    // the menu changed under the frame: no focus
    if s.focus.is_some_and(|f| !items.contains(&f)) {
        s.focus = None;
    }
    if !items.is_empty() {
        if let Some(d) = repeat(&mut s, direction(buttons, stick), now) {
            let from = s.focus.map_or(menu.as_ref().map_or((0, 0), |m| m.mouse), |f| centre(&f));
            let cur = s.focus.and_then(|f| items.iter().position(|&r| r == f));
            let came = s.came.and_then(|(r, way)| Some((items.iter().position(|&q| q == r)?, way)));
            if let Some(r) = nearest(&items, cur, came, from, d).map(|k| items[k]) {
                s.came = s.focus.map(|f| (f, d));
                let (x, y) = centre(&r);
                if trace() {
                    log!("menu pad: {d:?} from {from:?} to {x},{y} ({} items)", items.len());
                }
                input.push_back(Input::Move(x, y));
                s.focus = Some(r);
            }
        }
        if pressed & A != 0 && !s.clicked {
            s.clicked = true;
            input.push_back(Input::Button(0, true));
        }
    }
    let overlay = (s.drawn != s.focus).then(|| s.focus.map(frame_around));
    s.drawn = s.focus;
    Step {
        focus: s.focus.is_some(),
        overlay,
    }
}

/// A direction held: when it moves (at once, again after `FIRST_REPEAT`, then every `REPEAT`).
fn repeat(s: &mut State, d: Option<(i32, i32)>, now: Instant) -> Option<(i32, i32)> {
    let Some(d) = d else {
        s.dir = None;
        return None;
    };
    let wait = match s.dir {
        Some((was, at)) if was == d => (now >= at).then_some(REPEAT)?,
        _ => FIRST_REPEAT,
    };
    s.dir = Some((d, now + wait));
    Some(d)
}

/// The pad's input for the topmost dialog.
#[derive(Clone, Copy, Debug)]
pub enum PadEvent {
    Dir((i32, i32)),
    /// A down / up
    A(bool),
    B,
}

static DIALOG: Mutex<VecDeque<PadEvent>> = Mutex::new(VecDeque::new());

/// The pad's input for dialogs since the last call.
pub fn dialog_events() -> Vec<PadEvent> {
    DIALOG.lock().unwrap().drain(..).collect()
}

/// The focus frame around item r, as an overlay (ARGB, OV_W x OV_H).
fn frame_around(r: Rect) -> Vec<u32> {
    let mut px = vec![0u32; (OV_W * OV_H) as usize];
    // gold, 2 pixels wide, with a soft pixel either side
    let rings = [
        (4, 0x60ff_d040),
        (3, 0xffff_d040),
        (2, 0xffff_d040),
        (1, 0x60ff_d040),
    ];
    for (grow, colour) in rings {
        let (x0, y0, x1, y1) = (
            r.0 - grow,
            r.1 - grow,
            r.0 + r.2 - 1 + grow,
            r.1 + r.3 - 1 + grow,
        );
        for y in y0..=y1 {
            for x in x0..=x1 {
                let edge = x == x0 || x == x1 || y == y0 || y == y1;
                if edge && (0..OV_W).contains(&x) && (0..OV_H).contains(&y) {
                    px[(y * OV_W + x) as usize] = colour;
                }
            }
        }
    }
    px
}

fn space(down: bool) -> Input {
    Input::Key {
        vk: 0x20,
        scan: 0x39,
        down,
        repeat: false,
    }
}
