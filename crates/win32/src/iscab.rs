//! InstallShield 5 and 6 cabinets: the file table in the header, the file groups (which files
//! go together, e.g. "Program Files"), and the files' data (chunks of raw deflate, each after
//! its 16-bit length).
//!
//! Layout as unshield reads it. The header's common part (signature `ISc(`, version, ...,
//! cab descriptor offset) points to the cab descriptor; offsets inside it count from the
//! descriptor. Version 6 keeps the header in `data1.hdr`; version 5 has it either there or at
//! the start of `data1.cab` (Recoil).

use md5::{Digest, Md5};

/// A file in the cabinets.
#[derive(Clone, Debug)]
pub struct File {
    pub name: String,
    /// its folder below the group's destination (`SFX\Game`, or empty)
    pub dir: String,
    pub flags: u16,
    pub size: u64,
    pub packed: u64,
    /// where its data starts in its volume's cab; 0 for files beside the cabs on the disc
    pub offset: u64,
    pub md5: [u8; 16],
    /// its cab: 1 for data1.cab, ...
    pub volume: u16,
}

pub const OBFUSCATED: u16 = 2;
pub const COMPRESSED: u16 = 4;
pub const INVALID: u16 = 8;

impl File {
    /// A file the setup copies from the disc instead of from a cab.
    pub fn beside(&self) -> bool {
        self.flags & COMPRESSED == 0 && self.offset == 0
    }
}

/// A file group: the files `first..=last` go to `dest` (`<TARGETDIR>\Data\Textures`; empty
/// in version 5, where the setup script decides).
#[derive(Clone, Debug)]
pub struct Group {
    pub name: String,
    pub dest: String,
    pub first: u32,
    pub last: u32,
}

pub struct Header {
    /// 5 or 6
    pub major: u32,
    pub files: Vec<File>,
    pub groups: Vec<Group>,
}

fn u16_at(d: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(d.get(o..o + 2)?.try_into().ok()?))
}

fn u32_at(d: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(o..o + 4)?.try_into().ok()?))
}

fn u64_at(d: &[u8], o: usize) -> Option<u64> {
    Some(u64::from_le_bytes(d.get(o..o + 8)?.try_into().ok()?))
}

/// The zero-terminated string at `o` (Windows-1252 text read as Latin-1).
fn cstr(d: &[u8], o: usize) -> Option<String> {
    let s = d.get(o..)?;
    let n = s.iter().position(|&b| b == 0)?;
    Some(s[..n].iter().map(|&b| b as char).collect())
}

impl Header {
    pub fn parse(hdr: &[u8]) -> Result<Header, String> {
        Self::read(hdr).ok_or_else(|| "damaged or unknown InstallShield header".into())
    }

