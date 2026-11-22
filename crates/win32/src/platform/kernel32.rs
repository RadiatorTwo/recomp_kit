//! KERNEL32: process and C runtime start-up, memory, files, threads and synchronisation.

use super::{Import, alloc, module, alloc_aligned, arg, block_size, cstr, free, guest_call, host_path, log, mem, new_cstr, put_cstr, realloc, set_last_error};
use recomp::rt::Cpu;
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

const INVALID_HANDLE: u32 = 0xffff_ffff;
const WAIT_TIMEOUT: u32 = 0x102;
const INFINITE: u32 = 0xffff_ffff;
const ERROR_FILE_NOT_FOUND: u32 = 2;
const ERROR_INVALID_HANDLE: u32 = 6;

// ---- kernel objects -----------------------------------------------------------------------------

pub enum Obj {
    File(Arc<Mutex<std::fs::File>>),
    /// standard input/output/error (-10, -11, -12)
    Std(u32),
    Event { manual: bool, set: bool },
    Mutex { owner: u32, count: u32 },
    Thread { id: u32, done: bool, code: u32 },
    Heap,
    /// FindFirstFileA's results and the next one
    Find { entries: Vec<FindEntry>, next: usize },
}

pub struct Objects {
    map: BTreeMap<u32, Obj>,
    next: u32,
    /// critical sections by guest address: owner thread, recursion count
    cs: BTreeMap<u32, (u32, u32)>,
}

static OBJS: Mutex<Objects> = Mutex::new(Objects { map: BTreeMap::new(), next: 0x100, cs: BTreeMap::new() });
/// notified whenever an object becomes signalled
static SIGNAL: Condvar = Condvar::new();

pub fn objects() -> MutexGuard<'static, Objects> {
    OBJS.lock().unwrap_or_else(|e| e.into_inner())
}

/// For a savestate's fingerprint (`savestate.rs`): each handle and its kind.
pub fn object_kinds() -> Vec<(u32, u32)> {
    objects().map.iter().map(|(&h, o)| (h, match o { Obj::File(_) => 1, Obj::Std(_) => 2, Obj::Event { .. } => 3, Obj::Mutex { .. } => 4, Obj::Thread { .. } => 5, Obj::Heap => 6, Obj::Find { .. } => 7 })).collect()
}

/// What a savestate keeps of the objects: events' state, files' positions.
pub fn object_states() -> Vec<(u32, u64)> {
    use std::io::Seek;
    let g = objects();
    let mut v = Vec::new();
    for (&h, o) in &g.map {
        match o {
            Obj::Event { set, .. } => v.push((h, *set as u64)),
            Obj::File(f) => v.push((h, f.lock().unwrap().stream_position().unwrap_or(0))),
            _ => {}
        }
    }
    v
}

/// A savestate's events and file positions put back.
pub fn set_object_states(states: &[(u32, u64)]) {
    use std::io::{Seek, SeekFrom};
    let mut g = objects();
    for &(h, s) in states {
        match g.map.get_mut(&h) {
            Some(Obj::Event { set, .. }) => *set = s != 0,
            Some(Obj::File(f)) => {
                let _ = f.lock().unwrap().seek(SeekFrom::Start(s));
            }
            _ => {}
        }
    }
    drop(g);
    SIGNAL.notify_all();
}

impl Objects {
    pub fn add(&mut self, o: Obj) -> u32 {
        self.next += 4;
        self.map.insert(self.next, o);
        self.next
    }
}

fn wait_signal(g: MutexGuard<'static, Objects>, until: Option<Instant>) -> (MutexGuard<'static, Objects>, bool) {
    match until {
        None => (SIGNAL.wait(g).unwrap_or_else(|e| e.into_inner()), false),
        Some(t) => {
            let now = Instant::now();
            if now >= t {
                return (g, true);
            }
            let (g, r) = SIGNAL.wait_timeout(g, t - now).unwrap_or_else(|e| e.into_inner());
            (g, r.timed_out() && Instant::now() >= t)
        }
    }
}

static NEXT_TID: AtomicU32 = AtomicU32::new(1);
thread_local! {
    static TID: u32 = NEXT_TID.fetch_add(1, Ordering::Relaxed);
    static TLS: RefCell<Vec<u32>> = const { RefCell::new(Vec::new()) };
    static EXIT_CODE: Cell<u32> = const { Cell::new(0) };
}

pub fn thread_id() -> u32 {
    TID.with(|t| *t)
}

// ---- process ------------------------------------------------------------------------------------

fn get_version(_: &mut Cpu) -> u32 {
    0xc000_0a04 // Windows 98
}

fn get_command_line(_: &mut Cpu) -> u32 {
    static P: OnceLock<u32> = OnceLock::new();
    *P.get_or_init(|| new_cstr(crate::game::id().exe))
}

fn get_command_line_w(_: &mut Cpu) -> u32 {
    static P: OnceLock<u32> = OnceLock::new();
    *P.get_or_init(|| new_wstr(crate::game::id().exe))
}

/// A new NUL-terminated UTF-16 guest string (from the heap, never freed).
pub fn new_wstr(s: &str) -> u32 {
    let w: Vec<u16> = s.encode_utf16().chain([0]).collect();
    let p = alloc(2 * w.len() as u32);
    for (k, &u) in w.iter().enumerate() {
        mem().wr16(p + 2 * k as u32, u);
    }
    p
}

/// GetVersionExA(OSVERSIONINFOA*): Windows 98 (as GetVersion).
fn get_version_ex(c: &mut Cpu) -> u32 {
    let p = arg(c, 0);
    let m = mem();
    let size = m.rd32(p).clamp(148, 156);
    m.slice(p + 4, size as usize - 4).fill(0);
    m.wr32(p + 4, 4); // major
    m.wr32(p + 8, 10); // minor
    m.wr32(p + 12, 0x040a_07ce); // build 1998 with the version in the high word
    m.wr32(p + 16, 1); // VER_PLATFORM_WIN32_WINDOWS
    1
}

/// LocalAlloc(flags, size): our handles are the memory (always zeroed).
fn local_alloc(c: &mut Cpu) -> u32 {
    alloc(arg(c, 1))
}

/// LocalReAlloc / GlobalReAlloc(handle, size, flags): the handle is the memory; it may move
/// (as with LMEM_MOVEABLE).
fn local_realloc(c: &mut Cpu) -> u32 {
    realloc(arg(c, 0), arg(c, 1))
}

fn local_size(c: &mut Cpu) -> u32 {
    block_size(arg(c, 0))
}

fn local_free(c: &mut Cpu) -> u32 {
    free(arg(c, 0));
    0
}

/// 100 ns steps since 1601-01-01 (FILETIME) now, UTC.
fn file_time_now() -> u64 {
    let d = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    d.as_nanos() as u64 / 100 + 116_444_736_000_000_000
}

/// A FILETIME as SYSTEMTIME's eight fields: year, month, weekday, day, hour, minute, second,
/// milliseconds.
fn system_time(t: u64) -> [u16; 8] {
    let ms_total = t / 10_000;
    let (ms, secs) = (ms_total % 1000, ms_total / 1000);
    let days = (secs / 86400) as i64 - 134_774; // 1601-01-01 to 1970-01-01
    // civil from days (Howard Hinnant's algorithm)
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + (month <= 2) as i64;
    let weekday = (days + 4).rem_euclid(7);
    let s = secs % 86400;
    [year, month, weekday, day, (s / 3600) as i64, (s / 60 % 60) as i64, (s % 60) as i64, ms as i64].map(|v| v as u16)
}

fn put_system_time(p: u32, st: [u16; 8]) {
    for (k, v) in st.into_iter().enumerate() {
        mem().wr16(p + 2 * k as u32, v);
    }
}

/// GetSystemTime and GetLocalTime: UTC (the time zone is UTC).
fn get_local_time(c: &mut Cpu) -> u32 {
    put_system_time(arg(c, 0), system_time(file_time_now()));
    0
}

/// FileTimeToLocalFileTime and back: the same (UTC).
fn file_time_to_local_file_time(c: &mut Cpu) -> u32 {
    let m = mem();
    let (from, to) = (arg(c, 0), arg(c, 1));
    let (lo, hi) = (m.rd32(from), m.rd32(from + 4));
    m.wr32(to, lo);
    m.wr32(to + 4, hi);
    1
}

fn file_time_to_system_time(c: &mut Cpu) -> u32 {
    let m = mem();
    let (ft, st) = (arg(c, 0), arg(c, 1));
    put_system_time(st, system_time(m.rd32(ft) as u64 | (m.rd32(ft + 4) as u64) << 32));
    1
}

