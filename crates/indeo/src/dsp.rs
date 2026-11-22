// Indeo Video 5 decoder: inverse transforms, motion compensation, wavelet recomposition.
//
// Ported from FFmpeg's libavcodec/ivi_dsp.c (Copyright (c) 2009 Maxim Poliakovski).
//
// This library is free software; you can redistribute it and/or modify it under the terms of
// the GNU Lesser General Public License as published by the Free Software Foundation; either
// version 2.1 of the License, or (at your option) any later version. It is distributed in the
// hope that it will be useful, but WITHOUT ANY WARRANTY; see the GNU Lesser General Public
// License (the file COPYING in this crate) for more details.

//! The pixel work. Band buffers hold signed 16-bit values (the picture minus 128).
//!
//! The transforms compute in 64 bit so that no input can overflow; the results are cut to the
//! sizes FFmpeg stores them in, which gives the same numbers wherever FFmpeg's own arithmetic
//! is defined.

/// The inverse transform a band uses (chosen by plane and band number in the GOP header).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Transform {
    /// 2D slant 8x8 (luma, no scalability or the low band).
    #[default]
    Slant8x8,
    /// Slant over rows only (scalable luma band 1).
    RowSlant8,
    /// Slant over columns only (scalable luma band 2).
    ColSlant8,
    /// No transform, the coefficients are the pixels (scalable luma band 3).
    Put8x8,
    /// 2D slant 4x4 (chroma).
    Slant4x4,
}

impl Transform {
    /// The 2D transforms predict the DC coefficient of intra blocks from the block before.
    pub fn is_2d(self) -> bool {
        matches!(self, Transform::Slant8x8 | Transform::Slant4x4)
    }

    /// Inverse transform of the coefficients `inp` into `out` (rows `pitch` apart).
    /// `flags[i]` is non-zero when column `i` has a non-zero coefficient.
    pub fn inverse(self, inp: &[i32; 64], out: &mut [i16], pitch: usize, flags: &[u8; 8]) {
        match self {
            Transform::Slant8x8 => inverse_slant_8x8(inp, out, pitch, flags),
            Transform::RowSlant8 => row_slant8(inp, out, pitch),
            Transform::ColSlant8 => col_slant8(inp, out, pitch, flags),
            Transform::Put8x8 => put_pixels_8x8(inp, out, pitch),
            Transform::Slant4x4 => inverse_slant_4x4(inp, out, pitch, flags),
        }
    }

    /// The transform of a block that has only a DC coefficient `dc`.
    pub fn dc(self, dc: i32, out: &mut [i16], pitch: usize, blk_size: usize) {
        match self {
            Transform::Slant8x8 | Transform::Slant4x4 => dc_slant_2d(dc, out, pitch, blk_size),
            Transform::RowSlant8 => dc_row_slant(dc, out, pitch, blk_size),
            Transform::ColSlant8 => dc_col_slant(dc, out, pitch, blk_size),
            Transform::Put8x8 => put_dc_pixel_8x8(dc, out, pitch),
        }
    }
}

/// Butterfly: (a + b, a - b).
#[inline]
fn bfly(a: i64, b: i64) -> (i64, i64) {
    (a + b, a - b)
}

/// Reflection with a, b = 1/2, 5/4.
#[inline]
fn ireflect(s1: i64, s2: i64) -> (i64, i64) {
    (
        (((s1 + s2 * 2 + 2) >> 2) + s1),
        (((s1 * 2 - s2 + 2) >> 2) - s2),
    )
}

/// Reflection with a, b = 1/2, 7/8.
#[inline]
fn slant_part4(s1: i64, s2: i64) -> (i64, i64) {
    (
        s2 + ((s1 * 4 - s2 + 4) >> 3),
        s1 + ((-s1 - s2 * 4 + 4) >> 3),
    )
}

/// Inverse slant over 8 values, in the order they are stored (FFmpeg's IVI_INV_SLANT8 names
/// its inputs s1, s4, s8, s5, s2, s6, s3, s7).
#[inline]
fn inv_slant8(v: [i64; 8]) -> [i64; 8] {
    let [s1, s4, s8, s5, s2, s6, s3, s7] = v;
    let (t4, t5) = slant_part4(s4, s5);

    let (t1, t5) = bfly(s1, t5);
    let (t2, t6) = bfly(s2, s6);
    let (t7, t3) = bfly(s7, s3);
    let (t4, t8) = bfly(t4, s8);

    let (t1, t2) = bfly(t1, t2);
    let (t4, t3) = ireflect(t4, t3);
    let (t5, t6) = bfly(t5, t6);
    let (t8, t7) = ireflect(t8, t7);
    let (t1, t4) = bfly(t1, t4);
    let (t2, t3) = bfly(t2, t3);
    let (t5, t8) = bfly(t5, t8);
    let (t6, t7) = bfly(t6, t7);
    [t1, t2, t3, t4, t5, t6, t7, t8]
}

