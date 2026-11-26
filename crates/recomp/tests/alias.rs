//! A local written as a float, read back, then overwritten as an integer and read as a float
//! again (a game's bounding box at 0x4526a0 builds a square root's guess so): the second read
//! must see the integer store. With alias regions in the memory flags (tried 2026-11-26, no
//! measurable gain), a float load of guest memory tagged as the `Cpu` state's region let
//! Cranelift forward the first float store to it. Needs no game files.

mod common;

use common::{Guest, SCRATCH, Snippet};
use iced_x86::IcedError;
use iced_x86::code_asm::*;

/// (p): p[1] = p[0] through a float local, p[3] = the dword p[2] through the same local
fn reuse(a: &mut CodeAssembler) -> Result<(), IcedError> {
    a.push(ebp)?;
    a.mov(ebp, esp)?;
    a.sub(esp, 8)?;
    a.mov(edx, dword_ptr(ebp + 8))?;
    // a straight run with enough arithmetic for versions of its own (`lift::rz_runs`)
    a.fld(dword_ptr(edx))?;
    a.fadd(dword_ptr(edx + 16))?;
    a.fmul(dword_ptr(edx + 20))?;
    a.fstp(dword_ptr(ebp - 4))?;
    a.fld(dword_ptr(edx + 16))?;
    a.fmul(dword_ptr(edx + 20))?;
    a.fld(dword_ptr(ebp - 4))?;
    a.fmul(dword_ptr(ebp - 4))?;
    a.faddp(st1, st0)?;
    a.fstp(dword_ptr(ebp - 8))?;
    a.fld(dword_ptr(ebp - 4))?;
    a.fstp(dword_ptr(edx + 4))?;
    a.mov(eax, dword_ptr(ebp - 8))?;
    a.sar(eax, 1)?;
    a.mov(eax, dword_ptr(edx + 8))?;
    a.mov(dword_ptr(ebp - 4), eax)?;
    a.fld(dword_ptr(ebp - 4))?;
    a.fstp(dword_ptr(edx + 12))?;
    a.mov(esp, ebp)?;
    a.pop(ebp)?;
    a.ret()
}

const SNIPPETS: [(&str, Snippet); 1] = [("reuse", reuse)];

#[test]
fn integer_store_over_a_float_local() {
    let g = Guest::new(&SNIPPETS);
    let m = &g.mem;
    m.wr32(SCRATCH, 1.5f32.to_bits());
    m.wr32(SCRATCH + 8, 2.5f32.to_bits());
    m.wr32(SCRATCH + 16, 0f32.to_bits());
    m.wr32(SCRATCH + 20, 1f32.to_bits());
    g.call(0, &[SCRATCH]);
    assert_eq!(f32::from_bits(m.rd32(SCRATCH + 4)), 1.5);
    assert_eq!(f32::from_bits(m.rd32(SCRATCH + 12)), 2.5);
}
