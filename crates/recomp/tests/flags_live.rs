//! Flags are computed only where a later instruction reads them (`flag_liveness`): what must
//! still reach its reader. Needs no game files.

mod common;

use common::{Guest, Snippet, TEXT};
use iced_x86::IcedError;
use iced_x86::code_asm::*;

/// (a, b): flags of cmp a, b, read by the caller after the return
fn cmp_ret(a: &mut CodeAssembler) -> Result<(), IcedError> {
    a.mov(eax, dword_ptr(esp + 4))?;
    a.cmp(eax, dword_ptr(esp + 8))?;
    a.ret()
}

/// (a, b): eax = CF of cmp a, b as cmp_ret left it
fn after_call(a: &mut CodeAssembler) -> Result<(), IcedError> {
    a.push(dword_ptr(esp + 8))?;
    a.push(dword_ptr(esp + 8))?;
    a.call(TEXT as u64)?;
    a.mov(eax, 0)?;
    a.setb(al)?;
    a.add(esp, 8)?;
    a.ret()
}

/// (a, b, count): eax = CF after cmp a, b and a shift of 0 by `count` (0: cmp's CF)
fn shift_by(a: &mut CodeAssembler) -> Result<(), IcedError> {
    a.mov(eax, dword_ptr(esp + 4))?;
    a.mov(ecx, dword_ptr(esp + 12))?;
    a.xor(edx, edx)?;
    a.cmp(eax, dword_ptr(esp + 8))?;
    a.shl(edx, cl)?;
    a.mov(eax, 0)?;
    a.setb(al)?;
    a.ret()
}

/// (a, b): eax = CF of cmp a, b, kept by inc
fn inc_keeps_cf(a: &mut CodeAssembler) -> Result<(), IcedError> {
    a.mov(eax, dword_ptr(esp + 4))?;
    a.cmp(eax, dword_ptr(esp + 8))?;
    a.inc(edx)?;
    a.mov(eax, 0)?;
    a.adc(eax, 0)?;
    a.ret()
}

/// (x, n): eax = CF of the last of n additions of x to a sum, read after the loop (dec keeps
/// CF), ecx = the sum
fn loop_carry(a: &mut CodeAssembler) -> Result<(), IcedError> {
    let mut top = a.create_label();
    a.mov(edx, dword_ptr(esp + 4))?;
    a.mov(ecx, dword_ptr(esp + 8))?;
    a.xor(ebx, ebx)?;
    a.set_label(&mut top)?;
    a.add(ebx, edx)?;
    a.dec(ecx)?;
    a.jnz(top)?;
    a.mov(eax, 0)?;
    a.setb(al)?;
    a.mov(ecx, ebx)?;
    a.ret()
}

const SNIPPETS: [(&str, Snippet); 5] = [
    ("cmp_ret", cmp_ret),
    ("after_call", after_call),
    ("shift_by", shift_by),
    ("inc_keeps_cf", inc_keeps_cf),
    ("loop_carry", loop_carry),
];

#[test]
fn flags_reach_their_readers() {
    let g = Guest::new(&SNIPPETS);
    for (x, y) in [(1u32, 2u32), (2, 1), (5, 5)] {
        let cf = (x < y) as u32;
        assert_eq!(g.call(1, &[x, y]).r[0], cf, "across a return: {x} {y}");
        assert_eq!(g.call(2, &[x, y, 0]).r[0], cf, "shift by 0: {x} {y}");
        assert_eq!(g.call(2, &[x, y, 1]).r[0], 0, "shift by 1: {x} {y}");
        assert_eq!(g.call(3, &[x, y]).r[0], cf, "inc: {x} {y}");
    }
    // 3 x 0x60000000: the third addition carries
    let cpu = g.call(4, &[0x6000_0000, 3]);
    assert_eq!((cpu.r[0], cpu.r[1]), (1, 0x2000_0000));
    let cpu = g.call(4, &[0x6000_0000, 2]);
    assert_eq!((cpu.r[0], cpu.r[1]), (0, 0xc000_0000));
}