/// SystemTimeToFileTime(SYSTEMTIME*, FILETIME*)
fn system_time_to_file_time(c: &mut Cpu) -> u32 {
    let m = mem();
    let (st, ft) = (arg(c, 0), arg(c, 1));
    let (y, mo, d) = (m.rd16(st) as i64, m.rd16(st + 2) as i64, m.rd16(st + 6) as i64);
    let (h, mi, s, ms) = (m.rd16(st + 8) as i64, m.rd16(st + 10) as i64, m.rd16(st + 12) as i64, m.rd16(st + 14) as i64);
    // days since 1970-01-01 (Howard Hinnant's algorithm), then since 1601
    let y2 = if mo <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let doy = (153 * (if mo > 2 { mo - 3 } else { mo + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468 + 134_774;
    let t = ((((days * 24 + h) * 60 + mi) * 60 + s) * 1000 + ms) as u64 * 10_000;
    m.wr32(ft, t as u32);
    m.wr32(ft + 4, (t >> 32) as u32);
    1
}

/// GetUserDefaultLCID: English (United States).
fn get_user_default_lcid(_: &mut Cpu) -> u32 {
    0x0409
}

/// MulDiv(a, b, c): a * b / c rounded, -1 when c is 0.
fn mul_div(c: &mut Cpu) -> u32 {
    let (a, b, d) = (arg(c, 0) as i32 as i64, arg(c, 1) as i32 as i64, arg(c, 2) as i32 as i64);
    if d == 0 {
        return u32::MAX;
    }
    let p = a * b;
    let half = d.abs() / 2;
    let r = if (p < 0) != (d < 0) { (p - half * p.signum()) / d } else { (p + half * p.signum()) / d };
    r as i32 as u32
}

/// lstrcpynA(dst, src, n): at most n - 1 characters and the NUL.
fn lstrcpyn(c: &mut Cpu) -> u32 {
    let (dst, src, n) = (arg(c, 0), arg(c, 1), arg(c, 2));
    put_cstr(dst, &cstr(src), n);
    dst
}

/// FindResourceA(module, name, type): a block with the data's address and size (HRSRC).
fn find_resource(c: &mut Cpu) -> u32 {
    let (h, name, ty) = (arg(c, 0), arg(c, 1), arg(c, 2));
    let (name, ty) = (super::ResId::from_guest(name, cstr), super::ResId::from_guest(ty, cstr));
    match super::resource_in(h, ty, name) {
        Some((at, size)) => {
            let p = alloc(8);
            mem().wr32(p, at);
            mem().wr32(p + 4, size);
            p
        }
        None => {
            set_last_error(1814); // ERROR_RESOURCE_NAME_NOT_FOUND
            0
        }
    }
}

/// LoadResource(module, HRSRC): the data's address (LockResource gives it back as it is).
fn load_resource(c: &mut Cpu) -> u32 {
    let r = arg(c, 1);
    if r == 0 { 0 } else { mem().rd32(r) }
}

fn sizeof_resource(c: &mut Cpu) -> u32 {
    let r = arg(c, 1);
    if r == 0 { 0 } else { mem().rd32(r + 4) }
}

/// GetSystemDirectoryA(buf, n): Windows 98's.
fn get_system_directory(c: &mut Cpu) -> u32 {
    let (buf, n) = (arg(c, 0), arg(c, 1));
    let dir = r"C:\WINDOWS\SYSTEM";
    if dir.len() as u32 >= n {
        return dir.len() as u32 + 1;
    }
    put_cstr(buf, dir, n)
}

/// Message `id` of a module's message table (RT_MESSAGETABLE 1): blocks of (low, high,
/// offset), entries of (length, flags: 1 is UTF-16, text).
fn message_text(base: u32, id: u32) -> Option<String> {
    let (at, _) = super::resource_in(base, super::ResId::Id(11), super::ResId::Id(1))?;
    let m = mem();
    for b in 0..m.rd32(at) {
        let blk = at + 4 + 12 * b;
        let (lo, hi, mut e) = (m.rd32(blk), m.rd32(blk + 4), at + m.rd32(blk + 8));
        if !(lo..=hi).contains(&id) {
            continue;
        }
        for _ in lo..id {
            e += m.rd16(e) as u32;
        }
        let (len, wide) = (m.rd16(e) as u32, m.rd16(e + 2) & 1 != 0);
        let body = e + 4;
        let s = if wide {
            let units: Vec<u16> = (0..(len - 4) / 2).map(|k| m.rd16(body + 2 * k)).take_while(|&u| u != 0).collect();
            String::from_utf16_lossy(&units)
        } else {
            (0..len - 4).map(|k| m.rd8(body + k)).take_while(|&b| b != 0).map(|b| b as char).collect()
        };
        return Some(s);
    }
    None
}

/// FormatMessageA(flags, source, id, language, buf, n, args): from a module's message table
/// (FROM_HMODULE 0x800, else the EXE) or a string (FROM_STRING 0x400); inserts %1.. from a
/// va_list or an array (ARGUMENT_ARRAY 0x2000), unless IGNORE_INSERTS (0x200). System
/// messages (FROM_SYSTEM 0x1000) are "Error n". ALLOCATE_BUFFER (0x100) writes a new buffer's
/// address to `buf`.
fn format_message(c: &mut Cpu) -> u32 {
    let (flags, src, id, buf, n, args) = (arg(c, 0), arg(c, 1), arg(c, 2), arg(c, 4), arg(c, 5), arg(c, 6));
    let text = if flags & 0x400 != 0 {
        Some(cstr(src))
    } else if flags & 0x800 != 0 {
        message_text(src, id)
    } else if flags & 0x1000 != 0 {
        Some(format!("Error {id}\r\n"))
    } else {
        message_text(super::IMAGE_BASE, id)
    };
    let Some(text) = text else {
        set_last_error(317); // ERROR_MR_MID_NOT_FOUND
        return 0;
    };
    let m = mem();
    let insert = |k: u32| -> u32 {
        if args == 0 {
            0
        } else if flags & 0x2000 != 0 {
            m.rd32(args + 4 * k)
        } else {
            // a va_list*: the list starts where it points
            m.rd32(m.rd32(args) + 4 * k)
        }
    };
    let mut out = String::new();
    let ch: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < ch.len() {
        if ch[i] != '%' || i + 1 >= ch.len() || flags & 0x200 != 0 {
            out.push(ch[i]);
            i += 1;
            continue;
        }
        i += 1;
        match ch[i] {
            'n' => out += "\r\n",
            '0' => break,
            d if d.is_ascii_digit() => {
                let mut k = 0;
                while i < ch.len() && ch[i].is_ascii_digit() {
                    k = k * 10 + ch[i].to_digit(10).unwrap();
                    i += 1;
                }
                // %k!fmt! : a printf format, default s
                let mut fmt = "%s".to_string();
                if i < ch.len() && ch[i] == '!' {
                    if let Some(end) = ch[i + 1..].iter().position(|&c| c == '!') {
                        fmt = format!("%{}", ch[i + 1..i + 1 + end].iter().collect::<String>());
                        i += end + 2;
                    }
                }
                let v = insert(k.saturating_sub(1));
                out += &super::printf(&fmt, |_| v);
                continue;
            }
            other => out.push(other),
        }
        i += 1;
    }
    let len = out.len() as u32;
    if flags & 0x100 != 0 {
        let p = alloc(len.max(n) + 1);
        put_cstr(p, &out, len + 1);
        m.wr32(buf, p);
    } else {
        put_cstr(buf, &out, n);
    }
    len.min(if flags & 0x100 != 0 { len } else { n.saturating_sub(1) })
}

fn lstrcmp(c: &mut Cpu) -> u32 {
    cstr(arg(c, 0)).cmp(&cstr(arg(c, 1))) as i32 as u32
}

fn lstrcat(c: &mut Cpu) -> u32 {
    let (dst, src) = (arg(c, 0), arg(c, 1));
    let s = cstr(src);
    let at = dst + cstr(dst).len() as u32;
    put_cstr(at, &s, s.len() as u32 + 1);
    dst
}

/// lstrcmpiA: compared without case (as the C locale).
fn lstrcmpi(c: &mut Cpu) -> u32 {
    let (a, b) = (cstr(arg(c, 0)).to_ascii_lowercase(), cstr(arg(c, 1)).to_ascii_lowercase());
    a.cmp(&b) as i32 as u32
}

/// GlobalMemoryStatus(MEMORYSTATUS*): 1 GB of memory, 768 MB free, 2 GB address space.
fn global_memory_status(c: &mut Cpu) -> u32 {
    let p = arg(c, 0);
    let m = mem();
    for (k, v) in [32u32, 25, 1 << 30, 768 << 20, 2 << 30, 2 << 30, 0x7ffe_0000, 0x6000_0000].into_iter().enumerate() {
        m.wr32(p + 4 * k as u32, v);
    }
    0
}

fn interlocked_exchange(c: &mut Cpu) -> u32 {
    let (p, v) = (arg(c, 0), arg(c, 1));
    let old = mem().rd32(p);
    mem().wr32(p, v);
    old
}

/// GetProcessVersion: Windows 4.10 for every process.
fn get_process_version(_: &mut Cpu) -> u32 {
    0x0004_000a
}

fn get_current_process_id(_: &mut Cpu) -> u32 {
    0x100
}

/// No time zone (UTC): TIME_ZONE_INFORMATION zeroed, TIME_ZONE_ID_UNKNOWN.
fn get_time_zone_information(c: &mut Cpu) -> u32 {
    mem().slice(arg(c, 0), 172).fill(0);
    0
}

fn get_startup_info(c: &mut Cpu) -> u32 {
    let p = arg(c, 0);
    mem().slice(p, 68).fill(0);
    mem().wr32(p, 68);
    0
}

/// The EXE (no name), a game DLL whose DllMain has run, the EXE by its name, or a system DLL.
fn get_module_handle(c: &mut Cpu) -> u32 {
    let name = arg(c, 0);
    if name == 0 {
        return super::IMAGE_BASE;
    }
    let name = cstr(name);
    if let Some(m) = module::module(&name) {
        return if module::attached(m) { m.base } else { 0 };
    }
    let file = name.rsplit(['\\', '/']).next().unwrap_or(&name);
    let exe = crate::game::id().exe;
    if file.eq_ignore_ascii_case(exe) || exe.rsplit_once('.').is_some_and(|(s, _)| file.eq_ignore_ascii_case(s)) {
        return super::IMAGE_BASE;
    }
    if file.eq_ignore_ascii_case("glide2x.dll") || file.eq_ignore_ascii_case("glide2x") {
        return super::glide::MODULE;
    }
    match module::system_module(&name) {
        Some(h) => h,
        None => {
            set_last_error(126);
            0
        }
    }
}

/// Where the EXE is, as the game sees it.
fn exe_path() -> String {
    format!("{}\\{}", crate::game::id().install_dir, crate::game::id().exe)
}

fn get_module_file_name(c: &mut Cpu) -> u32 {
    let (h, buf, n) = (arg(c, 0), arg(c, 1), arg(c, 2));
    let path = match module::module_at(h) {
        Some(m) => m.guest_path(),
        None => exe_path(),
    };
    put_cstr(buf, &path, n)
}

fn get_environment_strings(_: &mut Cpu) -> u32 {
    alloc(4) // empty block: two NULs (also as UTF-16)
}

fn exit_process(c: &mut Cpu) -> u32 {
    let code = arg(c, 0);
    log!("ExitProcess({code})");
    super::advapi32::save();
    terminate(code)
}

/// Ends the process at once, without the libraries' clean-up: with our audio, gamepad, video
/// and OpenGL threads killed mid-way, a driver's clean-up could wait forever for a lock one of
/// them held (issue #7: "not responding" after the credits). Everything worth keeping is
/// written before.
/// The log tells where a hang was: after "ending the process" it is inside Windows (a thread
/// stuck in a driver keeps the process alive), before it in our saving.
pub fn terminate(code: u32) -> ! {
    log!("ending the process");
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::Foundation::GetLastError;
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, TerminateProcess};
        if TerminateProcess(GetCurrentProcess(), code) == 0 {
            log!("TerminateProcess failed (error {}), exiting normally", GetLastError());
        }
    }
    #[cfg(unix)]
    unsafe {
        libc::_exit(code as i32);
    }
    #[allow(unreachable_code)]
    std::process::exit(code as i32)
}

fn exit_thread(c: &mut Cpu) -> u32 {
    EXIT_CODE.set(arg(c, 0));
    c.stop = 1;
    0
}

fn get_system_info(c: &mut Cpu) -> u32 {
    let p = arg(c, 0);
    let m = mem();
    m.slice(p, 36).fill(0);
    m.wr32(p + 4, 0x1000); // page size
    m.wr32(p + 8, 0x10000);
    m.wr32(p + 12, 0x7ffe_ffff);
    m.wr32(p + 16, 1);
    m.wr32(p + 20, 1); // processors
    m.wr32(p + 24, 586); // Pentium
    m.wr32(p + 28, 0x10000);
    0
}

fn get_computer_name(c: &mut Cpu) -> u32 {
    let (buf, len) = (arg(c, 0), arg(c, 1));
    let n = mem().rd32(len);
    let k = put_cstr(buf, crate::game::id().computer_name, n);
    mem().wr32(len, k);
    1
}

fn output_debug_string(c: &mut Cpu) -> u32 {
    log!("debug: {}", cstr(arg(c, 0)).trim_end());
    0
}

fn raise_exception(c: &mut Cpu) -> u32 {
    log!("RaiseException {:#x}", arg(c, 0));
    std::process::exit(102)
}

// ---- drives (the CD check) ----------------------------------------------------------------------

fn get_logical_drives(_: &mut Cpu) -> u32 {
    1 << 2 | 1 << 3 // C: and D:
}

/// GetLogicalDriveStringsA(n, buf): "C:\", "D:\" (the CD), each NUL-terminated, then a NUL.
fn get_logical_drive_strings(c: &mut Cpu) -> u32 {
    let (n, buf) = (arg(c, 0), arg(c, 1));
    let s = b"C:\\\0D:\\\0\0";
    if n < s.len() as u32 {
        return s.len() as u32;
    }
    mem().slice(buf, s.len()).copy_from_slice(s);
    s.len() as u32 - 1
}

fn get_drive_type(c: &mut Cpu) -> u32 {
    let p = arg(c, 0);
    match if p == 0 { b'C' } else { mem().rd8(p).to_ascii_uppercase() } {
        b'C' => 3, // fixed
        b'D' => 5, // CD-ROM
        _ => 1,    // no root directory
    }
}

fn get_volume_information(c: &mut Cpu) -> u32 {
    let root = arg(c, 0);
    let cd = root != 0 && mem().rd8(root).eq_ignore_ascii_case(&b'D');
    let m = mem();
    let (label, serial) = crate::game::id().cd.filter(|_| cd).unwrap_or(("SYSTEM", 0x1234_5678));
    put_cstr(arg(c, 1), label, arg(c, 2));
    for (k, v) in [(3, serial), (4, 255), (5, 0)] {
        if arg(c, k) != 0 {
            m.wr32(arg(c, k), v);
        }
    }
    put_cstr(arg(c, 6), if cd { "CDFS" } else { "FAT32" }, arg(c, 7));
    1
}

// ---- strings and code pages ---------------------------------------------------------------------

fn get_acp(_: &mut Cpu) -> u32 {
    1252
}
fn get_oemcp(_: &mut Cpu) -> u32 {
    437
}

fn get_cp_info(c: &mut Cpu) -> u32 {
    let p = arg(c, 1);
    let m = mem();
    m.slice(p, 20).fill(0);
    m.wr32(p, 1); // single-byte
    m.wr8(p + 4, b'?');
    1
}

/// Length of a string argument given as (pointer, count), count -1 meaning NUL-terminated
/// including the NUL.
fn str_len(p: u32, n: u32, wide: bool) -> u32 {
    if n != 0xffff_ffff {
        return n;
    }
    let m = mem();
    let mut k = 0;
    while if wide { m.rd16(p + 2 * k) != 0 } else { m.rd8(p + k) != 0 } {
        k += 1;
    }
    k + 1
}

fn multi_byte_to_wide_char(c: &mut Cpu) -> u32 {
    let (src, n, dst, dn) = (arg(c, 2), arg(c, 3), arg(c, 4), arg(c, 5));
    let n = str_len(src, n, false);
    if dn == 0 {
        return n;
    }
    let m = mem();
    let k = n.min(dn);
    for i in 0..k {
        m.wr16(dst + 2 * i, m.rd8(src + i) as u16);
    }
    k
}

fn wide_char_to_multi_byte(c: &mut Cpu) -> u32 {
    let (src, n, dst, dn) = (arg(c, 2), arg(c, 3), arg(c, 4), arg(c, 5));
    let n = str_len(src, n, true);
    if dn == 0 {
        return n;
    }
    let m = mem();
    let k = n.min(dn);
    for i in 0..k {
        let w = m.rd16(src + 2 * i);
        m.wr8(dst + i, if w < 256 { w as u8 } else { b'?' });
    }
    k
}

fn ctype(ch: u16) -> u16 {
    let Ok(b) = u8::try_from(ch) else { return 0x100 };
    let c = b as char;
    let mut t = 0;
    if c.is_ascii_uppercase() || (0xc0..=0xde).contains(&b) && b != 0xd7 {
        t |= 0x101;
    }
    if c.is_ascii_lowercase() || (0xdf..=0xff).contains(&b) && b != 0xf7 {
        t |= 0x102;
    }
    if c.is_ascii_digit() {
        t |= 4;
    }
    if matches!(b, 9..=13 | 32) {
        t |= 8;
    }
    if c.is_ascii_punctuation() {
        t |= 0x10;
    }
    if b < 32 || b == 127 {
        t |= 0x20;
    }
    if b == 9 || b == 32 {
        t |= 0x40;
    }
    if c.is_ascii_hexdigit() {
        t |= 0x80;
    }
    t
}

fn get_string_type_a(c: &mut Cpu) -> u32 {
    let (src, n, out) = (arg(c, 2), arg(c, 3), arg(c, 4));
    let n = str_len(src, n, false);
    for i in 0..n {
        mem().wr16(out + 2 * i, ctype(mem().rd8(src + i) as u16));
    }
    1
}

fn get_string_type_w(c: &mut Cpu) -> u32 {
    let (src, n, out) = (arg(c, 1), arg(c, 2), arg(c, 3));
    let n = str_len(src, n, true);
    for i in 0..n {
        mem().wr16(out + 2 * i, ctype(mem().rd16(src + 2 * i)));
    }
    1
}

fn lc_map(c: &mut Cpu, wide: bool) -> u32 {
    let (flags, src, n, dst, dn) = (arg(c, 1), arg(c, 2), arg(c, 3), arg(c, 4), arg(c, 5));
    let n = str_len(src, n, wide);
    if dn == 0 {
        return n;
    }
    let m = mem();
    let k = n.min(dn);
    for i in 0..k {
        let v = if wide { m.rd16(src + 2 * i) as u8 } else { m.rd8(src + i) };
        let v = match flags & 0x300 {
            0x100 => v.to_ascii_lowercase(),
            0x200 => v.to_ascii_uppercase(),
            _ => v,
        };
        if wide { m.wr16(dst + 2 * i, v as u16) } else { m.wr8(dst + i, v) }
    }
    k
}
fn lc_map_string_a(c: &mut Cpu) -> u32 {
    lc_map(c, false)
}
fn lc_map_string_w(c: &mut Cpu) -> u32 {
    lc_map(c, true)
}

fn compare(c: &mut Cpu, wide: bool) -> u32 {
    let (flags, a, an, b, bn) = (arg(c, 1), arg(c, 2), arg(c, 3), arg(c, 4), arg(c, 5));
    let get = |p: u32, n: u32| -> Vec<u8> {
        let n = str_len(p, n, wide);
        let mut v: Vec<u8> = (0..n).map(|i| if wide { mem().rd16(p + 2 * i) as u8 } else { mem().rd8(p + i) }).collect();
        while v.last() == Some(&0) {
            v.pop();
        }
        if flags & 1 != 0 {
            v.make_ascii_lowercase();
        }
        v
    };
    match get(a, an).cmp(&get(b, bn)) {
        std::cmp::Ordering::Less => 1,
        std::cmp::Ordering::Equal => 2,
        std::cmp::Ordering::Greater => 3,
    }
}
fn compare_string_a(c: &mut Cpu) -> u32 {
    compare(c, false)
}
fn compare_string_w(c: &mut Cpu) -> u32 {
    compare(c, true)
}

fn locale_info(kind: u32) -> &'static str {
    match kind & 0xffff {
        0x1004 => "1252", // LOCALE_IDEFAULTANSICODEPAGE
        0xb => "437",     // LOCALE_IDEFAULTCODEPAGE
        0x1 => "0409",    // LOCALE_ILANGUAGE
        0x3 => "ENU",
        0x7 => "USA",
        0xe => ".",
        0xf => ",",
        _ => "",
    }
}

