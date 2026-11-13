//! DirectInput 5: the game's joysticks on gamepads through gilrs, and the system keyboard and
//! mouse (many games of the time read the keyboard with GetAsyncKeyState instead; see the
//! section at the end).
//!
//! Always two pads are reported (many games look for pads only at their start and have room
//! for two): the real pads behind them are chosen at every read (`connected`), so pads plugged
//! in later work. Errors make games give a pad up, so every call answers DI_OK. Button order:
//! 0 RT, 1 LT, 2 B, 3 A, 4 X, 5 Y, 6 LB, 7 RB, 8 Back, 9 R3, 10 L3, 11...14 D-pad up, down,
//! left, right; the left stick gives lX/lY; Start is the Escape key. A game's own pad bindings
//! (Future Cop: analog movement, right-stick keys) go in its hooks, built on `current_pad`.
//!
//! gilrs lives on the main thread and is polled at every `window::pump`; the game's threads
//! read the last state. `<PREFIX>_PAD_LOG=1` logs pads and the game's use of them. `pad@...`
//! items in `<PREFIX>_INPUT` drive a scripted pad (pad 0, before the real ones).

use super::{Import, alloc, arg, free, guest_call, log, mem, put_cstr};
use recomp::rt::Cpu;
use gilrs::{Axis, Button, Gilrs};
use std::cell::RefCell;
use std::sync::Mutex;

const DI_OK: u32 = 0;
const E_NOTIMPL: u32 = 0x8000_4001;
const E_NOINTERFACE: u32 = 0x8000_4002;
const E_INVALIDARG: u32 = 0x8007_0057;
const DIERR_DEVICENOTREG: u32 = 0x8004_0154;

const DIDEVTYPE_JOYSTICK: u32 = 4;
const DIDEVTYPEJOYSTICK_GAMEPAD: u32 = 3;
const DIDC_ATTACHED: u32 = 1;
const DIPROP_RANGE: u32 = 4;
const DIPROP_DEADZONE: u32 = 5;
const DIPH_DEVICE: u32 = 0;
const DIPH_BYOFFSET: u32 = 1;

/// A GUID as its 16 bytes in memory.
const fn guid(d1: u32, d2: u16, d3: u16, d4: [u8; 8]) -> [u8; 16] {
    let (a, b, c) = (d1.to_le_bytes(), d2.to_le_bytes(), d3.to_le_bytes());
    [a[0], a[1], a[2], a[3], b[0], b[1], c[0], c[1], d4[0], d4[1], d4[2], d4[3], d4[4], d4[5], d4[6], d4[7]]
}

const DI_D4: [u8; 8] = [0xbf, 0xc7, 0x44, 0x45, 0x53, 0x54, 0x00, 0x00];
const IID_IUNKNOWN: [u8; 16] = guid(0, 0, 0, [0xc0, 0, 0, 0, 0, 0, 0, 0x46]);
const IID_IDIRECTINPUTA: [u8; 16] = guid(0x8952_1360, 0xaa8a, 0x11cf, DI_D4);
const IID_IDIRECTINPUTDEVICEA: [u8; 16] = guid(0x5944_e680, 0xc92e, 0x11cf, DI_D4);
const IID_IDIRECTINPUTDEVICE2A: [u8; 16] = guid(0x5944_e682, 0xc92e, 0x11cf, DI_D4);
/// Pad n's instance GUID has 0xfc0b0000 + n in its first dword; the product GUID is the base.
const PAD_GUID: u32 = 0xfc0b_0000;

fn pad_guid(n: u32) -> [u8; 16] {
    guid(PAD_GUID + n, 0xd5a0, 0x11cf, DI_D4)
}

fn trace() -> bool {
    static T: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *T.get_or_init(|| recomp::config::env("PAD_LOG").ok_or(()).is_ok_and(|v| v != "0"))
}

macro_rules! pad_log {
    ($($t:tt)*) => { if trace() { log!("pad: {}", format!($($t)*)) } };
}

/// Stick values inside this are 0 (the game's own dead zone is 25%).
const AXIS_DEAD_ZONE: f32 = 0.25;

// ---- pad state --------------------------------------------------------------------------------

/// One pad as the game sees it: left and right sticks (-1...1, y down), our DirectInput
/// buttons (bit n = button n), Start.
#[derive(Clone, Copy, Default, PartialEq)]
pub struct Pad {
    pub stick: [f32; 4],
    buttons: u32,
    start: bool,
}

/// A connected pad: its state, the buttons pressed since the game last read it (a short press
/// between two reads still counts: the Controller dialog reads every 250 ms), gilrs' id (None:
/// the scripted pad), its name.
struct Entry {
    pad: Pad,
    pressed: u32,
    id: Option<usize>,
    name: String,
}