    fn read(hdr: &[u8]) -> Option<Header> {
        if hdr.get(..4)? != b"ISc(" {
            return None;
        }
        let version = u32_at(hdr, 4)?;
        // the 0x01xxxxxx form: the major version in bits 12..16, where 0 is version 5
        // (0x01000004); the other forms keep it in the low 16 bits times 100
        let major = match version >> 24 {
            1 => match (version >> 12) & 0xf {
                0 => 5,
                m => m,
            },
            _ => (version & 0xffff) / 100,
        };
        if !(5..=6).contains(&major) {
            return None;
        }
        let cd = u32_at(hdr, 12)? as usize;
        let at = |o: usize| u32_at(hdr, cd + o);
        let table = cd + at(0x0c)? as usize;
        let (dirs, count, descs) = (at(0x1c)? as usize, at(0x28)? as usize, at(0x2c)? as usize);
        let offset = |k: usize| u32_at(hdr, table + 4 * k);

        let dirs: Vec<String> = (0..dirs)
            .map(|k| cstr(hdr, table + offset(k)? as usize))
            .collect::<Option<_>>()?;
        let mut files = Vec::with_capacity(count);
        for k in 0..count {
            files.push(if major == 5 {
                // a list of descriptor offsets after the folders' names
                let d = hdr
                    .get(table + offset(dirs.len() + k)? as usize..)?
                    .get(..0x2a)?;
                let flags = u16_at(d, 8)?;
                let (name, dir) = if flags & INVALID != 0 {
                    (String::new(), String::new())
                } else {
                    (
                        cstr(hdr, table + u32_at(d, 0)? as usize)?,
                        dirs.get(u32_at(d, 4)? as usize)?.clone(),
                    )
                };
                File {
                    name,
                    dir,
                    flags,
                    size: u32_at(d, 0x0a)? as u64,
                    packed: u32_at(d, 0x0e)? as u64,
                    offset: u32_at(d, 0x26)? as u64,
                    // the bytes after the offset are no MD5 in Recoil's cab: no check
                    md5: [0; 16],
                    volume: 1,
                }
            } else {
                let d = hdr.get(table + descs + k * 0x57..)?.get(..0x57)?;
                let flags = u16_at(d, 0)?;
                let (name, dir) = if flags & INVALID != 0 {
                    (String::new(), String::new())
                } else {
                    (
                        cstr(hdr, table + u32_at(d, 0x3a)? as usize)?,
                        dirs.get(u16_at(d, 0x3e)? as usize)?.clone(),
                    )
                };
                File {
                    name,
                    dir,
                    flags,
                    size: u64_at(d, 0x02)?,
                    packed: u64_at(d, 0x0a)?,
                    offset: u64_at(d, 0x12)?,
                    md5: d[0x1a..0x2a].try_into().ok()?,
                    volume: u16_at(d, 0x55)?,
                }
            });
        }

        // 71 lists of groups, each entry (name, descriptor, next) with offsets from cd
        let mut groups = Vec::new();
        for k in 0..71 {
            let mut entry = at(0x3e + 4 * k)? as usize;
            while entry != 0 {
                let g = cd + u32_at(hdr, cd + entry + 4)? as usize;
                let text = |o: usize| match u32_at(hdr, g + o)? {
                    0 => Some(String::new()),
                    s => cstr(hdr, cd + s as usize),
                };
                let range = if major == 5 { 0x4c } else { 0x16 };
                groups.push(Group {
                    name: text(0)?,
                    dest: if major == 5 {
                        String::new()
                    } else {
                        text(0x3a)?
                    },
                    first: u32_at(hdr, g + range)?,
                    last: u32_at(hdr, g + range + 4)?,
                });
                entry = u32_at(hdr, cd + entry + 8)? as usize;
            }
        }
        Some(Header {
            major,
            files,
            groups,
        })
    }
}

/// A file's contents from its `packed` bytes in the cab, checked against its MD5.
pub fn unpack(f: &File, packed: &[u8]) -> Result<Vec<u8>, String> {
    if packed.len() as u64 != f.packed {
        return Err(format!(
            "{}: {} of {} bytes in the cab (spans volumes?)",
            f.name,
            packed.len(),
            f.packed
        ));
    }
    if f.flags & OBFUSCATED != 0 {
        return Err(format!("{}: obfuscated (not supported yet)", f.name));
    }
    let out = if f.flags & COMPRESSED == 0 {
        packed.to_vec()
    } else {
        let mut out = Vec::with_capacity(f.size as usize);
        let mut at = 0;
        while at < packed.len() {
            let n = u16_at(packed, at).ok_or("cut chunk")? as usize;
            let chunk = packed
                .get(at + 2..at + 2 + n)
                .ok_or_else(|| format!("{}: cut chunk", f.name))?;
            out.extend(
                miniz_oxide::inflate::decompress_to_vec(chunk)
                    .map_err(|e| format!("{}: {e:?}", f.name))?,
            );
            at += 2 + n;
        }
        out
    };
    if out.len() as u64 != f.size {
        return Err(format!(
            "{}: {} bytes unpacked, {} expected",
            f.name,
            out.len(),
            f.size
        ));
    }
    if f.md5 != [0; 16] && Md5::digest(&out)[..] != f.md5 {
        return Err(format!("{}: MD5 differs", f.name));
    }
    Ok(out)
}
