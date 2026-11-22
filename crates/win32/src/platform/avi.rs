//! Video for Windows, as far as games play AVI files with it themselves: AVIFIL32's stream
//! functions read the file (`AVIStreamOpenFromFileA`, `AVIStreamInfoA`, `AVIStreamReadFormat`,
//! `AVIStreamRead`), MSVFW32's decompressor turns a frame into the game's pixel format
//! (`ICLocate`, `ICDecompress`). Codecs: Cinepak (`cinepak.rs`) and Indeo 5 (the `indeo` crate).
//!
//! Recoil (0x463f70): opens the video stream, reads its format, finds a decompressor to its own
//! 16-bit (BI_BITFIELDS) or 24-bit format, then reads and decompresses frame after frame; the
//! sound stream (PCM) it reads in one piece and plays through DirectSound.

use super::{Import, arg, host_path, log, mem};
use recomp::rt::Cpu;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

const AVIERR_NODATA: u32 = 0x8004_4071;
const AVIERR_FILEOPEN: u32 = 0x8004_4067;
const AVIERR_BUFFERTOOSMALL: u32 = 0x8004_4074;
const AVIERR_BADPARAM: u32 = 0x8004_4066;
const ICERR_OK: u32 = 0;
const ICERR_BADFORMAT: u32 = (-2i32) as u32;

/// One stream of an AVI file.
struct Stream {
    /// `strh`: type, handler, flags, priority/language, initial frames, scale, rate, start,
    /// length, suggested buffer size, quality, sample size (the AVISTREAMINFO fields)
    header: Vec<u8>,
    /// `strf`: BITMAPINFOHEADER or WAVEFORMATEX
    format: Vec<u8>,
    /// the data chunks in order (video: one frame each)
    chunks: Vec<(usize, usize)>,
    /// the whole file
    file: Arc<Vec<u8>>,
}

impl Stream {
    fn u32(&self, at: usize) -> u32 {
        self.header.get(at..at + 4).map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn kind(&self) -> u32 {
        self.u32(0)
    }

    fn sample_size(&self) -> u32 {
        // audio: the format's block align
        if self.kind() == u32::from_le_bytes(*b"auds") && self.format.len() >= 14 {
            return u16::from_le_bytes([self.format[12], self.format[13]]).max(1) as u32;
        }
        self.u32(0x2c)
    }

    /// Samples (frames, or audio blocks).
    fn length(&self) -> u32 {
        match self.sample_size() {
            0 => self.chunks.len() as u32,
            s => (self.chunks.iter().map(|c| c.1).sum::<usize>() / s as usize) as u32,
        }
    }

    /// The bytes of samples `start..start + n`.
    fn read(&self, start: u32, n: u32) -> Vec<u8> {
        let f = &self.file;
        match self.sample_size() {
            0 => self.chunks.get(start as usize).map_or(Vec::new(), |&(at, len)| f[at..at + len].to_vec()),
            s => {
                let (from, to) = (start as usize * s as usize, (start + n) as usize * s as usize);
                let mut out = Vec::with_capacity(to - from);
                let mut pos = 0;
                for &(at, len) in &self.chunks {
                    let (a, b) = (from.max(pos), to.min(pos + len));
                    if a < b {
                        out.extend_from_slice(&f[at + a - pos..at + b - pos]);
                    }
                    pos += len;
                }
                out
            }
        }
    }
}

/// The streams of an AVI file: RIFF 'AVI ', LIST 'hdrl' with a LIST 'strl' per stream
/// (strh, strf), LIST 'movi' with the data chunks ('00dc', '01wb': stream number, type).
fn parse(file: Arc<Vec<u8>>) -> Vec<Stream> {
    let d = &file[..];
    let le = |at: usize| d.get(at..at + 4).map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])) as usize;
    let mut streams: Vec<Stream> = Vec::new();
    let mut chunks: Vec<Vec<(usize, usize)>> = Vec::new();
    // walk chunks in [at, end), going into LISTs
    fn walk(d: &[u8], at: usize, end: usize, f: &mut dyn FnMut(&[u8; 4], usize, usize)) {
        let mut at = at;
        while at + 8 <= end {
            let id: [u8; 4] = d[at..at + 4].try_into().unwrap();
            let size = u32::from_le_bytes(d[at + 4..at + 8].try_into().unwrap()) as usize;
            let body = (at + 8, (at + 8 + size).min(end));
            if &id == b"LIST" || &id == b"RIFF" {
                walk(d, body.0 + 4, body.1, f);
            } else {
                f(&id, body.0, body.1 - body.0);
            }
            at = body.0 + size + (size & 1);
        }
    }
    if d.len() < 12 || &d[0..4] != b"RIFF" || &d[8..12] != b"AVI " {
        return streams;
    }
    let _ = le;
    walk(d, 12, d.len(), &mut |id, at, len| match id {
        b"strh" => {
            streams.push(Stream { header: d[at..at + len].to_vec(), format: Vec::new(), chunks: Vec::new(), file: Arc::clone(&file) });
            chunks.push(Vec::new());
        }
        b"strf" => {
            if let Some(s) = streams.last_mut() {
                s.format = d[at..at + len].to_vec();
            }
        }
        _ if id[0].is_ascii_digit() && id[1].is_ascii_digit() => {
            let n = ((id[0] - b'0') * 10 + (id[1] - b'0')) as usize;
            if let Some(c) = chunks.get_mut(n) {
                c.push((at, len));
            }
        }
        _ => {}
    });
    for (s, c) in streams.iter_mut().zip(chunks) {
        s.chunks = c;
    }
    streams
}

