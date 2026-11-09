//! PE images: exports by name and ordinal, relocations applied by `rebase`, and `combine`.
//! A tiny DLL is put together here byte by byte. Needs no game files.

use recomp::image::Image;

const BASE: u32 = 0x1000_0000;

/// Headers, .text at rva 0x1000 (two functions; a dword pointing at the second), .edata at
/// 0x2000 (the second exported by name, the first by ordinal only), .reloc at 0x3000.
fn dll() -> Vec<u8> {
    let mut d = vec![0u8; 0x800];
    let put = |d: &mut Vec<u8>, at: usize, b: &[u8]| d[at..at + b.len()].copy_from_slice(b);
    let w32 = |d: &mut Vec<u8>, at: usize, v: u32| d[at..at + 4].copy_from_slice(&v.to_le_bytes());
    let w16 = |d: &mut Vec<u8>, at: usize, v: u16| d[at..at + 2].copy_from_slice(&v.to_le_bytes());
    put(&mut d, 0, b"MZ");
    let pe = 0x40;
    w32(&mut d, 0x3c, pe as u32);
    put(&mut d, pe, b"PE\0\0");
    w16(&mut d, pe + 4, 0x14c);
    w16(&mut d, pe + 6, 3); // sections
    w16(&mut d, pe + 20, 0xe0); // optional header size
    let opt = pe + 24;
    w16(&mut d, opt, 0x10b);
    w32(&mut d, opt + 16, 0x1000); // entry
    w32(&mut d, opt + 28, BASE);
    w32(&mut d, opt + 96, 0x2000); // export directory
    w32(&mut d, opt + 100, 0x100);
    w32(&mut d, opt + 136, 0x3000); // relocations
    w32(&mut d, opt + 140, 12);
    // name, vsize, rva, rsize, raw, flags
    for (k, (name, rva, raw, flags)) in
        [(".text", 0x1000, 0x200, 0x6000_0020u32), (".edata", 0x2000, 0x400, 0x4000_0040), (".reloc", 0x3000, 0x600, 0x4200_0040)].into_iter().enumerate()
    {
        let o = opt + 0xe0 + 40 * k;
        put(&mut d, o, name.as_bytes());
        w32(&mut d, o + 8, 0x200);
        w32(&mut d, o + 12, rva);
        w32(&mut d, o + 16, 0x200);
        w32(&mut d, o + 20, raw);
        w32(&mut d, o + 36, flags);
    }
    // .text: ret at 0x1000, mov eax, [BASE + 0x1010]; ret at 0x1004; a pointer at 0x1010
    put(&mut d, 0x200, &[0xc3]);
    put(&mut d, 0x204, &[0xa1]);
    w32(&mut d, 0x205, BASE + 0x1010);
    put(&mut d, 0x209, &[0xc3]);
    w32(&mut d, 0x210, BASE + 0x1004);
    // .edata: ordinal base 5, 2 functions, 1 name
    let e = 0x400;
    w32(&mut d, e + 16, 5);
    w32(&mut d, e + 20, 2);
    w32(&mut d, e + 24, 1);
    w32(&mut d, e + 28, 0x2040); // functions
    w32(&mut d, e + 32, 0x2050); // names
    w32(&mut d, e + 36, 0x2060); // name ordinals
    w32(&mut d, e + 0x40, 0x1000);
    w32(&mut d, e + 0x44, 0x1004);
    w32(&mut d, e + 0x50, 0x2070);
    w16(&mut d, e + 0x60, 1);
    put(&mut d, e + 0x70, b"Second\0");
    // .reloc: page 0x1000, HIGHLOW at 0x005 and 0x010
    let r = 0x600;
    w32(&mut d, r, 0x1000);
    w32(&mut d, r + 4, 12);
    w16(&mut d, r + 8, 0x3005);
    w16(&mut d, r + 10, 0x3010);
    d
}

#[test]
fn exports() {
    let img = Image::parse(dll());
    assert_eq!(img.exports(), vec![(BASE + 0x1000, "#5".to_string()), (BASE + 0x1004, "Second".to_string())]);
    assert_eq!(img.export_ordinals(), vec![(BASE + 0x1000, 5), (BASE + 0x1004, 6)]);
}

#[test]
fn rebase() {
    let mut img = Image::parse(dll());
    let to = 0x2000_0000;
    img.rebase(to);
    assert_eq!((img.base, img.entry), (to, to + 0x1000));
    assert!(img.is_code(to + 0x1004) && !img.is_code(BASE + 0x1004));
    assert_eq!(img.u32(to + 0x1005), to + 0x1010, "the instruction's address moved");
    assert_eq!(img.u32(to + 0x1010), to + 0x1004, "the pointer moved");
    assert_eq!(img.exports()[1], (to + 0x1004, "Second".to_string()));
}

#[test]
fn combine() {
    let a = Image::parse(dll());
    let mut b = Image::parse(dll());
    b.rebase(0x2000_0000);
    let both = Image::combine(&[&a, &b]);
    assert_eq!((both.base, both.entry), (BASE, BASE + 0x1000));
    assert_eq!(both.sections.len(), 6);
    assert_eq!(both.u32(BASE + 0x1010), BASE + 0x1004);
    assert_eq!(both.u32(0x2000_1010), 0x2000_1004);
    assert!(both.is_code(BASE + 0x1000) && both.is_code(0x2000_1000));
}

/// `Image::patch` changes code bytes only where they are what the port expects.
#[test]
fn patch_checks_old_bytes() {
    let mut img = Image::parse(dll());
    let at = BASE + 0x1000;
    let old = img.read(at, 2).unwrap();
    assert!(img.patch(at, &[old[0] ^ 0xff, old[1]], &[0x90, 0x90]).is_err());
    assert_eq!(img.read(at, 2).unwrap(), old);
    img.patch(at, &old, &[0xeb, 0x00]).unwrap();
    assert_eq!(img.read(at, 2).unwrap(), [0xeb, 0x00]);
    assert!(img.patch(BASE + 0x9000, &[0], &[0]).is_err());
    assert!(img.patch(at, &[0xeb], &[0x90, 0x90]).is_err());
}
