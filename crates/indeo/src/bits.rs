// Indeo Video 5 decoder: bit reader and Huffman (VLC) tables.
//
// Ported from FFmpeg's libavcodec/ivi.c (ivi_create_huff_from_desc) and the little-endian
// mode of libavcodec/get_bits.h and vlc.c (Copyright (c) 2009 Maxim Poliakovski and the FFmpeg
// developers).
//
// This library is free software; you can redistribute it and/or modify it under the terms of
// the GNU Lesser General Public License as published by the Free Software Foundation; either
// version 2.1 of the License, or (at your option) any later version. It is distributed in the
// hope that it will be useful, but WITHOUT ANY WARRANTY; see the GNU Lesser General Public
// License (the file COPYING in this crate) for more details.

//! Reading bits the way Indeo does: bytes in order, inside each byte the lowest bit first.

use crate::tables::HuffDesc;

/// Longest Huffman code in Indeo (bits); the VLC table has `1 << VLC_BITS` entries.
pub const VLC_BITS: u32 = 13;

/// Reads bits, lowest bit of each byte first. Reading past the end gives zero bits, and the
/// position stops 8 bits after the end, like FFmpeg's checked reader with its zero padding.
pub struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
    /// Size in bits plus 8: the position never goes beyond this.
    limit: usize,
}

impl<'a> BitReader<'a> {
    pub fn new(data: &'a [u8]) -> BitReader<'a> {
        BitReader {
            data,
            pos: 0,
            limit: data.len() * 8 + 8,
        }
    }

    /// The bit position (bits read so far).
    pub fn pos(&self) -> usize {
        self.pos
    }

    /// Bits left until the end of the data (negative after reading past it).
    pub fn left(&self) -> isize {
        (self.data.len() * 8) as isize - self.pos as isize
    }

    /// The 32 bits starting at byte `i`, little-endian, zeros past the end.
    fn word_at(&self, i: usize) -> u32 {
        let mut b = [0u8; 4];
        for (k, v) in b.iter_mut().enumerate() {
            *v = self.data.get(i + k).copied().unwrap_or(0);
        }
        u32::from_le_bytes(b)
    }

    /// The next `n` bits (n <= 25) without moving on; the first bit is bit 0 of the result.
    pub fn peek(&self, n: u32) -> u32 {
        debug_assert!(n <= 25);
        if n == 0 {
            return 0;
        }
        (self.word_at(self.pos >> 3) >> (self.pos & 7)) & ((1u32 << n) - 1)
    }

    pub fn skip(&mut self, n: usize) {
        self.pos = (self.pos + n).min(self.limit);
    }

    /// Read `n` bits, n <= 25.
    pub fn bits(&mut self, n: u32) -> u32 {
        let v = self.peek(n);
        self.skip(n as usize);
        v
    }

    pub fn bit(&mut self) -> bool {
        self.bits(1) != 0
    }

    /// Read `n` bits, n <= 32.
    pub fn bits_long(&mut self, n: u32) -> u32 {
        if n <= 25 {
            return self.bits(n);
        }
        let lo = self.bits(16);
        lo | (self.bits(n - 16) << 16)
    }

    /// Go on to the next byte boundary.
    pub fn align(&mut self) {
        let n = self.pos.wrapping_neg() & 7;
        self.skip(n);
    }

    /// Decode one Huffman symbol. A bit pattern that is no code gives -1 and reads nothing
    /// (FFmpeg does the same; the callers then treat it as bad data or a harmless value).
    pub fn vlc(&mut self, vlc: &Vlc) -> i32 {
        let e = vlc.table[self.peek(VLC_BITS) as usize];
        self.skip(e.len as usize);
        e.sym as i32
    }
}

#[derive(Clone, Copy)]
struct Entry {
    sym: i16,
    len: u8,
}

/// A Huffman decoding table: indexed by the next 13 bits of the stream.
#[derive(Clone)]
pub struct Vlc {
    table: Vec<Entry>,
}

impl Vlc {
    /// Build the codes from a compact descriptor (FFmpeg's ivi_create_huff_from_desc).
    ///
    /// Row `i` starts with `i` one bits, then (unless it is the last row) a zero bit, then
    /// `xbits[i]` bits that number the codes of the row. Symbols are numbered in that order;
    /// at most 256 of them. `None` if a code would be longer than 13 bits.
    pub fn from_desc(desc: &HuffDesc) -> Option<Vlc> {
        let mut table = vec![Entry { sym: -1, len: 0 }; 1 << VLC_BITS];
        let mut sym = 0usize;
        'rows: for i in 0..desc.num_rows {
            let xbits = desc.xbits[i] as u32;
            let not_last_row = (i != desc.num_rows - 1) as u32;
            let prefix = ((1u32 << i) - 1) << (xbits + not_last_row);
            for j in 0..(1u32 << xbits) {
                if sym >= 256 {
                    // some descriptors describe more than 256 codes, only 256 are allowed
                    break 'rows;
                }
                let mut len = i as u32 + xbits + not_last_row;
                if len > VLC_BITS {
                    return None;
                }
                let code = prefix | j;
                if len == 0 {
                    len = 1;
                }
                // The first bit read is the highest bit of the code, so the table index is
                // the code with its bits reversed, plus any value in the bits after it.
                let first = code.reverse_bits() >> (32 - len);
                let mut k = first as usize;
                while k < table.len() {
                    table[k] = Entry {
                        sym: sym as i16,
                        len: len as u8,
                    };
                    k += 1 << len;
                }
                sym += 1;
            }
        }
        Some(Vlc { table })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bits_low_bit_first() {
        let data = [0b1010_0110u8, 0xff, 0x01];
        let mut r = BitReader::new(&data);
        assert_eq!(r.bits(1), 0);
        assert_eq!(r.bits(2), 0b11);
        assert_eq!(r.bits(5), 0b10100);
        assert_eq!(r.bits(12), 0x1ff);
        r.align();
        assert_eq!(r.pos(), 24);
        // past the end: zeros, position stops 8 bits after the end
        assert_eq!(r.bits(16), 0);
        assert_eq!(r.pos(), 32);
        assert_eq!(r.left(), -8);
    }

