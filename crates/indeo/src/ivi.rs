// Indeo Video 5 decoder: headers, bands, tiles, macroblocks and blocks.
//
// Ported from FFmpeg's libavcodec/indeo5.c and libavcodec/ivi.c
// (Copyright (c) 2009 Maxim Poliakovski).
//
// This library is free software; you can redistribute it and/or modify it under the terms of
// the GNU Lesser General Public License as published by the Free Software Foundation; either
// version 2.1 of the License, or (at your option) any later version. It is distributed in the
// hope that it will be useful, but WITHOUT ANY WARRANTY; see the GNU Lesser General Public
// License (the file COPYING in this crate) for more details.

//! How an Indeo 5 frame is built:
//!
//! - A frame has three colour planes: Y, V, U (in that order in the stream). Chroma has a
//!   quarter of the width and height (YUV 4:1:0).
//! - A plane has one band, or (luma in "scalable" streams) four wavelet bands of half size.
//! - A band is cut into tiles (usually one tile = the whole band), a tile into macroblocks
//!   (16x16 or 8x8), a macroblock into 1 or 4 blocks (8x8 luma, 4x4 chroma).
//! - Each block is either intra (transform coefficients only) or inter (a motion-compensated
//!   piece of the previous picture plus a coded difference).
//!
//! Key frames (INTRA) start with a GOP header that sets the picture layout for the frames
//! after it. Three buffers per band hold the picture being decoded and the reference picture.

use std::sync::OnceLock;

use crate::Error;
use crate::bits::{BitReader, Vlc};
use crate::dsp::{self, Transform};
use crate::tables::*;

// Indeo 5 frame types.
const FRAMETYPE_INTRA: u32 = 0;
/// Inter frame, used as reference.
const FRAMETYPE_INTER: u32 = 1;
/// Droppable inter frame of the scalability mode.
const FRAMETYPE_INTER_SCAL: u32 = 2;
/// Droppable inter frame.
const FRAMETYPE_INTER_NOREF: u32 = 3;
/// Empty frame, no data: the picture stays as it is.
const FRAMETYPE_NULL: u32 = 4;

const PIC_SIZE_ESC: u32 = 15;
const IS_PROTECTED: u8 = 0x20;

fn bad(what: &'static str) -> Error {
    Error::InvalidData(what)
}

