//! CD audio through MCI (`mciSendCommandA`, device type "cdaudio"): the game plays its music
//! from the CD's audio tracks. They are read straight from the player's CD image: a `.cue`
//! sheet in the game folder or the folder above it (`game_files/<name>.cue` next to the game's
//! folder), with its `.bin` files (raw sectors of 2352 bytes: 588 frames of 16-bit
//! stereo at 44.1 kHz, little-endian). Nothing is extracted; a track is read when it is played
//! and mixed by `dsound.rs`.
//! Without a cue sheet there is no device (the game goes on without music).
//!
//! What games do (Hogs of War 0x45f880...): open the device, set the time format to TMSF (track,
//! minute, second, frame), ask whether a CD is there, the number of tracks, each track's type
//! (audio or data) and length, then play from a track to the end of a track, and poll the mode
//! to start the next piece when one ends.

use super::{arg, log, mem};
use recomp::rt::Cpu;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

const MCIERR_DEVICE_NOT_INSTALLED: u32 = 256 + 50;
const MCIERR_UNRECOGNIZED_COMMAND: u32 = 256 + 5;
const MCIERR_OUTOFRANGE: u32 = 256 + 26;

const MCI_OPEN: u32 = 0x803;
const MCI_CLOSE: u32 = 0x804;
const MCI_PLAY: u32 = 0x806;
const MCI_SEEK: u32 = 0x807;
const MCI_STOP: u32 = 0x808;
const MCI_PAUSE: u32 = 0x809;
const MCI_SET: u32 = 0x80d;
const MCI_STATUS: u32 = 0x814;
const MCI_RESUME: u32 = 0x855;

const MCI_FROM: u32 = 4;
const MCI_TO: u32 = 8;
const MCI_TRACK: u32 = 0x10;
const MCI_STATUS_ITEM: u32 = 0x100;
const MCI_SET_TIME_FORMAT: u32 = 0x400;

const MODE_STOP: u32 = 525;
const MODE_PLAY: u32 = 526;
const MODE_PAUSE: u32 = 529;

/// The time format TMSF: track | minute << 8 | second << 16 | frame << 24.
const FORMAT_TMSF: u32 = 10;
/// The time format MSF: minute | second << 8 | frame << 16.
const FORMAT_MSF: u32 = 2;

/// Frames (sectors) per second.
const FPS: u32 = 75;
const SECTOR: u64 = 2352;

/// The device ID we hand out.
const DEVICE: u32 = 1;

struct Track {
    file: PathBuf,
    /// where its sectors start in the file (INDEX 01), in bytes
    start: u64,
    /// its length in sectors
    sectors: u32,
    audio: bool,
}

/// The CD image's tracks (track 1 first), None without a cue sheet.
fn tracks() -> Option<&'static [Track]> {
    static T: OnceLock<Option<Vec<Track>>> = OnceLock::new();
    T.get_or_init(|| {
        let dir = super::game_dir();
        let cue = [Some(dir), dir.parent()]
            .into_iter()
            .flatten()
            .find_map(find_cue)?;
        let t = parse_cue(&cue);
        log!("CD audio: {} ({} tracks)", cue.display(), t.len());
        (!t.is_empty()).then_some(t)
    })
    .as_deref()
}

fn find_cue(dir: &Path) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("cue")))
}

