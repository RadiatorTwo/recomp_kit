//! Loops shaped like Motorhead's (Watcom), recompiled with the x87 cache (`lift::X87_CACHE`)
//! and without, give the same memory and x87 registers; they compile at all (Cranelift's
//! verifier: a cached value used where it was not made). Needs no game files.

mod common;

use common::{Guest, Rng, SCRATCH, Snippet};
use iced_x86::IcedError;
use iced_x86::code_asm::*;

/// Motorhead's 0x418131 loop: values left popped before it, a branch over a long chain (a
/// run with versions of its own) to an integer loop, the loop's end jumping back.
fn skip_chain(a: &mut CodeAssembler) -> Result<(), IcedError> {
    a.mov(eax, dword_ptr(esp + 4))?;
    a.push(ebx)?;
    a.fild(qword_ptr(eax))?;
    a.fstp(dword_ptr(eax + 256))?;
    a.fld(dword_ptr(eax + 8))?;
    a.fld(dword_ptr(eax + 12))?;
    a.fmul_2(st0, st1)?;
    a.fstp(dword_ptr(eax + 260))?;
    a.fstp(dword_ptr(eax + 264))?;
    a.mov(ecx, 5u32)?;
    a.lea(ebx, dword_ptr(eax + 300))?;
    let (mut head, mut skip, mut inner) = (a.create_label(), a.create_label(), a.create_label());
    a.set_label(&mut head)?;
    a.test(dword_ptr(eax + 48), ecx)?;
    a.jnz(skip)?;
    a.fld(dword_ptr(eax + 16))?;
    a.fabs()?;
    a.fld(qword_ptr(eax + 24))?;
    a.fxch(st0, st1)?;
    a.fmul_2(st0, st1)?;
    a.fld1()?;
    a.fsubrp(st1, st0)?;
    a.fmul(dword_ptr(eax + 32))?;
    a.fstp(dword_ptr(ebx))?;
    a.fld(dword_ptr(eax + 16))?;
    a.fabs()?;
    a.fmul_2(st0, st1)?;
    a.fld1()?;
    a.fsubrp(st1, st0)?;
    a.fdivr(dword_ptr(eax + 36))?;
    a.fstp(dword_ptr(ebx + 4))?;
    a.fld(dword_ptr(eax + 16))?;
    a.fabs()?;
    a.fmulp(st1, st0)?;
    a.fld(dword_ptr(eax + 40))?;
    a.fld1()?;
    a.fsubrp(st2, st0)?;
    a.fmulp(st1, st0)?;
    a.fstp(dword_ptr(ebx + 8))?;
    a.set_label(&mut skip)?;
    a.add(ebx, 12u32)?;
    a.mov(edx, 3u32)?;
    a.set_label(&mut inner)?;
    a.dec(edx)?;
    a.jnz(inner)?;
    a.dec(ecx)?;
    a.jnz(head)?;
    a.pop(ebx)?;
    a.ret()
}

/// The 0x418222 loop: a sum of squares, its root stored and compared (fcomp, fnstsw, sahf)
/// with the smallest so far, which a branch keeps.
fn nearest(a: &mut CodeAssembler) -> Result<(), IcedError> {
    a.mov(eax, dword_ptr(esp + 4))?;
    a.mov(ecx, 6u32)?;
    a.mov(edx, eax)?;
    let (mut top, mut more) = (a.create_label(), a.create_label());
    a.set_label(&mut top)?;
    a.fld(dword_ptr(edx + 4))?;
    a.fsub(dword_ptr(eax + 128))?;
    a.fmul_2(st0, st0)?;
    a.fld(dword_ptr(edx))?;
    a.fsub(dword_ptr(eax + 132))?;
    a.fmul_2(st0, st0)?;
    a.faddp(st1, st0)?;
    a.fsqrt()?;
    a.fst(dword_ptr(eax + 256))?;
    a.fcomp(dword_ptr(eax + 260))?;
    a.fnstsw(ax)?;
    a.sahf()?;
    a.jae(more)?;
    a.mov(eax, dword_ptr(esp + 4))?;
    a.mov(edx, dword_ptr(eax + 256))?;
    a.mov(dword_ptr(eax + 260), edx)?;
    a.mov(edx, eax)?;
    a.set_label(&mut more)?;
    a.mov(eax, dword_ptr(esp + 4))?;
    a.add(edx, 8u32)?;
    a.dec(ecx)?;
    a.jnz(top)?;
    a.ret()
}

/// Watcom's overlapping instructions (0x4c8f33): `test ax, 0xcf89` hides `mov edi, ecx`,
/// a branch's target; both fall through to the same instruction, with an x87 value cached.
fn overlap(a: &mut CodeAssembler) -> Result<(), IcedError> {
    a.mov(eax, dword_ptr(esp + 4))?;
    a.push(edi)?;
    a.mov(ecx, dword_ptr(eax + 48))?;
    a.fld(dword_ptr(eax))?;
    a.fmul(dword_ptr(eax + 4))?;
    let mut inside = a.create_label();
    a.test(ecx, 1u32)?;
    a.jnz(inside)?;
    a.fld(dword_ptr(eax + 8))?;
    a.faddp(st1, st0)?;
    a.db(&[0x66, 0xa9])?;
    a.set_label(&mut inside)?;
    a.db(&[0x89, 0xcf])?;
    a.fstp(dword_ptr(eax + 256))?;
    a.mov(dword_ptr(eax + 260), edi)?;
    a.pop(edi)?;
    a.ret()
}

const SNIPPETS: [(&str, Snippet); 3] = [
    ("skip_chain", skip_chain),
    ("nearest", nearest),
    ("overlap", overlap),
];

fn run(k: usize, seed: u64, cached: bool) -> (Vec<u8>, Vec<u64>) {
    recomp::lift::X87_CACHE.store(cached, std::sync::atomic::Ordering::Relaxed);
    let g = Guest::new(&SNIPPETS);
    let m = &g.mem;
    let mut rng = Rng(seed);
    for j in 0..64 {
        let f = (rng.f64() as f32).to_bits();
        m.wr32(SCRATCH + 4 * j, f);
    }
    m.wr32(SCRATCH + 48, (rng.next() & 3) as u32);
    m.wr32(SCRATCH + 260, f32::MAX.to_bits());
    for j in 64..256 {
        m.wr32(SCRATCH + 4 * j, 0);
    }
    let cpu = g.call(k, &[SCRATCH]);
    (
        m.slice(SCRATCH, 1024).to_vec(),
        cpu.st.iter().map(|v| v.to_bits()).collect(),
    )
}

#[test]
fn x87_loops() {
    let mut rng = Rng(0x5851_f42d_4c95_7f2d);
    for n in 0..200 {
        let seed = rng.next();
        for k in 0..SNIPPETS.len() {
            let a = run(k, seed, false);
            let b = run(k, seed, true);
            assert!(a.0 == b.0, "snippet {k}, data {n}: memory differs");
            assert!(a.1 == b.1, "snippet {k}, data {n}: x87 registers differ");
        }
    }
}
