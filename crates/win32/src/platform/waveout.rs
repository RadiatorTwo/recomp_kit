//! waveOut: one output device that plays into the mixer's stream (`dsound::stream_*`). Smacker
//! (SMACKW32.DLL) plays a video's sound this way.
//!
//! A written header is marked done when the mixer has played its last frame: a host thread
//! looks every few ms and then tells the game as asked at the open (nothing, an event or a
//! function called with WOM_DONE).

use super::{Import, arg, dsound, guest_call, mem};
use recomp::rt::Cpu;
use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::Duration;

const HANDLE: u32 = 0x5a5a_0001;
const WHDR_DONE: u32 = 1;
const WHDR_PREPARED: u32 = 2;
const WHDR_INQUEUE: u32 = 0x10;
const WOM_OPEN: u32 = 0x3bb;
const WOM_CLOSE: u32 = 0x3bc;
const WOM_DONE: u32 = 0x3bd;
const NOERROR: u32 = 0;
const INVALHANDLE: u32 = 5;
const BADFORMAT: u32 = 32;

#[derive(Clone, Copy)]
enum Callback {
    None,
    Event(u32),
    Function(u32, u32),
}

struct Device {
    channels: u32,
    bits: u32,
    callback: Callback,
    rate: u32,
    /// headers written and not done: (guest address, the stream's frame count at their end)
    queue: VecDeque<(u32, u64)>,
}

static DEV: Mutex<Option<Device>> = Mutex::new(None);
static EPOCH: Mutex<u32> = Mutex::new(0);

/// The game is told a header is done (outside the lock: a callback may call waveOut again).
fn notify(cb: Callback, tc: Option<&mut Cpu>, msg: u32, hdr: u32) {
    match cb {
        Callback::None => {}
        Callback::Event(h) => {
            super::kernel32::set_event_state(h, true);
        }
        Callback::Function(f, user) => {
            if let Some(tc) = tc {
                guest_call(tc, f, &[HANDLE, msg, user, hdr, 0]);
            }
        }
    }
}

/// Headers the mixer has played: marked done, taken off the queue.
fn finished(all: bool) -> (Callback, Vec<u32>) {
    let mut g = DEV.lock().unwrap();
    let Some(d) = g.as_mut() else {
        return (Callback::None, Vec::new());
    };
    let played = dsound::stream_state().1;
    let mut done = Vec::new();
    while let Some(&(h, end)) = d.queue.front() {
        if !all && end > played {
            break;
        }
        d.queue.pop_front();
        let m = mem();
        m.wr32(h + 0x10, m.rd32(h + 0x10) & !WHDR_INQUEUE | WHDR_DONE);
        done.push(h);
    }
    (d.callback, done)
}

/// The watcher: marks headers done as the mixer plays them, until the device is closed or
/// opened again.
fn watch(epoch: u32) {
    let stack = super::thread_stack();
    std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(move || {
            let mut tc = super::thread_cpu(stack);
            loop {
                std::thread::sleep(Duration::from_millis(5));
                if *EPOCH.lock().unwrap() != epoch {
                    break;
                }
                let (cb, done) = finished(false);
                for h in done {
                    notify(cb, Some(&mut tc), WOM_DONE, h);
                }
            }
        })
        .expect("cannot start the waveOut thread");
}

/// waveOutOpen(out, device, format, callback, instance, flags)
fn open(c: &mut Cpu) -> u32 {
    let (out, fmt, cb, inst, flags) = (arg(c, 0), arg(c, 2), arg(c, 3), arg(c, 4), arg(c, 5));
    let m = mem();
    // WAVEFORMATEX: tag, channels, rate, bytes/s, align, bits
    let tag = m.rd16(fmt) as u32;
    let channels = m.rd16(fmt + 2) as u32;
    let rate = m.rd32(fmt + 4);
    let bits = m.rd16(fmt + 14) as u32;
    if tag != 1 || !(1..=2).contains(&channels) || !(bits == 8 || bits == 16) || rate == 0 {
        super::log!("warn: waveOutOpen: format {tag} {channels}ch {rate} Hz {bits} bits not played");
        return BADFORMAT;
    }
    if flags & 1 != 0 {
        return NOERROR; // WAVE_FORMAT_QUERY
    }
    let callback = match flags & 0x7_0000 {
        0x5_0000 => Callback::Event(cb),
        0x3_0000 => Callback::Function(cb, inst),
        0 => Callback::None,
        k => {
            super::log!("warn: waveOutOpen: callback kind {k:#x} not supported, none used");
            Callback::None
        }
    };
    super::log!("waveOut: {rate} Hz, {channels} channels, {bits} bits, callback {:#x}", flags & 0x7_0000);
    let epoch = {
        let mut e = EPOCH.lock().unwrap();
        *e += 1;
        *e
    };
    dsound::stream_start(rate);
    *DEV.lock().unwrap() = Some(Device {
        channels,
        bits,
        rate,
        callback,
        queue: VecDeque::new(),
    });
    if out != 0 {
        m.wr32(out, HANDLE);
    }
    notify(callback, Some(c), WOM_OPEN, 0);
    watch(epoch);
    NOERROR
}