/// Inverse slant over 4 values (FFmpeg's IVI_INV_SLANT4 names its inputs s1, s4, s2, s3).
#[inline]
fn inv_slant4(v: [i64; 4]) -> [i64; 4] {
    let [s1, s4, s2, s3] = v;
    let (t1, t2) = bfly(s1, s2);
    let (t4, t3) = ireflect(s4, s3);
    let (t1, t4) = bfly(t1, t4);
    let (t2, t3) = bfly(t2, t3);
    [t1, t2, t3, t4]
}

/// The rounding halving of the second pass.
#[inline]
fn compensate(x: i64) -> i16 {
    ((x + 1) >> 1) as i16
}

fn inverse_slant_8x8(inp: &[i32; 64], out: &mut [i16], pitch: usize, flags: &[u8; 8]) {
    // columns
    let mut tmp = [0i32; 64];
    for i in 0..8 {
        if flags[i] != 0 {
            let col = inv_slant8(std::array::from_fn(|k| inp[i + 8 * k] as i64));
            for k in 0..8 {
                tmp[i + 8 * k] = col[k] as i32;
            }
        }
    }
    // rows
    for i in 0..8 {
        let src = &tmp[i * 8..i * 8 + 8];
        let row = &mut out[i * pitch..i * pitch + 8];
        if src.iter().all(|&v| v == 0) {
            row.fill(0);
        } else {
            let r = inv_slant8(std::array::from_fn(|k| src[k] as i64));
            for k in 0..8 {
                row[k] = compensate(r[k]);
            }
        }
    }
}

fn inverse_slant_4x4(inp: &[i32; 64], out: &mut [i16], pitch: usize, flags: &[u8; 8]) {
    let mut tmp = [0i32; 16];
    for i in 0..4 {
        if flags[i] != 0 {
            let col = inv_slant4(std::array::from_fn(|k| inp[i + 4 * k] as i64));
            for k in 0..4 {
                tmp[i + 4 * k] = col[k] as i32;
            }
        }
    }
    for i in 0..4 {
        let src = &tmp[i * 4..i * 4 + 4];
        let row = &mut out[i * pitch..i * pitch + 4];
        if src.iter().all(|&v| v == 0) {
            row.fill(0);
        } else {
            let r = inv_slant4(std::array::from_fn(|k| src[k] as i64));
            for k in 0..4 {
                row[k] = compensate(r[k]);
            }
        }
    }
}

fn row_slant8(inp: &[i32; 64], out: &mut [i16], pitch: usize) {
    for i in 0..8 {
        let src = &inp[i * 8..i * 8 + 8];
        let row = &mut out[i * pitch..i * pitch + 8];
        if src.iter().all(|&v| v == 0) {
            row.fill(0);
        } else {
            let r = inv_slant8(std::array::from_fn(|k| src[k] as i64));
            for k in 0..8 {
                row[k] = compensate(r[k]);
            }
        }
    }
}

fn col_slant8(inp: &[i32; 64], out: &mut [i16], pitch: usize, flags: &[u8; 8]) {
    for i in 0..8 {
        if flags[i] != 0 {
            let c = inv_slant8(std::array::from_fn(|k| inp[i + 8 * k] as i64));
            for k in 0..8 {
                out[i + k * pitch] = compensate(c[k]);
            }
        } else {
            for k in 0..8 {
                out[i + k * pitch] = 0;
            }
        }
    }
}

fn put_pixels_8x8(inp: &[i32; 64], out: &mut [i16], pitch: usize) {
    for y in 0..8 {
        for x in 0..8 {
            out[y * pitch + x] = inp[y * 8 + x] as i16;
        }
    }
}

fn dc_slant_2d(dc: i32, out: &mut [i16], pitch: usize, blk_size: usize) {
    let v = ((dc as i64 + 1) >> 1) as i16;
    for y in 0..blk_size {
        out[y * pitch..y * pitch + blk_size].fill(v);
    }
}

fn dc_row_slant(dc: i32, out: &mut [i16], pitch: usize, blk_size: usize) {
    let v = ((dc as i64 + 1) >> 1) as i16;
    out[..blk_size].fill(v);
    for y in 1..blk_size {
        out[y * pitch..y * pitch + blk_size].fill(0);
    }
}

fn dc_col_slant(dc: i32, out: &mut [i16], pitch: usize, blk_size: usize) {
    let v = ((dc as i64 + 1) >> 1) as i16;
    for y in 0..blk_size {
        let row = &mut out[y * pitch..y * pitch + blk_size];
        row.fill(0);
        row[0] = v;
    }
}

