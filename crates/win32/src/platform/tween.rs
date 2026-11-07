//! Pictures between a game's logic frames: the frame rate of the display, the game's logic at
//! the rate it was made for. Taken from Nuclear Strike (`pictures.rs`, `frames.rs`) and Future
//! Cop (`fps.rs`); GUIDE.md "Bildrate" says how a port puts it together.
//!
//! Never shorten a game's logic frame: logic that counts frames, or takes whole frames from a
//! time step (`dt >> 8`, `dt / step` with the rest dropped), breaks above the rate it was made
//! for (explosions that never end, an autopilot that never moves). Instead:
//!
//! - [`LogicClock`]: the game's loop calls [`LogicClock::frame`] at the start of each logic
//!   frame (a hook of its clock read or frame wait). It draws pictures while the frame is not
//!   yet due, then waits (the turn handed over), and gives the frame's time on a steady grid.
//! - Every picture, the frame's own drawing too, shows the game a little more than a frame
//!   late, between the last two logic frames ([`LogicClock::alpha`]): what moves (objects, the
//!   camera) is put where it was at that time while it is drawn ([`Hist`], [`lerp`],
//!   [`lerp_angle`], [`mix_rotation`]) and put back after.
//! - A picture runs only the drawing part of the game's loop. What drawing changes besides the
//!   picture must be put back ([`Statics`]: the game's static data, with exceptions such as a
//!   memory manager's lists whose records live elsewhere); while it draws, no other guest
//!   thread may run ([`super::hold_turn`]), or their changes are undone too.
//! - Drawing code that prepares the next frame's data as it draws (a terrain renderer with two
//!   buffers) must draw pictures from the data as it was before the frame's own drawing.

use super::mem;
use recomp::rt::{Cpu, EAX, EBX, ECX, EDX};
use std::sync::Mutex;
use std::time::{Duration, Instant};

// ---- the logic's clock --------------------------------------------------------------------------

/// The logic's frames at a fixed rate from a start.
pub struct LogicClock {
    /// nanoseconds a frame
    frame_ns: u64,
    /// (start, frames since)
    state: Mutex<Option<(Instant, u64)>>,
}

impl LogicClock {
    pub const fn new(fps: u32) -> Self {
        LogicClock { frame_ns: 1_000_000_000 / fps as u64, state: Mutex::new(None) }
    }

    pub fn frame_time(&self) -> Duration {
        Duration::from_nanos(self.frame_ns)
    }

    /// Out of the loop (a menu): the next `frame` starts anew.
    pub fn reset(&self) {
        *self.state.lock().unwrap() = None;
    }

    /// The newest logic frame: the time it was due, and its number.
    pub fn newest(&self) -> Option<(Instant, u64)> {
        let s = *self.state.lock().unwrap();
        s.map(|(start, n)| (start + Duration::from_nanos(self.frame_ns * n), n))
    }

    /// At the start of a logic frame: `picture()` (draw and show one picture between frames)
    /// while the picture after this one (`next_shown()`: the next swap's time) comes well
    /// before the frame is due, then the wait (the turn handed over). Returns the frame's
    /// number and its time since the first frame: a whole number of frames (a computer too
    /// slow for the rate runs fewer frames, each a whole number of frames long).
    pub fn frame(&self, mut picture: impl FnMut(), next_shown: impl Fn() -> Instant) -> (u64, Duration) {
        let frame = self.frame_time();
        let due = {
            let mut s = self.state.lock().unwrap();
            match *s {
                None => {
                    *s = Some((Instant::now(), 0));
                    return (0, Duration::ZERO);
                }
                Some((start, n)) => start + frame * (n + 1) as u32,
            }
        };
        for _ in 0..16 {
            if next_shown() + frame / 4 >= due {
                break;
            }
            picture();
        }
        let now = Instant::now();
        if now < due {
            super::without_turn(|| std::thread::sleep(due - now));
        }
        let mut s = self.state.lock().unwrap();
        let (start, n) = s.as_mut().unwrap();
        *n += 1;
        let now = Instant::now();
        if now > due + frame {
            // behind: on to the frame of now
            *n = ((now - *start).as_nanos() / frame.as_nanos()) as u64;
        }
        (*n, Duration::from_nanos(self.frame_ns * *n))
    }

    /// The fraction (in 1/256) of the way from the last logic frame's places to the newest
    /// at which a picture shown at `shown` is: a frame behind. None outside the loop.
    pub fn alpha(&self, shown: Instant) -> Option<u32> {
        let (due, _) = self.newest()?;
        let d = shown.saturating_duration_since(due).as_nanos() * 256 / self.frame_ns as u128;
        Some(d.min(256) as u32)
    }
}

/// Something's values at the last two logic frames.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hist<T> {
    pub frame: u64,
    pub cur: T,
    pub prev: Option<T>,
}

