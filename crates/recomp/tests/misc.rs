//! MMX moves, port I/O, segment registers, xlat, 16-bit pushf/popf and bsf/bsr, checked
//! against Rust. Needs no game files.

mod common;

use common::{Guest, Rng, SCRATCH, Snippet};
use recomp::rt::{EAX, EBX, ECX, EDI, EDX, ESP};
use iced_x86::IcedError;
use iced_x86::code_asm::*;

/// (src, dst): 32 bytes through mm0-mm3 with movq, plus a dword through movd mm4/ecx
fn mmx(a: &mut CodeAssembler) -> Result<(), IcedError> {
    a.mov(edx, dword_ptr(esp + 4))?;
    a.mov(eax, dword_ptr(esp + 8))?;
    a.movq(mm0, qword_ptr(edx))?;
    a.movq(mm1, qword_ptr(edx + 8))?;
    a.movq(mm2, qword_ptr(edx + 16))?;
    a.movq(mm3, mm2)?;
    a.movq(mm2, qword_ptr(edx + 24))?;
    a.movq(qword_ptr(eax), mm0)?;
    a.movq(qword_ptr(eax + 8), mm1)?;
    a.movq(qword_ptr(eax + 16), mm3)?;
    a.movq(qword_ptr(eax + 24), mm2)?;
    a.movd(mm4, dword_ptr(edx + 32))?;
    a.movd(ecx, mm4)?;
    a.emms()?;
    a.ret()
}

/// (p): nothing on the ports: eax = in al (rest of eax kept), ecx = in eax; two insb up, one down
fn ports(a: &mut CodeAssembler) -> Result<(), IcedError> {
    a.mov(edi, dword_ptr(esp + 4))?;
    a.mov(dx, 0x201)?;
    a.mov(eax, 0x12345600)?;
    a.out(dx, al)?;
    a.in_(al, dx)?;
    a.mov(ecx, eax)?;
    a.in_(eax, 0x40)?;
    a.xchg(eax, ecx)?;
    a.insb()?;
    a.insb()?;
    a.std()?;
    a.insb()?;
    a.cld()?;
    a.ret()
}

/// push es / pop es keep the stack balanced
fn segs(a: &mut CodeAssembler) -> Result<(), IcedError> {
    a.mov(ecx, esp)?;
    a.push(es)?;
    a.push(es)?;
    a.pop(es)?;
    a.pop(es)?;
    a.sub(ecx, esp)?;
    a.ret()
}

/// (table, index): eax = the index with al replaced by table[al] (xlat)
fn xlat(a: &mut CodeAssembler) -> Result<(), IcedError> {
    a.push(ebx)?;
    a.mov(ebx, dword_ptr(esp + 8))?;
    a.mov(eax, dword_ptr(esp + 12))?;
    a.xlatb()?;
    a.pop(ebx)?;
    a.ret()
}

/// (p): eax = cs, ecx = fs, edx = gs, [p] = ds (a word), ebx = ss; writes to segment
/// registers are dropped
fn segmov(a: &mut CodeAssembler) -> Result<(), IcedError> {
    a.push(ebx)?;
    a.mov(edx, dword_ptr(esp + 8))?;
    a.mov(eax, 0x1234)?;
    a.mov(es, ax)?;
    a.mov(ds, word_ptr(edx))?;
    a.mov(word_ptr(edx), ds)?;
    a.mov(ebx, 0xffff_ffff_u32)?;
    a.mov(bx, ss)?;
    a.mov(ecx, ebx)?;
    a.pop(ebx)?;
    a.push(ecx)?;
    a.mov(eax, cs)?;
    a.mov(ecx, fs)?;
    a.mov(edx, gs)?;
    a.pop(ebx)?;
    a.ret()
}

/// 16-bit flags: eax = pushf after stc (upper half kept), ecx = cf after popf of 1, edx =
/// pushfd after popfd with AC and then popf (AC kept), ebx = esp moved
fn flags16(a: &mut CodeAssembler) -> Result<(), IcedError> {
    a.push(ebx)?;
    a.mov(ebx, esp)?;
    a.mov(eax, 0x5555_0000)?;
    a.stc()?;
    a.pushf()?;
    a.pop(ax)?;
    a.push(0x4_0000)?; // AC
    a.popfd()?;
    a.mov(cx, 1)?;
    a.push(cx)?;
    a.popf()?;
    a.setb(cl)?;
    a.movzx(ecx, cl)?;
    a.pushfd()?;
    a.pop(edx)?;
    a.push(0)?;
    a.popfd()?;
    a.sub(ebx, esp)?;
    a.mov(dword_ptr(esp), ebx)?; // the saved ebx is not needed back here
    a.pop(ebx)?;
    a.ret()
}