fn put_dc_pixel_8x8(dc: i32, out: &mut [i16], pitch: usize) {
    for y in 0..8 {
        out[y * pitch..y * pitch + 8].fill(0);
    }
    out[0] = dc as i16;
}

/// Motion compensation of one `size` x `size` block: take the block from the reference
/// picture, at half-pixel position `mc_type` (bit 0: half a pixel right, bit 1: half down).
/// `add` adds it to what is in `buf` (the decoded difference), otherwise it replaces it.
pub fn mc(buf: &mut [i16], reference: &[i16], pitch: usize, size: usize, mc_type: u32, add: bool) {
    for i in 0..size {
        let r0 = i * pitch; // this row of the reference
        let r1 = r0 + pitch; // the row below
        for j in 0..size {
            let a = reference[r0 + j] as i32;
            let v = match mc_type {
                0 => a,
                1 => (a + reference[r0 + j + 1] as i32) >> 1,
                2 => (a + reference[r1 + j] as i32) >> 1,
                _ => {
                    let sum = reference[r0 + j + 1] as i32
                        + reference[r1 + j] as i32
                        + reference[r1 + j + 1] as i32;
                    (a + sum) >> 2
                }
            } as i16;
            let d = &mut buf[i * pitch + j];
            *d = if add { d.wrapping_add(v) } else { v };
        }
    }
}

