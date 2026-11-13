//! Finding the code: every function's instructions (recursive descent from its entry,
//! following branches and switch tables), plus extra entry points for every place other
//! code jumps or calls into the middle of, and for code addresses stored in data.

use crate::image::Image;
use iced_x86::{Decoder, DecoderOptions, FlowControl, Instruction, Mnemonic, OpKind, Register};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

/// Decoding depends only on the address, so every instruction is decoded once.
pub struct Code<'a> {
    pub img: &'a Image,
    cache: HashMap<u32, Option<Instruction>>,
}

impl<'a> Code<'a> {
    pub fn new(img: &'a Image) -> Self {
        Code { img, cache: HashMap::new() }
    }

    pub fn at(&mut self, pc: u32) -> Option<Instruction> {
        let img = self.img;
        *self.cache.entry(pc).or_insert_with(|| {
            let bytes = img.read(pc, 16)?;
            let i = Decoder::with_ip(32, &bytes, pc as u64, DecoderOptions::NONE).decode();
            (!i.is_invalid()).then_some(i)
        })
    }
}

/// A direct branch or call target (jmp/jcc/loop/jecxz/call rel).
pub fn direct_target(i: &Instruction) -> Option<u32> {
    matches!(i.op0_kind(), OpKind::NearBranch32 | OpKind::NearBranch16).then(|| i.near_branch_target() as u32)
}

pub struct Func {
    pub entry: u32,
    pub lo: u32,
    pub hi: u32,
    pub name: String,
    pub ins: BTreeSet<u32>,
    /// jmp [index*4 + table]: the instruction's address -> its targets
    pub tables: HashMap<u32, Vec<u32>>,
    /// the bytes of those tables (start, end): MSVC puts them right after the function's
    /// code, often behind a call that does not return (`_exit`), which decoding must not
    /// run on into
    pub table_bytes: Vec<(u32, u32)>,
    pub targets: HashSet<u32>,
    pub calls: HashSet<u32>,
}

impl Func {
    pub fn inside(&self, a: u32) -> bool {
        self.lo <= a && a < self.hi
    }

    /// Whether `a` is one of the function's jump tables.
    pub fn in_table(&self, a: u32) -> bool {
        self.table_bytes.iter().any(|&(lo, hi)| lo <= a && a < hi)
    }

    fn new(code: &mut Code, entry: u32, lo: u32, hi: u32, name: String) -> Func {
        let mut f = Func {
            entry,
            lo,
            hi,
            name,
            ins: BTreeSet::new(),
            tables: HashMap::new(),
            table_bytes: Vec::new(),
            targets: HashSet::new(),
            calls: HashSet::new(),
        };
        f.decode(code);
        f
    }

    fn decode(&mut self, code: &mut Code) {
        let mut todo = vec![self.entry];
        while let Some(mut pc) = todo.pop() {
            if !self.inside(pc) {
                continue;
            }
            while !self.ins.contains(&pc) && !self.in_table(pc) {
                if !self.inside(pc) {
                    // runs on past the end: needs an entry there
                    self.targets.insert(pc);
                    break;
                }
                let Some(i) = code.at(pc) else { break };
                self.ins.insert(pc);
                let fc = i.flow_control();
                match fc {
                    FlowControl::ConditionalBranch | FlowControl::UnconditionalBranch if direct_target(&i).is_some() => {
                        let t = direct_target(&i).unwrap();
                        self.targets.insert(t);
                        if self.inside(t) {
                            todo.push(t);
                        }
                        if fc == FlowControl::UnconditionalBranch {
                            break;
                        }
                    }
                    FlowControl::IndirectBranch | FlowControl::UnconditionalBranch => {
                        // jmp [index*4 + table], or Watcom's jmp cs:[reg + table] (reg = index*4)
                        let scaled = i.memory_index() != Register::None && i.memory_index_scale() == 4 && i.memory_base() == Register::None;
                        let based = i.memory_index() == Register::None
                            && i.memory_base() != Register::None
                            && i.memory_base().size() == 4
                            && code.img.is_code(i.memory_displacement32());
                        if i.op0_kind() == OpKind::Memory && (scaled || based) {
                            // "neg ecx; jmp [ecx*4 + T]" (memcpy's tail copy) only reads entries before T
                            let neg = scaled
                                && self.ins.contains(&(pc.wrapping_sub(2)))
                                && code.at(pc - 2).is_some_and(|p| {
                                    p.mnemonic() == Mnemonic::Neg
                                        && p.op0_kind() == OpKind::Register
                                        && p.op0_register() == i.memory_index()
                                });
                            let (tbl, span) = self.table(code, i.memory_displacement32(), if neg { 8 } else { 4 });
                            self.table_bytes.push(span);
                            for &t in &tbl {
                                self.targets.insert(t);
                                if self.inside(t) {
                                    todo.push(t);
                                }
                            }
                            self.tables.insert(pc, tbl);
                        }
                        break;
                    }
                    _ => match i.mnemonic() {
                        Mnemonic::Ret | Mnemonic::Retf | Mnemonic::Iretd | Mnemonic::Int3 | Mnemonic::Hlt => break,
                        Mnemonic::Call => {
                            if let Some(t) = direct_target(&i) {
                                self.calls.insert(t);
                            }
                        }
                        _ => {}
                    },
                }
                pc += i.len() as u32;
            }
        }
    }

