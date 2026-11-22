// Indeo Video 5 (IV50) decoder.
//
// A port of FFmpeg's Indeo 5 decoder: libavcodec/indeo5.c, libavcodec/ivi.c and
// libavcodec/ivi_dsp.c (Copyright (c) 2009 Maxim Poliakovski), part of FFmpeg.
//
// This library is free software; you can redistribute it and/or modify it under the terms of
// the GNU Lesser General Public License as published by the Free Software Foundation; either
// version 2.1 of the License, or (at your option) any later version. It is distributed in the
// hope that it will be useful, but WITHOUT ANY WARRANTY; see the GNU Lesser General Public
// License (the file COPYING in this crate) for more details.

//! Decoder for Intel Indeo Video 5 (FOURCC `IV50`), as found in AVI files of the late 1990s.
//!
//! Feed it one AVI video chunk at a time with [`Decoder::decode`], then read the picture with
//! [`Decoder::planes`] (YUV 4:2:0) or [`Decoder::to_rgb`].
//!
//! The codec stores chroma at a quarter of the width and height (YUV 4:1:0); [`Decoder::planes`]
//! scales it up to half size (4:2:0) with a bilinear filter.

mod bits;
mod dsp;
mod ivi;
mod tables;

use std::fmt;

/// Why a frame could not be decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The data is broken (or not Indeo 5).
    InvalidData(&'static str),
    /// A feature of Indeo 5 this decoder (like FFmpeg's) does not have.
    Unsupported(&'static str),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Error::InvalidData(what) => write!(f, "invalid Indeo 5 data: {what}"),
            Error::Unsupported(what) => write!(f, "unsupported Indeo 5 feature: {what}"),
        }
    }
}

impl std::error::Error for Error {}

/// An Indeo 5 video decoder.
pub struct Decoder {
    ivi: ivi::Ivi,
    width: usize,
    height: usize,
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
    // the decoder's own quarter-size chroma, before scaling up
    u410: Vec<u8>,
    v410: Vec<u8>,
}

impl Decoder {
    /// A decoder for a video of this size (from the AVI header). The stream's key frames
    /// carry the size too; if it differs, the stream wins (see [`Decoder::width`]).
    pub fn new(width: u32, height: u32) -> Decoder {
        let mut d = Decoder {
            ivi: ivi::Ivi::new(width, height),
            width: 0,
            height: 0,
            y: Vec::new(),
            u: Vec::new(),
            v: Vec::new(),
            u410: Vec::new(),
            v410: Vec::new(),
        };
        d.resize(width as usize, height as usize);
        // black until the first picture
        d.u.fill(128);
        d.v.fill(128);
        d
    }

    fn resize(&mut self, width: usize, height: usize) {
        if (width, height) == (self.width, self.height) {
            return;
        }
        self.width = width;
        self.height = height;
        self.y = vec![0; width * height];
        self.u = vec![128; width.div_ceil(2) * height.div_ceil(2)];
        self.v = vec![128; width.div_ceil(2) * height.div_ceil(2)];
        self.u410 = vec![128; width.div_ceil(4) * height.div_ceil(4)];
        self.v410 = vec![128; width.div_ceil(4) * height.div_ceil(4)];
    }

    /// Decode one AVI frame's data (a whole IV50 frame). Returns Ok(true) when a picture was
    /// produced, Ok(false) for frames that produce none (empty chunks, null frames: the
    /// previous picture stays), Err on corrupt data. Never panics on bad input.
    pub fn decode(&mut self, data: &[u8]) -> Result<bool, Error> {
        if data.is_empty() {
            return Ok(false);
        }
        if !self.ivi.decode_frame(data)? {
            return Ok(false);
        }
        let (w, h) = self.ivi.size();
        self.resize(w as usize, h as usize);
        self.ivi.output(&mut self.y, &mut self.u410, &mut self.v410);
        let (cw, ch) = (self.width.div_ceil(4), self.height.div_ceil(4));
        let (w2, h2) = (self.width.div_ceil(2), self.height.div_ceil(2));
        upsample_chroma(&self.u410, cw, ch, &mut self.u, w2, h2);
        upsample_chroma(&self.v410, cw, ch, &mut self.v, w2, h2);
        Ok(true)
    }

    /// Picture width in pixels.
    pub fn width(&self) -> u32 {
        self.width as u32
    }

    /// Picture height in pixels.
    pub fn height(&self) -> u32 {
        self.height as u32
    }

    /// The current picture as YUV 4:2:0 planes (y: width*height, u/v:
    /// ((w+1)/2)*((h+1)/2)), 8 bit.
    pub fn planes(&self) -> (&[u8], &[u8], &[u8]) {
        (&self.y, &self.u, &self.v)
    }

