//! rep movs and rep stos (`rt_string`, with its bulk copy and fill) against a reference that
//! moves element by element: sizes 1, 2 and 4, forward and backward (std), and copies whose
//! destination overlaps the source from either side (a forward copy onto a later part of its
//! own source repeats a pattern). Needs no game files.

mod common;

use common::{Guest, Rng, SCRATCH, Snippet};
use iced_x86::IcedError;
use iced_x86::code_asm::*;
use recomp::rt::{ECX, EDI, ESI};

/// (src, dst, count, value, backward): one rep instruction of each kind and size, picked by
/// the snippet's index; esi, edi, ecx left as the instruction leaves them.
fn op(kind: usize) -> Snippet {
    const OPS: [Snippet; 6] = [movsb_, movsw_, movsd_, stosb_, stosw_, stosd_];
    OPS[kind]
}

fn args(a: &mut CodeAssembler) -> Result<(), IcedError> {
    a.mov(esi, dword_ptr(esp + 4))?;
    a.mov(edi, dword_ptr(esp + 8))?;
    a.mov(ecx, dword_ptr(esp + 12))?;
    a.mov(eax, dword_ptr(esp + 16))?;
    let mut fwd = a.create_label();
    a.cmp(dword_ptr(esp + 20), 0)?;
    a.je(fwd)?;
    a.std()?;
    a.set_label(&mut fwd)?;
    Ok(())
}

macro_rules! snippet {
    ($name:ident, $ins:ident) => {
        fn $name(a: &mut CodeAssembler) -> Result<(), IcedError> {
            args(a)?;
            a.rep().$ins()?;
            a.cld()?;
            a.ret()
        }
    };
}
snippet!(movsb_, movsb);
snippet!(movsw_, movsw);
snippet!(movsd_, movsd);
snippet!(stosb_, stosb);
snippet!(stosw_, stosw);
snippet!(stosd_, stosd);

/// What the instruction does, element by element: the memory and esi, edi after it.
fn reference(
    mem: &mut [u8],
    kind: usize,
    (mut src, mut dst, count, value, back): (u32, u32, u32, u32, bool),
) -> (u32, u32) {
    let size = [1u32, 2, 4][kind % 3];
    let step = if back { size.wrapping_neg() } else { size };
    for _ in 0..count {
        let v: Vec<u8> = if kind < 3 {
            mem[src as usize..][..size as usize].to_vec()
        } else {
            value.to_le_bytes()[..size as usize].to_vec()
        };
        mem[dst as usize..][..size as usize].copy_from_slice(&v);
        if kind < 3 {
            src = src.wrapping_add(step);
        }
        dst = dst.wrapping_add(step);
    }
    (src, dst)
}

#[test]
fn rep_movs_stos() {
    let snippets: Vec<(&str, Snippet)> = (0..6).map(|k| ("op", op(k))).collect();
    let g = Guest::new(&snippets);
    let m = &g.mem;
    let mut rng = Rng(0x5eed_0f57_1234_abcd);
    const LEN: u32 = 2048;
    for n in 0..3000 {
        let kind = (rng.next() % 6) as usize;
        let size = [1u32, 2, 4][kind % 3];
        let back = rng.next() % 4 == 0;
        let count = (rng.next() % 64) as u32;
        let span = count * size;
        // source and destination in the block, often overlapping
        let base = 256 + (rng.next() % 256) as u32;
        let shift = (rng.next() % (2 * span as u64 + 9)) as i64 - span as i64 - 4;
        let (src, dst) = if back {
            // moving down from the last element
            let s = base + 2 * span + 64;
            (s, (s as i64 + shift) as u32)
        } else {
            (base, (base as i64 + shift) as u32)
        };
        let value = rng.next() as u32;
        let mut before = vec![0u8; LEN as usize];
        for b in before.iter_mut() {
            *b = rng.next() as u8;
        }
        m.slice(SCRATCH, LEN as usize).copy_from_slice(&before);
        let c = g.call(
            kind,
            &[SCRATCH + src, SCRATCH + dst, count, value, back as u32],
        );
        let mut want = before.clone();
        let (ws, wd) = reference(&mut want, kind, (src, dst, count, value, back));
        let got = m.slice(SCRATCH, LEN as usize).to_vec();
        assert!(
            got == want,
            "case {n}: kind {kind} back {back} count {count} src {src} dst {dst}: memory"
        );
        assert_eq!(c.r[ECX], 0, "case {n}: ecx");
        assert_eq!(c.r[EDI], SCRATCH.wrapping_add(wd), "case {n}: edi");
        if kind < 3 {
            assert_eq!(c.r[ESI], SCRATCH.wrapping_add(ws), "case {n}: esi");
        }
    }
}