    /// Jump table entries; MSVC's memcpy indexes some tables with negative values, so valid
    /// entries just before the table count too. Also gives the bytes the table occupies.
    fn table(&self, code: &Code, at: u32, back: u32) -> (Vec<u32>, (u32, u32)) {
        let mut out = Vec::new();
        for k in 1..=back {
            let t = code.img.u32(at - 4 * k);
            if !self.inside(t) || self.ins.contains(&(at - 4 * k)) {
                break;
            }
            out.push(t);
        }
        // tables sometimes point into a neighbouring "function" (gap-filling splits):
        // accept any code address, foreign ones become extra entry points
        let mut fwd: Vec<u32> = Vec::new();
        loop {
            let slot = at + 4 * fwd.len() as u32;
            let mut t = code.img.u32(slot);
            if fwd.len() > 512 || (!fwd.is_empty() && self.ins.contains(&slot)) {
                break;
            }
            if !code.img.is_code(t) {
                if !fwd.is_empty() {
                    break; // an unused leading slot may hold anything
                }
                t = 0;
            }
            fwd.push(t);
        }
        let span = (at - 4 * out.len() as u32, at + 4 * fwd.len() as u32);
        out.extend(fwd.into_iter().filter(|&x| x != 0));
        (out, span)
    }
}

pub struct Program {
    /// entry -> function
    pub fns: BTreeMap<u32, Func>,
}