/// A decompressor.
enum Codec {
    Cinepak(super::cinepak::Decoder),
    Indeo(Box<indeo::Decoder>),
}

struct State {
    streams: BTreeMap<u32, Arc<Stream>>,
    codecs: BTreeMap<u32, Codec>,
    next: u32,
    /// the size of what AVIStreamRead last put at each buffer (ICDecompress gets no size)
    read_size: BTreeMap<u32, usize>,
    /// a frame decoded as 0x00RRGGBB, reused
    rgb: Vec<u32>,
}

static STATE: Mutex<State> =
    Mutex::new(State { streams: BTreeMap::new(), codecs: BTreeMap::new(), next: 1, read_size: BTreeMap::new(), rgb: Vec::new() });

fn st() -> MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Handles for streams and decompressors (the game only passes them back).
const STREAM_BASE: u32 = 0x7a00_0000;
const CODEC_BASE: u32 = 0x7b00_0000;

fn with_stream<R>(h: u32, f: impl FnOnce(&Stream) -> R) -> Option<R> {
    let s = st().streams.get(&h).cloned()?;
    Some(f(&s))
}

/// AVIStreamOpenFromFileA(out stream, file, type, which of that type, mode, handler)
fn stream_open_from_file(c: &mut Cpu) -> u32 {
    let (out, name, kind, which) = (arg(c, 0), super::cstr(arg(c, 1)), arg(c, 2), arg(c, 3));
    let m = mem();
    if out != 0 {
        m.wr32(out, 0);
    }
    let path = host_path(&name);
    let Ok(data) = std::fs::read(&path) else {
        log!("AVI {name}: not found");
        return AVIERR_FILEOPEN;
    };
    let mut streams = parse(Arc::new(data));
    let found = (0..streams.len()).filter(|&k| kind == 0 || streams[k].kind() == kind).nth(which as usize);
    let Some(k) = found else {
        return AVIERR_NODATA;
    };
    let s = streams.swap_remove(k);
    let mut g = st();
    let h = STREAM_BASE + g.next;
    g.next += 1;
    g.streams.insert(h, Arc::new(s));
    if out != 0 {
        m.wr32(out, h);
    }
    0
}

fn stream_release(c: &mut Cpu) -> u32 {
    st().streams.remove(&arg(c, 0));
    0
}

/// AVIStreamInfoA(stream, AVISTREAMINFOA*, size)
fn stream_info(c: &mut Cpu) -> u32 {
    let (h, p, size) = (arg(c, 0), arg(c, 1), arg(c, 2));
    let m = mem();
    with_stream(h, |s| {
        let mut info = vec![0u8; 0x8c];
        let mut put = |at: usize, v: u32| info[at..at + 4].copy_from_slice(&v.to_le_bytes());
        put(0x00, s.kind());
        put(0x04, s.u32(4));
        put(0x08, s.u32(8));
        put(0x10, s.u32(0xc)); // priority, language
        put(0x14, s.u32(0x14).max(1)); // scale
        put(0x18, s.u32(0x18)); // rate
        put(0x1c, s.u32(0x1c)); // start
        put(0x20, s.length());
        put(0x24, s.u32(0x10)); // initial frames
        put(0x28, s.u32(0x24).max(s.chunks.iter().map(|c| c.1).max().unwrap_or(0) as u32));
        put(0x2c, s.u32(0x28)); // quality
        put(0x30, s.sample_size());
        // the frame rectangle: from the video's size
        if s.kind() == u32::from_le_bytes(*b"vids") && s.format.len() >= 12 {
            put(0x3c, u32::from_le_bytes(s.format[4..8].try_into().unwrap()));
            put(0x40, u32::from_le_bytes(s.format[8..12].try_into().unwrap()));
        }
        let n = (size as usize).min(info.len());
        m.slice(p, n).copy_from_slice(&info[..n]);
        0
    })
    .unwrap_or(AVIERR_BADPARAM)
}