/// The connected pads (the scripted one first).
static PADS: Mutex<Vec<Entry>> = Mutex::new(Vec::new());
/// Start's new state on the first pad, for the next pump (it becomes the Escape key).
static START: Mutex<Option<bool>> = Mutex::new(None);
/// The scripted pad, once <PREFIX>_INPUT has a `pad@` item.
static SCRIPTED: Mutex<Option<Pad>> = Mutex::new(None);

fn scripted_pad() -> bool {
    static S: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *S.get_or_init(|| super::window::script_text().contains("pad@"))
}

thread_local! {
    /// gilrs on the main thread; None when it could not start (then no pads)
    /// (never dropped: its thread must not end at exit)
    static GILRS: RefCell<Option<Option<&'static mut Gilrs>>> = const { RefCell::new(None) };
}

const BUTTONS: [Button; 13] = [
    Button::East,
    Button::South,
    Button::West,
    Button::North,
    Button::LeftTrigger,
    Button::RightTrigger,
    Button::Select,
    Button::RightThumb,
    Button::LeftThumb,
    Button::DPadUp,
    Button::DPadDown,
    Button::DPadLeft,
    Button::DPadRight,
];
/// our button numbers for `BUTTONS`
const BUTTON_NUMBERS: [u32; 13] = [2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14];

/// Our button for a gilrs event that presses one.
fn pressed_by(e: &gilrs::EventType) -> Option<u32> {
    match *e {
        gilrs::EventType::ButtonChanged(Button::RightTrigger2, v, _) if v > 0.3 => Some(0),
        gilrs::EventType::ButtonChanged(Button::LeftTrigger2, v, _) if v > 0.3 => Some(1),
        gilrs::EventType::ButtonPressed(b, _) => BUTTONS.iter().position(|&x| x == b).map(|k| BUTTON_NUMBERS[k]),
        _ => None,
    }
}

fn read(g: &gilrs::Gamepad) -> Pad {
    let trigger = |b| g.button_data(b).map_or(0.0, |d| d.value()) > 0.3;
    let mut buttons = trigger(Button::RightTrigger2) as u32 | (trigger(Button::LeftTrigger2) as u32) << 1;
    for (b, n) in BUTTONS.into_iter().zip(BUTTON_NUMBERS) {
        buttons |= (g.is_pressed(b) as u32) << n;
    }
    Pad {
        stick: [
            g.value(Axis::LeftStickX),
            -g.value(Axis::LeftStickY),
            g.value(Axis::RightStickX),
            -g.value(Axis::RightStickY),
        ],
        buttons,
        start: g.is_pressed(Button::Start),
    }
}

fn new_gilrs() -> Option<&'static mut Gilrs> {
    let g = Gilrs::new().map_err(|e| log!("warn: no gamepads: {e}")).ok()?;
    Some(Box::leak(Box::new(g)))
}

/// Start gilrs on the main thread before the game: pads are found some time after (Windows
/// reports them ~75 ms later), and the game looks for them once, at its start.
pub fn start() {
    GILRS.with(|g| {
        g.borrow_mut().get_or_insert_with(new_gilrs);
    });
}

/// Read the pads (main thread, at every pump). Returns Start's new state when it changed on the
/// first pad (it becomes the Escape key).
pub fn poll() -> Option<bool> {
    update();
    START.lock().unwrap().take()
}

fn update() {
    let scripted = {
        let mut s = SCRIPTED.lock().unwrap();
        if scripted_pad() {
            s.get_or_insert_default();
        }
        *s
    };
    let mut pads = PADS.lock().unwrap();
    let pressed_before = |id| pads.iter().find(|e| e.id == id).map_or(0, |e| e.pressed);
    let mut now: Vec<Entry> = scripted
        .map(|pad| Entry { pad, pressed: pressed_before(None) | pad.buttons, id: None, name: "Scripted pad".to_string() })
        .into_iter()
        .collect();
    GILRS.with(|g| {
        let mut g = g.borrow_mut();
        let Some(g) = g.get_or_insert_with(new_gilrs) else { return };
        let mut pressed: Vec<(usize, u32)> = Vec::new();
        while let Some(e) = g.next_event() {
            match e.event {
                gilrs::EventType::Connected => pad_log!("connected: {}", g.gamepad(e.id).name()),
                gilrs::EventType::Disconnected => pad_log!("disconnected: {}", g.gamepad(e.id).name()),
                ref ev => {
                    if let Some(b) = pressed_by(ev) {
                        pressed.push((e.id.into(), 1 << b));
                    }
                }
            }
        }
        now.extend(g.gamepads().map(|(id, p)| {
            let id: usize = id.into();
            let pad = read(&p);
            let new = pressed.iter().filter(|e| e.0 == id).fold(0, |a, e| a | e.1);
            Entry { pad, pressed: pressed_before(Some(id)) | new | pad.buttons, id: Some(id), name: p.name().to_string() }
        }));
    });
    if super::record::on() {
        // the first real pad (with a press between two reads held for one)
        let e = now.iter().find(|e| e.id.is_some());
        let p = e.map_or(Pad::default(), |e| e.pad);
        let buttons = p.buttons | e.map_or(0, |e| e.pressed & !pressed_before(e.id));
        let mut v = [0.0; CONTROLS.len()];
        for (k, x) in v.iter_mut().enumerate().take(15) {
            *x = (buttons >> k & 1) as f32;
        }
        v[15] = p.start as u8 as f32;
        v[16..].copy_from_slice(&[p.stick[0], -p.stick[1], p.stick[2], -p.stick[3]]);
        super::record::pad(&v);
    }
    let was = pads.first().is_some_and(|e| e.pad.start);
    let start = now.first().is_some_and(|e| e.pad.start);
    *pads = now;
    if was != start {
        *START.lock().unwrap() = Some(start);
    }
}