impl Program {
    pub fn build(code: &mut Code, ranges: &[(u32, u32, String)]) -> Program {
        let mut fns: BTreeMap<u32, Func> = BTreeMap::new();
        for (lo, hi, name) in ranges {
            fns.insert(*lo, Func::new(code, *lo, *lo, *hi, name.clone()));
        }
        // code addresses stored in data (function pointer and jump target tables the
        // self-modifying renderer uses) must be enterable too
        let mut insn_at = HashSet::new();
        let mut inside = HashSet::new(); // bytes strictly inside a decoded instruction
        for f in fns.values() {
            for &a in &f.ins {
                insn_at.insert(a);
                let len = code.at(a).unwrap().len() as u32;
                inside.extend(a + 1..a + len);
            }
        }
        let mut data_ptrs = HashSet::new();
        for s in &code.img.sections {
            if !matches!(s.name.as_str(), ".rdata" | ".data" | "UVA_DATA") {
                continue;
            }
            let blob = code.img.read(s.va, s.rsize as usize).unwrap_or_default();
            for k in (0..blob.len().saturating_sub(3)).step_by(4) {
                let t = u32::from_le_bytes([blob[k], blob[k + 1], blob[k + 2], blob[k + 3]]);
                let plausible = insn_at.contains(&t)
                    || (code.img.is_code(t) && !inside.contains(&t) && !matches!(code.img.byte(t), 0x00 | 0x90 | 0xCC));
                if plausible && !fns.contains_key(&t) {
                    data_ptrs.insert(t);
                }
            }
        }
        // add entries until nothing new turns up
        let los: Vec<u32> = ranges.iter().map(|r| r.0).collect();
        let mut todo: Vec<Vec<u32>> =
            fns.values().map(|f| f.targets.iter().chain(&f.calls).copied().filter(|&t| !(f.inside(t) && f.ins.contains(&t))).collect()).collect();
        todo.push(data_ptrs.into_iter().collect());
        while !todo.is_empty() {
            let mut next = Vec::new();
            let mut wanted: Vec<u32> = todo.into_iter().flatten().collect();
            wanted.sort_unstable();
            wanted.dedup();
            for t in wanted {
                if fns.contains_key(&t) || !code.img.is_code(t) {
                    continue;
                }
                let k = los.partition_point(|&lo| lo <= t);
                if k == 0 {
                    continue;
                }
                let (lo, hi, name) = &ranges[k - 1];
                let e = if t < *hi {
                    Func::new(code, t, *lo, *hi, format!("{name}+0x{:x}", t - lo))
                } else {
                    // a function functions.json does not know (one only another version has):
                    // up to the next one
                    let end = code.img.code_section(t).map_or(t + 1, |s| s.va + s.vsize.max(s.rsize));
                    let next = los.get(k).copied().unwrap_or(end).min(end);
                    Func::new(code, t, t, next, format!("FUN_{t:08x}"))
                };
                next.push(e.targets.iter().chain(&e.calls).copied().filter(|&x| !(e.inside(x) && e.ins.contains(&x))).collect());
                fns.insert(t, e);
            }
            todo = next;
        }
        Program { fns }
    }
}

/// The dwords of a section's raw data, aligned.
fn words(img: &Image, s: &crate::image::Section) -> Vec<u32> {
    let blob = img.read(s.va, s.rsize as usize).unwrap_or_default();
    blob.chunks_exact(4).map(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]])).collect()
}

