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

fn mci_get_error_string(c: &mut Cpu) -> u32 {
    let (buf, n) = (arg(c, 1), arg(c, 2));
    super::put_cstr(buf, "The device is not available.", n);
    1
}

/// AVIStreamOpenFromFileA and AVIFileOpenA: no AVI videos yet (Indeo needs a decoder):
/// AVIERR_FILEOPEN, the game goes on without the video.
fn avi_open(c: &mut Cpu) -> u32 {
    super::log!(
        "AVI {}: not played (no decoder yet)",
        super::cstr(arg(c, 1))
    );
    if arg(c, 0) != 0 {
        mem().wr32(arg(c, 0), 0);
    }
    0x8004_4067
}

/// The host file a guest path names, if it exists.
fn existing(guest: u32) -> Option<std::path::PathBuf> {
    let p = super::host_path(&super::cstr(guest));
    p.exists().then_some(p)
}

/// FindExecutableA(file, dir, result): a program for the file when it exists (the host opens
/// it with its own; 42 > 32 is success), else SE_ERR_FNF.
fn find_executable(c: &mut Cpu) -> u32 {
    if existing(arg(c, 0)).is_none() {
        return 2;
    }
    super::put_cstr(arg(c, 2), "C:\\Windows\\explorer.exe", 260);
    42
}

/// ShellExecuteA(hwnd, verb, file, params, dir, show): the host opens the file with its own
/// program (Recoil opens its manual, Docs\Index.html, in the browser).
fn shell_execute(c: &mut Cpu) -> u32 {
    let Some(p) = existing(arg(c, 2)) else {
        return 2;
    };
    #[cfg(windows)]
    let r = std::process::Command::new("explorer").arg(&p).spawn();
    #[cfg(not(windows))]
    let r = std::process::Command::new("xdg-open").arg(&p).spawn();
    match r {
        Ok(_) => 42,
        Err(e) => {
            super::log!("ShellExecuteA {}: {e}", p.display());
            31 // SE_ERR_NOASSOC
        }
    }
}

/// GetOpenFileNameA: no file dialog; as if the player cancelled (Recoil's launcher window,
/// which is skipped, has one).
fn get_open_file_name(_: &mut Cpu) -> u32 {
    super::log!("GetOpenFileNameA: no file dialog, cancelled");
    0
}

/// WSAStartup(version, WSADATA*): Winsock 2.2. Recoil only reads the version and
/// description; network play is DirectPlay (`dplay.rs`).
fn wsa_startup(c: &mut Cpu) -> u32 {
    let (m, d) = (mem(), arg(c, 1));
    m.wr16(d, 0x0202);
    m.wr16(d + 2, 0x0202);
    super::put_cstr(d + 4, "WinSock 2.0", 257);
    super::put_cstr(d + 4 + 257, "Running", 129);
    m.wr16(d + 390, 0); // iMaxSockets, iMaxUdpDg, lpVendorInfo (aligned)
    m.wr16(d + 392, 0);
    m.wr32(d + 396, 0);
    0
}

/// DirectPlayLobbyCreateA(guid, lobby*, outer, data, size): no lobby (the game was not
/// started by one); DPERR_UNAVAILABLE.
fn direct_play_lobby_create(c: &mut Cpu) -> u32 {
    if arg(c, 1) != 0 {
        mem().wr32(arg(c, 1), 0);
    }
    0x8877_0000 | 1000
}

pub const IMPORTS: &[Import] = &[
    ("AVIFileOpenA", 4, avi_open),
    ("mciSendCommandA", 4, super::cdaudio::mci_send_command),
    ("mciGetErrorStringA", 3, mci_get_error_string),
    ("auxGetNumDevs", 0, super::cdaudio::aux_get_num_devs),
    ("auxGetDevCapsA", 3, super::cdaudio::aux_get_dev_caps),
    ("auxGetVolume", 2, super::cdaudio::aux_get_volume),
    ("auxSetVolume", 2, super::cdaudio::aux_set_volume),
    ("timeGetTime", 0, time_get_time),
    ("timeGetDevCaps", 2, time_get_dev_caps),
    ("timeBeginPeriod", 1, ret0),
    ("timeEndPeriod", 1, ret0),
    ("timeSetEvent", 5, time_set_event),
    ("timeKillEvent", 1, time_kill_event),
    ("CoInitialize", 1, ret0),
    ("CoUninitialize", 0, ret0),
    ("#17", 0, ret0), // InitCommonControls
    ("FindExecutableA", 3, find_executable),
    ("ShellExecuteA", 6, shell_execute),
    ("GetOpenFileNameA", 1, get_open_file_name),
    ("WSAStartup", 2, wsa_startup),
    ("WSACleanup", 0, ret0),
    ("DirectPlayLobbyCreateA", 5, direct_play_lobby_create),
];