/// All pads together, as menus read them (`menupad`): buttons held on any pad (our numbers),
/// the left stick of the first pad that has it out of its dead zone (y down).
pub fn menu_state() -> (u32, [f32; 2]) {
    let pads = PADS.lock().unwrap();
    let buttons = pads.iter().fold(0, |a, e| a | e.pad.buttons);
    let stick = pads
        .iter()
        .map(|e| [e.pad.stick[0], e.pad.stick[1]])
        .find(|s| s[0].abs() > AXIS_DEAD_ZONE || s[1].abs() > AXIS_DEAD_ZONE)
        .unwrap_or_default();
    (buttons, stick)
}

/// Forget the presses the game has not read yet (a dialog used them).
pub fn forget_presses() {
    for e in PADS.lock().unwrap().iter_mut() {
        e.pressed = 0;
    }
}

/// The connected pad behind the game's pad n. The game always sees two pads (`enum_devices`),
/// so pads plugged in later work: with two connected pad i is pad i, one serves both (as the
/// game shares a single pad), none reads as centred and released.
fn connected(pads: &[Entry], n: u32) -> Option<usize> {
    match pads.len() {
        0 => None,
        1 => Some(0),
        _ => Some(n.min(1) as usize),
    }
}

/// Pad n as the game reads it: its buttons held or pressed since the last read.
fn pad(n: u32) -> Pad {
    // fresh on the main thread (the Controller dialog reads from there)
    if GILRS.with(|g| g.borrow().is_some()) {
        update();
    }
    let mut pads = PADS.lock().unwrap();
    let Some(k) = connected(&pads, n) else { return Pad::default() };
    let e = &mut pads[k];
    let mut p = e.pad;
    p.buttons |= std::mem::take(&mut e.pressed);
    p
}

/// The scripted pad's controls (<PREFIX>_INPUT `pad@A*500`, `pad@LX=-1*1000`): our buttons in
/// order (the D-pad's too), then Start and both sticks' axes (y up, as gilrs has it).
pub const CONTROLS: [&str; 20] = [
    "RT", "LT", "B", "A", "X", "Y", "LB", "RB", "Back", "R3", "L3", "Up", "Down", "Left", "Right", "Start", "LX", "LY",
    "RX", "RY",
];

/// Set one control of the scripted pad (`CONTROLS[k]`): held when `v` is not 0, or the axis' value.
pub fn script(k: usize, v: f32) {
    let mut s = SCRIPTED.lock().unwrap();
    let p = s.get_or_insert_default();
    let on = v != 0.0;
    match k {
        0..=14 => p.buttons = p.buttons & !(1 << k) | (on as u32) << k,
        15 => p.start = on,
        16 => p.stick[0] = v,
        17 => p.stick[1] = -v,
        18 => p.stick[2] = v,
        _ => p.stick[3] = -v,
    }
}

/// Make sure the pads were read once (EnumDevices may come before the first pump).
fn ensure_polled() {
    super::window::pump(std::time::Duration::ZERO);
}

// ---- IDirectInputA ----------------------------------------------------------------------------
// object: vtable, references

fn guid_at(p: u32) -> [u8; 16] {
    mem().slice(p, 16).try_into().unwrap()
}

fn direct_input_create(c: &mut Cpu) -> u32 {
    let (version, out) = (arg(c, 1), arg(c, 2));
    let o = super::com_new(DIRECT_INPUT, 4);
    mem().wr32(o + 4, 1);
    mem().wr32(out, o);
    pad_log!("DirectInputCreateA(version {version:#x})");
    DI_OK
}

fn di_query(c: &mut Cpu) -> u32 {
    let (this, iid, out) = (arg(c, 0), guid_at(arg(c, 1)), arg(c, 2));
    if iid != IID_IUNKNOWN && iid != IID_IDIRECTINPUTA {
        mem().wr32(out, 0);
        return E_NOINTERFACE;
    }
    add_ref(c);
    mem().wr32(out, this);
    DI_OK
}

/// AddRef and Release of both interfaces: the count is at +4.
fn add_ref(c: &mut Cpu) -> u32 {
    let o = arg(c, 0);
    let n = mem().rd32(o + 4) + 1;
    mem().wr32(o + 4, n);
    n
}

