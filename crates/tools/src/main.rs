//! Tools for porting a game, all on the player's own EXE (nothing of it is written into the
//! repository but addresses):
//!
//! ```text
//! rtools imports  <exe>                     the imported functions, by DLL
//! rtools missing  <exe> [options]           instructions the lifter does not support yet
//! rtools dis      <exe> <addr|all> [opt]    a function as discovery sees it
//! rtools at       <exe> <addr>...           the instruction at each address
//! rtools smc      <exe> [options]           self-modifying code: fields and rewritten code
//! rtools dialogs  <exe>                     dialog templates and their controls
//! rtools iso      <image>                   the files of a CD image (.iso, .bin)
//! rtools movie    <file.mpg> [out dir]      decode an MPEG-1 movie (frames.rgb, audio.raw)
//! rtools vermap   <reference exe> <other exe> <functions.json>   address map between versions
//!
//! options: --functions <functions.json>     function list (a Ghidra export; default: found)
//!          --symbols <symbols.txt>          names for addresses
//!          --patch <address>:<variants>     an SMC patch table (smc)
//! ```
//!
//! Addresses are hexadecimal.

mod vermap;

use recomp::discover::{Code, Program};
use recomp::image::{Image, named, symbols};
use recomp::smc::{PatchTable, Smc, SmcSpec};
use std::path::Path;

fn hex(s: &str) -> u32 {
    u32::from_str_radix(s.trim_start_matches("0x"), 16).unwrap_or_else(|_| panic!("not a hex address: {s}"))
}

/// The value of option `--name`, if given.
fn opt<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter().position(|a| a == name).and_then(|k| args.get(k + 1)).map(String::as_str)
}

/// The arguments without the options and their values.
fn plain(args: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut k = 0;
    while k < args.len() {
        if args[k].starts_with("--") {
            k += 2;
        } else {
            out.push(args[k].clone());
            k += 1;
        }
    }
    out
}

/// The EXE (with symbols when given) and its functions: from `--functions`, else found.
fn load(args: &[String]) -> (Image, Vec<(u32, u32, String)>) {
    let p = plain(args);
    let img = Image::load(Path::new(p.get(1).expect("path to the EXE"))).unwrap();
    if let Some(s) = opt(args, "--symbols") {
        recomp::image::set_symbols(Box::leak(std::fs::read_to_string(s).expect("symbols.txt").into_boxed_str()));
    }
    let ranges = match opt(args, "--functions") {
        Some(f) => recomp::image::load_functions(&std::fs::read_to_string(f).expect("functions.json")),
        None => {
            // from the entry point and the exports (a DLL)
            let mut seeds = vec![(img.entry, "entry".to_string())];
            seeds.extend(img.exports());
            let mut code = Code::new(&img);
            recomp::discover::find_functions(&mut code, &seeds)
        }
    };
    (img, ranges)
}

fn smc_spec(args: &[String]) -> SmcSpec {
    let patch_tables = args
        .windows(2)
        .filter(|w| w[0] == "--patch")
        .map(|w| {
            let (at, n) = w[1].split_once(':').expect("--patch <address>:<variants>");
            PatchTable { at: hex(at), variants: n.parse().expect("variants") }
        })
        .collect();
    SmcSpec { patch_tables, imm_fields: Vec::new() }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("imports") => imports(&args),
        Some("missing") => missing(&args),
        Some("dis") => dis(&args),
        Some("at") => at(&args),
        Some("smc") => smc(&args),
        Some("dialogs") => dialogs(&args),
        Some("iso") => iso(&args),
        Some("movie") => movie(&args),
        Some("vermap") => vermap::run(&args),
        _ => {
            eprintln!("usage: rtools imports|missing|dis|at|smc|dialogs|iso|movie|vermap ... (see the source's head)");
            std::process::exit(2);
        }
    }
}

fn imports(args: &[String]) {
    let img = Image::load(Path::new(&args[1])).unwrap();
    let imports = img.imports();
    let mut dll = String::new();
    for i in &imports {
        if i.dll != dll {
            dll = i.dll.clone();
            println!("{dll}:");
        }
        println!("  {:#x}  {}", i.iat, i.name);
    }
    println!("{} imports", imports.len());
}

fn missing(args: &[String]) {
    let (img, ranges) = load(args);
    let mut code = Code::new(&img);
    let prog = Program::build(&mut code, &ranges);
    eprintln!("{} functions", prog.fns.len());
    let mut seen = std::collections::BTreeSet::new();
    for f in prog.fns.values() {
        for &a in &f.ins {
            let i = code.at(a).unwrap();
            if let Some(why) = recomp::lift::unsupported(&i) {
                if seen.insert(a) {
                    println!("{a:#x}  {:<40} [{why}]  in {}", i.to_string(), f.name);
                }
            }
        }
    }
    eprintln!("{} instructions not supported", seen.len());
}

fn dis(args: &[String]) {
    let (img, ranges) = load(args);
    let p = plain(args);
    let mut code = Code::new(&img);
    let prog = Program::build(&mut code, &ranges);
    let line = |a: u32, i: &iced_x86::Instruction| {
        let target = (i.near_branch_target() as u32 != 0).then(|| symbols().get(&(i.near_branch_target() as u32))).flatten();
        match target {
            Some(n) => println!("{a:08x}  {i}  ; {n}"),
            None => println!("{a:08x}  {i}"),
        }
    };
    if p[2] == "all" {
        for (addr, f) in &prog.fns {
            println!("== {addr:08x} {}", symbols().get(addr).map_or("", |n| n.as_str()));
            for &a in &f.ins {
                line(a, &code.at(a).unwrap());
            }
        }
        return;
    }
    let addr = hex(&p[2]);
    let Some(f) = prog.fns.get(&addr) else {
        eprintln!("no function at {addr:#x}");
        std::process::exit(1);
    };
    println!("== {} ({:x}..{:x}, {})", named(addr), f.lo, f.hi, f.name);
    for &a in &f.ins {
        line(a, &code.at(a).unwrap());
    }
}