fn get_locale_info_a(c: &mut Cpu) -> u32 {
    let (kind, buf, n) = (arg(c, 1), arg(c, 2), arg(c, 3));
    let s = locale_info(kind);
    if n == 0 { s.len() as u32 + 1 } else { put_cstr(buf, s, n) + 1 }
}

fn get_locale_info_w(c: &mut Cpu) -> u32 {
    let (kind, buf, n) = (arg(c, 1), arg(c, 2), arg(c, 3));
    let s = locale_info(kind);
    if n == 0 {
        return s.len() as u32 + 1;
    }
    let k = (s.len() as u32).min(n - 1);
    for (i, b) in s.bytes().take(k as usize).enumerate() {
        mem().wr16(buf + 2 * i as u32, b as u16);
    }
    mem().wr16(buf + 2 * k, 0);
    k + 1
}

fn lstrlen(c: &mut Cpu) -> u32 {
    let p = arg(c, 0);
    if p == 0 { 0 } else { str_len(p, 0xffff_ffff, false) - 1 }
}

fn lstrcpy(c: &mut Cpu) -> u32 {
    let (d, s) = (arg(c, 0), arg(c, 1));
    let n = str_len(s, 0xffff_ffff, false);
    let b = mem().slice(s, n as usize).to_vec();
    mem().slice(d, n as usize).copy_from_slice(&b);
    d
}