/// Read a cue sheet: FILE "name" BINARY, TRACK nn AUDIO|MODE1/2352, INDEX 01 mm:ss:ff. A track
/// ends where the next one in the same file begins (its INDEX 00 or 01), or at the file's end.
fn parse_cue(cue: &Path) -> Vec<Track> {
    let Ok(text) = std::fs::read_to_string(cue) else {
        return Vec::new();
    };
    let dir = cue.parent().unwrap_or(Path::new("."));
    let msf = |s: &str| -> Option<u64> {
        let v: Vec<u64> = s
            .split(':')
            .map(|x| x.parse().ok())
            .collect::<Option<_>>()?;
        (v.len() == 3).then(|| (v[0] * 60 + v[1]) * FPS as u64 + v[2])
    };
    // (file, audio, first sector of INDEX 00 or 01, sector of INDEX 01)
    let mut raw: Vec<(PathBuf, bool, Option<u64>, Option<u64>)> = Vec::new();
    let mut file = PathBuf::new();
    for line in text.lines() {
        let line = line.trim();
        let word = line
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_ascii_uppercase();
        match word.as_str() {
            "FILE" => {
                let name = line
                    .split('"')
                    .nth(1)
                    .map(str::to_string)
                    .or_else(|| line.split_whitespace().nth(1).map(str::to_string));
                file = dir.join(name.unwrap_or_default());
            }
            "TRACK" => {
                let audio = line.to_ascii_uppercase().contains("AUDIO");
                raw.push((file.clone(), audio, None, None));
            }
            "INDEX" => {
                let mut it = line.split_whitespace().skip(1);
                let (Some(n), Some(at)) = (it.next(), it.next().and_then(msf)) else {
                    continue;
                };
                if let Some(t) = raw.last_mut() {
                    if t.2.is_none() {
                        t.2 = Some(at);
                    }
                    if n == "01" {
                        t.3 = Some(at);
                    }
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    for (k, (file, audio, first, start)) in raw.iter().enumerate() {
        let start = start.or(*first).unwrap_or(0);
        let end = match raw.get(k + 1) {
            Some((f, _, Some(next), _)) if f == file => *next,
            _ => std::fs::metadata(file).map_or(start, |m| m.len() / SECTOR),
        };
        out.push(Track {
            file: file.clone(),
            start: start * SECTOR,
            sectors: end.saturating_sub(start) as u32,
            audio: *audio,
        });
    }
    out
}

/// What MCI was told and what plays.
struct State {
    open: bool,
    format: u32,
    /// the track playing (1-based) and the sector of it where the samples start
    from: (u32, u32),
    /// the window told when the play ends (MCI_NOTIFY): MM_MCINOTIFY
    notify: Option<u32>,
}

static STATE: Mutex<State> = Mutex::new(State {
    open: false,
    format: FORMAT_MSF,
    from: (1, 0),
    notify: None,
});

const MCI_NOTIFY: u32 = 1;
const MM_MCINOTIFY: u32 = 0x3b9;
const MCI_NOTIFY_SUCCESSFUL: u32 = 1;
const MCI_NOTIFY_SUPERSEDED: u32 = 2;
const MCI_NOTIFY_ABORTED: u32 = 4;

/// Tell the window waiting for the play's end (if one is) how it ended.
fn notify(st: &mut State, how: u32) {
    if let Some(hwnd) = st.notify.take() {
        super::user32::post(hwnd, MM_MCINOTIFY, how, DEVICE);
    }
}

/// From the message loop: a play with MCI_NOTIFY that has reached its end tells its window
/// (Recoil plays its music one track at a time and starts it again on this).
pub fn poll() {
    let mut st = state();
    if st.notify.is_some() && super::dsound::cd_state().is_none_or(|(pos, len, _)| pos >= len) {
        notify(&mut st, MCI_NOTIFY_SUCCESSFUL);
    }
}

fn state() -> std::sync::MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

fn msf(sectors: u32) -> u32 {
    let (m, s, f) = (sectors / FPS / 60, sectors / FPS % 60, sectors % FPS);
    m | s << 8 | f << 16
}

/// A position in the time format as (track, sector of that track).
fn position(format: u32, v: u32, tracks: &[Track]) -> (u32, u32) {
    let b = |k: u32| (v >> (8 * k)) & 0xff;
    if format == FORMAT_TMSF {
        (b(0).max(1), (b(1) * 60 + b(2)) * FPS + b(3))
    } else {
        // MSF or milliseconds on the whole disc: find the track
        let mut at = if format == FORMAT_MSF {
            (b(0) * 60 + b(1)) * FPS + b(2)
        } else {
            v * FPS / 1000
        };
        for (k, t) in tracks.iter().enumerate() {
            if at < t.sectors {
                return (k as u32 + 1, at);
            }
            at -= t.sectors;
        }
        (tracks.len() as u32 + 1, 0)
    }
}

/// The samples from (track, sector) to (track, sector), audio tracks only.
fn read(tracks: &[Track], from: (u32, u32), to: (u32, u32)) -> Vec<i16> {
    let mut out = Vec::new();
    for t in from.0..=to.0.min(tracks.len() as u32) {
        let tr = &tracks[t as usize - 1];
        let first = if t == from.0 { from.1 } else { 0 };
        let last = if t == to.0 {
            to.1.min(tr.sectors)
        } else {
            tr.sectors
        };
        if !tr.audio || first >= last {
            continue;
        }
        use std::io::{Read, Seek, SeekFrom};
        let mut buf = vec![0u8; (last - first) as usize * SECTOR as usize];
        let ok = std::fs::File::open(&tr.file).and_then(|mut f| {
            f.seek(SeekFrom::Start(tr.start + first as u64 * SECTOR))?;
            f.read_exact(&mut buf)
        });
        if let Err(e) = ok {
            log!("CD audio: track {t}: {}: {e}", tr.file.display());
            continue;
        }
        out.extend(
            buf.chunks_exact(2)
                .map(|b| i16::from_le_bytes([b[0], b[1]])),
        );
    }
    out
}

/// mciSendCommandA(device, message, flags, parameters)
pub fn mci_send_command(c: &mut Cpu) -> u32 {
    let (msg, flags, p) = (arg(c, 1), arg(c, 2), arg(c, 3));
    let Some(tracks) = tracks() else {
        if super::trace_api() {
            log!("mciSendCommandA(message {msg:#x}, flags {flags:#x}): no CD image");
        }
        return MCIERR_DEVICE_NOT_INSTALLED;
    };
    let m = mem();
    let mut st = state();
    if super::trace_api() {
        log!("CD audio: MCI message {msg:#x} flags {flags:#x}");
    }
    match msg {
        MCI_OPEN => {
            st.open = true;
            st.format = FORMAT_MSF;
            m.wr32(p + 4, DEVICE);
        }
        MCI_CLOSE => {
            st.open = false;
            notify(&mut st, MCI_NOTIFY_ABORTED);
            super::dsound::cd_stop();
        }
        MCI_SET => {
            if flags & MCI_SET_TIME_FORMAT != 0 {
                st.format = m.rd32(p + 4);
            }
        }
        MCI_PLAY => {
            let from = if flags & MCI_FROM != 0 {
                position(st.format, m.rd32(p + 4), tracks)
            } else {
                // on from where it is
                let played = super::dsound::cd_state().map_or(0, |(pos, _, _)| pos as u32 / 588);
                (st.from.0, st.from.1 + played)
            };
            let to = if flags & MCI_TO != 0 {
                position(st.format, m.rd32(p + 8), tracks)
            } else {
                (tracks.len() as u32, u32::MAX)
            };
            if from.0 == 0 || from.0 as usize > tracks.len() {
                return MCIERR_OUTOFRANGE;
            }
            log!(
                "CD audio: play track {} at {} to track {} at {} ({} s)",
                from.0,
                from.1,
                to.0,
                to.1,
                super::kernel32::ms_since_start() / 1000
            );
            st.from = from;
            notify(
                &mut st,
                if flags & MCI_NOTIFY != 0 {
                    MCI_NOTIFY_SUPERSEDED
                } else {
                    MCI_NOTIFY_ABORTED
                },
            );
            super::dsound::cd_play(Arc::new(read(tracks, from, to)));
            if flags & MCI_NOTIFY != 0 {
                st.notify = Some(m.rd32(p));
            }
            return 0;
        }
        MCI_SEEK | MCI_STOP => {
            notify(&mut st, MCI_NOTIFY_ABORTED);
            super::dsound::cd_stop();
        }
        MCI_PAUSE => super::dsound::cd_pause(true),
        MCI_RESUME => super::dsound::cd_pause(false),
        MCI_STATUS if flags & MCI_STATUS_ITEM != 0 => {
            let track = m.rd32(p + 12);
            let tr = (flags & MCI_TRACK != 0)
                .then(|| tracks.get((track as usize).wrapping_sub(1)))
                .flatten();
            let playing = super::dsound::cd_state();
            let v = match m.rd32(p + 8) {
                // length: of a track, or of the disc
                1 => msf(tr.map_or_else(|| tracks.iter().map(|t| t.sectors).sum(), |t| t.sectors)),
                // position: where a track starts, or where it plays
                2 => match tr {
                    Some(_) => track,
                    None => {
                        let at = st.from.1 + playing.map_or(0, |(pos, _, _)| pos as u32 / 588);
                        let (mm, s, f) = (at / FPS / 60, at / FPS % 60, at % FPS);
                        st.from.0 | mm << 8 | s << 16 | f << 24
                    }
                },
                3 => tracks.len() as u32,
                4 => match playing {
                    Some((pos, len, paused)) if pos < len => {
                        if paused {
                            MODE_PAUSE
                        } else {
                            MODE_PLAY
                        }
                    }
                    _ => MODE_STOP,
                },
                // media present, ready
                5 | 7 => 1,
                6 => st.format,
                8 => st.from.0,
                // track type: audio or other
                0x4001 => {
                    if tr.is_some_and(|t| t.audio) {
                        0x440
                    } else {
                        0x441
                    }
                }
                item => {
                    log!("warn: MCI status item {item:#x} not supported");
                    0
                }
            };
            m.wr32(p + 4, v);
        }
        _ => {
            log!("warn: MCI message {msg:#x} (flags {flags:#x}) not supported");
            return MCIERR_UNRECOGNIZED_COMMAND;
        }
    }
    // the other commands are done at once
    if flags & MCI_NOTIFY != 0 {
        super::user32::post(m.rd32(p), MM_MCINOTIFY, MCI_NOTIFY_SUCCESSFUL, DEVICE);
    }
    0
}

// ---- aux: the CD audio device's volume ----------------------------------------------------------
//
// Recoil reads the CD volume at start and sets it from its music volume option.

/// The volume, left in the low word, right in the high word (0...0xffff each).
static AUX_VOLUME: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(u32::MAX);

/// auxGetNumDevs: one device, the CD's (none without a CD image).
pub fn aux_get_num_devs(_: &mut Cpu) -> u32 {
    tracks().is_some() as u32
}

/// auxGetDevCapsA(device, AUXCAPSA*, size): CD audio with a left and right volume.
pub fn aux_get_dev_caps(c: &mut Cpu) -> u32 {
    let (p, size) = (arg(c, 1), arg(c, 2));
    if tracks().is_none() {
        return 2; // MMSYSERR_BADDEVICEID
    }
    let m = mem();
    m.slice(p, size.min(48) as usize).fill(0);
    if size >= 48 {
        let name = b"CD Audio";
        m.slice(p + 8, name.len()).copy_from_slice(name);
        m.wr16(p + 40, 1); // AUXCAPS_CDAUDIO
        m.wr32(p + 44, 3); // AUXCAPS_VOLUME | AUXCAPS_LRVOLUME
    }
    0
}

/// auxGetVolume(device, out volume)
pub fn aux_get_volume(c: &mut Cpu) -> u32 {
    let p = arg(c, 1);
    if p != 0 {
        mem().wr32(p, AUX_VOLUME.load(std::sync::atomic::Ordering::Relaxed));
    }
    0
}

/// auxSetVolume(device, volume): the music's volume, both channels' average.
pub fn aux_set_volume(c: &mut Cpu) -> u32 {
    let v = arg(c, 1);
    AUX_VOLUME.store(v, std::sync::atomic::Ordering::Relaxed);
    if super::trace_api() {
        log!("CD volume {v:#x}");
    }
    super::dsound::cd_volume(((v & 0xffff) + (v >> 16)) as f32 / 2.0 / 65535.0);
    0
}