fn release(c: &mut Cpu) -> u32 {
    let o = arg(c, 0);
    let n = mem().rd32(o + 4).saturating_sub(1);
    mem().wr32(o + 4, n);
    if n == 0 {
        free(o);
    }
    n
}

fn create_device(c: &mut Cpu) -> u32 {
    let (g, out) = (guid_at(arg(c, 1)), arg(c, 2));
    match g {
        GUID_SYSKEYBOARD => return km_create(KEYBOARD, out),
        GUID_SYSMOUSE => return km_create(MOUSE, out),
        _ => {}
    }
    let n = u32::from_le_bytes(g[..4].try_into().unwrap()).wrapping_sub(PAD_GUID);
    if n >= 2 || g != pad_guid(n) {
        mem().wr32(out, 0);
        return DIERR_DEVICENOTREG;
    }
    let d = super::com_new(DEVICE, DEVICE_SIZE);
    let m = mem();
    m.wr32(d + 4, 1);
    m.wr32(d + 8, n);
    for axis in 0..2 {
        m.wr32(d + RANGE + 8 * axis, 0);
        m.wr32(d + RANGE + 8 * axis + 4, 0xffff);
    }
    m.wr32(out, d);
    pad_log!("CreateDevice: pad {n}");
    DI_OK
}

/// EnumDevices(type, callback, ref, flags): always two pads, DIDEVICEINSTANCEA (DX5). The game
/// looks for pads only at its start; `connected` finds the real one behind each when it reads.
fn enum_devices(c: &mut Cpu) -> u32 {
    let (kind, cb, user) = (arg(c, 1) & 0xff, arg(c, 2), arg(c, 3));
    // the keyboard and the mouse first, as DirectInput lists them
    if !km_enum(c, kind, cb, user) {
        return DI_OK;
    }
    if kind != 0 && kind != DIDEVTYPE_JOYSTICK {
        return DI_OK;
    }
    ensure_polled();
    let names: Vec<String> = {
        let pads = PADS.lock().unwrap();
        (0..2).map(|n| pads.get(n).map_or_else(|| format!("Gamepad {}", n + 1), |e| e.name.clone())).collect()
    };
    let ddi = alloc(0x244);
    let m = mem();
    for (n, name) in names.iter().enumerate() {
        pad_log!("EnumDevices: pad {n}: {name}");
        m.slice(ddi, 0x244).fill(0);
        m.wr32(ddi, 0x244);
        m.slice(ddi + 4, 16).copy_from_slice(&pad_guid(n as u32));
        m.slice(ddi + 0x14, 16).copy_from_slice(&pad_guid(0));
        m.wr32(ddi + 0x24, DIDEVTYPE_JOYSTICK | DIDEVTYPEJOYSTICK_GAMEPAD << 8);
        put_cstr(ddi + 0x28, name, 260);
        put_cstr(ddi + 0x12c, name, 260);
        if guest_call(c, cb, &[ddi, user]) == 0 {
            break;
        }
    }
    free(ddi);
    DI_OK
}

fn ok(_: &mut Cpu) -> u32 {
    DI_OK
}

fn not_impl(_: &mut Cpu) -> u32 {
    E_NOTIMPL
}

static DIRECT_INPUT: &[Import] = &[
    ("IDirectInputA::QueryInterface", 3, di_query),
    ("IDirectInputA::AddRef", 1, add_ref),
    ("IDirectInputA::Release", 1, release),
    ("IDirectInputA::CreateDevice", 4, create_device),
    ("IDirectInputA::EnumDevices", 5, enum_devices),
    ("IDirectInputA::GetDeviceStatus", 2, ok),
    ("IDirectInputA::RunControlPanel", 3, ok),
    ("IDirectInputA::Initialize", 3, ok),
];

// ---- IDirectInputDevice2A ---------------------------------------------------------------------
// object: vtable, references, pad number, X and Y: range (min, max), dead zone (0...10000)

const RANGE: u32 = 0xc;
const DEAD_ZONE: u32 = 0x1c;
const DEVICE_SIZE: u32 = 0x20;

fn dev_query(c: &mut Cpu) -> u32 {
    let (this, iid, out) = (arg(c, 0), guid_at(arg(c, 1)), arg(c, 2));
    if ![IID_IUNKNOWN, IID_IDIRECTINPUTDEVICEA, IID_IDIRECTINPUTDEVICE2A].contains(&iid) {
        mem().wr32(out, 0);
        return E_NOINTERFACE;
    }
    add_ref(c);
    mem().wr32(out, this);
    DI_OK
}

/// GetCapabilities(DIDEVCAPS): attached, no force feedback, 2 axes, 15 buttons, no POV.
fn get_capabilities(c: &mut Cpu) -> u32 {
    let p = arg(c, 1);
    let m = mem();
    let size = m.rd32(p);
    if size < 24 {
        return E_INVALIDARG;
    }
    m.slice(p + 4, size as usize - 4).fill(0);
    for (k, v) in [DIDC_ATTACHED, DIDEVTYPE_JOYSTICK | DIDEVTYPEJOYSTICK_GAMEPAD << 8, 2, 15, 0].into_iter().enumerate() {
        m.wr32(p + 4 + 4 * k as u32, v);
    }
    DI_OK
}