fn at(args: &[String]) {
    let img = Image::load(Path::new(&args[1])).unwrap();
    let mut code = Code::new(&img);
    for a in &args[2..] {
        let a = hex(a);
        println!("{a:08x}  {}", code.at(a).map_or("?".into(), |i| i.to_string()));
    }
}

fn smc(args: &[String]) {
    let (img, ranges) = load(args);
    let mut code = Code::new(&img);
    let prog = Program::build(&mut code, &ranges);
    let smc = Smc::find(&prog, &mut code, &smc_spec(args));
    let show = |what: &str, m: &std::collections::HashMap<u32, recomp::smc::Field>, code: &mut Code| {
        let mut v: Vec<_> = m.iter().collect();
        v.sort_by_key(|e| e.0);
        for (a, f) in v {
            println!("{what:<5} {a:#x}  {:<36} field {:#x} ({} bytes)", code.at(*a).unwrap().to_string(), f.at, f.size);
        }
    };
    show("imm", &smc.imm, &mut code);
    show("disp", &smc.disp, &mut code);
    let mut v: Vec<_> = smc.variants.iter().collect();
    v.sort_by_key(|e| e.0);
    for (a, vs) in v {
        let alts: Vec<String> = vs.iter().map(|(_, i)| i.to_string()).collect();
        println!("var   {a:#x}  {:<36} -> {}", code.at(*a).unwrap().to_string(), alts.join(" | "));
    }
    println!("{} immediates/offsets, {} displacements, {} rewritten instructions", smc.imm.len(), smc.disp.len(), smc.variants.len());
}

fn dialogs(args: &[String]) {
    use recomp::image::{ResId, list_resources};
    use win32::platform::dialog::template::Template;
    let img = Image::load(Path::new(&args[1])).unwrap();
    let rd = |a: u32| img.byte(a);
    for (name, at, size) in list_resources(&rd, img.base, ResId::Id(5)) {
        let bytes = img.read(at, size as usize).unwrap();
        let Some(t) = Template::parse(&bytes) else {
            println!("{name:?}: cannot parse");
            continue;
        };
        println!("{name:?} \"{}\" {:?} style {:#x} font {:?}", t.title, t.rect, t.style, t.font);
        for i in &t.items {
            println!("    {:>5} {:<12} {:?} style {:#010x} {:?}", i.id as i32, format!("{:?}", i.class), i.rect, i.style, i.text);
        }
    }
}

fn iso(args: &[String]) {
    use win32::iso::{Entry, Iso};
    fn show(iso: &mut Iso, dir: &Entry, indent: usize) {
        for e in iso.list(dir).expect("cannot read the image") {
            println!("{:indent$}{}{} {}", "", e.name, if e.dir { "/" } else { "" }, e.size);
            if e.dir {
                show(iso, &e, indent + 2);
            }
        }
    }
    let mut iso = Iso::open(args[1].as_ref()).expect("not a CD image");
    let root = iso.root();
    show(&mut iso, &root, 0);
}

fn movie(args: &[String]) {
    use std::io::Write;
    use win32::mpeg;
    let data = std::fs::read(&args[1]).expect("cannot read the movie");
    let out = args.get(2);
    let t = std::time::Instant::now();
    let s = mpeg::demux(&data);
    println!("video {} bytes (start {:?}), audio {} bytes (start {:?})", s.video.len(), s.video_start, s.audio.len(), s.audio_start);
    let mut v = mpeg::video::Decoder::new(&s.video).expect("no video sequence header");
    println!("{}x{} at {} fps", v.width, v.height, v.fps);
    let mut rgb = Vec::new();
    let mut frames = out.map(|d| std::io::BufWriter::new(std::fs::File::create(format!("{d}/frames.rgb")).unwrap()));
    let mut n = 0;
    while let Some(f) = v.next_frame() {
        f.to_rgb(&mut rgb);
        if let Some(w) = &mut frames {
            for p in &rgb {
                w.write_all(&[(p >> 16) as u8, (p >> 8) as u8, *p as u8]).unwrap();
            }
        }
        n += 1;
    }
    println!("{n} frames ({:.2} s)", n as f64 / v.fps);
    if let Some(mut a) = mpeg::mp2::Decoder::new(&s.audio) {
        let mut raw = out.map(|d| std::io::BufWriter::new(std::fs::File::create(format!("{d}/audio.raw")).unwrap()));
        let mut samples = 0;
        while let Some(frame) = a.next_frame() {
            if let Some(w) = &mut raw {
                for s in frame {
                    w.write_all(&s[0].to_le_bytes()).unwrap();
                    w.write_all(&s[1].to_le_bytes()).unwrap();
                }
            }
            samples += frame.len();
        }
        println!("audio {} Hz, {samples} samples ({:.2} s)", a.sample_rate, samples as f64 / a.sample_rate as f64);
    }
    println!("decoded in {:.2} s", t.elapsed().as_secs_f64());
}