// ---- memory -------------------------------------------------------------------------------------

fn heap_create(_: &mut Cpu) -> u32 {
    objects().add(Obj::Heap)
}

fn heap_alloc(c: &mut Cpu) -> u32 {
    alloc(arg(c, 2))
}

fn heap_free(c: &mut Cpu) -> u32 {
    free(arg(c, 2)) as u32
}

fn heap_realloc(c: &mut Cpu) -> u32 {
    // HEAP_REALLOC_IN_PLACE_ONLY: only when it fits already
    let (flags, p, n) = (arg(c, 1), arg(c, 2), arg(c, 3));
    if flags & 0x10 != 0 {
        return if block_size(p) >= n { p } else { 0 };
    }
    realloc(p, n)
}

fn heap_size(c: &mut Cpu) -> u32 {
    let p = arg(c, 2);
    match block_size(p) {
        0 => 0xffff_ffff,
        n => n,
    }
}

fn virtual_alloc(c: &mut Cpu) -> u32 {
    let (p, n) = (arg(c, 0), arg(c, 1));
    if p != 0 {
        // committing pages of an earlier reservation: ours are committed already
        return p;
    }
    // Windows' allocation granularity: Nuclear Strike's memory manager (SSMemAllocAlign)
    // corrupted its free list when a heap did not start 64 KB aligned
    alloc_aligned(n, 0x10000)
}

fn virtual_free(c: &mut Cpu) -> u32 {
    let (p, kind) = (arg(c, 0), arg(c, 2));
    if kind & 0x8000 != 0 {
        free(p);
    }
    1
}

fn virtual_protect(c: &mut Cpu) -> u32 {
    let old = arg(c, 3);
    if old != 0 {
        mem().wr32(old, 4); // PAGE_READWRITE
    }
    1
}

fn global_alloc(c: &mut Cpu) -> u32 {
    alloc(arg(c, 1))
}
fn global_free(c: &mut Cpu) -> u32 {
    free(arg(c, 0));
    0
}
/// GlobalLock, GlobalUnlock, GlobalHandle: our handles are the memory itself
fn global_same(c: &mut Cpu) -> u32 {
    arg(c, 0)
}
fn global_unlock(_: &mut Cpu) -> u32 {
    0
}

// ---- thread-local storage, critical sections, interlocked ---------------------------------------

static TLS_NEXT: AtomicU32 = AtomicU32::new(0);

fn tls_alloc(_: &mut Cpu) -> u32 {
    TLS_NEXT.fetch_add(1, Ordering::Relaxed)
}

fn tls_get_value(c: &mut Cpu) -> u32 {
    let k = arg(c, 0) as usize;
    set_last_error(0);
    TLS.with(|t| t.borrow().get(k).copied().unwrap_or(0))
}

fn tls_set_value(c: &mut Cpu) -> u32 {
    let (k, v) = (arg(c, 0) as usize, arg(c, 1));
    TLS.with(|t| {
        let mut t = t.borrow_mut();
        if t.len() <= k {
            t.resize(k + 1, 0);
        }
        t[k] = v;
    });
    1
}

fn init_cs(c: &mut Cpu) -> u32 {
    let p = arg(c, 0);
    mem().slice(p, 24).fill(0);
    objects().cs.insert(p, (0, 0));
    0
}

fn delete_cs(c: &mut Cpu) -> u32 {
    objects().cs.remove(&arg(c, 0));
    0
}

fn enter_cs(c: &mut Cpu) -> u32 {
    let p = arg(c, 0);
    let me = thread_id();
    let mut g = objects();
    loop {
        let e = g.cs.entry(p).or_insert((0, 0));
        if e.0 == 0 || e.0 == me {
            e.0 = me;
            e.1 += 1;
            return 0;
        }
        g = wait_signal(g, None).0;
    }
}