impl<T: Copy> Hist<T> {
    /// The value `v` at logic frame `f`, after `h` (None: first seen, or something new at the
    /// same address: no previous).
    pub fn note(h: Option<&Self>, f: u64, v: T) -> Self {
        match h {
            Some(h) if h.frame == f => Hist { cur: v, ..*h },
            Some(h) if h.frame + 1 == f => Hist { frame: f, cur: v, prev: Some(h.cur) },
            _ => Hist { frame: f, cur: v, prev: None },
        }
    }
}

// ---- interpolation ------------------------------------------------------------------------------

/// `a` to `b` by `alpha` / 256.
pub fn lerp(a: i64, b: i64, alpha: u32) -> i64 {
    a + ((b - a) * alpha as i64 >> 8)
}

/// Angles of `bits` bits for a full turn, the short way round.
pub fn lerp_angle(a: u32, b: u32, alpha: u32, bits: u32) -> u32 {
    let full = 1i64 << bits;
    let d = ((b as i64 - a as i64 + full / 2) & (full - 1)) - full / 2;
    ((a as i64 + (d * alpha as i64 >> 8)) & (full - 1)) as u32
}

/// Whether a matrix is a rotation (rows of length 1 within `tol`).
pub fn is_rotation(m: &[f64; 9], tol: f64) -> bool {
    (0..3).all(|r| ((m[3 * r] * m[3 * r] + m[3 * r + 1] * m[3 * r + 1] + m[3 * r + 2] * m[3 * r + 2]).sqrt() - 1.0).abs() < tol)
}

/// A rotation matrix's quaternion (w, x, y, z).
pub fn quat(m: &[f64; 9]) -> [f64; 4] {
    let (t, q);
    if m[8] < 0.0 {
        if m[0] > m[4] {
            t = 1.0 + m[0] - m[4] - m[8];
            q = [m[7] - m[5], t, m[1] + m[3], m[6] + m[2]];
        } else {
            t = 1.0 - m[0] + m[4] - m[8];
            q = [m[2] - m[6], m[1] + m[3], t, m[5] + m[7]];
        }
    } else if m[0] < -m[4] {
        t = 1.0 - m[0] - m[4] + m[8];
        q = [m[3] - m[1], m[6] + m[2], m[5] + m[7], t];
    } else {
        t = 1.0 + m[0] + m[4] + m[8];
        q = [t, m[7] - m[5], m[2] - m[6], m[3] - m[1]];
    }
    let n = 0.5 / t.sqrt();
    q.map(|v| v * n)
}

/// A unit quaternion's rotation matrix.
pub fn from_quat(q: [f64; 4]) -> [f64; 9] {
    let [w, x, y, z] = q;
    [
        1.0 - 2.0 * (y * y + z * z),
        2.0 * (x * y - w * z),
        2.0 * (x * z + w * y),
        2.0 * (x * y + w * z),
        1.0 - 2.0 * (x * x + z * z),
        2.0 * (y * z - w * x),
        2.0 * (x * z - w * y),
        2.0 * (y * z + w * x),
        1.0 - 2.0 * (x * x + y * y),
    ]
}

/// The most a part may turn in a frame to be interpolated (cos of half the turn: 60°). A rotor
/// turns more: which way it turned cannot be told, element-wise mixing made it a fan.
const MAX_TURN_COS: f64 = 0.866;

/// A fixed-point 3x3 matrix (`one` = 1.0, e.g. 4096 for 4.12) at `alpha` / 256 between `a`
/// and `b`: rotations the short way (quaternions); scaled matrices (something growing)
/// element by element. None: it turned too far in a frame (take `b`).
pub fn mix_rotation(a: &[i16; 9], b: &[i16; 9], one: f64, alpha: u32) -> Option<[i16; 9]> {
    if a == b {
        return Some(*b);
    }
    let (ma, mb) = (a.map(|v| v as f64 / one), b.map(|v| v as f64 / one));
    if !(is_rotation(&ma, 0.02) && is_rotation(&mb, 0.02)) {
        return Some(std::array::from_fn(|k| lerp(a[k] as i64, b[k] as i64, alpha) as i16));
    }
    let (qa, mut qb) = (quat(&ma), quat(&mb));
    let mut dot: f64 = (0..4).map(|i| qa[i] * qb[i]).sum();
    if dot < 0.0 {
        qb = qb.map(|v| -v);
        dot = -dot;
    }
    if dot < MAX_TURN_COS {
        return None;
    }
    let t = alpha as f64 / 256.0;
    let q: [f64; 4] = std::array::from_fn(|i| qa[i] + (qb[i] - qa[i]) * t);
    let n = q.iter().map(|v| v * v).sum::<f64>().sqrt();
    let m = from_quat(q.map(|v| v / n));
    Some(m.map(|v| (v * one).round().clamp(-32768.0, 32767.0) as i16))
}

// ---- what drawing changes -----------------------------------------------------------------------

/// Guest memory put back after a picture: `range` except `left` (kept as the picture left it:
/// a memory manager's lists whose records are elsewhere; anything other threads may change).
pub struct Statics {
    pub range: (u32, u32),
    pub left: &'static [(u32, u32)],
}

impl Statics {
    pub fn save(&self) -> Vec<u8> {
        mem().slice(self.range.0, (self.range.1 - self.range.0) as usize).to_vec()
    }