fn stream_length(c: &mut Cpu) -> u32 {
    with_stream(arg(c, 0), |s| s.length()).unwrap_or(u32::MAX)
}

/// AVIStreamReadFormat(stream, position, buffer or 0, in/out size)
fn stream_read_format(c: &mut Cpu) -> u32 {
    let (h, p, size) = (arg(c, 0), arg(c, 2), arg(c, 3));
    let m = mem();
    with_stream(h, |s| {
        let have = m.rd32(size) as usize;
        if p != 0 {
            let n = have.min(s.format.len());
            m.slice(p, n).copy_from_slice(&s.format[..n]);
        }
        m.wr32(size, s.format.len() as u32);
        0
    })
    .unwrap_or(AVIERR_BADPARAM)
}

/// AVIStreamRead(stream, start, samples (-1: what is convenient), buffer, size, out bytes,
/// out samples)
fn stream_read(c: &mut Cpu) -> u32 {
    let (h, start, n, buf, size, out_bytes, out_samples) = (arg(c, 0), arg(c, 1), arg(c, 2), arg(c, 3), arg(c, 4), arg(c, 5), arg(c, 6));
    let m = mem();
    let Some(s) = st().streams.get(&h).cloned() else { return AVIERR_BADPARAM };
    let len = s.length();
    let n = if n == u32::MAX {
        if s.sample_size() == 0 { 1 } else { (size / s.sample_size()).max(1) }
    } else {
        n
    };
    let n = n.min(len.saturating_sub(start));
    let data = s.read(start, n);
    if buf != 0 && data.len() > size as usize {
        return AVIERR_BUFFERTOOSMALL;
    }
    if buf != 0 {
        m.slice(buf, data.len()).copy_from_slice(&data);
        st().read_size.insert(buf, data.len());
    }
    if out_bytes != 0 {
        m.wr32(out_bytes, data.len() as u32);
    }
    if out_samples != 0 {
        m.wr32(out_samples, n);
    }
    0
}

fn ret0(_: &mut Cpu) -> u32 {
    0
}

/// A BITMAPINFOHEADER's size and compression.
fn bitmap(p: u32) -> (u32, u32, u32) {
    let m = mem();
    (m.rd32(p + 4), m.rd32(p + 8), m.rd32(p + 16))
}

/// ICLocate(type, handler, input format, output format, mode): a decompressor for the input's
/// compression; the output is any RGB the game asks for.
fn ic_locate(c: &mut Cpu) -> u32 {
    let input = arg(c, 2);
    if input == 0 {
        return 0;
    }
    let (w, h, comp) = bitmap(input);
    let (w, hh) = (w, (h as i32).unsigned_abs());
    let codec = match &comp.to_le_bytes() {
        b"cvid" | b"CVID" => Codec::Cinepak(super::cinepak::Decoder::new(w, hh)),
        b"IV50" | b"iv50" => Codec::Indeo(Box::new(indeo::Decoder::new(w, hh))),
        f => {
            log!("warn: no decompressor for {}", String::from_utf8_lossy(f));
            return 0;
        }
    };
    let mut g = st();
    let id = CODEC_BASE + g.next;
    g.next += 1;
    g.codecs.insert(id, codec);
    id
}

fn ic_close(c: &mut Cpu) -> u32 {
    st().codecs.remove(&arg(c, 0));
    ICERR_OK
}

/// ICSendMessage(decompressor, message, p1, p2): begin / end and the queries answer OK.
fn ic_send_message(_: &mut Cpu) -> u32 {
    ICERR_OK
}