fn leave_cs(c: &mut Cpu) -> u32 {
    let p = arg(c, 0);
    let mut g = objects();
    if let Some(e) = g.cs.get_mut(&p) {
        e.1 = e.1.saturating_sub(1);
        if e.1 == 0 {
            e.0 = 0;
            SIGNAL.notify_all();
        }
    }
    0
}

fn interlocked(c: &mut Cpu, d: u32) -> u32 {
    let p = arg(c, 0);
    let _g = objects();
    let v = mem().rd32(p).wrapping_add(d);
    mem().wr32(p, v);
    v
}
fn interlocked_increment(c: &mut Cpu) -> u32 {
    interlocked(c, 1)
}
fn interlocked_decrement(c: &mut Cpu) -> u32 {
    interlocked(c, u32::MAX)
}

// ---- events, mutexes, waiting -------------------------------------------------------------------

fn create_event(c: &mut Cpu) -> u32 {
    objects().add(Obj::Event { manual: arg(c, 1) != 0, set: arg(c, 2) != 0 })
}

/// Set or reset event `h` (also from host threads: `dplay.rs`); 0 if it is no event.
pub fn set_event_state(h: u32, on: bool) -> u32 {
    let mut g = objects();
    match g.map.get_mut(&h) {
        Some(Obj::Event { set, .. }) => {
            *set = on;
            SIGNAL.notify_all();
            1
        }
        _ => 0,
    }
}
fn set_event(c: &mut Cpu) -> u32 {
    set_event_state(arg(c, 0), true)
}
fn reset_event(c: &mut Cpu) -> u32 {
    set_event_state(arg(c, 0), false)
}
fn pulse_event(c: &mut Cpu) -> u32 {
    // wakes the waiters there are now: approximated by a short signalled moment
    let h = arg(c, 0);
    set_event_state(h, true);
    std::thread::yield_now();
    set_event_state(h, false)
}

fn create_mutex(c: &mut Cpu) -> u32 {
    let owner = if arg(c, 1) != 0 { thread_id() } else { 0 };
    objects().add(Obj::Mutex { owner, count: (owner != 0) as u32 })
}

/// Whether `h` is signalled for this thread; with `take`, also what a successful wait does to it.
fn signalled(g: &mut Objects, h: u32, take: bool) -> bool {
    let me = thread_id();
    match g.map.get_mut(&h) {
        Some(Obj::Event { manual, set }) => {
            let s = *set;
            if s && take && !*manual {
                *set = false;
            }
            s
        }
        Some(Obj::Mutex { owner, count }) => {
            let s = *owner == 0 || *owner == me;
            if s && take {
                *owner = me;
                *count += 1;
            }
            s
        }
        Some(Obj::Thread { done, .. }) => *done,
        _ => true,
    }
}

fn wait(handles: &[u32], all: bool, ms: u32) -> u32 {
    let until = (ms != INFINITE).then(|| Instant::now() + Duration::from_millis(ms as u64));
    let mut g = objects();
    loop {
        if all {
            if handles.iter().all(|&h| signalled(&mut g, h, false)) {
                for &h in handles {
                    signalled(&mut g, h, true);
                }
                return 0;
            }
        } else if let Some(k) = handles.iter().position(|&h| signalled(&mut g, h, false)) {
            signalled(&mut g, handles[k], true);
            return k as u32;
        }
        let (ng, timed_out) = wait_signal(g, until);
        g = ng;
        if timed_out {
            return WAIT_TIMEOUT;
        }
    }
}

fn wait_for_single_object(c: &mut Cpu) -> u32 {
    wait(&[arg(c, 0)], false, arg(c, 1))
}

fn wait_for_multiple_objects(c: &mut Cpu) -> u32 {
    let (n, p, all, ms) = (arg(c, 0), arg(c, 1), arg(c, 2), arg(c, 3));
    let hs: Vec<u32> = (0..n).map(|k| mem().rd32(p + 4 * k)).collect();
    wait(&hs, all != 0, ms)
}

fn close_handle(c: &mut Cpu) -> u32 {
    let h = arg(c, 0);
    let mut g = objects();
    // a thread handle stays until the thread ends (it only marks itself done)
    if matches!(g.map.get(&h), Some(Obj::Thread { done: false, .. })) {
        return 1;
    }
    if g.map.remove(&h).is_some() {
        1
    } else {
        set_last_error(ERROR_INVALID_HANDLE);
        0
    }
}

fn duplicate_handle(c: &mut Cpu) -> u32 {
    let (src, out) = (arg(c, 1), arg(c, 3));
    let h = if src == 0xffff_fffe { thread_handle_of(thread_id()) } else { src };
    if out != 0 {
        mem().wr32(out, h);
    }
    1
}

// ---- threads ------------------------------------------------------------------------------------

fn thread_handle_of(id: u32) -> u32 {
    let g = objects();
    g.map.iter().find(|(_, o)| matches!(o, Obj::Thread { id: i, .. } if *i == id)).map(|(&h, _)| h).unwrap_or(0xffff_fffe)
}

fn create_thread(c: &mut Cpu) -> u32 {
    let (start, param, flags, id_out) = (arg(c, 2), arg(c, 3), arg(c, 4), arg(c, 5));
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(move || {
            let id = thread_id();
            if flags & 4 != 0 {
                // CREATE_SUSPENDED: it waits in guest_call (turn_take) for ResumeThread
                super::guest_thread(id).suspend();
            }
            tx.send(id).unwrap();
            let mut tc = super::new_thread_cpu();
            let r = guest_call(&mut tc, start, &[param]);
            let code = if tc.stop != 0 { EXIT_CODE.get() } else { r };
            let mut g = objects();
            for o in g.map.values_mut() {
                if let Obj::Thread { id: i, done, code: c, .. } = o {
                    if *i == id {
                        *done = true;
                        *c = code;
                    }
                }
            }
            SIGNAL.notify_all();
        })
        .expect("cannot start a thread");
    let id = rx.recv().unwrap();
    if id_out != 0 {
        mem().wr32(id_out, id);
    }
    objects().add(Obj::Thread { id, done: false, code: 0 })
}

/// The guest thread a handle names (GetCurrentThread's pseudo handle: the caller).
fn thread_of(h: u32) -> Option<Arc<super::GuestThread>> {
    if h == 0xffff_fffe {
        return Some(super::guest_thread(thread_id()));
    }
    match objects().map.get(&h) {
        Some(&Obj::Thread { id, .. }) => Some(super::guest_thread(id)),
        _ => None,
    }
}

fn resume_thread(c: &mut Cpu) -> u32 {
    thread_of(arg(c, 0)).map_or(0xffff_ffff, |t| t.resume())
}

/// SuspendThread: the thread stops at its next loop head (see `GuestThread`).
fn suspend_thread(c: &mut Cpu) -> u32 {
    let Some(t) = thread_of(arg(c, 0)) else { return 0xffff_ffff };
    let was = t.suspend();
    t.wait_if_self();
    was
}

/// SetThreadPriority: the turn goes to waiting threads by priority (see `turn_yield`). Games
/// set their message loop's thread low, timers and audio high, and TIME_CRITICAL while they
/// measure the processor's speed.
fn set_thread_priority(c: &mut Cpu) -> u32 {
    let Some(t) = thread_of(arg(c, 0)) else { return 0 };
    t.prio.store(arg(c, 1) as i32, Ordering::Relaxed);
    1
}

fn get_thread_priority(c: &mut Cpu) -> u32 {
    thread_of(arg(c, 0)).map_or(0x7fff_ffff, |t| t.prio.load(Ordering::Relaxed) as u32)
}

fn terminate_thread(c: &mut Cpu) -> u32 {
    log!("warn: TerminateThread({:#x}) ignored", arg(c, 0));
    1
}

fn get_current_thread_id(_: &mut Cpu) -> u32 {
    thread_id()
}

// ---- time ---------------------------------------------------------------------------------------

pub fn start_time() -> Instant {
    static T: OnceLock<Instant> = OnceLock::new();
    *T.get_or_init(Instant::now)
}

pub fn ms_since_start() -> u32 {
    start_time().elapsed().as_millis() as u32
}

/// The guest's clocks (QueryPerformanceCounter, GetTickCount, timeGetTime) are this many
/// µs ahead of `start_time`: a savestate puts them back to its time (`savestate.rs`).
static CLOCK_SHIFT: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

/// The guest's clock in µs.
pub fn guest_us() -> u64 {
    (start_time().elapsed().as_micros() as i64 + CLOCK_SHIFT.load(Ordering::Relaxed)).max(0) as u64
}