/// Clip to 0..=255.
#[inline]
pub fn clip_u8(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

/// Put the 4 bands of a scalable luma plane back together (5/3 wavelet) into 8-bit pixels.
/// `bands` are the band buffers, all `pitch` wide; the picture is `width` x `height`.
pub fn recompose53(
    bands: [&[i16]; 4],
    pitch: usize,
    width: usize,
    height: usize,
    dst: &mut [u8],
    dst_pitch: usize,
) {
    let [b0, b1, b2, b3] = bands;
    // FFmpeg walks with pointers that step back one value at the right edge and repeat the
    // last row at the bottom; here the same as indices. `base` is where row y/2 starts.
    let at = |b: &[i16], i: isize| -> i32 { b[i as usize] as i32 };
    let mut base: isize = 0;
    let mut back_pitch: isize = 0;
    let mut pitch_i = pitch as isize;
    let mut dst_row = 0usize;
    let mut y = 0;
    while y < height {
        if y + 2 >= height {
            pitch_i = 0;
        }
        let mut p = base;

        let mut b0_1 = at(b0, p);
        let mut b0_2 = at(b0, p + pitch_i);

        let mut b1_1 = at(b1, p + back_pitch);
        let mut b1_2 = at(b1, p);
        let mut b1_3 = b1_1 - b1_2 * 6 + at(b1, p + pitch_i);

        let mut b2_2 = at(b2, p);
        let mut b2_3 = b2_2;
        let mut b2_5 = at(b2, p + pitch_i);
        let mut b2_6 = b2_5;

        let mut b3_2 = at(b3, p + back_pitch);
        let mut b3_3 = b3_2;
        let mut b3_5 = at(b3, p);
        let mut b3_6 = b3_5;
        let mut b3_8 = b3_2 - b3_5 * 6 + at(b3, p + pitch_i);
        let mut b3_9 = b3_8;

        let mut x = 0;
        let mut indx: isize = 0;
        while x < width {
            if x + 2 >= width {
                p -= 1;
            }

            let b2_1 = b2_2;
            b2_2 = b2_3;
            let b2_4 = b2_5;
            b2_5 = b2_6;
            let b3_1 = b3_2;
            b3_2 = b3_3;
            let b3_4 = b3_5;
            b3_5 = b3_6;
            let b3_7 = b3_8;
            b3_8 = b3_9;

            // LL band: low pass both ways
            let tmp0 = b0_1;
            let tmp2 = b0_2;
            b0_1 = at(b0, p + indx + 1);
            b0_2 = at(b0, p + pitch_i + indx + 1);
            let tmp1 = tmp0 + b0_1;

            let mut p0 = tmp0 * 16;
            let mut p1 = tmp1 * 8;
            let mut p2 = (tmp0 + tmp2) * 8;
            let mut p3 = (tmp1 + tmp2 + b0_2) * 4;

            // HL band: high pass vertically, low pass horizontally
            let tmp0 = b1_2;
            let tmp1 = b1_1;
            b1_2 = at(b1, p + indx + 1);
            b1_1 = at(b1, p + back_pitch + indx + 1);
            let tmp2 = tmp1 - tmp0 * 6 + b1_3;
            b1_3 = b1_1 - b1_2 * 6 + at(b1, p + pitch_i + indx + 1);

            p0 += (tmp0 + tmp1) * 8;
            p1 += (tmp0 + tmp1 + b1_1 + b1_2) * 4;
            p2 += tmp2 * 4;
            p3 += (tmp2 + b1_3) * 2;

            // LH band: low pass vertically, high pass horizontally
            b2_3 = at(b2, p + indx + 1);
            b2_6 = at(b2, p + pitch_i + indx + 1);
            let tmp0 = b2_1 + b2_2;
            let tmp1 = b2_1 - b2_2 * 6 + b2_3;

            p0 += tmp0 * 8;
            p1 += tmp1 * 4;
            p2 += (tmp0 + b2_4 + b2_5) * 4;
            p3 += (tmp1 + b2_4 - b2_5 * 6 + b2_6) * 2;

            // HH band: high pass both ways
            b3_6 = at(b3, p + indx + 1);
            b3_3 = at(b3, p + back_pitch + indx + 1);
            let tmp0 = b3_1 + b3_4;
            let tmp1 = b3_2 + b3_5;
            let tmp2 = b3_3 + b3_6;
            b3_9 = b3_3 - b3_6 * 6 + at(b3, p + pitch_i + indx + 1);

            p0 += (tmp0 + tmp1) * 4;
            p1 += (tmp0 - tmp1 * 6 + tmp2) * 2;
            p2 += (b3_7 + b3_8) * 2;
            p3 += b3_7 - b3_8 * 6 + b3_9;

            dst[dst_row + x] = clip_u8((p0 >> 6) + 128);
            if x + 1 < width {
                dst[dst_row + x + 1] = clip_u8((p1 >> 6) + 128);
            }
            if y + 1 < height {
                dst[dst_row + dst_pitch + x] = clip_u8((p2 >> 6) + 128);
                if x + 1 < width {
                    dst[dst_row + dst_pitch + x + 1] = clip_u8((p3 >> 6) + 128);
                }
            }

            x += 2;
            indx += 1;
        }

        dst_row += dst_pitch * 2;
        back_pitch = -pitch_i;
        base = p + pitch_i + 1;
        y += 2;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dc_only_slant_8x8_is_flat() {
        // A block with only a DC coefficient: the full transform and the DC shortcut agree.
        for dc in [-300, -7, -1, 0, 1, 5, 64, 1001] {
            let mut inp = [0i32; 64];
            inp[0] = dc;
            let mut flags = [0u8; 8];
            flags[0] = (dc != 0) as u8;
            let mut full = vec![0i16; 8 * 8];
            Transform::Slant8x8.inverse(&inp, &mut full, 8, &flags);
            let mut short = vec![0i16; 8 * 8];
            Transform::Slant8x8.dc(dc, &mut short, 8, 8);
            assert_eq!(full, short, "dc {dc}");
        }
    }

    #[test]
    fn dc_only_slant_4x4_is_flat() {
        for dc in [-50, -3, 0, 2, 77] {
            let mut inp = [0i32; 64];
            inp[0] = dc;
            let flags = [1u8, 0, 0, 0, 0, 0, 0, 0];
            let mut full = vec![0i16; 4 * 4];
            Transform::Slant4x4.inverse(&inp, &mut full, 4, &flags);
            let mut short = vec![0i16; 4 * 4];
            Transform::Slant4x4.dc(dc, &mut short, 4, 4);
            assert_eq!(full, short, "dc {dc}");
        }
    }

    #[test]
    fn slant8_known_values() {
        // One coefficient (row 0, column 1), worked out by hand from FFmpeg's macros: the
        // column pass turns column 1 into eight 16s; the row pass of [0, 16, 0, ...] gives
        // [24, 18, 10, 4, -4, -10, -18, -24], halved with rounding.
        let mut inp = [0i32; 64];
        inp[1] = 16;
        let mut flags = [0u8; 8];
        flags[1] = 1;
        let mut out = vec![0i16; 64];
        Transform::Slant8x8.inverse(&inp, &mut out, 8, &flags);
        for y in 0..8 {
            assert_eq!(&out[y * 8..y * 8 + 8], &[12, 9, 5, 2, -2, -5, -9, -12]);
        }
    }

    #[test]
    fn mc_halfpel() {
        let pitch = 4;
        let reference: Vec<i16> = (0..16).map(|v| v * 2).collect(); // rows 0,2,4,6 / 8,10..
        let mut buf = vec![0i16; 16];
        mc(&mut buf, &reference, pitch, 2, 0, false);
        assert_eq!(&buf[..2], &[0, 2]);
        mc(&mut buf, &reference, pitch, 2, 1, false);
        assert_eq!(&buf[..2], &[1, 3]);
        mc(&mut buf, &reference, pitch, 2, 2, false);
        assert_eq!(&buf[..2], &[4, 6]);
        mc(&mut buf, &reference, pitch, 2, 3, false);
        assert_eq!(&buf[..2], &[5, 7]);
        mc(&mut buf, &reference, pitch, 2, 0, true);
        assert_eq!(&buf[..2], &[5, 9]);
    }
}