/// The axes (0 X, 1 Y) a property header (dwObj +8, dwHow +0xc) names.
fn axes_of(h: u32) -> std::ops::Range<u32> {
    match (mem().rd32(h + 0xc), mem().rd32(h + 8)) {
        (DIPH_DEVICE, _) => 0..2,
        (DIPH_BYOFFSET, o @ (0 | 4)) => o / 4..o / 4 + 1,
        _ => 0..0,
    }
}

/// SetProperty(property, header): the range and the dead zone of X and Y; others are accepted.
fn set_property(c: &mut Cpu) -> u32 {
    let (d, prop, h) = (arg(c, 0), arg(c, 1), arg(c, 2));
    let m = mem();
    for axis in axes_of(h) {
        match prop {
            DIPROP_RANGE => {
                m.wr32(d + RANGE + 8 * axis, m.rd32(h + 0x10));
                m.wr32(d + RANGE + 8 * axis + 4, m.rd32(h + 0x14));
            }
            DIPROP_DEADZONE => m.wr32(d + DEAD_ZONE + 4 * axis, m.rd32(h + 0x10)),
            _ => {}
        }
    }
    pad_log!("SetProperty({prop}) of pad {}", m.rd32(d + 8));
    DI_OK
}

fn get_property(c: &mut Cpu) -> u32 {
    let (d, prop, h) = (arg(c, 0), arg(c, 1), arg(c, 2));
    let m = mem();
    let axis = axes_of(h).start.min(1);
    match prop {
        DIPROP_RANGE => {
            m.wr32(h + 0x10, m.rd32(d + RANGE + 8 * axis));
            m.wr32(h + 0x14, m.rd32(d + RANGE + 8 * axis + 4));
        }
        DIPROP_DEADZONE => m.wr32(h + 0x10, m.rd32(d + DEAD_ZONE + 4 * axis)),
        _ => return E_NOTIMPL,
    }
    DI_OK
}

fn acquire(c: &mut Cpu) -> u32 {
    pad_log!("Acquire: pad {}", mem().rd32(arg(c, 0) + 8));
    DI_OK
}

/// An axis value in the device's range: the stick outside its dead zone, else exactly the
/// centre (the game tests the sign).
fn axis_value(d: u32, axis: u32, p: &Pad) -> u32 {
    let m = mem();
    let (lo, hi) = (m.rd32(d + RANGE + 8 * axis) as i32 as f32, m.rd32(d + RANGE + 8 * axis + 4) as i32 as f32);
    let dz = m.rd32(d + DEAD_ZONE + 4 * axis).min(9999) as f32 / 10000.0;
    let s = p.stick[axis as usize].clamp(-1.0, 1.0);
    let v = if s.abs() > dz { s.signum() * (s.abs() - dz) / (1.0 - dz) } else { 0.0 };
    let centre = (lo + hi) / 2.0;
    if v == 0.0 {
        return centre.floor() as i32 as u32;
    }
    let x = centre + v * (hi - lo) / 2.0;
    // away from the centre, so a small push never reads as the centre
    (if v > 0.0 { x.ceil() } else { x.floor() }).clamp(lo, hi) as i32 as u32
}

/// GetDeviceState(size, DIJOYSTATE): lX, lY, the other axes centred, POVs centred, buttons.
fn get_device_state(c: &mut Cpu) -> u32 {
    let (d, size, out) = (arg(c, 0), arg(c, 1), arg(c, 2));
    let m = mem();
    let p = pad(m.rd32(d + 8));
    let mut s = [0u8; 0x50];
    s[0..4].copy_from_slice(&axis_value(d, 0, &p).to_le_bytes());
    s[4..8].copy_from_slice(&axis_value(d, 1, &p).to_le_bytes());
    s[0x20..0x30].fill(0xff);
    for b in 0..32 {
        s[0x30 + b] = if p.buttons >> b & 1 != 0 { 0x80 } else { 0 };
    }
    let n = (size as usize).min(s.len());
    m.slice(out, n).copy_from_slice(&s[..n]);
    DI_OK
}