/// The guest's clock reads `us` now.
pub fn set_guest_us(us: u64) {
    CLOCK_SHIFT.store(us as i64 - start_time().elapsed().as_micros() as i64, Ordering::Relaxed);
}

pub fn guest_ms() -> u32 {
    (guest_us() / 1000) as u32
}

fn get_tick_count(_: &mut Cpu) -> u32 {
    guest_ms()
}

fn query_performance_frequency(c: &mut Cpu) -> u32 {
    let p = arg(c, 0);
    mem().wr32(p, 1_000_000);
    mem().wr32(p + 4, 0);
    1
}

fn query_performance_counter(c: &mut Cpu) -> u32 {
    let p = arg(c, 0);
    let t = guest_us();
    mem().wr32(p, t as u32);
    mem().wr32(p + 4, (t >> 32) as u32);
    1
}

fn sleep(c: &mut Cpu) -> u32 {
    let ms = arg(c, 0);
    // Sleep(0) gives the rest of the time slice to the threads that are ready. Guest threads run
    // one at a time here, and a thread back from an import waits for the running one to hand
    // over at a loop head: Nuclear Strike's message loop (PeekMessageA, Sleep(0)) took the turn
    // back some 800 000 times a second, the game thread got little done (slow menu, long
    // loading). With another thread running it really sleeps a millisecond. So does a thread
    // below normal priority: a LOWEST message loop yielded at once whenever the game thread was
    // in host code (Glide, swaps), took and gave the turn ~340 000 times a second and kept a
    // core busy; the game thread was ~10% slower and its frame rate swung more.
    let low = super::my_thread().prio.load(Ordering::Relaxed) < 0;
    if ms == 0 && !super::others_running() && !low {
        std::thread::yield_now()
    } else {
        std::thread::sleep(Duration::from_millis(ms.max(1) as u64))
    }
    0
}

// ---- files --------------------------------------------------------------------------------------

/// The mission file opened last (lower case): a savestate is only for the mission it was
/// made in.
pub static LAST_MISSION: Mutex<String> = Mutex::new(String::new());

/// Whether a host path lies inside the game folder (its folder exists and is below it). The
/// CD's top folder (the game folder's parent, read as a fallback) is never written.
fn inside_game(p: &std::path::Path) -> bool {
    let Some(dir) = p.parent().and_then(|d| std::fs::canonicalize(d).ok()) else { return false };
    std::fs::canonicalize(super::game_dir()).is_ok_and(|g| dir.starts_with(g))
}

fn create_file(c: &mut Cpu) -> u32 {
    let (name, access, disp) = (cstr(arg(c, 0)), arg(c, 1), arg(c, 4));
    let path = host_path(&name);
    let write = access & 0x4000_0000 != 0;
    if (write || matches!(disp, 2 | 5)) && !inside_game(&path) {
        log!("CreateFileA {name}: writing outside the game folder refused ({})", path.display());
        set_last_error(5); // ERROR_ACCESS_DENIED
        return INVALID_HANDLE;
    }

    let mut o = std::fs::OpenOptions::new();
    o.read(access & 0x8000_0000 != 0 || !write).write(write);
    match disp {
        1 => o.create_new(true),
        2 => o.create(true).truncate(true),
        4 => o.create(true),
        5 => o.truncate(true),
        _ => &mut o,
    };
    match o.open(&path) {
        Ok(f) => {
            if super::trace_api() {
                log!("CreateFileA {name} (access {access:#x}, disposition {disp})");
            }
            if name.to_ascii_lowercase().contains("missions\\") {
                *LAST_MISSION.lock().unwrap() = name.to_ascii_lowercase();
            }
            objects().add(Obj::File(Arc::new(Mutex::new(f))))
        }
        Err(e) => {
            if super::trace_api() {
                log!("CreateFileA {name}: {e}");
            }
            set_last_error(if e.kind() == std::io::ErrorKind::AlreadyExists { 80 } else { ERROR_FILE_NOT_FOUND });
            INVALID_HANDLE
        }
    }
}

/// A file or folder found by FindFirstFileA.
pub struct FindEntry {
    name: String,
    attributes: u32,
    size: u64,
    time: u64,
}

/// FILE_ATTRIBUTE_ flags: directory 0x10, read-only 1, else normal 0x80.
fn attributes(md: &std::fs::Metadata) -> u32 {
    if md.is_dir() {
        0x10
    } else if md.permissions().readonly() {
        1
    } else {
        0x80
    }
}

/// A host file time as a FILETIME.
fn file_time(t: std::time::SystemTime) -> u64 {
    let d = t.duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    d.as_nanos() as u64 / 100 + 116_444_736_000_000_000
}

/// `*` and `?` matched without case; `*.*` matches every name.
fn wildcard(mask: &str, name: &str) -> bool {
    fn go(m: &[u8], n: &[u8]) -> bool {
        match (m.first(), n.first()) {
            (None, None) => true,
            (Some(b'*'), _) => go(&m[1..], n) || !n.is_empty() && go(m, &n[1..]),
            (Some(b'?'), Some(_)) => go(&m[1..], &n[1..]),
            (Some(a), Some(b)) => a.eq_ignore_ascii_case(b) && go(&m[1..], &n[1..]),
            _ => false,
        }
    }
    mask == "*.*" || go(mask.as_bytes(), name.as_bytes())
}

/// WIN32_FIND_DATAA: attributes, three times, size, reserved, name (260), short name (14).
fn write_find_data(p: u32, e: &FindEntry) {
    let m = mem();
    m.slice(p, 320).fill(0);
    m.wr32(p, e.attributes);
    for k in 0..3 {
        m.wr32(p + 4 + 8 * k, e.time as u32);
        m.wr32(p + 8 + 8 * k, (e.time >> 32) as u32);
    }
    m.wr32(p + 28, (e.size >> 32) as u32);
    m.wr32(p + 32, e.size as u32);
    put_cstr(p + 44, &e.name, 260);
}

fn find_first_file(c: &mut Cpu) -> u32 {
    let (pattern, data) = (cstr(arg(c, 0)), arg(c, 1));
    let (dir, mask) = match pattern.rfind(['\\', '/']) {
        Some(k) => (&pattern[..k + 1], &pattern[k + 1..]),
        None => ("", pattern.as_str()),
    };
    let entry = |name: String, md: &std::fs::Metadata| FindEntry { name, attributes: attributes(md), size: md.len(), time: md.modified().map_or(0, file_time) };
    let mut entries = Vec::new();
    if !mask.contains(['*', '?']) {
        // a path without wildcards: that file or folder itself
        if let Ok(md) = std::fs::metadata(host_path(&pattern)) {
            entries.push(entry(mask.to_string(), &md));
        }
    } else if let Ok(list) = std::fs::read_dir(host_path(if dir.is_empty() { "." } else { dir })) {
        for e in list.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if wildcard(mask, &name) {
                if let Ok(md) = e.metadata() {
                    entries.push(entry(name, &md));
                }
            }
        }
    }
    entries.sort_by_key(|e| e.name.to_lowercase());
    if super::trace_api() {
        log!("FindFirstFileA {pattern}: {} found", entries.len());
    }
    let Some(first) = entries.first() else {
        set_last_error(ERROR_FILE_NOT_FOUND);
        return INVALID_HANDLE;
    };
    write_find_data(data, first);
    objects().add(Obj::Find { entries, next: 1 })
}

fn find_next_file(c: &mut Cpu) -> u32 {
    let (h, data) = (arg(c, 0), arg(c, 1));
    let mut g = objects();
    let Some(Obj::Find { entries, next }) = g.map.get_mut(&h) else {
        drop(g);
        set_last_error(ERROR_INVALID_HANDLE);
        return 0;
    };
    match entries.get(*next) {
        Some(e) => {
            write_find_data(data, e);
            *next += 1;
            1
        }
        None => {
            drop(g);
            set_last_error(18); // ERROR_NO_MORE_FILES
            0
        }
    }
}

fn find_close(c: &mut Cpu) -> u32 {
    objects().map.remove(&arg(c, 0)).is_some() as u32
}

fn get_file_attributes(c: &mut Cpu) -> u32 {
    match std::fs::metadata(host_path(&cstr(arg(c, 0)))) {
        Ok(md) => attributes(&md),
        Err(_) => {
            set_last_error(ERROR_FILE_NOT_FOUND);
            INVALID_HANDLE
        }
    }
}

fn get_file_size(c: &mut Cpu) -> u32 {
    let (h, hi) = (arg(c, 0), arg(c, 1));
    let Some(f) = file_of(h) else {
        set_last_error(ERROR_INVALID_HANDLE);
        return INVALID_HANDLE;
    };
    let n = f.lock().unwrap().metadata().map_or(0, |m| m.len());
    if hi != 0 {
        mem().wr32(hi, (n >> 32) as u32);
    }
    n as u32
}

