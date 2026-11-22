//! Cinepak (`cvid`) video frames, as Video for Windows' decoder gives them. A frame is split into
//! horizontal strips; each strip carries codebooks of 2x2 pixel blocks (four luma values and a
//! shared chroma pair) and then, per 4x4 block, either one entry stretched over the block (V1)
//! or four entries, one per 2x2 quarter (V4). Inter frames skip unchanged blocks; codebooks a
//! strip does not update come from the strip above it.
//!
//! All numbers are big-endian. Frame: flags (1), size (3), width (2), height (2), strips (2).
//! Strip: id (1), size (3, header included), top (2), left (2), bottom (2), right (2), then
//! chunks: id (1), size (3, header included).

/// A codebook entry as colours: the four pixels of a 2x2 block, 0x00RRGGBB.
type Entry = [u32; 4];

#[derive(Clone)]
struct Codebooks {
    v1: [Entry; 256],
    v4: [Entry; 256],
}

pub struct Decoder {
    width: usize,
    height: usize,
    /// the picture, 0x00RRGGBB, top row first (inter frames change only parts of it)
    pixels: Vec<u32>,
    /// each strip's codebooks, kept for the next frame
    strips: Vec<Codebooks>,
}

fn be16(d: &[u8], at: usize) -> usize {
    u16::from_be_bytes([d[at], d[at + 1]]) as usize
}

fn be24(d: &[u8], at: usize) -> usize {
    (d[at] as usize) << 16 | (d[at + 1] as usize) << 8 | d[at + 2] as usize
}

/// Bits read from the top of big-endian 32-bit words, as the flag chunks store them.
struct Flags<'a> {
    d: &'a [u8],
    at: usize,
    word: u32,
    left: u32,
}

impl Flags<'_> {
    fn bit(&mut self) -> Option<bool> {
        if self.left == 0 {
            let w = self.d.get(self.at..self.at + 4)?;
            self.word = u32::from_be_bytes([w[0], w[1], w[2], w[3]]);
            self.at += 4;
            self.left = 32;
        }
        self.left -= 1;
        Some(self.word >> self.left & 1 != 0)
    }
}

/// Cinepak's own colour: r = y + 2v, g = y - u/2 - v, b = y + 2u (u, v signed).
fn rgb(y: u8, u: i32, v: i32) -> u32 {
    let y = y as i32;
    let c = |x: i32| x.clamp(0, 255) as u32;
    c(y + 2 * v) << 16 | c(y - u / 2 - v) << 8 | c(y + 2 * u)
}

impl Decoder {
    pub fn new(width: u32, height: u32) -> Decoder {
        let (width, height) = (width as usize, height as usize);
        Decoder {
            width,
            height,
            pixels: vec![0; width * height],
            strips: Vec::new(),
        }
    }

    /// The picture, 0x00RRGGBB, top row first.
    pub fn pixels(&self) -> &[u32] {
        &self.pixels
    }

    /// Decode one frame; None when its data is cut short or wrong (what was decoded stays).
    pub fn decode(&mut self, d: &[u8]) -> Option<()> {
        if d.len() < 10 {
            return None;
        }
        let flags = d[0];
        let n = be16(d, 8);
        let end = be24(d, 1).clamp(10, d.len());
        let mut at = 10;
        let mut top = 0;
        for k in 0..n {
            if at + 12 > end {
                break;
            }
            let size = be24(d, at + 1).min(end - at);
            if size < 12 {
                return None;
            }
            // the height: bottom - top (some encoders give the top as 0 and the bottom as the
            // strip's height, others both on the frame)
            let (y1, y2) = (be16(d, at + 4), be16(d, at + 8));
            let h = if y2 > y1 { y2 - y1 } else { y2 };
            while self.strips.len() <= k {
                let last = self.strips.last().cloned();
                self.strips.push(last.unwrap_or(Codebooks {
                    v1: [[0; 4]; 256],
                    v4: [[0; 4]; 256],
                }));
            }
            // codebooks a strip does not send are the strip above's (not for flags bit 0)
            if k > 0 && flags & 1 == 0 {
                self.strips[k] = self.strips[k - 1].clone();
            }
            self.strip(k, &d[at + 12..at + size], top, (top + h).min(self.height))?;
            top += h;
            at += size;
        }
        Some(())
    }