static DEVICE: &[Import] = &[
    ("IDirectInputDevice2A::QueryInterface", 3, dev_query),
    ("IDirectInputDevice2A::AddRef", 1, add_ref),
    ("IDirectInputDevice2A::Release", 1, release),
    ("IDirectInputDevice2A::GetCapabilities", 2, get_capabilities),
    ("IDirectInputDevice2A::EnumObjects", 4, ok),
    ("IDirectInputDevice2A::GetProperty", 3, get_property),
    ("IDirectInputDevice2A::SetProperty", 3, set_property),
    ("IDirectInputDevice2A::Acquire", 1, acquire),
    ("IDirectInputDevice2A::Unacquire", 1, ok),
    ("IDirectInputDevice2A::GetDeviceState", 3, get_device_state),
    ("IDirectInputDevice2A::GetDeviceData", 5, not_impl),
    ("IDirectInputDevice2A::SetDataFormat", 2, ok),
    ("IDirectInputDevice2A::SetEventNotification", 2, ok),
    ("IDirectInputDevice2A::SetCooperativeLevel", 3, ok),
    ("IDirectInputDevice2A::GetObjectInfo", 4, not_impl),
    ("IDirectInputDevice2A::GetDeviceInfo", 2, not_impl),
    ("IDirectInputDevice2A::RunControlPanel", 3, ok),
    ("IDirectInputDevice2A::Initialize", 4, ok),
    ("IDirectInputDevice2A::CreateEffect", 5, not_impl),
    ("IDirectInputDevice2A::EnumEffects", 4, ok),
    ("IDirectInputDevice2A::GetEffectInfo", 3, not_impl),
    ("IDirectInputDevice2A::GetForceFeedbackState", 2, not_impl),
    ("IDirectInputDevice2A::SendForceFeedbackCommand", 2, not_impl),
    ("IDirectInputDevice2A::EnumCreatedEffectObjects", 4, ok),
    ("IDirectInputDevice2A::Escape", 2, not_impl),
    ("IDirectInputDevice2A::Poll", 1, ok),
    ("IDirectInputDevice2A::SendDeviceData", 5, not_impl),
];

/// The game's pad n as it is now.
pub fn current_pad(n: u32) -> Pad {
    let pads = PADS.lock().unwrap();
    connected(&pads, n).map_or(Pad::default(), |k| pads[k].pad)
}

/// Player p's own pad for the gamepad bindings (`gamepad.rs`): pad p only (with one pad,
/// player 2 has none).
pub fn player_pad(p: u32) -> Option<Pad> {
    PADS.lock().unwrap().get(p as usize).map(|e| e.pad)
}

pub const IMPORTS: &[Import] = &[("DirectInputCreateA", 4, direct_input_create)];

// ---- the system keyboard and mouse ---------------------------------------------------------------
//
// From nuclear_rust_strike. Keys and buttons come from `user32::poll` (`key`, `button`, `wheel`),
// mouse motion from the window system's raw motion (`motion`): DirectInput's mouse is relative.
// Both are read as a state (GetDeviceState: 256 key bytes; DIMOUSESTATE) or as buffered events
// (GetDeviceData: DIDEVICEOBJECTDATA, after SetProperty(DIPROP_BUFFERSIZE)). Their devices have
// their own vtable (`KM_DEVICE`); `<PREFIX>_PAD_LOG=1` logs them too.

const DIPROP_BUFFERSIZE: u32 = 1;
const DIDEVTYPE_MOUSE: u32 = 2;
const DIDEVTYPE_KEYBOARD: u32 = 3;
const GUID_SYSMOUSE: [u8; 16] = guid(0x6f1d_2b60, 0xd5a0, 0x11cf, DI_D4);
const GUID_SYSKEYBOARD: [u8; 16] = guid(0x6f1d_2b61, 0xd5a0, 0x11cf, DI_D4);

/// Device kinds: index into `KM`'s queues.
const KEYBOARD: usize = 0;
const MOUSE: usize = 1;
const KM_NAMES: [&str; 2] = ["keyboard", "mouse"];

struct KmState {
    /// DIK codes: 0x80 while down
    keys: [u8; 256],
    /// mouse motion not yet read by GetDeviceState, wheel, buttons
    dx: i32,
    dy: i32,
    dz: i32,
    buttons: [u8; 4],
    /// buffered events per device: (offset, data, time)
    events: [std::collections::VecDeque<(u32, u32, u32)>; 2],
    sequence: u32,
}

static KM: Mutex<KmState> = Mutex::new(KmState {
    keys: [0; 256],
    dx: 0,
    dy: 0,
    dz: 0,
    buttons: [0; 4],
    events: [std::collections::VecDeque::new(), std::collections::VecDeque::new()],
    sequence: 0,
});

/// Room in each device's buffer (more events than that are dropped, oldest first).
const BUFFER: usize = 256;

fn km_push(s: &mut KmState, dev: usize, ofs: u32, data: u32) {
    let q = &mut s.events[dev];
    if q.len() >= BUFFER {
        q.pop_front();
    }
    q.push_back((ofs, data, super::kernel32::ms_since_start()));
}

/// The DirectInput key code (DIK) of a Windows virtual key and its scan code: the scan code,
/// with 0x80 for the keys of the extended set (arrows, the block above them, keypad /).
pub fn dik(vk: u8, scan: u8) -> u8 {
    match vk {
        0x21..=0x28 | 0x2d | 0x2e | 0x6f => scan | 0x80,
        _ => scan,
    }
}

