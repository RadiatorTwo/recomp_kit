//! Finding functions without a disassembler's export: through calls, through addresses in
//! instruction operands and in data, and not in jump tables or data decoded as code. Needs no
//! game files.

use iced_x86::BlockEncoderOptions;
use iced_x86::code_asm::*;
use recomp::discover::{Code, Program, find_functions};
use recomp::image::{Image, Section};

const TEXT: u32 = 0x401000;
const DATA: u32 = 0x402000;

#[test]
fn finds_functions() {
    let mut a = CodeAssembler::new(32).unwrap();
    let (mut called, mut pushed, mut stored) = (a.create_label(), a.create_label(), a.create_label());
    // entry: calls one function, passes another's address (a callback)
    a.call(called).unwrap();
    a.push(0x7777_7777).unwrap(); // patched below: the callback's address
    a.ret().unwrap();
    a.int3().unwrap();
    a.set_label(&mut called).unwrap();
    a.mov(eax, 1).unwrap();
    a.ret().unwrap();
    a.int3().unwrap();
    a.set_label(&mut pushed).unwrap();
    a.mov(eax, 2).unwrap();
    a.ret().unwrap();
    a.int3().unwrap();
    a.set_label(&mut stored).unwrap();
    a.mov(eax, 3).unwrap();
    a.ret().unwrap();
    a.int3().unwrap();
    // never referenced
    a.mov(eax, 4).unwrap();
    a.ret().unwrap();
    let r = a.assemble_options(TEXT as u64, BlockEncoderOptions::RETURN_NEW_INSTRUCTION_OFFSETS).unwrap();
    let at = |l: &CodeLabel| r.label_ip(l).unwrap() as u32;
    let mut text = r.inner.code_buffer.clone();
    // `push imm32` is 68 xx xx xx xx, right after the 5-byte call
    text[6..10].copy_from_slice(&at(&pushed).to_le_bytes());
    text.resize(0x1000, 0xcc);
    // data: a pointer to a function, and one into the middle of an instruction (`mov eax, 3`
    // is 5 bytes; data that merely looks like an address)
    let mut data = Vec::new();
    data.extend(at(&stored).to_le_bytes());
    data.extend((at(&stored) + 2).to_le_bytes());
    data.resize(0x100, 0);
    let n = data.len() as u32;
    let img = Image {
        data: [text, data].concat(),
        base: 0x400000,
        entry: TEXT,
        sections: vec![
            Section { name: ".text".into(), va: TEXT, vsize: 0x1000, raw: 0, rsize: 0x1000, exec: true },
            Section { name: ".data".into(), va: DATA, vsize: n, raw: 0x1000, rsize: n, exec: false },
        ],
    };
    let mut code = Code::new(&img);
    let found: Vec<u32> = find_functions(&mut code, &[(TEXT, "entry".into())]).iter().map(|f| f.0).collect();
    assert_eq!(found, vec![TEXT, at(&called), at(&pushed), at(&stored)]);
}

/// A switch's jump table right after a call that does not return (`_exit`), as MSVC lays it
/// out: decoding must not run on into the table (whose bytes decode as `in`, which would make
/// the function, known only from a pointer in data, look like data). Gun Metal's 0x4d5a60.
#[test]
fn table_after_noreturn_call() {
    let f = TEXT + 0x10;
    let (case0, case1, exit) = (TEXT + 0xe5, TEXT + 0xf0, TEXT + 0x100);
    let mut text = vec![0xcc; 0x1000];
    text[0] = 0xc3; // the entry: ret
    let mut put = |at: u32, b: &[u8]| text[(at - TEXT) as usize..][..b.len()].copy_from_slice(b);
    put(f, &[0x83, 0xf8, 0x01, 0x77, 0x07]); // cmp eax,1; ja default
    put(f + 5, &[0xff, 0x24, 0x85]); // jmp [eax*4 + table]
    put(f + 8, &(TEXT + 0x24).to_le_bytes());
    put(f + 12, &[0x6a, 0x00, 0xe8]); // default: push 0; call exit
    put(f + 15, &(exit - (f + 19)).to_le_bytes());
    put(f + 19, &[0x90]);
    put(TEXT + 0x24, &[case0.to_le_bytes(), case1.to_le_bytes()].concat()); // e5 .. = in eax,..
    put(case0, &[0xb8, 1, 0, 0, 0, 0xc3]);
    put(case1, &[0xb8, 2, 0, 0, 0, 0xc3]);
    put(exit, &[0xeb, 0xfe]);
    let mut data = f.to_le_bytes().to_vec();
    data.resize(0x100, 0);
    let n = data.len() as u32;
    let img = Image {
        data: [text, data].concat(),
        base: 0x400000,
        entry: TEXT,
        sections: vec![
            Section { name: ".text".into(), va: TEXT, vsize: 0x1000, raw: 0, rsize: 0x1000, exec: true },
            Section { name: ".data".into(), va: DATA, vsize: n, raw: 0x1000, rsize: n, exec: false },
        ],
    };
    let mut code = Code::new(&img);
    let found: Vec<u32> = find_functions(&mut code, &[(TEXT, "entry".into())]).iter().map(|f| f.0).collect();
    assert_eq!(found, vec![TEXT, f, exit]);
}