/// GetCurrentDirectoryA(n, buf): the installed game's folder (relative paths are the game
/// folder's, `host_path`).
fn get_current_directory(c: &mut Cpu) -> u32 {
    let (n, buf) = (arg(c, 0), arg(c, 1));
    let dir = crate::game::id().install_dir;
    if dir.len() as u32 >= n {
        return dir.len() as u32 + 1;
    }
    put_cstr(buf, dir, n)
}

/// SetCurrentDirectoryA: relative paths stay the game folder's.
fn set_current_directory(c: &mut Cpu) -> u32 {
    if super::trace_api() {
        log!("SetCurrentDirectoryA {}", cstr(arg(c, 0)));
    }
    1
}

/// GetFullPathNameA(name, n, buf, &file part).
fn get_full_path_name(c: &mut Cpu) -> u32 {
    let (name, n, buf, part) = (cstr(arg(c, 0)), arg(c, 1), arg(c, 2), arg(c, 3));
    let full = if name.len() >= 2 && name.as_bytes()[1] == b':' {
        name.clone()
    } else if let Some(rest) = name.strip_prefix(['\\', '/']) {
        format!("C:\\{rest}")
    } else {
        format!("{}\\{}", crate::game::id().install_dir, name.trim_start_matches(".\\"))
    };
    if full.len() as u32 >= n {
        return full.len() as u32 + 1;
    }
    let len = put_cstr(buf, &full, n);
    if part != 0 {
        let k = full.rfind('\\').map_or(0, |k| k + 1) as u32;
        mem().wr32(part, buf + k);
    }
    len
}

fn create_directory(c: &mut Cpu) -> u32 {
    let p = host_path(&cstr(arg(c, 0)));
    if p.exists() {
        set_last_error(183); // ERROR_ALREADY_EXISTS
        return 0;
    }
    std::fs::create_dir(&p).is_ok() as u32
}

/// DeleteFileA: only inside the game folder (a name found in the CD's top folder, the parent,
/// is never deleted).
fn delete_file(c: &mut Cpu) -> u32 {
    let name = cstr(arg(c, 0));
    let path = host_path(&name);
    if !inside_game(&path) {
        log!("DeleteFileA {name}: outside the game folder, kept ({})", path.display());
        set_last_error(5); // ERROR_ACCESS_DENIED
        return 0;
    }
    if std::fs::remove_file(&path).is_ok() {
        1
    } else {
        set_last_error(ERROR_FILE_NOT_FOUND);
        0
    }
}

/// GetDiskFreeSpaceA(root, sectors a cluster, bytes a sector, free clusters, clusters): 1 GB
/// free of 2 GB.
fn get_disk_free_space(c: &mut Cpu) -> u32 {
    for (k, v) in [(1, 8), (2, 512), (3, 262_144), (4, 524_288)] {
        if arg(c, k) != 0 {
            mem().wr32(arg(c, k), v);
        }
    }
    1
}

fn file_of(h: u32) -> Option<Arc<Mutex<std::fs::File>>> {
    match objects().map.get(&h) {
        Some(Obj::File(f)) => Some(f.clone()),
        _ => None,
    }
}

fn read_file(c: &mut Cpu) -> u32 {
    let (h, buf, n, done) = (arg(c, 0), arg(c, 1), arg(c, 2), arg(c, 3));
    let Some(f) = file_of(h) else {
        set_last_error(ERROR_INVALID_HANDLE);
        return 0;
    };
    let dst = mem().slice(buf, n as usize);
    let mut f = f.lock().unwrap();
    let mut got = 0;
    while got < dst.len() {
        match f.read(&mut dst[got..]) {
            Ok(0) => break,
            Ok(k) => got += k,
            Err(_) => return 0,
        }
    }
    if done != 0 {
        mem().wr32(done, got as u32);
    }
    1
}

fn write_file(c: &mut Cpu) -> u32 {
    let (h, buf, n, done) = (arg(c, 0), arg(c, 1), arg(c, 2), arg(c, 3));
    let data = mem().slice(buf, n as usize).to_vec();
    let ok = match objects().map.get(&h) {
        Some(Obj::Std(_)) => {
            eprint!("{}", String::from_utf8_lossy(&data));
            true
        }
        Some(Obj::File(f)) => f.lock().unwrap().write_all(&data).is_ok(),
        _ => false,
    };
    if done != 0 {
        mem().wr32(done, if ok { n } else { 0 });
    }
    ok as u32
}

fn set_file_pointer(c: &mut Cpu) -> u32 {
    let (h, lo, hi_p, how) = (arg(c, 0), arg(c, 1), arg(c, 2), arg(c, 3));
    let Some(f) = file_of(h) else {
        set_last_error(ERROR_INVALID_HANDLE);
        return 0xffff_ffff;
    };
    let d = if hi_p != 0 { ((mem().rd32(hi_p) as u64) << 32 | lo as u64) as i64 } else { lo as i32 as i64 };
    let pos = match how {
        0 => SeekFrom::Start(d as u64),
        1 => SeekFrom::Current(d),
        _ => SeekFrom::End(d),
    };
    match f.lock().unwrap().seek(pos) {
        Ok(p) => {
            if hi_p != 0 {
                mem().wr32(hi_p, (p >> 32) as u32);
            }
            p as u32
        }
        Err(_) => {
            set_last_error(131);
            0xffff_ffff
        }
    }
}

fn set_end_of_file(c: &mut Cpu) -> u32 {
    let Some(f) = file_of(arg(c, 0)) else { return 0 };
    let mut f = f.lock().unwrap();
    let Ok(p) = f.stream_position() else { return 0 };
    f.set_len(p).is_ok() as u32
}

fn flush_file_buffers(c: &mut Cpu) -> u32 {
    file_of(arg(c, 0)).map_or(0, |f| f.lock().unwrap().flush().is_ok() as u32)
}

fn get_std_handle(c: &mut Cpu) -> u32 {
    let n = arg(c, 0);
    let mut g = objects();
    if let Some((&h, _)) = g.map.iter().find(|(_, o)| matches!(o, Obj::Std(k) if *k == n)) {
        return h;
    }
    g.add(Obj::Std(n))
}

fn get_file_type(c: &mut Cpu) -> u32 {
    match objects().map.get(&arg(c, 0)) {
        Some(Obj::File(_)) => 1,
        Some(Obj::Std(_)) => 2,
        _ => 0,
    }
}

// ---- simple ones --------------------------------------------------------------------------------

fn ret0(_: &mut Cpu) -> u32 {
    0
}
fn ret1(_: &mut Cpu) -> u32 {
    1
}
fn get_last_error(_: &mut Cpu) -> u32 {
    super::LAST_ERROR.get()
}
fn set_last_error_imp(c: &mut Cpu) -> u32 {
    set_last_error(arg(c, 0));
    0
}
fn get_current_process(_: &mut Cpu) -> u32 {
    0xffff_ffff
}
fn get_current_thread(_: &mut Cpu) -> u32 {
    0xffff_fffe
}
fn first_arg(c: &mut Cpu) -> u32 {
    arg(c, 0)
}
/// The DLLs the game loads itself: glide2x.dll is ours (`glide.rs`).
fn load_library(c: &mut Cpu) -> u32 {
    let name = cstr(arg(c, 0));
    if name.eq_ignore_ascii_case("glide2x.dll") {
        return super::glide::MODULE;
    }
    // a game DLL: its DllMain runs now if it has not yet
    if let Some(m) = module::module(&name) {
        module::attach(c, m);
        return m.base;
    }
    if let Some(h) = module::system_module(&name) {
        return h;
    }
    log!("LoadLibraryA({name}): not available");
    set_last_error(126);
    0
}
fn get_proc_address(c: &mut Cpu) -> u32 {
    let (h, name) = (arg(c, 0), arg(c, 1));
    let name = if name > 0xffff { cstr(name) } else { format!("#{name}") };
    if h == super::glide::MODULE {
        return super::glide::proc_address(&name);
    }
    let a = match module::module_at(h) {
        Some(m) => m.export(&name),
        None => module::system_proc(h, &name),
    };
    if a == 0 {
        log!("GetProcAddress({h:#x}, {name}): not available");
        set_last_error(127);
    }
    a
}