    #[test]
    fn bits_long_32() {
        let data = [0x78, 0x56, 0x34, 0x12, 0xaa];
        let mut r = BitReader::new(&data);
        r.skip(4);
        assert_eq!(r.bits_long(32), 0xa123_4567);
    }

    #[test]
    fn vlc_codes() {
        // rows: "0" + 1 bit (2 codes), "10" + 0 bits, "11" (last row, no zero bit) + 1 bit
        let desc = HuffDesc {
            num_rows: 3,
            xbits: [1, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        };
        let vlc = Vlc::from_desc(&desc).unwrap();
        // codes, first bit first: 0 0 -> 0, 0 1 -> 1, 1 0 -> 2, 1 1 0 -> 3, 1 1 1 -> 4
        // stream bits in reading order: 01 | 110 | 10 | 111 | 00
        let bits = [0, 1, 1, 1, 0, 1, 0, 1, 1, 1, 0, 0];
        let mut data = [0u8; 2];
        for (i, b) in bits.iter().enumerate() {
            data[i / 8] |= b << (i % 8);
        }
        let mut r = BitReader::new(&data);
        let syms: Vec<i32> = (0..5).map(|_| r.vlc(&vlc)).collect();
        assert_eq!(syms, [1, 3, 2, 4, 0]);
        assert_eq!(r.pos(), 12);
    }

    #[test]
    fn predefined_tables_build() {
        for d in crate::tables::MB_HUFF_DESC
            .iter()
            .chain(&crate::tables::BLK_HUFF_DESC)
        {
            assert!(Vlc::from_desc(d).is_some());
        }
    }
}