/// A code address found as an operand of a candidate just decoded comes before the next
/// candidate from data: else a word of data pointing into that function's middle would come
/// first and split it.
#[test]
fn operands_before_data() {
    let (a, b) = (TEXT + 0x10, TEXT + 0x20);
    let mut text = vec![0xcc; 0x1000];
    text[0] = 0xc3; // the entry: ret
    let mut put = |at: u32, x: &[u8]| text[(at - TEXT) as usize..][..x.len()].copy_from_slice(x);
    put(a, &[0x68]); // push b; pop eax; ret
    put(a + 1, &b.to_le_bytes());
    put(a + 5, &[0x58, 0xc3]);
    put(b, &[0xb8, 1, 0, 0, 0, 0xb9, 2, 0, 0, 0, 0xc3]); // mov eax, 1; mov ecx, 2; ret
    // data: a pointer to a, and one to b's second instruction
    let mut data = [a.to_le_bytes(), (b + 5).to_le_bytes()].concat();
    data.resize(0x100, 0);
    let n = data.len() as u32;
    let img = Image {
        data: [text, data].concat(),
        base: 0x400000,
        entry: TEXT,
        sections: vec![
            Section { name: ".text".into(), va: TEXT, vsize: 0x1000, raw: 0, rsize: 0x1000, exec: true },
            Section { name: ".data".into(), va: DATA, vsize: n, raw: 0x1000, rsize: n, exec: false },
        ],
    };
    let mut code = Code::new(&img);
    let found: Vec<u32> = find_functions(&mut code, &[(TEXT, "entry".into())]).iter().map(|f| f.0).collect();
    assert_eq!(found, vec![TEXT, a, b]);
}

/// `Program::build` with known boundaries: decoding stops at a jump table behind a call that
/// does not return, and the table's bytes are noted.
#[test]
fn build_stops_at_table() {
    let f = TEXT + 0x10;
    let (case0, case1, exit) = (TEXT + 0xe5, TEXT + 0xf0, TEXT + 0x100);
    let mut text = vec![0xcc; 0x1000];
    let mut put = |at: u32, b: &[u8]| text[(at - TEXT) as usize..][..b.len()].copy_from_slice(b);
    put(f, &[0x83, 0xf8, 0x01, 0x77, 0x07]); // cmp eax,1; ja default
    put(f + 5, &[0xff, 0x24, 0x85]); // jmp [eax*4 + table]
    put(f + 8, &(TEXT + 0x24).to_le_bytes());
    put(f + 12, &[0x6a, 0x00, 0xe8]); // default: push 0; call exit
    put(f + 15, &(exit - (f + 19)).to_le_bytes());
    put(f + 19, &[0x90]);
    put(TEXT + 0x24, &[case0.to_le_bytes(), case1.to_le_bytes()].concat());
    put(case0, &[0xb8, 1, 0, 0, 0, 0xc3]);
    put(case1, &[0xb8, 2, 0, 0, 0, 0xc3]);
    put(exit, &[0xeb, 0xfe]);
    let img = Image {
        data: text,
        base: 0x400000,
        entry: f,
        sections: vec![Section { name: ".text".into(), va: TEXT, vsize: 0x1000, raw: 0, rsize: 0x1000, exec: true }],
    };
    let mut code = Code::new(&img);
    let prog = Program::build(&mut code, &[(f, exit, "f".into()), (exit, exit + 2, "exit".into())]);
    let func = &prog.fns[&f];
    assert_eq!(func.table_bytes, vec![(TEXT + 0x24, TEXT + 0x2c)]);
    assert!(func.ins.range(TEXT + 0x24..TEXT + 0x2c).next().is_none(), "decoded into the table");
    assert!(func.ins.contains(&case0) && func.ins.contains(&case1) && func.ins.contains(&(f + 19)));
}