    pub fn restore(&self, mut saved: Vec<u8>) {
        let m = mem();
        for &(lo, hi) in self.left {
            let at = (lo - self.range.0) as usize;
            saved[at..at + (hi - lo) as usize].copy_from_slice(m.slice(lo, (hi - lo) as usize));
        }
        m.slice(self.range.0, saved.len()).copy_from_slice(&saved);
    }
}

// ---- calls ------------------------------------------------------------------------------------

/// Registers and x87 state, kept around our own calls into guest code from a hook.
pub struct Saved {
    r: [u32; 8],
    flags: [u8; 6],
    st: [f64; 8],
    st_int: [i64; 8],
    st_int_of: [u64; 8],
    top: u32,
    fsw: u16,
    fcw: u16,
}

pub fn save(c: &Cpu) -> Saved {
    Saved { r: c.r, flags: [c.cf, c.pf, c.zf, c.sf, c.of, c.df], st: c.st, st_int: c.st_int, st_int_of: c.st_int_of, top: c.top, fsw: c.fsw, fcw: c.fcw }
}

pub fn restore(c: &mut Cpu, s: &Saved) {
    c.r = s.r;
    [c.cf, c.pf, c.zf, c.sf, c.of, c.df] = s.flags;
    c.st = s.st;
    c.st_int = s.st_int;
    c.st_int_of = s.st_int_of;
    c.top = s.top;
    c.fsw = s.fsw;
    c.fcw = s.fcw;
}

/// Call a register-convention function (Watcom: eax, edx, ebx, ecx) from a hook; the
/// caller's registers and x87 state are kept. Returns eax.
pub fn call_regs(c: &mut Cpu, addr: u32, args: [u32; 4]) -> u32 {
    let s = save(c);
    (c.r[EAX], c.r[EDX], c.r[EBX], c.r[ECX]) = (args[0], args[1], args[2], args[3]);
    let r = super::guest_call(c, addr, &[]);
    restore(c, &s);
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rot(ax: [f64; 3], a: f64) -> [f64; 9] {
        let (s, c) = a.sin_cos();
        let [x, y, z] = ax;
        let t = 1.0 - c;
        [t * x * x + c, t * x * y - s * z, t * x * z + s * y, t * x * y + s * z, t * y * y + c, t * y * z - s * x, t * x * z - s * y, t * y * z + s * x, t * z * z + c]
    }

    #[test]
    fn quaternion_round_trip() {
        let n = 14f64.sqrt();
        for ax in [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [1.0 / n, 2.0 / n, -3.0 / n]] {
            for k in 0..24 {
                let m = rot(ax, k as f64 * 0.27);
                let back = from_quat(quat(&m));
                assert!((0..9).all(|i| (m[i] - back[i]).abs() < 1e-9), "{ax:?} {k}");
            }
        }
    }

    fn fixed(m: [f64; 9]) -> [i16; 9] {
        m.map(|v| (v * 4096.0).round() as i16)
    }

    #[test]
    fn rotation_halfway() {
        let (a, b) = (fixed(rot([0.0, 1.0, 0.0], 0.0)), fixed(rot([0.0, 1.0, 0.0], 0.4)));
        let mid = mix_rotation(&a, &b, 4096.0, 128).unwrap();
        let want = fixed(rot([0.0, 1.0, 0.0], 0.2));
        assert!((0..9).all(|i| (mid[i] - want[i]).abs() <= 2), "{mid:?} {want:?}");
        // a rotor's turn: not interpolated
        assert_eq!(mix_rotation(&a, &fixed(rot([0.0, 1.0, 0.0], 2.0)), 4096.0, 128), None);
        // scaled: element by element
        let s = [8192, 0, 0, 0, 8192, 0, 0, 0, 8192];
        assert_eq!(mix_rotation(&a, &s, 4096.0, 128).unwrap()[0], 6144);
    }

    #[test]
    fn angles_short_way() {
        assert_eq!(lerp_angle(4000, 100, 128, 12), (4000 + 98) & 4095);
        assert_eq!(lerp_angle(100, 300, 64, 12), 150);
    }

    #[test]
    fn history() {
        let h = Hist::note(None, 5, 1);
        assert_eq!(h.prev, None);
        let h = Hist::note(Some(&h), 6, 2);
        assert_eq!((h.prev, h.cur), (Some(1), 2));
        let h2 = Hist::note(Some(&h), 6, 3);
        assert_eq!((h2.prev, h2.cur), (Some(1), 3));
        assert_eq!(Hist::note(Some(&h), 9, 4).prev, None);
    }

    #[test]
    fn clock_alpha() {
        let c = LogicClock::new(60);
        assert_eq!(c.alpha(Instant::now()), None);
        c.frame(|| {}, Instant::now);
        let (due, _) = c.newest().unwrap();
        assert_eq!(c.alpha(due), Some(0));
        assert_eq!(c.alpha(due + c.frame_time() / 2), Some(128));
        assert_eq!(c.alpha(due + c.frame_time() * 3), Some(256));
    }
}