/// (x): eax = bsf x, ecx = bsr x (both 0x77 for 0: left as they were), edx = zf
fn bsfr(a: &mut CodeAssembler) -> Result<(), IcedError> {
    a.mov(edx, dword_ptr(esp + 4))?;
    a.mov(eax, 0x77)?;
    a.mov(ecx, 0x77)?;
    a.bsf(eax, edx)?;
    a.bsr(ecx, dword_ptr(esp + 4))?;
    a.setz(dl)?;
    a.movzx(edx, dl)?;
    a.ret()
}

const SNIPPETS: [(&str, Snippet); 7] =
    [("mmx", mmx), ("ports", ports), ("segs", segs), ("xlat", xlat), ("segmov", segmov), ("flags16", flags16), ("bsfr", bsfr)];

#[test]
fn misc() {
    let g = Guest::new(&SNIPPETS);
    let m = &g.mem;
    let mut rng = Rng(0x1234_5678_9abc_def1);
    for _ in 0..100 {
        let src: Vec<u8> = (0..36).map(|_| rng.next() as u8).collect();
        m.slice(SCRATCH, 36).copy_from_slice(&src);
        m.slice(SCRATCH + 64, 32).fill(0);
        let cpu = g.call(0, &[SCRATCH, SCRATCH + 64]);
        assert_eq!(m.slice(SCRATCH + 64, 32), &src[..32]);
        assert_eq!(cpu.r[ECX], u32::from_le_bytes(src[32..36].try_into().unwrap()));
    }

    m.slice(SCRATCH, 4).fill(0);
    let cpu = g.call(1, &[SCRATCH + 1]);
    assert_eq!(cpu.r[EAX], 0x123456ff);
    assert_eq!(cpu.r[ECX], 0xffffffff);
    assert_eq!(m.slice(SCRATCH, 4), &[0, 0xff, 0xff, 0xff]);
    assert_eq!(cpu.r[EDI], SCRATCH + 2, "insb: up, up, then down");

    let cpu = g.call(2, &[]);
    assert_eq!(cpu.r[ECX], 0);
    assert_eq!(cpu.r[ESP], 0x0020_0000 - 0x100, "the return popped the return address");

    for k in 0..256u32 {
        m.wr8(SCRATCH + k, (k as u8).wrapping_mul(7) ^ 0x5a);
    }
    for v in [0x1234_5600u32, 0xdead_beff, 0x80] {
        let cpu = g.call(3, &[SCRATCH, v]);
        assert_eq!(cpu.r[EAX], v & !0xff | ((v as u8).wrapping_mul(7) ^ 0x5a) as u32);
    }

    m.wr32(SCRATCH, 0xaaaa_5555);
    let cpu = g.call(4, &[SCRATCH]);
    assert_eq!((cpu.r[EAX], cpu.r[ECX], cpu.r[EDX]), (0x1b, 0x3b, 0), "cs, fs, gs");
    assert_eq!(cpu.r[EBX], 0xffff_0023, "ss into bx");
    assert_eq!(m.rd32(SCRATCH), 0xaaaa_0023, "ds as a word");

    let cpu = g.call(5, &[]);
    assert_eq!(cpu.r[EAX], 0x5555_0203, "pushf: IF, the fixed bit and CF");
    assert_eq!(cpu.r[ECX], 1, "popf set CF");
    assert_eq!(cpu.r[EDX] & 0x4_0000, 0x4_0000, "popf left AC");
    assert_eq!(cpu.r[EDX] & 0xffff, 0x203);
    assert_eq!(cpu.r[EBX], 0, "stack balanced");
    assert_eq!(cpu.r[ESP], 0x0020_0000 - 0x100);

    for x in [1u32, 0x8000_0000, 0x0010_0100, 0xffff_ffff, 6] {
        let cpu = g.call(6, &[x]);
        assert_eq!(cpu.r[EAX], x.trailing_zeros(), "bsf {x:#x}");
        assert_eq!(cpu.r[ECX], 31 - x.leading_zeros(), "bsr {x:#x}");
        assert_eq!(cpu.r[EDX], 0);
    }
    let cpu = g.call(6, &[0]);
    assert_eq!((cpu.r[EAX], cpu.r[ECX], cpu.r[EDX]), (0x77, 0x77, 1), "zero source");
}