/// The predefined codebooks, built once: [0] macroblock tables, [1] block tables.
fn static_vlcs() -> &'static [Vec<Vlc>; 2] {
    static TABLES: OnceLock<[Vec<Vlc>; 2]> = OnceLock::new();
    TABLES.get_or_init(|| {
        let build = |descs: &[HuffDesc; 8]| -> Vec<Vlc> {
            descs
                .iter()
                .map(|d| Vlc::from_desc(d).expect("predefined Indeo codebook"))
                .collect()
        };
        [build(&MB_HUFF_DESC), build(&BLK_HUFF_DESC)]
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum HuffKind {
    Mb = 0,
    Blk = 1,
}

/// Which codebook a band (or the macroblock info) uses: a predefined one or one sent in
/// the stream.
struct HuffTab {
    kind: HuffKind,
    /// `Some(n)`: predefined table n; `None`: the custom table.
    sel: Option<usize>,
    cust_desc: HuffDesc,
    cust_tab: Option<Vlc>,
}

impl HuffTab {
    fn new(kind: HuffKind) -> HuffTab {
        HuffTab {
            kind,
            sel: Some(7),
            cust_desc: HuffDesc::default(),
            cust_tab: None,
        }
    }

    fn vlc(&self) -> Result<&Vlc, Error> {
        match self.sel {
            Some(n) => Ok(&static_vlcs()[self.kind as usize][n]),
            None => self.cust_tab.as_ref().ok_or(bad("no custom Huffman table")),
        }
    }

    /// Read a codebook choice from the stream (FFmpeg's ff_ivi_dec_huff_desc).
    fn decode(&mut self, gb: &mut BitReader, desc_coded: bool) -> Result<(), Error> {
        if !desc_coded {
            self.sel = Some(7);
            return Ok(());
        }
        let tab_sel = gb.bits(3) as usize;
        if tab_sel != 7 {
            self.sel = Some(tab_sel);
            return Ok(());
        }
        // a custom table, sent as a descriptor
        let mut new = HuffDesc {
            num_rows: gb.bits(4) as usize,
            xbits: [0; 16],
        };
        if new.num_rows == 0 {
            return Err(bad("empty custom Huffman table"));
        }
        for i in 0..new.num_rows {
            new.xbits[i] = gb.bits(4) as u8;
        }
        let n = new.num_rows;
        let same = self.cust_desc.num_rows == n && self.cust_desc.xbits[..n] == new.xbits[..n];
        if !same || self.cust_tab.is_none() {
            self.cust_desc = new;
            self.cust_tab = Vlc::from_desc(&new);
            if self.cust_tab.is_none() {
                self.cust_desc.num_rows = 0;
                return Err(bad("invalid custom Huffman table"));
            }
        }
        self.sel = None;
        Ok(())
    }
}

#[derive(Clone, Copy, Default)]
struct MbInfo {
    xpos: i32,
    ypos: i32,
    /// Where the macroblock starts in the band buffer.
    buf_offs: i32,
    /// 0 intra, 1 inter.
    mb_type: u8,
    /// Coded block pattern: bit n set = block n has coefficients.
    cbp: u8,
    q_delta: i8,
    mv_x: i8,
    mv_y: i8,
}

#[derive(Clone, Default)]
struct Tile {
    xpos: i32,
    ypos: i32,
    width: i32,
    height: i32,
    mb_size: i32,
    mbs: Vec<MbInfo>,
    /// The macroblocks of the first luma band's tile with the same number are the reference
    /// for inherited motion vectors and quant deltas (all bands but the first luma band).
    has_ref: bool,
}

fn mbs_per_tile(width: i32, height: i32, mb_size: i32) -> usize {
    (((width + mb_size - 1) / mb_size) * ((height + mb_size - 1) / mb_size)) as usize
}

/// "Unsigned to signed": odd values are positive, even ones negative (FFmpeg's IVI_TOSIGNED on an int).
fn to_signed(v: i32) -> i32 {
    ((v >> 1) ^ (v & 1).wrapping_neg()).wrapping_neg()
}

/// The same on an unsigned value (FFmpeg applies it to unsigned escape values too, where the
/// shift is logical; only matters for broken streams).
fn to_signed_u(v: u32) -> i32 {
    ((v >> 1) ^ (v & 1).wrapping_neg()).wrapping_neg() as i32
}

/// A quant delta: FFmpeg stores the code in an int8 first, then converts it.
fn q_delta_from(sym: i32) -> i8 {
    to_signed(sym as i8 as i32) as i8
}

/// Scale a motion vector down to a band with smaller macroblocks.
fn scale_mv(mv: i32, mv_scale: i32) -> i32 {
    (mv + (mv > 0) as i32 + (mv_scale - 1)) >> mv_scale
}

struct Band {
    plane: usize,
    band_num: usize,
    width: i32,
    height: i32,
    /// Buffer height (aligned).
    aheight: i32,
    /// Buffer width (aligned) = distance between rows.
    pitch: i32,
    /// The three picture buffers (empty until used), each `pitch * aheight` values.
    bufs: [Vec<i16>; 3],
    /// The buffer decoded last (what is shown).
    cur: Option<usize>,
    is_empty: bool,
    mb_size: i32,
    blk_size: i32,
    is_halfpel: bool,
    inherit_mv: bool,
    inherit_qdelta: bool,
    qdelta_present: bool,
    glob_quant: i32,
    scan: &'static [u8],
    blk_vlc: HuffTab,
    num_corr: usize,
    corr: [u8; 61 * 2],
    rvmap_sel: usize,
    tiles: Vec<Tile>,
    transform: Transform,
    transform_size: i32,
    intra_base: &'static [u16],
    inter_base: &'static [u16],
    intra_scale: &'static [u8],
    inter_scale: &'static [u8],
}

impl Band {
    fn new(plane: usize, band_num: usize) -> Band {
        Band {
            plane,
            band_num,
            width: 0,
            height: 0,
            aheight: 0,
            pitch: 0,
            bufs: Default::default(),
            cur: None,
            is_empty: false,
            mb_size: 0,
            blk_size: 0,
            is_halfpel: false,
            inherit_mv: false,
            inherit_qdelta: false,
            qdelta_present: false,
            glob_quant: 0,
            scan: &[],
            blk_vlc: HuffTab::new(HuffKind::Blk),
            num_corr: 0,
            corr: [0; 61 * 2],
            rvmap_sel: 8,
            tiles: Vec::new(),
            transform: Transform::default(),
            transform_size: 0,
            intra_base: &[],
            inter_base: &[],
            intra_scale: &[],
            inter_scale: &[],
        }
    }

    fn buf_size(&self) -> i64 {
        self.pitch as i64 * self.aheight as i64
    }

    /// Decode a band header (indeo5.c decode_band_hdr).
    fn decode_header(&mut self, gb: &mut BitReader, frame_flags: u8) -> Result<(), Error> {
        let band_flags = gb.bits(8);
        self.is_empty = band_flags & 1 != 0;
        if self.is_empty {
            return Ok(());
        }

        // the band's data size: not needed
        if frame_flags & 0x80 != 0 {
            gb.skip(24);
        }

        self.inherit_mv = band_flags & 2 != 0;
        self.inherit_qdelta = band_flags & 8 != 0;
        self.qdelta_present = band_flags & 4 != 0;
        if !self.qdelta_present {
            self.inherit_qdelta = true;
        }

        // corrections to the run/value table: pairs of entries to swap
        self.num_corr = 0;
        if band_flags & 0x10 != 0 {
            let n = gb.bits(8) as usize;
            if n > 61 {
                return Err(bad("too many rvmap corrections"));
            }
            self.num_corr = n;
            for i in 0..n * 2 {
                self.corr[i] = gb.bits(8) as u8;
            }
        }

        self.rvmap_sel = if band_flags & 0x40 != 0 {
            gb.bits(3) as usize
        } else {
            8
        };

        self.blk_vlc.decode(gb, band_flags & 0x80 != 0)?;

        // checksum: only for debugging
        if gb.bit() {
            gb.skip(16);
        }

        self.glob_quant = gb.bits(5) as i32;

        if band_flags & 0x20 != 0 {
            gb.align();
            skip_hdr_extension(gb);
        }
        gb.align();
        Ok(())
    }
}

/// Skip a header extension: byte counts, each followed by that many bytes, until a zero.
fn skip_hdr_extension(gb: &mut BitReader) {
    loop {
        let len = gb.bits(8) as isize;
        if 8 * len > gb.left() {
            return;
        }
        gb.skip(8 * len as usize);
        if len == 0 {
            return;
        }
    }
}

struct Plane {
    width: i32,
    height: i32,
    bands: Vec<Band>,
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
struct PicConfig {
    pic_width: i32,
    pic_height: i32,
    chroma_width: i32,
    chroma_height: i32,
    tile_width: i32,
    tile_height: i32,
    luma_bands: usize,
    chroma_bands: usize,
}

/// What macroblock and block decoding need to know about the frame.
struct FrameInfo<'a> {
    frame_type: u32,
    frame_flags: u8,
    mb_vlc: &'a Vlc,
    /// Macroblock size of the first luma band.
    luma_mb_size: i32,
}

/// The decoder state (FFmpeg's IVI45DecContext, Indeo 5 parts only).
pub struct Ivi {
    /// Our copy of the run/value tables: bands patch them while they are decoded.
    rvmap_tabs: Box<[RvMap; 9]>,
    frame_type: u32,
    prev_frame_type: u32,
    is_scalable: bool,
    inter_scal: bool,
    frame_flags: u8,
    pic_conf: PicConfig,
    planes: [Plane; 3],
    buf_switch: usize,
    dst_buf: usize,
    ref_buf: usize,
    ref2_buf: usize,
    mb_vlc: HuffTab,
    gop_flags: u8,
    gop_invalid: bool,
    buf_invalid: [bool; 3],
}

/// Whether a picture size is acceptable (FFmpeg's av_image_check_size).
fn size_ok(w: i32, h: i32) -> bool {
    w > 0 && h > 0 && (w as i64 + 128) * (h as i64 + 128) < (i32::MAX / 8) as i64
}

impl Ivi {
    pub fn new(width: u32, height: u32) -> Ivi {
        let w = width.min(8191) as i32;
        let h = height.min(8191) as i32;
        let empty = || Plane {
            width: 0,
            height: 0,
            bands: Vec::new(),
        };
        let mut ivi = Ivi {
            rvmap_tabs: Box::new(RVMAP_TABS),
            frame_type: FRAMETYPE_INTRA,
            prev_frame_type: FRAMETYPE_INTRA,
            is_scalable: false,
            inter_scal: false,
            frame_flags: 0,
            // the basic profile: one band per plane, one tile, YVU9
            pic_conf: PicConfig {
                pic_width: w,
                pic_height: h,
                chroma_width: (w + 3) >> 2,
                chroma_height: (h + 3) >> 2,
                tile_width: w,
                tile_height: h,
                luma_bands: 1,
                chroma_bands: 1,
            },
            planes: [empty(), empty(), empty()],
            buf_switch: 0,
            dst_buf: 0,
            ref_buf: 0,
            ref2_buf: 0,
            mb_vlc: HuffTab::new(HuffKind::Mb),
            gop_flags: 0,
            gop_invalid: true,
            buf_invalid: [false; 3],
        };
        let conf = ivi.pic_conf;
        // a bad size here is not fatal: the first key frame sets the real one
        let _ = ivi.init_planes(&conf);
        ivi
    }

    /// The decoded picture size.
    pub fn size(&self) -> (i32, i32) {
        (self.planes[0].width, self.planes[0].height)
    }

    /// Set up planes and bands for a picture layout (ff_ivi_init_planes). Frees all buffers.
    fn init_planes(&mut self, cfg: &PicConfig) -> Result<(), Error> {
        for p in &mut self.planes {
            p.bands.clear();
        }
        if !size_ok(cfg.pic_width, cfg.pic_height) || cfg.luma_bands < 1 || cfg.chroma_bands < 1 {
            return Err(bad("invalid picture size"));
        }
        self.planes[0].width = cfg.pic_width;
        self.planes[0].height = cfg.pic_height;
        for p in 1..3 {
            self.planes[p].width = (cfg.pic_width + 3) >> 2;
            self.planes[p].height = (cfg.pic_height + 3) >> 2;
        }
        for p in 0..3 {
            let num_bands = if p == 0 {
                cfg.luma_bands
            } else {
                cfg.chroma_bands
            };
            let plane = &mut self.planes[p];
            // one band: full size; several bands: each half the size
            let (bw, bh) = if num_bands == 1 {
                (plane.width, plane.height)
            } else {
                ((plane.width + 1) >> 1, (plane.height + 1) >> 1)
            };
            // buffers are aligned to the largest macroblock: 16 luma, 8 chroma
            let align = if p == 0 { 16 } else { 8 };
            let wa = (bw + align - 1) / align * align;
            let ha = (bh + align - 1) / align * align;
            plane.bands = (0..num_bands)
                .map(|b| {
                    let mut band = Band::new(p, b);
                    band.width = bw;
                    band.height = bh;
                    band.pitch = wa;
                    band.aheight = ha;
                    band
                })
                .collect();
        }
        Ok(())
    }

    /// Cut all bands into tiles and macroblocks (ff_ivi_init_tiles).
    fn init_tiles(&mut self, tile_width: i32, tile_height: i32) -> Result<(), Error> {
        let mut luma_tile_mbs: Vec<usize> = Vec::new();
        for p in 0..3 {
            let mut t_width = if p == 0 {
                tile_width
            } else {
                (tile_width + 3) >> 2
            };
            let mut t_height = if p == 0 {
                tile_height
            } else {
                (tile_height + 3) >> 2
            };
            if p == 0 && self.planes[0].bands.len() == 4 {
                if t_width % 2 != 0 || t_height % 2 != 0 {
                    return Err(Error::Unsupported("odd tile size"));
                }
                t_width >>= 1;
                t_height >>= 1;
            }
            if t_width <= 0 || t_height <= 0 {
                return Err(bad("invalid tile size"));
            }
            for b in 0..self.planes[p].bands.len() {
                let band = &mut self.planes[p].bands[b];
                if band.mb_size <= 0 {
                    return Err(bad("macroblock size not set"));
                }
                band.tiles.clear();
                let mut y = 0;
                while y < band.height {
                    let mut x = 0;
                    while x < band.width {
                        let width = (band.width - x).min(t_width);
                        let height = (band.height - y).min(t_height);
                        let num_mbs = mbs_per_tile(width, height, band.mb_size);
                        let mut has_ref = false;
                        if p != 0 || b != 0 {
                            let t = band.tiles.len();
                            if luma_tile_mbs.get(t) != Some(&num_mbs) {
                                return Err(bad("tile does not match the luma tile"));
                            }
                            has_ref = true;
                        }
                        band.tiles.push(Tile {
                            xpos: x,
                            ypos: y,
                            width,
                            height,
                            mb_size: band.mb_size,
                            mbs: vec![MbInfo::default(); num_mbs],
                            has_ref,
                        });
                        x += t_width;
                    }
                    y += t_height;
                }
                if p == 0 && b == 0 {
                    luma_tile_mbs = band.tiles.iter().map(|t| t.mbs.len()).collect();
                }
            }
        }
        Ok(())
    }

    /// Decode the GOP header of a key frame (indeo5.c decode_gop_header).
    fn decode_gop_header(&mut self, gb: &mut BitReader) -> Result<(), Error> {
        self.gop_flags = gb.bits(8) as u8;

        // header size: not needed
        if self.gop_flags & 1 != 0 {
            gb.skip(16);
        }
        // lock word of password-protected clips (refused later)
        if self.gop_flags & IS_PROTECTED != 0 {
            gb.bits_long(32);
        }

        let tile_size = if self.gop_flags & 0x40 != 0 {
            64 << gb.bits(2)
        } else {
            0
        };
        if tile_size > 256 {
            return Err(bad("invalid tile size"));
        }

        // number of wavelet bands: levels * 3 + 1
        let mut conf = PicConfig {
            luma_bands: gb.bits(2) as usize * 3 + 1,
            chroma_bands: gb.bits(1) as usize * 3 + 1,
            ..PicConfig::default()
        };
        let is_scalable = conf.luma_bands != 1 || conf.chroma_bands != 1;
        if is_scalable && (conf.luma_bands != 4 || conf.chroma_bands != 1) {
            return Err(Error::Unsupported("scalability with this band layout"));
        }

        let pic_size_indx = gb.bits(4);
        if pic_size_indx == PIC_SIZE_ESC {
            conf.pic_height = gb.bits(13) as i32;
            conf.pic_width = gb.bits(13) as i32;
        } else {
            let i = pic_size_indx as usize * 2;
            conf.pic_height = (IVI5_COMMON_PIC_SIZES[i + 1] as i32) << 2;
            conf.pic_width = (IVI5_COMMON_PIC_SIZES[i] as i32) << 2;
        }

        if self.gop_flags & 2 != 0 {
            return Err(Error::Unsupported("YV12 picture format"));
        }

        conf.chroma_height = (conf.pic_height + 3) >> 2;
        conf.chroma_width = (conf.pic_width + 3) >> 2;

        if tile_size == 0 {
            conf.tile_height = conf.pic_height;
            conf.tile_width = conf.pic_width;
        } else {
            conf.tile_height = tile_size;
            conf.tile_width = tile_size;
        }

        // a new picture layout: new buffers
        let mut blk_size_changed = false;
        if conf != self.pic_conf || self.gop_invalid {
            self.init_planes(&conf)?;
            self.pic_conf = conf;
            self.is_scalable = is_scalable;
            blk_size_changed = true;
        }

        for p in 0..=1 {
            let num_bands = if p == 0 {
                conf.luma_bands
            } else {
                conf.chroma_bands
            };
            for i in 0..num_bands {
                let band = &mut self.planes[p].bands[i];

                band.is_halfpel = gb.bit();

                let mb_size_bit = gb.bit();
                let blk_size = 8 >> gb.bits(1);
                let mb_size = if mb_size_bit { blk_size } else { blk_size << 1 };

                if p == 0 && blk_size == 4 {
                    return Err(Error::Unsupported("4x4 luma blocks"));
                }

                // FFmpeg keeps only the last band's answer here (`=`); any change counts
                if mb_size != band.mb_size || blk_size != band.blk_size {
                    blk_size_changed = true;
                    band.mb_size = mb_size;
                    band.blk_size = blk_size;
                }

                if gb.bit() {
                    return Err(Error::Unsupported("extended transform info"));
                }

                // transform and scan order by plane and band number
                let (transform, scan, size): (Transform, &'static [u8], i32) = match (p << 2) + i {
                    0 => (Transform::Slant8x8, &ZIGZAG_DIRECT, 8),
                    1 => (Transform::RowSlant8, &VERTICAL_SCAN_8X8, 8),
                    2 => (Transform::ColSlant8, &HORIZONTAL_SCAN_8X8, 8),
                    3 => (Transform::Put8x8, &HORIZONTAL_SCAN_8X8, 8),
                    _ => (Transform::Slant4x4, &DIRECT_SCAN_4X4, 4),
                };
                band.transform = transform;
                band.scan = scan;
                band.transform_size = size;

                if band.transform_size != band.blk_size {
                    return Err(bad("transform and block size mismatch"));
                }

                // dequantisation matrix by plane and band number
                let quant_mat = if p == 0 {
                    if conf.luma_bands > 1 { i + 1 } else { 0 }
                } else {
                    5
                };
                if band.blk_size == 8 {
                    if quant_mat >= 5 {
                        return Err(bad("quant matrix index too large"));
                    }
                    band.intra_base = &IVI5_BASE_QUANT_8X8_INTRA[quant_mat];
                    band.inter_base = &IVI5_BASE_QUANT_8X8_INTER[quant_mat];
                    band.intra_scale = &IVI5_SCALE_QUANT_8X8_INTRA[quant_mat];
                    band.inter_scale = &IVI5_SCALE_QUANT_8X8_INTER[quant_mat];
                } else {
                    band.intra_base = &IVI5_BASE_QUANT_4X4_INTRA;
                    band.inter_base = &IVI5_BASE_QUANT_4X4_INTER;
                    band.intra_scale = &IVI5_SCALE_QUANT_4X4_INTRA;
                    band.inter_scale = &IVI5_SCALE_QUANT_4X4_INTER;
                }

                if gb.bits(2) != 0 {
                    return Err(bad("GOP band end marker missing"));
                }
            }
        }

        // the second chroma plane uses the parameters of the first
        for i in 0..conf.chroma_bands {
            let [_, p1, p2] = &mut self.planes;
            let (b1, b2) = (&p1.bands[i], &mut p2.bands[i]);
            b2.width = b1.width;
            b2.height = b1.height;
            b2.mb_size = b1.mb_size;
            b2.blk_size = b1.blk_size;
            b2.is_halfpel = b1.is_halfpel;
            b2.intra_base = b1.intra_base;
            b2.inter_base = b1.inter_base;
            b2.intra_scale = b1.intra_scale;
            b2.inter_scale = b1.inter_scale;
            b2.scan = b1.scan;
            b2.transform = b1.transform;
            b2.transform_size = b1.transform_size;
        }

        if blk_size_changed {
            self.init_tiles(conf.tile_width, conf.tile_height)?;
        }

        if self.gop_flags & 8 != 0 {
            if gb.bits(3) != 0 {
                return Err(bad("GOP alignment bits not zero"));
            }
            // transparency fill colour: transparency is not decoded
            if gb.bit() {
                gb.skip(24);
            }
        }

        gb.align();
        gb.skip(23); // unknown meaning

        // GOP extension: 16-bit words while the top bit is set
        if gb.bit() {
            while gb.bits(16) & 0x8000 != 0 {}
        }
        gb.align();
        Ok(())
    }

    /// Decode the picture header (indeo5.c decode_pic_hdr).
    fn decode_pic_hdr(&mut self, gb: &mut BitReader) -> Result<(), Error> {
        if gb.bits(5) != 0x1f {
            return Err(bad("invalid picture start code"));
        }
        self.prev_frame_type = self.frame_type;
        self.frame_type = gb.bits(3);
        if self.frame_type >= 5 {
            self.frame_type = FRAMETYPE_INTRA;
            return Err(bad("invalid frame type"));
        }
        gb.skip(8); // frame number

        if self.frame_type == FRAMETYPE_INTRA {
            if let Err(e) = self.decode_gop_header(gb) {
                self.gop_invalid = true;
                return Err(e);
            }
            self.gop_invalid = false;
        }

        if self.frame_type == FRAMETYPE_INTER_SCAL && !self.is_scalable {
            self.frame_type = FRAMETYPE_INTER;
            return Err(bad("scalable inter frame in a non-scalable stream"));
        }

        if self.frame_type != FRAMETYPE_NULL {
            self.frame_flags = gb.bits(8) as u8;
            if self.frame_flags & 1 != 0 {
                gb.skip(24); // picture header size
            }
            if self.frame_flags & 0x10 != 0 {
                gb.skip(16); // checksum
            }
            if self.frame_flags & 0x20 != 0 {
                skip_hdr_extension(gb);
            }
            self.mb_vlc.decode(gb, self.frame_flags & 0x40 != 0)?;
            gb.skip(3); // unknown meaning
        }
        gb.align();
        Ok(())
    }

    /// Pick the buffers to decode into and to predict from (indeo5.c switch_buffers).
    fn switch_buffers(&mut self) {
        match self.prev_frame_type {
            FRAMETYPE_INTRA | FRAMETYPE_INTER => {
                self.buf_switch ^= 1;
                self.dst_buf = self.buf_switch;
                self.ref_buf = self.buf_switch ^ 1;
            }
            FRAMETYPE_INTER_SCAL => {
                if !self.inter_scal {
                    self.ref2_buf = 2;
                    self.inter_scal = true;
                }
                std::mem::swap(&mut self.dst_buf, &mut self.ref2_buf);
                self.ref_buf = self.ref2_buf;
            }
            // a droppable frame was not kept: decode into the same buffer again
            FRAMETYPE_INTER_NOREF => {}
            _ => {}
        }
        match self.frame_type {
            FRAMETYPE_INTRA | FRAMETYPE_INTER => {
                if self.frame_type == FRAMETYPE_INTRA {
                    self.buf_switch = 0;
                }
                self.inter_scal = false;
                self.dst_buf = self.buf_switch;
                self.ref_buf = self.buf_switch ^ 1;
            }
            _ => {}
        }
    }

    /// Decode one frame (ff_ivi_decode_frame). Ok(true): a new picture is ready.
    pub fn decode_frame(&mut self, data: &[u8]) -> Result<bool, Error> {
        let mut gb = BitReader::new(data);
        self.decode_pic_hdr(&mut gb)?;
        if self.gop_invalid {
            return Err(bad("no valid key frame yet"));
        }
        if self.gop_flags & IS_PROTECTED != 0 {
            return Err(Error::Unsupported("password-protected clip"));
        }
        if self.planes[0].bands.is_empty() {
            return Err(bad("colour planes not set up"));
        }

        self.switch_buffers();

        if self.frame_type != FRAMETYPE_NULL {
            let (dst, refb) = (self.dst_buf, self.ref_buf);
            self.buf_invalid[dst] = true;
            let info = FrameInfo {
                frame_type: self.frame_type,
                frame_flags: self.frame_flags,
                mb_vlc: self.mb_vlc.vlc()?,
                luma_mb_size: self.planes[0].bands[0].mb_size,
            };
            let three_bufs = self.pic_conf.luma_bands > 1;
            for p in 0..3 {
                for b in 0..self.planes[p].bands.len() {
                    // take the band out so the first luma band can be read alongside it
                    let mut band = std::mem::replace(&mut self.planes[p].bands[b], Band::new(p, b));
                    let ref_tiles = if p == 0 && b == 0 {
                        None
                    } else {
                        Some(&self.planes[0].bands[0].tiles[..])
                    };
                    let result = decode_band(
                        &mut gb,
                        &info,
                        &mut band,
                        ref_tiles,
                        &mut self.rvmap_tabs,
                        dst,
                        refb,
                        three_bufs,
                    );
                    self.planes[p].bands[b] = band;
                    result?;
                }
            }
            self.buf_invalid[dst] = false;
        } else {
            if self.is_scalable {
                return Err(bad("null frame in a scalable stream"));
            }
            if self.planes.iter().any(|p| p.bands[0].cur.is_none()) {
                return Err(bad("null frame before any picture"));
            }
        }
        if self.buf_invalid[self.dst_buf] {
            return Err(bad("frame buffer is not valid"));
        }
        Ok(self.frame_type != FRAMETYPE_NULL)
    }

    /// Write the picture as 8-bit planes: Y (`width` x `height`), U and V (quarter size each
    /// way, `(width + 3) / 4` x `(height + 3) / 4`).
    pub fn output(&self, y: &mut [u8], u: &mut [u8], v: &mut [u8]) {
        let luma = &self.planes[0];
        if self.is_scalable {
            if let Some(cur) = luma.bands[0].cur {
                let bands: [&[i16]; 4] = std::array::from_fn(|b| &luma.bands[b].bufs[cur][..]);
                if bands.iter().all(|b| !b.is_empty()) {
                    let pitch = luma.bands[0].pitch as usize;
                    dsp::recompose53(
                        bands,
                        pitch,
                        luma.width as usize,
                        luma.height as usize,
                        y,
                        luma.width as usize,
                    );
                }
            }
        } else {
            output_plane(luma, y);
        }
        // the stream has V before U
        output_plane(&self.planes[2], u);
        output_plane(&self.planes[1], v);
    }
}

/// Add the bias of 128 back and clip (ivi_output_plane).
fn output_plane(plane: &Plane, dst: &mut [u8]) {
    let band = &plane.bands[0];
    let Some(cur) = band.cur else { return };
    let src = &band.bufs[cur];
    let (w, h, pitch) = (
        plane.width as usize,
        plane.height as usize,
        band.pitch as usize,
    );
    if src.len() < pitch * h || dst.len() < w * h {
        return;
    }
    for row in 0..h {
        let s = &src[row * pitch..row * pitch + w];
        let d = &mut dst[row * w..row * w + w];
        for (d, &s) in d.iter_mut().zip(s) {
            *d = dsp::clip_u8(s as i32 + 128);
        }
    }
}

/// Swap the corrected entries of a run/value table (done before a band, undone after it).
fn apply_corrections(rv: &mut RvMap, corr: &[u8]) {
    let swap = |rv: &mut RvMap, a: u8, b: u8| {
        rv.runtab.swap(a as usize, b as usize);
        rv.valtab.swap(a as usize, b as usize);
        if a == rv.eob_sym || b == rv.eob_sym {
            rv.eob_sym ^= a ^ b;
        }
        if a == rv.esc_sym || b == rv.esc_sym {
            rv.esc_sym ^= a ^ b;
        }
    };
    for pair in corr.as_chunks::<2>().0 {
        swap(rv, pair[0], pair[1]);
    }
}

fn undo_corrections(rv: &mut RvMap, corr: &[u8]) {
    for pair in corr.as_chunks::<2>().0.iter().rev() {
        apply_corrections(rv, pair);
    }
}

/// The band's picture buffers while it is decoded.
struct Bufs<'a> {
    buf: &'a mut [i16],
    reference: &'a [i16],
}

/// Decode one band (ivi.c decode_band).
#[allow(clippy::too_many_arguments)]
fn decode_band(
    gb: &mut BitReader,
    info: &FrameInfo,
    band: &mut Band,
    ref_tiles: Option<&[Tile]>,
    rvmap_tabs: &mut [RvMap; 9],
    dst: usize,
    refb: usize,
    three_bufs: bool,
) -> Result<(), Error> {
    // buffer 2 is only used by scalable streams
    if (!three_bufs && (dst == 2 || refb == 2)) || dst == refb {
        return Err(bad("band buffer points to no data"));
    }
    let size = band.buf_size() as usize;
    for i in [dst, refb] {
        if band.bufs[i].is_empty() {
            band.bufs[i] = vec![0; size];
        }
    }
    band.cur = Some(dst);

    band.decode_header(gb, info.frame_flags)?;
    if band.is_empty {
        return Err(bad("empty band"));
    }

    let rvmap = &mut rvmap_tabs[band.rvmap_sel];
    let corr = band.corr;
    let corr = &corr[..band.num_corr * 2];
    apply_corrections(rvmap, corr);

    // take buffers and tiles out of the band so the band itself can be read alongside
    let mut buf = std::mem::take(&mut band.bufs[dst]);
    let reference = std::mem::take(&mut band.bufs[refb]);
    let mut tiles = std::mem::take(&mut band.tiles);
    let mut bufs = Bufs {
        buf: &mut buf,
        reference: &reference,
    };

    let result = decode_tiles(gb, info, band, &mut tiles, ref_tiles, rvmap, &mut bufs);

    band.bufs[dst] = buf;
    band.bufs[refb] = reference;
    band.tiles = tiles;

    // FFmpeg leaves the table patched on some errors; we always restore it
    undo_corrections(rvmap, corr);

    gb.align();
    result
}

fn decode_tiles(
    gb: &mut BitReader,
    info: &FrameInfo,
    band: &Band,
    tiles: &mut [Tile],
    ref_tiles: Option<&[Tile]>,
    rvmap: &RvMap,
    bufs: &mut Bufs,
) -> Result<(), Error> {
    let blk_vlc = band.blk_vlc.vlc()?;
    let mut pos = gb.pos() as i64;
    for (t, tile) in tiles.iter_mut().enumerate() {
        if tile.mb_size != band.mb_size || info.luma_mb_size < band.mb_size {
            return Err(bad("macroblock sizes do not match"));
        }
        let ref_mbs = match (tile.has_ref, ref_tiles) {
            (true, Some(r)) => Some(&r.get(t).ok_or(bad("no reference tile"))?.mbs[..]),
            _ => None,
        };
        let is_empty = gb.bit();
        if is_empty {
            let mv_scale = (info.luma_mb_size >> 3) - (band.mb_size >> 3);
            process_empty_tile(band, tile, ref_mbs, mv_scale, bufs)?;
        } else {
            let data_size = tile_data_size(gb);
            if data_size == 0 {
                return Err(bad("tile data size is zero"));
            }
            decode_mb_info(gb, info, band, tile, ref_mbs)?;
            decode_blocks(gb, band, tile, blk_vlc, rvmap, bufs)?;
            if (gb.pos() as i64 - pos) >> 3 != data_size as i64 {
                return Err(bad("tile data size mismatch"));
            }
            pos += (data_size as i64) << 3;
        }
    }
    Ok(())
}

/// Size of a tile's data in bytes: one byte, or 0xff and three more bytes.
fn tile_data_size(gb: &mut BitReader) -> u32 {
    let mut len = 0;
    if gb.bit() {
        len = gb.bits(8);
        if len == 255 {
            len = gb.bits(24);
        }
    }
    gb.align();
    len
}

/// Decode type, coded block pattern, quant delta and motion vector of all macroblocks of a
/// tile (indeo5.c decode_mb_info).
fn decode_mb_info(
    gb: &mut BitReader,
    info: &FrameInfo,
    band: &Band,
    tile: &mut Tile,
    ref_mbs: Option<&[MbInfo]>,
) -> Result<(), Error> {
    let pitch = band.pitch;
    let mb_size = band.mb_size;
    let row_offset = mb_size * pitch;
    let mut offs = tile.ypos * pitch + tile.xpos;

    if ref_mbs.is_none() && ((band.qdelta_present && band.inherit_qdelta) || band.inherit_mv) {
        return Err(bad("inheritance without a reference band"));
    }
    if tile.mbs.len() != mbs_per_tile(tile.width, tile.height, mb_size) {
        return Err(bad("tile size does not match"));
    }

    let mv_scale = (info.luma_mb_size >> 3) - (mb_size >> 3);
    let inherit = |r: &MbInfo| -> (i8, i8) {
        if mv_scale != 0 {
            (
                scale_mv(r.mv_x as i32, mv_scale) as i8,
                scale_mv(r.mv_y as i32, mv_scale) as i8,
            )
        } else {
            (r.mv_x, r.mv_y)
        }
    };
    // the quant delta of the first luma band is always sent when frame flag 8 says so
    let first_band_q = band.plane == 0 && band.band_num == 0 && info.frame_flags & 8 != 0;
    let mut mv_x: i32 = 0;
    let mut mv_y: i32 = 0;
    let mut n = 0usize;

    let mut y = tile.ypos;
    while y < tile.ypos + tile.height {
        let mut mb_offset = offs;
        let mut x = tile.xpos;
        while x < tile.xpos + tile.width {
            let ref_mb = ref_mbs.and_then(|r| r.get(n)).copied();
            let inherited = if band.inherit_mv { ref_mb } else { None };
            let mut mb = MbInfo {
                xpos: x,
                ypos: y,
                buf_offs: mb_offset,
                ..MbInfo::default()
            };

            if gb.bit() {
                // empty macroblock: inter, no blocks coded
                if info.frame_type == FRAMETYPE_INTRA {
                    return Err(bad("empty macroblock in an intra frame"));
                }
                mb.mb_type = 1;
                mb.cbp = 0;
                if first_band_q {
                    mb.q_delta = q_delta_from(gb.vlc(info.mb_vlc));
                }
                if let Some(r) = inherited {
                    (mb.mv_x, mb.mv_y) = inherit(&r);
                }
            } else {
                mb.mb_type = if let Some(r) = inherited {
                    r.mb_type
                } else if info.frame_type == FRAMETYPE_INTRA {
                    0
                } else {
                    gb.bit() as u8
                };

                let blks_per_mb = if band.mb_size != band.blk_size { 4 } else { 1 };
                mb.cbp = gb.bits(blks_per_mb) as u8;

                if band.qdelta_present {
                    if band.inherit_qdelta {
                        if let Some(r) = ref_mb {
                            mb.q_delta = r.q_delta;
                        }
                    } else if mb.cbp != 0 || first_band_q {
                        mb.q_delta = q_delta_from(gb.vlc(info.mb_vlc));
                    }
                }

                if mb.mb_type != 0 {
                    if let Some(r) = inherited {
                        (mb.mv_x, mb.mv_y) = inherit(&r);
                    } else {
                        // motion vector deltas: y first
                        mv_y = mv_y.wrapping_add(to_signed(gb.vlc(info.mb_vlc)));
                        mv_x = mv_x.wrapping_add(to_signed(gb.vlc(info.mb_vlc)));
                        mb.mv_x = mv_x as i8;
                        mb.mv_y = mv_y as i8;
                    }
                }
            }

            if mb.mb_type != 0 {
                // the reference block must lie inside the buffer
                let s = band.is_halfpel as i32;
                let (mvx, mvy) = (mb.mv_x as i64, mb.mv_y as i64);
                let (x, y, pitch, m) = (x as i64, y as i64, pitch as i64, mb_size as i64);
                let first = x + (mvx >> s) + (y + (mvy >> s)) * pitch;
                let last = x + ((mvx + s as i64) >> s) + m - 1
                    + (y + m - 1 + ((mvy + s as i64) >> s)) * pitch;
                if first < 0 || last > band.buf_size() - 1 {
                    return Err(bad("motion vector outside the reference"));
                }
            }

            tile.mbs[n] = mb;
            n += 1;
            mb_offset += mb_size;
            x += mb_size;
        }
        offs += row_offset;
        y += mb_size;
    }

    gb.align();
    Ok(())
}

/// Whether a macroblock's motion vector points inside the reference buffer.
fn mv_in_bounds(band: &Band, mb: &MbInfo) -> bool {
    let h = band.is_halfpel as i32;
    let (mv_x, mv_y) = (mb.mv_x as i32, mb.mv_y as i32);
    let (dmv_x, dmv_y) = (mv_x >> h, mv_y >> h);
    let (cx, cy) = (mv_x & h, mv_y & h);
    !(mb.xpos + dmv_x < 0
        || mb.xpos + dmv_x + band.mb_size + cx > band.pitch
        || mb.ypos + dmv_y < 0
        || mb.ypos + dmv_y + band.mb_size + cy > band.aheight)
}

/// Motion compensation of one block (ivi.c ivi_mc, single reference only: Indeo 5 has no
/// B-frames). `add`: add the prediction to the decoded difference in the buffer.
fn block_mc(
    band: &Band,
    bufs: &mut Bufs,
    offs: i64,
    mv_x: i32,
    mv_y: i32,
    mc_type: u32,
    add: bool,
) -> Result<(), Error> {
    let pitch = band.pitch as i64;
    let blk = band.blk_size as i64;
    let ref_offs = offs + mv_y as i64 * pitch + mv_x as i64;
    let buf_size = band.buf_size();
    let min_size = pitch * (blk - 1) + blk;
    let ref_size = (mc_type > 1) as i64 * pitch + (mc_type & 1) as i64;
    if offs < 0
        || ref_offs < 0
        || buf_size - min_size < offs
        || buf_size - min_size - ref_size < ref_offs
        || bufs.buf.len() as i64 != buf_size
        || bufs.reference.len() as i64 != buf_size
    {
        return Err(bad("motion compensation outside the buffer"));
    }
    dsp::mc(
        &mut bufs.buf[offs as usize..],
        &bufs.reference[ref_offs as usize..],
        pitch as usize,
        blk as usize,
        mc_type,
        add,
    );
    Ok(())
}

/// Decode the blocks of all macroblocks of a tile: coefficients, dequantisation, inverse
/// transform, motion compensation (ivi.c ivi_decode_blocks).
fn decode_blocks(
    gb: &mut BitReader,
    band: &Band,
    tile: &Tile,
    blk_vlc: &Vlc,
    rvmap: &RvMap,
    bufs: &mut Bufs,
) -> Result<(), Error> {
    let blk_size = band.blk_size as i64;
    let pitch = band.pitch as i64;
    let num_blocks = if band.mb_size != band.blk_size { 4 } else { 1 };
    // DC prediction for intra blocks runs through the whole tile
    let mut prev_dc: i32 = 0;
    // like FFmpeg, these keep their values from the macroblock before
    let mut mc_type: u32 = 0;
    let (mut mv_x, mut mv_y) = (0i32, 0i32);

    for mb in &tile.mbs {
        let is_intra = mb.mb_type == 0;
        let mut cbp = mb.cbp;
        let mut buf_offs = mb.buf_offs as i64;

        let mut quant = (band.glob_quant + mb.q_delta as i32).clamp(0, 23) as usize;
        let scale_tab = if is_intra {
            band.intra_scale
        } else {
            band.inter_scale
        };
        if !scale_tab.is_empty() {
            quant = scale_tab[quant] as usize;
        }

        if !is_intra {
            mv_x = mb.mv_x as i32;
            mv_y = mb.mv_y as i32;
            if band.is_halfpel {
                mc_type = (((mv_y & 1) << 1) | (mv_x & 1)) as u32;
                mv_x >>= 1;
                mv_y >>= 1;
            }
            if !mv_in_bounds(band, mb) {
                return Err(bad("motion vector out of bounds"));
            }
        }

        for blk in 0..num_blocks {
            // block position inside the macroblock: 0 1 / 2 3
            if blk & 1 != 0 {
                buf_offs += blk_size;
            } else if blk == 2 {
                buf_offs -= blk_size;
                buf_offs += blk_size * pitch;
            }

            if cbp & 1 != 0 {
                decode_coded_block(
                    gb,
                    band,
                    blk_vlc,
                    rvmap,
                    bufs,
                    mv_x,
                    mv_y,
                    &mut prev_dc,
                    is_intra,
                    mc_type,
                    quant as u32,
                    buf_offs,
                )?;
            } else {
                // not coded: intra blocks get the DC of the block before, inter blocks only
                // the motion compensation
                let min_size = (blk_size - 1) * pitch + blk_size;
                if min_size > band.buf_size() - buf_offs || buf_offs < 0 {
                    return Err(bad("block outside the buffer"));
                }
                if is_intra {
                    band.transform.dc(
                        prev_dc,
                        &mut bufs.buf[buf_offs as usize..],
                        pitch as usize,
                        blk_size as usize,
                    );
                } else {
                    block_mc(band, bufs, buf_offs, mv_x, mv_y, mc_type, false)?;
                }
            }
            cbp >>= 1;
        }
    }

    gb.align();
    Ok(())
}

/// Decode one coded block (ivi.c ivi_decode_coded_blocks).
#[allow(clippy::too_many_arguments)]
fn decode_coded_block(
    gb: &mut BitReader,
    band: &Band,
    blk_vlc: &Vlc,
    rvmap: &RvMap,
    bufs: &mut Bufs,
    mv_x: i32,
    mv_y: i32,
    prev_dc: &mut i32,
    is_intra: bool,
    mc_type: u32,
    quant: u32,
    offs: i64,
) -> Result<(), Error> {
    let base_tab = if is_intra {
        band.intra_base
    } else {
        band.inter_base
    };
    let blk_size = band.blk_size as usize;
    let num_coeffs = (blk_size * blk_size) as i32;
    let col_mask = blk_size - 1;
    let pitch = band.pitch as i64;
    let ts = band.transform_size as i64;
    let min_size = pitch * (ts - 1) + ts;
    if offs < 0 || min_size > band.buf_size() - offs {
        return Err(bad("block outside the buffer"));
    }
    if band.scan.len() < num_coeffs as usize || base_tab.len() < num_coeffs as usize {
        return Err(bad("scan pattern is not set"));
    }

    let mut trvec = [0i32; 64];
    let mut col_flags = [0u8; 8];
    let mut scan_pos: i32 = -1;
    let mut sym: i32 = 0;
    let eob = rvmap.eob_sym as i32;
    let esc = rvmap.esc_sym as i32;

    while scan_pos <= num_coeffs {
        sym = gb.vlc(blk_vlc);
        if sym == eob {
            break;
        }
        let (run, val): (i32, i32);
        if sym == esc {
            // run and value sent explicitly in three codes
            run = gb.vlc(blk_vlc) + 1;
            let lo = gb.vlc(blk_vlc) as u32;
            let hi = gb.vlc(blk_vlc) as u32;
            val = to_signed_u(hi.wrapping_shl(6) | lo);
        } else {
            if sym as u32 >= 256 {
                return Err(bad("invalid block symbol"));
            }
            run = rvmap.runtab[sym as usize] as i32;
            val = rvmap.valtab[sym as usize] as i32;
        }

        // de-zigzag and dequantise
        scan_pos += run;
        if scan_pos >= num_coeffs || scan_pos < 0 {
            break;
        }
        let pos = band.scan[scan_pos as usize] as usize;

        let q = (base_tab[pos] as u32 * quant) >> 9;
        let mut val = val;
        if q > 1 {
            let sign: i32 = if val > 0 { 1 } else { -1 };
            val = val
                .wrapping_mul(q as i32)
                .wrapping_add(sign.wrapping_mul((((q ^ 1) - 1) >> 1) as i32));
        }
        trvec[pos] = val;
        // remember which columns have non-zero coefficients
        col_flags[pos & col_mask] |= (val != 0) as u8;
    }

    if scan_pos < 0 || (scan_pos >= num_coeffs && sym != eob) {
        return Err(bad("corrupt block data"));
    }

    // intra blocks of 2D transforms send the DC as difference to the block before
    if is_intra && band.transform.is_2d() {
        *prev_dc = prev_dc.wrapping_add(trvec[0]);
        trvec[0] = *prev_dc;
        col_flags[0] |= (*prev_dc != 0) as u8;
    }

    if band.transform_size > band.blk_size {
        return Err(bad("transform larger than the block"));
    }

    band.transform.inverse(
        &trvec,
        &mut bufs.buf[offs as usize..],
        pitch as usize,
        &col_flags,
    );

    if !is_intra {
        block_mc(band, bufs, offs, mv_x, mv_y, mc_type, true)?;
    }
    Ok(())
}

/// A tile without data: copy it from the reference picture, or motion-compensate it with
/// vectors inherited from the first luma band (ivi.c ivi_process_empty_tile).
fn process_empty_tile(
    band: &Band,
    tile: &mut Tile,
    ref_mbs: Option<&[MbInfo]>,
    mv_scale: i32,
    bufs: &mut Bufs,
) -> Result<(), Error> {
    let mb_size = band.mb_size;
    let pitch = band.pitch;
    if tile.mbs.len() != mbs_per_tile(tile.width, tile.height, mb_size) {
        return Err(bad("tile size does not match"));
    }
    let clear_first = !band.qdelta_present && band.plane == 0 && band.band_num == 0;
    let mut offs = tile.ypos * pitch + tile.xpos;
    let mut need_mc = false;
    let mut n = 0usize;

    let mut y = tile.ypos;
    while y < tile.ypos + tile.height {
        let mut mb_offset = offs;
        let mut x = tile.xpos;
        while x < tile.xpos + tile.width {
            let mb = &mut tile.mbs[n];
            mb.xpos = x;
            mb.ypos = y;
            mb.buf_offs = mb_offset;
            mb.mb_type = 1;
            mb.cbp = 0;
            if clear_first {
                mb.q_delta = band.glob_quant as i8;
                mb.mv_x = 0;
                mb.mv_y = 0;
            }
            if let Some(r) = ref_mbs.and_then(|r| r.get(n)) {
                if band.inherit_qdelta {
                    mb.q_delta = r.q_delta;
                }
                if band.inherit_mv {
                    if mv_scale != 0 {
                        mb.mv_x = scale_mv(r.mv_x as i32, mv_scale) as i8;
                        mb.mv_y = scale_mv(r.mv_y as i32, mv_scale) as i8;
                    } else {
                        mb.mv_x = r.mv_x;
                        mb.mv_y = r.mv_y;
                    }
                    need_mc |= mb.mv_x != 0 || mb.mv_y != 0;
                    if !mv_in_bounds(band, mb) {
                        return Err(bad("motion vector out of bounds"));
                    }
                }
            }
            n += 1;
            mb_offset += mb_size;
            x += mb_size;
        }
        offs += mb_size * pitch;
        y += mb_size;
    }

    if band.inherit_mv && need_mc {
        // at least one motion vector: motion-compensate every block
        let num_blocks = if mb_size != band.blk_size { 4 } else { 1 };
        let blk = band.blk_size as i64;
        for mb in &tile.mbs {
            let (mut mv_x, mut mv_y) = (mb.mv_x as i32, mb.mv_y as i32);
            let mc_type = if band.is_halfpel {
                let t = (((mv_y & 1) << 1) | (mv_x & 1)) as u32;
                mv_x >>= 1;
                mv_y >>= 1;
                t
            } else {
                0
            };
            for b in 0..num_blocks {
                let offs = mb.buf_offs as i64
                    + blk * ((b & 1) as i64 + ((b & 2) != 0) as i64 * pitch as i64);
                block_mc(band, bufs, offs, mv_x, mv_y, mc_type, false)?;
            }
        }
    } else {
        // copy the tile from the reference picture
        let (w, p) = (tile.width as usize, pitch as usize);
        if bufs.reference.len() != bufs.buf.len() {
            return Err(bad("reference buffer missing"));
        }
        for row in 0..tile.height as usize {
            let start = (tile.ypos as usize + row) * p + tile.xpos as usize;
            bufs.buf[start..start + w].copy_from_slice(&bufs.reference[start..start + w]);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_from_lowest_bit() {
        assert_eq!(to_signed(0), 0);
        assert_eq!(to_signed(1), 1);
        assert_eq!(to_signed(2), -1);
        assert_eq!(to_signed(3), 2);
        assert_eq!(to_signed(4), -2);
        // FFmpeg's IVI_TOSIGNED of -1 (a missing code)
        assert_eq!(to_signed(-1), 0);
        assert_eq!(to_signed_u(u32::MAX), i32::MIN);
        assert_eq!(q_delta_from(200), 28);
    }

    #[test]
    fn mv_scaling() {
        assert_eq!(scale_mv(0, 1), 0);
        assert_eq!(scale_mv(5, 1), 3);
        assert_eq!(scale_mv(-5, 1), -3);
        assert_eq!(scale_mv(-1, 1), -1);
    }

    #[test]
    fn corrections_round_trip() {
        let orig = RVMAP_TABS[0].clone();
        let mut rv = orig.clone();
        let corr = [5, 7, 2, 9, 7, 2];
        apply_corrections(&mut rv, &corr);
        assert_ne!(rv.eob_sym, orig.eob_sym);
        undo_corrections(&mut rv, &corr);
        assert_eq!(rv.runtab, orig.runtab);
        assert_eq!(rv.valtab, orig.valtab);
        assert_eq!((rv.eob_sym, rv.esc_sym), (orig.eob_sym, orig.esc_sym));
    }

    #[test]
    fn garbage_never_panics() {
        // a pseudo-random byte soup, also with a valid start code and intra frame type
        let mut ivi = Ivi::new(320, 240);
        let mut seed = 0x1234_5678u32;
        for len in [0usize, 1, 2, 5, 17, 100, 1000, 5000] {
            for round in 0..20 {
                let mut data: Vec<u8> = (0..len)
                    .map(|_| {
                        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                        (seed >> 24) as u8
                    })
                    .collect();
                if round % 2 == 0 && !data.is_empty() {
                    data[0] = 0x1f; // start code, intra
                }
                let _ = ivi.decode_frame(&data);
            }
        }
    }
}