/// ICDecompress(decompressor, flags, input format, data, output format, pixels): cdecl.
fn ic_decompress(c: &mut Cpu) -> u32 {
    let (id, data, out_fmt, bits) = (arg(c, 0), arg(c, 3), arg(c, 4), arg(c, 5));
    let m = mem();
    let mut g = st();
    let size = g.read_size.get(&data).copied().unwrap_or(0);
    let input = m.slice(data, size).to_vec();
    let State { codecs, rgb, .. } = &mut *g;
    let Some(codec) = codecs.get_mut(&id) else { return ICERR_BADFORMAT };
    let (w, h) = match codec {
        Codec::Cinepak(d) => {
            if !input.is_empty() {
                d.decode(&input);
            }
            rgb.clear();
            rgb.extend_from_slice(d.pixels());
            let (ow, oh, _) = bitmap(out_fmt);
            (ow as usize, (oh as i32).unsigned_abs() as usize)
        }
        Codec::Indeo(d) => {
            if !input.is_empty() {
                if let Err(e) = d.decode(&input) {
                    log!("warn: Indeo 5 frame: {e:?}");
                }
            }
            let (ow, oh, _) = bitmap(out_fmt);
            let (ow, oh) = (ow as usize, (oh as i32).unsigned_abs() as usize);
            rgb.resize(ow * oh, 0);
            d.to_rgb(rgb);
            (ow, oh)
        }
    };
    if rgb.len() < w * h {
        return ICERR_BADFORMAT;
    }
    write_pixels(rgb, w, h, out_fmt, bits);
    ICERR_OK
}

/// Put an 0x00RRGGBB picture into a DIB: 16 bits (BI_RGB 555, or BI_BITFIELDS with the masks
/// after the header), 24 or 32; bottom row first for a positive height.
fn write_pixels(rgb: &[u32], w: usize, h: usize, fmt: u32, bits: u32) {
    let m = mem();
    let height = m.rd32(fmt + 8) as i32;
    let bpp = m.rd16(fmt + 14) as usize;
    let comp = m.rd32(fmt + 16);
    let stride = (w * bpp / 8 + 3) & !3;
    let out = m.slice(bits, stride * h);
    let (rm, gm, bm) = if comp == 3 { (m.rd32(fmt + 40), m.rd32(fmt + 44), m.rd32(fmt + 48)) } else { (0x7c00, 0x03e0, 0x001f) };
    // a mask as (shift of its top bit from bit 7, bits): 8-bit channel to field
    let field = |mask: u32| {
        let top = 31 - mask.leading_zeros() as i32;
        let n = mask.count_ones() as i32;
        (top - 7, n)
    };
    let (rf, gf, bf) = (field(rm), field(gm), field(bm));
    let put = |v: u32, (sh, n): (i32, i32), mask: u32| {
        let v = v & (0xff << (8 - n.min(8))) as u32;
        (if sh >= 0 { v << sh } else { v >> -sh }) & mask
    };
    for y in 0..h {
        let row = if height > 0 { h - 1 - y } else { y };
        let line = &mut out[row * stride..][..w * bpp / 8];
        let src = &rgb[y * w..][..w];
        match bpp {
            16 => {
                for (o, &p) in line.chunks_exact_mut(2).zip(src) {
                    let v = put(p >> 16 & 0xff, rf, rm) | put(p >> 8 & 0xff, gf, gm) | put(p & 0xff, bf, bm);
                    o.copy_from_slice(&(v as u16).to_le_bytes());
                }
            }
            24 => {
                for (o, &p) in line.chunks_exact_mut(3).zip(src) {
                    o.copy_from_slice(&[p as u8, (p >> 8) as u8, (p >> 16) as u8]);
                }
            }
            32 => {
                for (o, &p) in line.chunks_exact_mut(4).zip(src) {
                    o.copy_from_slice(&p.to_le_bytes());
                }
            }
            _ => return,
        }
    }
}

pub const IMPORTS: &[Import] = &[
    ("AVIFileInit", 0, ret0),
    ("AVIFileExit", 0, ret0),
    ("AVIStreamOpenFromFileA", 6, stream_open_from_file),
    ("AVIStreamRelease", 1, stream_release),
    ("AVIStreamInfoA", 3, stream_info),
    ("AVIStreamLength", 1, stream_length),
    ("AVIStreamReadFormat", 4, stream_read_format),
    ("AVIStreamRead", 7, stream_read),
    ("ICLocate", 5, ic_locate),
    ("ICClose", 1, ic_close),
    ("ICSendMessage", 4, ic_send_message),
    // cdecl: the caller removes the arguments
    ("ICDecompress", -1, ic_decompress),
];

