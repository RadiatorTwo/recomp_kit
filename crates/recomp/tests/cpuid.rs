//! CPUID: MMX (edx bit 23) only when the game asks for it (`config::set_cpuid_mmx`), as only
//! movd/movq are lifted; the vendor string and features in the right registers. Its own
//! test binary: the setting is global. Needs no game files.

mod common;

use common::{Guest, SCRATCH, Snippet};
use recomp::rt::{EAX, EBX, ECX, EDX};
use iced_x86::IcedError;
use iced_x86::code_asm::*;

/// eax = family/model/stepping, edx = feature bits of leaf 1, ebx = ecx = 0
fn cpuid1(a: &mut CodeAssembler) -> Result<(), IcedError> {
    a.mov(eax, 1)?;
    a.cpuid()?;
    a.ret()
}

/// (p): leaf 0's vendor string as ebx edx ecx at p, eax = the highest leaf
fn vendor(a: &mut CodeAssembler) -> Result<(), IcedError> {
    a.mov(esi, dword_ptr(esp + 4))?;
    a.xor(eax, eax)?;
    a.cpuid()?;
    a.mov(dword_ptr(esi), ebx)?;
    a.mov(dword_ptr(esi + 4), edx)?;
    a.mov(dword_ptr(esi + 8), ecx)?;
    a.ret()
}

const SNIPPETS: [(&str, Snippet); 2] = [("cpuid1", cpuid1), ("vendor", vendor)];

#[test]
fn mmx_bit() {
    let g = Guest::new(&SNIPPETS);
    let cpu = g.call(1, &[SCRATCH]);
    assert_eq!(cpu.r[EAX], 1);
    assert_eq!(g.mem.slice(SCRATCH, 12), b"GenuineIntel");
    let cpu = g.call(0, &[]);
    assert_eq!(cpu.r[EAX], 0x543, "a Pentium");
    assert_eq!((cpu.r[EBX], cpu.r[ECX]), (0, 0));
    assert_eq!(cpu.r[EDX], 0x0000_8111, "FPU, TSC, CX8, CMOV, no MMX by default");
    recomp::config::set_cpuid_mmx(true);
    assert_eq!(g.call(0, &[]).r[EDX], 0x0080_8111, "with MMX");
    recomp::config::set_cpuid_mmx(false);
    assert_eq!(g.call(0, &[]).r[EDX], 0x0000_8111);
}