/// A key went down or up (from `user32::poll`).
pub fn key(dik: u8, down: bool) {
    let mut s = KM.lock().unwrap();
    let v = if down { 0x80 } else { 0 };
    if s.keys[dik as usize] != v || down {
        s.keys[dik as usize] = v;
        km_push(&mut s, KEYBOARD, dik as u32, v as u32);
    }
}

/// Let every key go (the window lost the focus).
pub fn release_keys() {
    let mut s = KM.lock().unwrap();
    for k in 0..256 {
        if s.keys[k] != 0 {
            s.keys[k] = 0;
            km_push(&mut s, KEYBOARD, k as u32, 0);
        }
    }
}

/// A mouse button (0 left, 1 right, 2 middle, 3 X1) went down or up.
pub fn button(b: u8, down: bool) {
    let mut s = KM.lock().unwrap();
    let Some(k) = s.buttons.get_mut(b as usize) else { return };
    *k = if down { 0x80 } else { 0 };
    km_push(&mut s, MOUSE, 12 + b as u32, if down { 0x80 } else { 0 });
}

/// Raw mouse motion in counts (any thread).
pub fn motion(dx: i32, dy: i32) {
    let mut s = KM.lock().unwrap();
    s.dx += dx;
    s.dy += dy;
    if dx != 0 {
        km_push(&mut s, MOUSE, 0, dx as u32);
    }
    if dy != 0 {
        km_push(&mut s, MOUSE, 4, dy as u32);
    }
}

/// The mouse wheel, in lines.
pub fn wheel(lines: i32) {
    let mut s = KM.lock().unwrap();
    s.dz += lines * 120;
    km_push(&mut s, MOUSE, 8, (lines * 120) as u32);
}

// keyboard/mouse device object: vtable, references, kind (KEYBOARD/MOUSE), buffered (0/1)
const KM_KIND: u32 = 8;
const KM_BUFFERED: u32 = 0xc;
const KM_SIZE: u32 = 0x10;

fn km_create(kind: usize, out: u32) -> u32 {
    let d = super::com_new(KM_DEVICE, KM_SIZE);
    let m = mem();
    m.wr32(d + 4, 1);
    m.wr32(d + KM_KIND, kind as u32);
    m.wr32(d + KM_BUFFERED, 0);
    m.wr32(out, d);
    pad_log!("CreateDevice: {}", KM_NAMES[kind]);
    DI_OK
}

/// EnumDevices for the keyboard and the mouse (DIDEVICEINSTANCEA of DirectX 5); false: the
/// callback stopped.
fn km_enum(c: &mut Cpu, kind: u32, cb: u32, user: u32) -> bool {
    let ddi = alloc(0x244);
    let m = mem();
    let mut go_on = true;
    for (g, t, name) in [(GUID_SYSKEYBOARD, DIDEVTYPE_KEYBOARD, "Keyboard"), (GUID_SYSMOUSE, DIDEVTYPE_MOUSE, "Mouse")] {
        if kind != 0 && kind != t {
            continue;
        }
        m.slice(ddi, 0x244).fill(0);
        m.wr32(ddi, 0x244);
        m.slice(ddi + 4, 16).copy_from_slice(&g);
        m.slice(ddi + 0x14, 16).copy_from_slice(&g);
        m.wr32(ddi + 0x24, t);
        put_cstr(ddi + 0x28, name, 260);
        put_cstr(ddi + 0x12c, name, 260);
        if guest_call(c, cb, &[ddi, user]) == 0 {
            go_on = false;
            break;
        }
    }
    free(ddi);
    go_on
}

fn km_kind(d: u32) -> usize {
    mem().rd32(d + KM_KIND) as usize
}

/// GetCapabilities(DIDEVCAPS): attached; the keyboard's 256 keys, the mouse's 3 axes and 4
/// buttons.
fn km_capabilities(c: &mut Cpu) -> u32 {
    let (d, p) = (arg(c, 0), arg(c, 1));
    let m = mem();
    let size = m.rd32(p).max(24);
    m.slice(p + 4, size as usize - 4).fill(0);
    let caps = if km_kind(d) == KEYBOARD { [DIDC_ATTACHED, DIDEVTYPE_KEYBOARD, 0, 256, 0] } else { [DIDC_ATTACHED, DIDEVTYPE_MOUSE, 3, 4, 0] };
    for (k, v) in caps.into_iter().enumerate() {
        m.wr32(p + 4 + 4 * k as u32, v);
    }
    DI_OK
}

/// SetProperty: DIPROP_BUFFERSIZE turns buffered data on; others are accepted.
fn km_set_property(c: &mut Cpu) -> u32 {
    let (d, prop, h) = (arg(c, 0), arg(c, 1), arg(c, 2));
    let v = mem().rd32(h + 0x10);
    pad_log!("SetProperty({prop}, {v}) of the {}", KM_NAMES[km_kind(d)]);
    if prop == DIPROP_BUFFERSIZE {
        mem().wr32(d + KM_BUFFERED, (v != 0) as u32);
    }
    DI_OK
}