    /// The chroma planes as the codec stores them, before scaling up: U and V, each
    /// ((w+3)/4)*((h+3)/4) (YUV 4:1:0, FFmpeg's yuv410p).
    pub fn chroma_quarter(&self) -> (&[u8], &[u8]) {
        (&self.u410, &self.v410)
    }

    /// The current picture as 0x00RRGGBB, top row first (BT.601, limited range).
    /// `out` holds width*height pixels; a shorter `out` gets only the rows that fit.
    pub fn to_rgb(&self, out: &mut [u32]) {
        let (w, w2) = (self.width, self.width.div_ceil(2));
        if w == 0 {
            return;
        }
        for (row, line) in out.chunks_exact_mut(w).take(self.height).enumerate() {
            for (x, px) in line.iter_mut().enumerate() {
                let c = (row / 2) * w2 + x / 2;
                *px = yuv_to_rgb(self.y[row * w + x], self.u[c], self.v[c]);
            }
        }
    }
}

/// BT.601 limited range to 0x00RRGGBB, in integers.
fn yuv_to_rgb(y: u8, u: u8, v: u8) -> u32 {
    let c = (y as i32 - 16) * 298;
    let d = u as i32 - 128;
    let e = v as i32 - 128;
    let clip = |x: i32| ((x + 128) >> 8).clamp(0, 255) as u32;
    let r = clip(c + 409 * e);
    let g = clip(c - 100 * d - 208 * e);
    let b = clip(c + 516 * d);
    (r << 16) | (g << 8) | b
}

/// Scale a quarter-size chroma plane up to half size (twice each way), bilinear: each output
/// sample is 3/4 of the nearest source sample and 1/4 of the next one, in both directions.
fn upsample_chroma(src: &[u8], sw: usize, sh: usize, dst: &mut [u8], dw: usize, dh: usize) {
    if sw == 0 || sh == 0 {
        return;
    }
    // the source index and its neighbour for output index i
    let taps = |i: usize, n: usize| -> (usize, usize) {
        let k = (i / 2).min(n - 1);
        let other = if i.is_multiple_of(2) {
            k.saturating_sub(1)
        } else {
            (k + 1).min(n - 1)
        };
        (k, other)
    };
    for oy in 0..dh {
        let (y0, y1) = taps(oy, sh);
        for ox in 0..dw {
            let (x0, x1) = taps(ox, sw);
            let s = |x: usize, y: usize| src[y * sw + x] as u32;
            let v = 9 * s(x0, y0) + 3 * s(x1, y0) + 3 * s(x0, y1) + s(x1, y1);
            dst[oy * dw + ox] = ((v + 8) >> 4) as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgb_of_grey_and_primaries() {
        assert_eq!(yuv_to_rgb(16, 128, 128), 0x000000);
        assert_eq!(yuv_to_rgb(235, 128, 128), 0xffffff);
        assert_eq!(yuv_to_rgb(126, 128, 128), 0x808080);
        // pure red in BT.601 limited range: Y 81, U 90, V 240
        let red = yuv_to_rgb(81, 90, 240);
        assert!(
            red >> 16 >= 0xfc && (red >> 8) & 0xff <= 2 && red & 0xff <= 2,
            "{red:06x}"
        );
    }

    #[test]
    fn upsample_flat_and_edges() {
        let src = [10u8, 10, 10, 10];
        let mut dst = [0u8; 16];
        upsample_chroma(&src, 2, 2, &mut dst, 4, 4);
        assert!(dst.iter().all(|&v| v == 10));

        // a step: left 0, right 160 -> 0, 40, 120, 160
        let src = [0u8, 160];
        let mut dst = [0u8; 4];
        upsample_chroma(&src, 2, 1, &mut dst, 4, 1);
        assert_eq!(dst, [0, 40, 120, 160]);
    }

    #[test]
    fn empty_and_garbage_input() {
        let mut d = Decoder::new(320, 240);
        assert_eq!(d.decode(&[]), Ok(false));
        assert!(d.decode(&[0, 1, 2, 3]).is_err());
        // an inter frame before any key frame
        assert!(d.decode(&[0x3f, 0, 0, 0, 0, 0, 0, 0]).is_err());
        let (y, u, v) = d.planes();
        assert_eq!(
            (y.len(), u.len(), v.len()),
            (320 * 240, 160 * 120, 160 * 120)
        );
        let mut rgb = vec![0u32; 320 * 240];
        d.to_rgb(&mut rgb);
        assert!(rgb.iter().all(|&p| p == 0));
    }
}
