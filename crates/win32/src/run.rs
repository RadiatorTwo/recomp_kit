//! The program's start, the same for every game: find and set up the game files (`setup`),
//! compile the EXE (machine code kept in `compiled.bin` beside it for the next start) and run
//! it. A game's `main` is `win32::run(&GAME)`.
//!
//! ```text
//! <program> [path to the EXE]     the game (default: the EXE in `game_files`)
//! <program> <command> ...         the game's own commands (`Game::command`)
//! ```
//!
//! A release build should be a Windows program without a console window
//! (`#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]` in the game's
//! main.rs): messages go to the console it was started from, else to `Identity::log_file`
//! next to the program.

use crate::game::{self, Game};
use crate::{platform, setup, splash};
use recomp::discover::{Code, Program};
use recomp::image::Image;
use recomp::jit::compile_cached;
use recomp::smc::Smc;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// Without a console window of our own: messages to the console we were started from (or
/// wherever they were redirected), else to the log file next to the program.
#[cfg(windows)]
fn output() {
    use std::os::windows::io::IntoRawHandle;
    use windows_sys::Win32::System::Console::*;
    unsafe {
        if !GetStdHandle(STD_ERROR_HANDLE).is_null() {
            return;
        }
        let out = if AttachConsole(ATTACH_PARENT_PROCESS) != 0 {
            std::fs::OpenOptions::new().write(true).open("CONOUT$").ok()
        } else {
            let log = std::env::current_exe().ok().and_then(|p| Some(p.parent()?.join(game::id().log_file)));
            log.and_then(|p| std::fs::File::create(p).ok())
        };
        if let Some(f) = out {
            let h = f.into_raw_handle();
            SetStdHandle(STD_OUTPUT_HANDLE, h as _);
            SetStdHandle(STD_ERROR_HANDLE, h as _);
        }
    }
}

#[cfg(not(windows))]
fn output() {}

/// Run the game; does not return.
pub fn run(g: &'static dyn Game) -> ! {
    game::set(g);
    #[cfg(unix)]
    if std::env::args().nth(1).as_deref() == Some("--splash") {
        splash::child();
        std::process::exit(0);
    }
    // early, before other allocations may take its place, and before compiling: the code
    // takes the base as a constant when it is at rt::FIXED_BASE
    platform::reserve_memory();
    output();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(code) = g.command(&args) {
        std::process::exit(code);
    }
    // guest calls nest on the host stack like the original's: run on a thread with a large one
    let code = std::thread::Builder::new().stack_size(256 << 20).spawn(start).unwrap().join().unwrap();
    platform::kernel32::terminate(code);
}

fn start() -> u32 {
    let g = game::game();
    let id = game::id();
    splash::show();
    splash::status("Looking for the game...");
    let found = match std::env::args().nth(1) {
        Some(p) => setup::check(Path::new(&p)).map(|_| PathBuf::from(p)),
        None => setup::game_exe(&setup::root()),
    };
    let exe = match found {
        Ok(e) => e,
        Err(text) => {
            splash::fail(&text);
            std::process::exit(2);
        }
    };
    let mut img = Image::load(&exe).unwrap_or_else(|e| panic!("{}: {e}", exe.display()));
    // the addresses the game knows are its reference version's: where they are in this one
    if let Some(v) = recomp::ver::of_exe(id.versions, &img.data) {
        recomp::ver::select(v);
    }
    // before the cache key: patched code is compiled code of its own
    for (addr, old, new) in g.code_patches() {
        img.patch(addr, old, new).unwrap_or_else(|e| panic!("{e}"));
    }
    let ranges = g.functions(&img);
    platform::dinput::start(); // gamepads are found while we compile
    splash::status("Compiling the game...");
    let t = Instant::now();
    let mut code = Code::new(&img);
    let prog = Program::build(&mut code, &ranges);
    let smc = Smc::find(&prog, &mut code, &g.smc());
    let found = t.elapsed().as_millis();
    // the machine code is kept beside the EXE, for this program and this EXE
    let cache = exe.with_file_name(setup::COMPILED);
    let key = cache_key(&img);
    let compiled = compile_cached(&prog, &smc, &mut code, &g.hooks(), key.map(|k| (cache.as_path(), k)));
    let st = &compiled.stats;
    if st.cached {
        eprintln!("compiled code of {} functions loaded in {} ms", st.functions, t.elapsed().as_millis());
    } else {
        eprintln!(
            "compiled {} functions in {} ms (finding them {found}, lifting {}, compiling {})",
            st.functions,
            t.elapsed().as_millis(),
            st.lift_ms,
            st.compile_ms
        );
    }
    let dir = exe.parent().map(Path::to_path_buf).unwrap_or_default();
    let dir = std::fs::canonicalize(&dir).unwrap_or(dir);
    let code = platform::run_game(&img, dir);
    drop(compiled);
    code
}

/// What the compiled code depends on: this program (any change to the recompiler or the game
/// crate) and the game's EXE. None when our own file cannot be read.
fn cache_key(img: &Image) -> Option<[u8; 32]> {
    use sha2::{Digest, Sha256};
    let own = std::fs::read(std::env::current_exe().ok()?).ok()?;
    let mut h = Sha256::new();
    h.update(&own);
    h.update(&img.data);
    // the memory's base, a constant in the code when fixed
    h.update(recomp::lift::FIXED_BASE.load(std::sync::atomic::Ordering::Relaxed).to_le_bytes());
    Some(h.finalize().into())
}
