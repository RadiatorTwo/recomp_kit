//! The x87 environment: fnstenv/fldenv and fnsave/frstor keep the control word, TOP and
//! the registers across an fninit (the C runtime's maths error paths). Needs no game files.

mod common;

use common::{Guest, SCRATCH, Snippet, dbl};
use iced_x86::IcedError;
use iced_x86::code_asm::*;

/// (x, y, buf): push x, y; fnsave; clobber; frstor; [buf+108] = x - y
fn save(a: &mut CodeAssembler) -> Result<(), IcedError> {
    a.mov(eax, dword_ptr(esp + 20))?;
    a.fld(qword_ptr(esp + 4))?;
    a.fld(qword_ptr(esp + 12))?;
    a.fnsave(ptr(eax))?;
    a.fld1()?;
    a.fldz()?;
    a.frstor(ptr(eax))?;
    a.fsubp(st1, st0)?;
    a.fstp(qword_ptr(eax + 108))?;
    a.ret()
}

/// (cw, buf): fldcw cw; fnstenv; fninit; fldenv; [buf+28] = fnstcw
fn env(a: &mut CodeAssembler) -> Result<(), IcedError> {
    a.mov(eax, dword_ptr(esp + 8))?;
    a.fldcw(word_ptr(esp + 4))?;
    a.fnstenv(ptr(eax))?;
    a.fninit()?;
    a.fldenv(ptr(eax))?;
    a.fnstcw(word_ptr(eax + 28))?;
    a.ret()
}

const SNIPPETS: [(&str, Snippet); 2] = [("save", save), ("env", env)];

#[test]
fn x87_environment() {
    let g = Guest::new(&SNIPPETS);
    let m = &g.mem;
    let (x, y) = (3.25, -1.5);
    let cpu = g.call(0, &[dbl(x)[0], dbl(x)[1], dbl(y)[0], dbl(y)[1], SCRATCH]);
    let got = f64::from_bits(u64::from_le_bytes(m.slice(SCRATCH + 108, 8).try_into().unwrap()));
    assert_eq!(got, x - y, "registers kept by fnsave/frstor");
    assert_eq!(cpu.top, 0, "stack empty again");

    let cpu = g.call(1, &[0x0e7f, SCRATCH]);
    assert_eq!(u16::from_le_bytes([m.rd8(SCRATCH + 28), m.rd8(SCRATCH + 29)]), 0x0e7f, "control word kept by fnstenv/fldenv");
    assert_eq!(cpu.fcw, 0x0e7f);
}