    fn strip(&mut self, k: usize, d: &[u8], top: usize, bottom: usize) -> Option<()> {
        let mut at = 0;
        while at + 4 <= d.len() {
            let id = d[at];
            let size = be24(d, at + 1);
            if size < 4 || at + size > d.len() {
                break;
            }
            let c = &d[at + 4..at + size];
            match id {
                0x20..=0x27 => self.codebook(k, id, c),
                0x30..=0x32 => self.blocks(k, id, c, top, bottom),
                _ => {}
            }
            at += size;
        }
        Some(())
    }

    /// 0x20.. V4, 0x22.. V1; +1: only the entries a flag bit marks; +4: luma only.
    fn codebook(&mut self, k: usize, id: u8, d: &[u8]) {
        let grey = id & 4 != 0;
        let partial = id & 1 != 0;
        let book = if id & 2 != 0 {
            &mut self.strips[k].v1
        } else {
            &mut self.strips[k].v4
        };
        let step = if grey { 4 } else { 6 };
        let mut flags = Flags {
            d,
            at: 0,
            word: 0,
            left: 0,
        };
        let mut at = 0;
        for e in book.iter_mut() {
            if partial {
                match flags.bit() {
                    Some(true) => {}
                    Some(false) => continue,
                    None => return,
                }
                at = at.max(flags.at);
            }
            let Some(v) = d.get(at..at + step) else {
                return;
            };
            let (u, w) = if grey {
                (0, 0)
            } else {
                (v[4] as i8 as i32, v[5] as i8 as i32)
            };
            *e = [
                rgb(v[0], u, w),
                rgb(v[1], u, w),
                rgb(v[2], u, w),
                rgb(v[3], u, w),
            ];
            at += step;
            if partial {
                // the next flag word follows the entries read so far
                flags.at = at;
            }
        }
    }

    /// 0x30 every block V1 or V4 by a flag; 0x31 a flag to change the block first; 0x32 all V1.
    fn blocks(&mut self, k: usize, id: u8, d: &[u8], top: usize, bottom: usize) {
        let mut flags = Flags {
            d,
            at: 0,
            word: 0,
            left: 0,
        };
        let mut at = 0;
        let book = &self.strips[k];
        let w = self.width;
        for y in (top..bottom).step_by(4) {
            for x in (0..w).step_by(4) {
                let v4 = match id {
                    0x32 => false,
                    _ => {
                        if id == 0x31 {
                            match flags.bit() {
                                Some(true) => {}
                                Some(false) => continue,
                                None => return,
                            }
                        }
                        let Some(b) = flags.bit() else { return };
                        b
                    }
                };
                at = at.max(flags.at);
                let mut quad = [[0u32; 4]; 4];
                if v4 {
                    let Some(i) = d.get(at..at + 4) else { return };
                    for q in 0..4 {
                        quad[q] = book.v4[i[q] as usize];
                    }
                    at += 4;
                } else {
                    let Some(&i) = d.get(at) else { return };
                    let e = book.v1[i as usize];
                    // each pixel of the entry covers a 2x2 quarter
                    for q in 0..4 {
                        quad[q] = [e[q]; 4];
                    }
                    at += 1;
                }
                if id != 0x32 {
                    flags.at = at;
                }
                // quarters TL TR BL BR, each a 2x2 block of pixels TL TR BL BR
                for (q, e) in quad.iter().enumerate() {
                    for (p, &c) in e.iter().enumerate() {
                        let (px, py) = (x + (q & 1) * 2 + (p & 1), y + (q >> 1) * 2 + (p >> 1));
                        if px < w && py < bottom {
                            self.pixels[py * w + px] = c;
                        }
                    }
                }
            }
        }
    }
}
