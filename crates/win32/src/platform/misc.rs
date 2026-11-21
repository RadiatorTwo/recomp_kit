//! The small DLLs: WINMM timers, ole32 (COM start-up; CoCreateInstance is in `dshow.rs`, DirectPlay in `dplay.rs`),
//! COMCTL32.

use super::{Import, arg, guest_call, mem};
use recomp::rt::Cpu;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn time_get_time(_: &mut Cpu) -> u32 {
    super::kernel32::guest_ms()
}

fn time_get_dev_caps(c: &mut Cpu) -> u32 {
    let p = arg(c, 0);
    mem().wr32(p, 1);
    mem().wr32(p + 4, 1_000_000);
    0
}

static TIMERS: Mutex<BTreeMap<u32, Arc<AtomicBool>>> = Mutex::new(BTreeMap::new());

/// timeSetEvent(delay, resolution, proc, user, flags): proc(id, 0, user, 0, 0) runs on its own
/// thread, once or (TIME_PERIODIC) every `delay` ms.
fn time_set_event(c: &mut Cpu) -> u32 {
    let (delay, proc_, user, flags) = (arg(c, 0), arg(c, 2), arg(c, 3), arg(c, 4));
    let run = Arc::new(AtomicBool::new(true));
    let id = {
        let mut t = TIMERS.lock().unwrap();
        let id = t.keys().next_back().map_or(1, |k| k + 1);
        t.insert(id, run.clone());
        id
    };
    std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(move || {
            let mut tc = super::new_thread_cpu();
            let start = Instant::now();
            let mut n = 1u64;
            loop {
                let due = start + Duration::from_millis(delay.max(1) as u64 * n);
                std::thread::sleep(due.saturating_duration_since(Instant::now()));
                if !run.load(Ordering::Relaxed) {
                    break;
                }
                guest_call(&mut tc, proc_, &[id, 0, user, 0, 0]);
                if flags & 1 == 0 {
                    break;
                }
                n += 1;
            }
        })
        .expect("cannot start a timer thread");
    id
}

fn time_kill_event(c: &mut Cpu) -> u32 {
    match TIMERS.lock().unwrap().remove(&arg(c, 0)) {
        Some(r) => {
            r.store(false, Ordering::Relaxed);
            0
        }
        None => 97, // MMSYSERR_INVALPARAM
    }
}

fn ret0(_: &mut Cpu) -> u32 {
    0
}

/// mciSendCommandA: no MCI devices yet (CD audio comes from the CD image later):
/// MCIERR_INVALID_DEVICE_NAME.
fn mci_send_command(c: &mut Cpu) -> u32 {
    super::log!("mciSendCommandA({:#x}, {:#x}, {:#x}): no device", arg(c, 0), arg(c, 1), arg(c, 2));
    263
}

fn mci_get_error_string(c: &mut Cpu) -> u32 {
    let (buf, n) = (arg(c, 1), arg(c, 2));
    super::put_cstr(buf, "The device is not available.", n);
    1
}

fn aux_ret0(_: &mut Cpu) -> u32 {
    0
}

/// AVIStreamOpenFromFileA and AVIFileOpenA: no AVI videos yet (Indeo needs a decoder):
/// AVIERR_FILEOPEN, the game goes on without the video.
fn avi_open(c: &mut Cpu) -> u32 {
    super::log!("AVI {}: not played (no decoder yet)", super::cstr(arg(c, 1)));
    if arg(c, 0) != 0 {
        mem().wr32(arg(c, 0), 0);
    }
    0x8004_4067
}

pub const IMPORTS: &[Import] = &[
    ("AVIFileInit", 0, aux_ret0),
    ("AVIFileExit", 0, aux_ret0),
    ("AVIStreamOpenFromFileA", 6, avi_open),
    ("AVIFileOpenA", 4, avi_open),
    ("mciSendCommandA", 4, mci_send_command),
    ("mciGetErrorStringA", 3, mci_get_error_string),
    ("auxGetNumDevs", 0, aux_ret0),
    ("timeGetTime", 0, time_get_time),
    ("timeGetDevCaps", 2, time_get_dev_caps),
    ("timeBeginPeriod", 1, ret0),
    ("timeEndPeriod", 1, ret0),
    ("timeSetEvent", 5, time_set_event),
    ("timeKillEvent", 1, time_kill_event),
    ("CoInitialize", 1, ret0),
    ("CoUninitialize", 0, ret0),
    ("#17", 0, ret0), // InitCommonControls
];