fn km_get_property(c: &mut Cpu) -> u32 {
    let (prop, h) = (arg(c, 1), arg(c, 2));
    match prop {
        DIPROP_BUFFERSIZE => mem().wr32(h + 0x10, BUFFER as u32),
        _ => return E_NOTIMPL,
    }
    DI_OK
}

fn km_acquire(c: &mut Cpu) -> u32 {
    let kind = km_kind(arg(c, 0));
    let mut s = KM.lock().unwrap();
    // what happened before belongs to no one
    s.events[kind].clear();
    if kind == MOUSE {
        (s.dx, s.dy, s.dz) = (0, 0, 0);
    }
    DI_OK
}

/// GetDeviceState: the keyboard's 256 key bytes, or DIMOUSESTATE (motion since the last read,
/// 4 buttons).
fn km_device_state(c: &mut Cpu) -> u32 {
    let (d, size, out) = (arg(c, 0), arg(c, 1) as usize, arg(c, 2));
    let m = mem();
    let mut s = KM.lock().unwrap();
    if km_kind(d) == KEYBOARD {
        let n = size.min(256);
        m.slice(out, n).copy_from_slice(&s.keys[..n]);
    } else {
        let mut st = [0u8; 16];
        st[0..4].copy_from_slice(&s.dx.to_le_bytes());
        st[4..8].copy_from_slice(&s.dy.to_le_bytes());
        st[8..12].copy_from_slice(&s.dz.to_le_bytes());
        st[12..16].copy_from_slice(&s.buttons);
        (s.dx, s.dy, s.dz) = (0, 0, 0);
        let n = size.min(16);
        m.slice(out, n).copy_from_slice(&st[..n]);
    }
    DI_OK
}

/// GetDeviceData(object size, DIDEVICEOBJECTDATA[], in/out count, flags): buffered events
/// (offset, data, time, sequence); no array: only removes them (DIGDD_PEEK = 1 keeps them).
fn km_device_data(c: &mut Cpu) -> u32 {
    let (d, size, out, count_p, flags) = (arg(c, 0), arg(c, 1), arg(c, 2), arg(c, 3), arg(c, 4));
    let m = mem();
    let want = m.rd32(count_p) as usize;
    let mut s = KM.lock().unwrap();
    let kind = km_kind(d);
    let n = want.min(s.events[kind].len());
    for k in 0..n {
        let (ofs, data, time) = s.events[kind][k];
        s.sequence += 1;
        if out != 0 {
            let p = out + k as u32 * size.max(16);
            for (j, v) in [ofs, data, time, s.sequence].into_iter().enumerate() {
                m.wr32(p + 4 * j as u32, v);
            }
        }
    }
    if flags & 1 == 0 {
        s.events[kind].drain(..n);
    }
    m.wr32(count_p, n as u32);
    DI_OK
}

static KM_DEVICE: &[Import] = &[
    ("IDirectInputDevice2A::QueryInterface", 3, dev_query),
    ("IDirectInputDevice2A::AddRef", 1, add_ref),
    ("IDirectInputDevice2A::Release", 1, release),
    ("IDirectInputDevice2A::GetCapabilities", 2, km_capabilities),
    ("IDirectInputDevice2A::EnumObjects", 4, ok),
    ("IDirectInputDevice2A::GetProperty", 3, km_get_property),
    ("IDirectInputDevice2A::SetProperty", 3, km_set_property),
    ("IDirectInputDevice2A::Acquire", 1, km_acquire),
    ("IDirectInputDevice2A::Unacquire", 1, ok),
    ("IDirectInputDevice2A::GetDeviceState", 3, km_device_state),
    ("IDirectInputDevice2A::GetDeviceData", 5, km_device_data),
    ("IDirectInputDevice2A::SetDataFormat", 2, ok),
    ("IDirectInputDevice2A::SetEventNotification", 2, ok),
    ("IDirectInputDevice2A::SetCooperativeLevel", 3, ok),
    ("IDirectInputDevice2A::GetObjectInfo", 4, not_impl),
    ("IDirectInputDevice2A::GetDeviceInfo", 2, not_impl),
    ("IDirectInputDevice2A::RunControlPanel", 3, ok),
    ("IDirectInputDevice2A::Initialize", 4, ok),
    ("IDirectInputDevice2A::CreateEffect", 5, not_impl),
    ("IDirectInputDevice2A::EnumEffects", 4, ok),
    ("IDirectInputDevice2A::GetEffectInfo", 3, not_impl),
    ("IDirectInputDevice2A::GetForceFeedbackState", 2, not_impl),
    ("IDirectInputDevice2A::SendForceFeedbackCommand", 2, not_impl),
    ("IDirectInputDevice2A::EnumCreatedEffectObjects", 4, ok),
    ("IDirectInputDevice2A::Escape", 2, not_impl),
    ("IDirectInputDevice2A::Poll", 1, ok),
    ("IDirectInputDevice2A::SendDeviceData", 5, not_impl),
];