fn is_open(c: &Cpu) -> bool {
    arg(c, 0) == HANDLE && DEV.lock().unwrap().is_some()
}

/// waveOutClose(h): headers still queued are done first (the game resets before it closes).
fn close(c: &mut Cpu) -> u32 {
    if !is_open(c) {
        return INVALHANDLE;
    }
    let cb = reset_queue(c);
    *DEV.lock().unwrap() = None;
    *EPOCH.lock().unwrap() += 1;
    dsound::stream_stop();
    notify(cb, Some(c), WOM_CLOSE, 0);
    NOERROR
}

/// Every queued header done, the stream emptied.
fn reset_queue(c: &mut Cpu) -> Callback {
    let (cb, done) = finished(true);
    for h in done {
        notify(cb, Some(c), WOM_DONE, h);
    }
    cb
}

/// waveOutReset(h): playing stops, the headers come back done.
fn reset(c: &mut Cpu) -> u32 {
    if !is_open(c) {
        return INVALHANDLE;
    }
    // a new stream: what was queued is not heard any more
    let rate = DEV.lock().unwrap().as_ref().unwrap().rate;
    dsound::stream_start(rate);
    reset_queue(c);
    NOERROR
}

/// waveOutPrepareHeader / waveOutUnprepareHeader (h, hdr, size)
fn prepare(c: &mut Cpu) -> u32 {
    let h = arg(c, 1);
    let m = mem();
    m.wr32(h + 0x10, m.rd32(h + 0x10) | WHDR_PREPARED);
    NOERROR
}
fn unprepare(c: &mut Cpu) -> u32 {
    let h = arg(c, 1);
    let m = mem();
    if m.rd32(h + 0x10) & WHDR_INQUEUE != 0 {
        return 33; // WAVERR_STILLPLAYING
    }
    m.wr32(h + 0x10, m.rd32(h + 0x10) & !WHDR_PREPARED);
    NOERROR
}

/// waveOutWrite(h, hdr, size): the samples go into the stream at once; the header is done
/// when they are played.
fn write(c: &mut Cpu) -> u32 {
    if !is_open(c) {
        return INVALHANDLE;
    }
    let h = arg(c, 1);
    let m = mem();
    let (data, len) = (m.rd32(h), m.rd32(h + 4));
    let mut g = DEV.lock().unwrap();
    let d = g.as_mut().unwrap();
    let bytes = m.slice(data, len as usize);
    let frames: Vec<[f32; 2]> = match (d.bits, d.channels) {
        (16, ch) => bytes
            .chunks_exact(2 * ch as usize)
            .map(|f| {
                let s = |i: usize| i16::from_le_bytes([f[2 * i], f[2 * i + 1]]) as f32 / 32768.0;
                if ch == 2 { [s(0), s(1)] } else { [s(0), s(0)] }
            })
            .collect(),
        (_, ch) => bytes
            .chunks_exact(ch as usize)
            .map(|f| {
                let s = |i: usize| (f[i] as f32 - 128.0) / 128.0;
                if ch == 2 { [s(0), s(1)] } else { [s(0), s(0)] }
            })
            .collect(),
    };
    // done when the mixer has played everything queued up to its last frame
    dsound::stream_push(&frames);
    let (queued, played) = dsound::stream_state();
    d.queue.push_back((h, played + queued as u64));
    m.wr32(h + 0x10, m.rd32(h + 0x10) & !WHDR_DONE | WHDR_INQUEUE);
    NOERROR
}

/// waveOutSetVolume(h, volume): low word left, high word right (0...0xffff).
fn set_volume(c: &mut Cpu) -> u32 {
    let v = arg(c, 1);
    let l = (v & 0xffff).max(v >> 16) as f32 / 65535.0;
    let db100 = if l <= 0.0 { -10000 } else { (2000.0 * l.log10()) as i32 };
    dsound::stream_volume(db100);
    NOERROR
}

/// waveOutGetID(h, id*)
fn get_id(c: &mut Cpu) -> u32 {
    if !is_open(c) {
        return INVALHANDLE;
    }
    if arg(c, 1) != 0 {
        mem().wr32(arg(c, 1), 0);
    }
    NOERROR
}

pub const IMPORTS: &[Import] = &[
    ("waveOutGetNumDevs", 0, |_| 1),
    ("waveOutOpen", 6, open),
    ("waveOutClose", 1, close),
    ("waveOutGetID", 2, get_id),
    ("waveOutPrepareHeader", 3, prepare),
    ("waveOutUnprepareHeader", 3, unprepare),
    ("waveOutWrite", 3, write),
    ("waveOutReset", 1, reset),
    ("waveOutSetVolume", 2, set_volume),
];