pub const IMPORTS: &[Import] = &[
    ("GetVersion", 0, get_version),
    ("GetCommandLineA", 0, get_command_line),
    ("GetCommandLineW", 0, get_command_line_w),
    ("GetTimeZoneInformation", 1, get_time_zone_information),
    ("GetVersionExA", 1, get_version_ex),
    ("LocalAlloc", 2, local_alloc),
    ("LocalFree", 1, local_free),
    ("LocalReAlloc", 3, local_realloc),
    ("LocalLock", 1, global_same),
    ("LocalUnlock", 1, ret1),
    ("LocalSize", 1, local_size),
    ("GlobalReAlloc", 3, local_realloc),
    ("GlobalSize", 1, local_size),
    ("GlobalFlags", 1, ret0),
    ("GetSystemTime", 1, get_local_time),
    ("GetLocalTime", 1, get_local_time),
    ("FileTimeToLocalFileTime", 2, file_time_to_local_file_time),
    ("LocalFileTimeToFileTime", 2, file_time_to_local_file_time),
    ("FileTimeToSystemTime", 2, file_time_to_system_time),
    ("SystemTimeToFileTime", 2, system_time_to_file_time),
    ("GetUserDefaultLCID", 0, get_user_default_lcid),
    ("GetThreadLocale", 0, get_user_default_lcid),
    ("GetSystemDefaultLangID", 0, get_user_default_lcid),
    ("IsValidCodePage", 1, ret1),
    ("IsValidLocale", 2, ret1),
    ("IsDBCSLeadByte", 1, ret0),
    ("MulDiv", 3, mul_div),
    ("lstrcatA", 2, lstrcat),
    ("lstrcpynA", 3, lstrcpyn),
    ("lstrcmpA", 2, lstrcmp),
    ("FindResourceA", 3, find_resource),
    ("FindFirstFileA", 2, find_first_file),
    ("FindNextFileA", 2, find_next_file),
    ("FindClose", 1, find_close),
    ("GetFileAttributesA", 1, get_file_attributes),
    ("GetFileSize", 2, get_file_size),
    ("GetCurrentDirectoryA", 2, get_current_directory),
    ("SetCurrentDirectoryA", 1, set_current_directory),
    ("GetFullPathNameA", 4, get_full_path_name),
    ("CreateDirectoryA", 2, create_directory),
    ("DeleteFileA", 1, delete_file),
    ("GetDiskFreeSpaceA", 5, get_disk_free_space),
    ("FormatMessageA", 7, format_message),
    ("GetSystemDirectoryA", 2, get_system_directory),
    ("LoadResource", 2, load_resource),
    ("LockResource", 1, first_arg),
    ("SizeofResource", 2, sizeof_resource),
    ("FreeResource", 1, ret1),
    ("lstrcmpiA", 2, lstrcmpi),
    ("GlobalMemoryStatus", 1, global_memory_status),
    ("InterlockedExchange", 2, interlocked_exchange),
    ("GetCurrentProcessId", 0, get_current_process_id),
    ("GetProcessVersion", 1, get_process_version),
    ("SetUnhandledExceptionFilter", 1, ret0),
    ("IsBadReadPtr", 2, ret0),
    ("IsBadWritePtr", 2, ret0),
    ("IsBadCodePtr", 1, ret0),
    ("TlsFree", 1, ret1),
    ("DisableThreadLibraryCalls", 1, ret1),
    ("GetStartupInfoA", 1, get_startup_info),
    ("GetModuleHandleA", 1, get_module_handle),
    ("GetModuleFileNameA", 3, get_module_file_name),
    ("GetEnvironmentStrings", 0, get_environment_strings),
    ("GetEnvironmentStringsW", 0, get_environment_strings),
    ("FreeEnvironmentStringsA", 1, ret1),
    ("FreeEnvironmentStringsW", 1, ret1),
    ("SetEnvironmentVariableA", 2, ret1),
    ("SetEnvironmentVariableW", 2, ret1),
    ("ExitProcess", 1, exit_process),
    ("TerminateProcess", 2, exit_process_from_terminate),
    ("ExitThread", 1, exit_thread),
    ("GetSystemInfo", 1, get_system_info),
    ("GetComputerNameA", 2, get_computer_name),
    ("OutputDebugStringA", 1, output_debug_string),
    ("RaiseException", 4, raise_exception),
    ("RtlUnwind", 4, ret0),
    ("UnhandledExceptionFilter", 1, ret0),
    ("DebugBreak", 0, ret0),
    ("Beep", 2, ret1),
    ("SetErrorMode", 1, ret0),
    ("GetLastError", 0, get_last_error),
    ("SetLastError", 1, set_last_error_imp),
    ("GetCurrentProcess", 0, get_current_process),
    ("GetCurrentThread", 0, get_current_thread),
    ("GetCurrentThreadId", 0, get_current_thread_id),
    ("SetHandleCount", 1, first_arg),
    ("SetStdHandle", 2, ret1),
    ("LoadLibraryA", 1, load_library),
    ("FreeLibrary", 1, ret1),
    ("GetProcAddress", 2, get_proc_address),
    ("GetLogicalDrives", 0, get_logical_drives),
    ("GetDriveTypeA", 1, get_drive_type),
    ("GetLogicalDriveStringsA", 2, get_logical_drive_strings),
    ("GetVolumeInformationA", 8, get_volume_information),
    ("GetACP", 0, get_acp),
    ("GetOEMCP", 0, get_oemcp),
    ("GetCPInfo", 2, get_cp_info),
    ("MultiByteToWideChar", 6, multi_byte_to_wide_char),
    ("WideCharToMultiByte", 8, wide_char_to_multi_byte),
    ("GetStringTypeA", 5, get_string_type_a),
    ("GetStringTypeW", 4, get_string_type_w),
    ("LCMapStringA", 6, lc_map_string_a),
    ("LCMapStringW", 6, lc_map_string_w),
    ("CompareStringA", 6, compare_string_a),
    ("CompareStringW", 6, compare_string_w),
    ("GetLocaleInfoA", 4, get_locale_info_a),
    ("GetLocaleInfoW", 4, get_locale_info_w),
    ("lstrlenA", 1, lstrlen),
    ("lstrcpyA", 2, lstrcpy),
    ("HeapCreate", 3, heap_create),
    ("HeapDestroy", 1, ret1),
    ("HeapAlloc", 3, heap_alloc),
    ("HeapFree", 3, heap_free),
    ("HeapReAlloc", 4, heap_realloc),
    ("HeapSize", 3, heap_size),
    ("VirtualAlloc", 4, virtual_alloc),
    ("VirtualFree", 3, virtual_free),
    ("VirtualProtect", 4, virtual_protect),
    ("GlobalAlloc", 2, global_alloc),
    ("GlobalFree", 1, global_free),
    ("GlobalLock", 1, global_same),
    ("GlobalHandle", 1, global_same),
    ("GlobalUnlock", 1, global_unlock),
    ("TlsAlloc", 0, tls_alloc),
    ("TlsGetValue", 1, tls_get_value),
    ("TlsSetValue", 2, tls_set_value),
    ("InitializeCriticalSection", 1, init_cs),
    ("DeleteCriticalSection", 1, delete_cs),
    ("EnterCriticalSection", 1, enter_cs),
    ("LeaveCriticalSection", 1, leave_cs),
    ("InterlockedIncrement", 1, interlocked_increment),
    ("InterlockedDecrement", 1, interlocked_decrement),
    ("CreateEventA", 4, create_event),
    ("SetEvent", 1, set_event),
    ("ResetEvent", 1, reset_event),
    ("PulseEvent", 1, pulse_event),
    ("CreateMutexA", 3, create_mutex),
    ("WaitForSingleObject", 2, wait_for_single_object),
    ("WaitForMultipleObjects", 4, wait_for_multiple_objects),
    ("WaitForMultipleObjectsEx", 5, wait_for_multiple_objects),
    ("CloseHandle", 1, close_handle),
    ("DuplicateHandle", 7, duplicate_handle),
    ("CreateThread", 6, create_thread),
    ("ResumeThread", 1, resume_thread),
    ("SuspendThread", 1, suspend_thread),
    ("TerminateThread", 2, terminate_thread),
    ("SetThreadPriority", 2, set_thread_priority),
    ("GetThreadPriority", 1, get_thread_priority),
    ("GetTickCount", 0, get_tick_count),
    ("QueryPerformanceFrequency", 1, query_performance_frequency),
    ("QueryPerformanceCounter", 1, query_performance_counter),
    ("Sleep", 1, sleep),
    ("SleepEx", 2, sleep),
    ("CreateFileA", 7, create_file),
    ("ReadFile", 5, read_file),
    ("WriteFile", 5, write_file),
    ("SetFilePointer", 4, set_file_pointer),
    ("SetEndOfFile", 1, set_end_of_file),
    ("FlushFileBuffers", 1, flush_file_buffers),
    ("GetStdHandle", 1, get_std_handle),
    ("GetFileType", 1, get_file_type),
];

fn exit_process_from_terminate(c: &mut Cpu) -> u32 {
    let code = arg(c, 1);
    log!("TerminateProcess({code})");
    super::advapi32::save();
    terminate(code)
}