/// Function boundaries without a disassembler's export: every function entry reachable from
/// `seeds` (the entry point, a DLL's exports) through calls, plus code addresses stored in data
/// that no decoded code covers yet (callbacks, tables of functions). Each function runs to the
/// next entry; sorted (start, end, name).
pub fn find_functions(code: &mut Code, seeds: &[(u32, String)]) -> Vec<(u32, u32, String)> {
    let img = code.img;
    let section_end = |a: u32| img.code_section(a).map(|s| s.va + s.vsize.max(s.rsize)).unwrap();
    let mut names: HashMap<u32, String> = seeds.iter().filter(|s| img.is_code(s.0)).cloned().collect();
    let mut entries: BTreeSet<u32> = names.keys().copied().collect();
    // decoded instruction starts, and bytes strictly inside a decoded instruction
    let mut seen: (HashSet<u32>, HashSet<u32>) = Default::default();
    // returns the instructions it decoded first
    let decode = |code: &mut Code, entries: &mut BTreeSet<u32>, seen: &mut (HashSet<u32>, HashSet<u32>), mut todo: Vec<u32>| {
        let mut new = Vec::new();
        while let Some(e) = todo.pop() {
            let s = img.code_section(e).unwrap();
            let f = Func::new(code, e, s.va, s.va + s.vsize.max(s.rsize), String::new());
            for &a in &f.ins {
                if seen.0.insert(a) {
                    seen.1.extend(a + 1..a + code.at(a).unwrap().len() as u32);
                    new.push(a);
                }
            }
            // jump tables are data: no candidate may start or run inside one
            for &(lo, hi) in &f.table_bytes {
                seen.1.extend((lo..hi).filter(|b| !seen.0.contains(b)));
            }
            for &t in &f.calls {
                if img.is_code(t) && entries.insert(t) {
                    todo.push(t);
                }
            }
        }
        new
    };
    decode(code, &mut entries, &mut seen, names.keys().copied().collect());
    // code addresses that nothing decoded reaches: functions only called through pointers,
    // stored in data (also in the code sections, where the linker put some tables of
    // functions: words no decoded instruction covers) or as an instruction's operand (window
    // procedures, callbacks). One at a time, so one found inside another's code does not
    // split it.
    let data: Vec<u32> = img.sections.iter().filter(|s| !s.exec).flat_map(|s| words(img, s)).collect();
    let mut tried = HashSet::new();
    loop {
        let covered = |a: u32| (a..a + 4).any(|b| seen.0.contains(&b) || seen.1.contains(&b));
        let in_code: Vec<u32> = img
            .sections
            .iter()
            .filter(|s| s.exec)
            .flat_map(|s| (s.va..s.va + s.vsize.min(s.rsize)).step_by(4))
            .filter(|&a| !covered(a))
            .map(|a| img.u32(a))
            .collect();
        let operands = seen.0.iter().filter_map(|&a| {
            let i = code.at(a).unwrap();
            (i.op_kinds().any(|k| k == OpKind::Immediate32)).then(|| i.immediate32())
        });
        let mut wanted: Vec<u32> = data.iter().copied().chain(in_code).chain(operands.collect::<Vec<_>>()).filter(|&t| img.is_code(t) && tried.insert(t)).collect();
        if wanted.is_empty() {
            break;
        }
        wanted.sort_unstable();
        for t in wanted {
            // the code addresses in the operands of what a candidate's code decoded are taken
            // right after it, before the next candidate: a word of data that happens to point
            // into a function must not come first (an atexit callback pushed by the code just
            // found, and data pointing at its third instruction)
            let mut next = vec![t];
            while let Some(t) = next.pop() {
                if !seen.0.contains(&t)
                    && !seen.1.contains(&t)
                    && !matches!(img.byte(t), 0x00 | 0x90 | 0xCC)
                    && plausible(code, t, &seen)
                    && entries.insert(t)
                {
                    tried.insert(t);
                    let new = decode(code, &mut entries, &mut seen, vec![t]);
                    for a in new {
                        let i = code.at(a).unwrap();
                        if i.op_kinds().any(|k| k == OpKind::Immediate32) && img.is_code(i.immediate32()) && tried.insert(i.immediate32()) {
                            next.push(i.immediate32());
                        }
                    }
                }
            }
        }
    }
    let list: Vec<u32> = entries.into_iter().collect();
    list.iter()
        .enumerate()
        .map(|(k, &lo)| {
            let end = section_end(lo);
            let hi = list.get(k + 1).copied().filter(|&n| n < end).unwrap_or(end);
            (lo, hi, names.remove(&lo).unwrap_or_else(|| format!("sub_{lo:x}")))
        })
        .collect()
}

/// Whether `t`, found only as a number, looks like the start of a function: its code decodes
/// without running into the middle of known instructions (`seen`: their starts, and the
/// bytes inside them; known code it jumps to is fine) and without instructions compiled
/// 32-bit code does not use (what data decoded as code is full of).
fn plausible(code: &mut Code, t: u32, seen: &(HashSet<u32>, HashSet<u32>)) -> bool {
    use Mnemonic::*;
    let img = code.img;
    let Some(s) = img.sections.iter().find(|s| s.exec && t >= s.va && t < s.va + s.vsize) else { return false };
    let f = Func::new(code, t, s.va, s.va + s.vsize, String::new());
    f.ins.iter().filter(|&&a| !seen.0.contains(&a) && !f.in_table(a)).all(|&a| {
        let i = code.at(a).unwrap();
        let odd = matches!(
            i.mnemonic(),
            Aaa | Aad | Aam | Aas | Daa | Das | Arpl | Bound | Lds | Les | Lfs | Lgs | Lss | Into | Int1 | Iretd | Retf | Salc
                | Insb | Insd | Insw | Outsb | Outsd | Outsw | In | Out | Hlt | Cli | Sti | Enter
        ) || i.op_kinds().any(|k| matches!(k, OpKind::FarBranch16 | OpKind::FarBranch32))
            || (i.memory_base() != Register::None && i.memory_base().size() == 2)
            || !matches!(i.segment_prefix(), Register::None | Register::FS);
        !odd && !(a..a + i.len() as u32).any(|b| seen.0.contains(&b) && b != a || seen.1.contains(&b))
    }) && !f.ins.is_empty()
}
