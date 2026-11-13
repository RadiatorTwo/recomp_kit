# CLAUDE.md — recomp_kit

Hand-over for future sessions. What is done and what is open: **PLAN.md** (keep it current).

## Goal (the user's)

One framework for native Rust ports of late-1990s Windows games (32-bit x86 EXE, DirectX /
Glide): the player's EXE recompiled with Cranelift, Windows and DirectX in Rust. Extracted
on 2026-11-07 from future_rust_cop (the most advanced port); the sibling ports (rust_metal,
rustschweine, nuclear_rust_strike, black_white_recomp) each still have their own copy of the
core and stay that way (they do not move onto the kit). New games start from `template/`
with their own copy of the crates.

Only code is shared. **Never commit anything derived from a game's EXE** (code, data, the
EXE's function list belongs in the game's repository, not here).

## The user

- New to reverse engineering; explain in plain words, give a recommendation, not a survey.
- Speaks German (docs/GUIDE.md is German, the code and its comments English); short sessions,
  watches usage limits.
- Confirm before anything outward-facing or destructive (pushing, deleting).
- Clean Rust first, performance later. Never run cargo fmt (it reformats whole crates).
- No Co-Authored-By or other AI attribution in commits.
- The goal is **better than the original**, not identical to it like an emulator (the user
  never liked that about emulators). So:
  - the game's own functions have no standing when they produce faults (wrong sorting,
    glitches, crashes): they may be replaced by new Rust code (a hook) instead of being
    worked around in ever more complex ways;
  - game code may be changed or replaced when it brings an advantage (speed) and the result
    is identical (checked by a switch running both, GUIDE 8a);
  - limits of the technology of the time may be lifted (fixed logic rate with pictures
    between, widescreen, resolution, depth buffer, resolution locks, idle loops).
- Speed before modern effects (GUIDE 8a, then 11).
- With these old games the GPU is never the limit: a low frame rate is always CPU side
  (game code, our code, draw calls, driver). Never blame the GPU (GUIDE 8a, LESSONS Speed).

## Layout

- `crates/recomp`: the core, no game and no Windows in it.
  - `image.rs` PE images (sections with an `exec` flag: `Image::is_code`; imports, exports,
    resources, `rebase`, `combine` for DLLs, `patch` (code bytes changed before compiling,
    old bytes checked: `Game::code_patches`)), `load_functions(json)`, `set_symbols(txt)`.
  - `discover.rs` the code: functions from a list (`Program::build`) plus extra entries;
    `find_functions(code, seeds)` without a list; jump tables end decoding (`table_bytes`).
  - `lift.rs`, `lift/x87.rs` x86 -> Cranelift IR; `unsupported()` checks an instruction.
    x87 registers are cached in Cranelift values along straight runs (`x87::Cache`, written
    back at `Lifter::joins` and before non-plain instructions; `X87_CACHE` off for tests);
    24-bit rounding inline (`round24_between`, `round24`); straight runs under 0xc7f lifted
    twice with the SSE rounding set to truncate (`rz_runs`, `versions`, `arith_rz`,
    `rt_mxcsr_rz`); `tests/x87_bench.rs` times it (ignored).
  - `smc.rs` self-modifying code; what a game has beyond `mov [code], x` comes as `SmcSpec`.
  - `jit.rs` compiles everything (cache file `RCMPJIT1`), hooks as `(address, fn)`;
    `<PREFIX>_DUMP=<hex>` disassembles one function's code. `rt_lookup` reads a dense table.
  - `rt.rs` `Cpu`, guest memory (reserved at `FIXED_BASE` if free: the lifter then takes the
    base as a constant, `lift::FIXED_BASE`, in the cache key; `MEM_WRITE_WATCH`,
    `Memory::written`), helpers, call counters (`counter`, `counts`); `rt_hook` gets the
    hook's index from its stub.
  - `ver.rs` versions of an EXE: `Us(0x...)` is an address of the game's reference version,
    `.at()` its place in the version that runs (`select` after `of_exe(versions, data)`).
  - `config.rs` the switches' prefix (`env("COUNT")` = `FCOP_COUNT` for Future Cop), the CPUID
    MMX bit (default off).
- `crates/win32`: the Windows layer (from future_rust_cop's `platform/`, without its
  game modules): `game.rs` (`Identity`, the `Game` trait and its extension traits
  `DialogExt`, `GlideExt`; `game()` / `id()` give the running game), `run.rs` (the program's
  start: setup, compile with cache, run), `setup.rs` (game files, CD images), `splash.rs`,
  `iso.rs`, `mpeg/`, `platform/` (kernel32, user32, gdi32, advapi32, ddraw, dsound, dinput
  (gamepads, and the system keyboard and mouse with GetDeviceData), dshow, dplay, misc,
  window, gl, glide (the version before Future Cop's effects; global state, drawn from any
  thread with its own shared context and `Local` objects, `glFlush` only while two threads
  draw; the alpha test as vertex data), display,
  dialog, gl (`set_vsync`: swap interval 1, the pace stays the game's; `share`/`Shared`
  and `stage`: Glide pictures are shown by a present thread, `window::present_later`,
  `<PREFIX>_PRESENT=0` off), sample (sampling profiler, `<PREFIX>_SAMPLE`), menupad (gamepad in menus; the game gives its items with `Game::menu`), record,
  savestate (the game calls `level_start` / `frame`), cheats (`Game::cheats`, run by the game's
  hook with `run_pending`), setting (registry + switch), hd (replacement textures), tween
  (pictures between logic frames: `LogicClock`, `Pacer` (sleep and spin), `Hist`,
  interpolation with quaternions, `Statics` put back by the pages written, `call_regs`; with
  `hold_turn` / `without_turn` in `platform/mod.rs`)). Guest threads (`platform/mod.rs`):
  one turn, given by priority (SetThreadPriority), Suspend/ResumeThread at loop heads,
  `POLL` cleared while the slice lasts; Sleep(0) sleeps when others run or below normal
  priority. Glide skips texture rows downloaded again unchanged.
- `crates/tools`: `rtools` (imports, missing, dis, at, smc, dialogs, iso, movie, vermap).
- `examples/fcop_plain`: Future Cop with only its `Identity`, patch table and function list
  (read at run time from `FCOP_FUNCTIONS`, default ../future_rust_cop/data/functions.json): the
  check that the framework runs a real game. Run with the EXE's path as argument (a copy of the
  game files: setup and the registry write into the EXE's folder).
- `template/` + `tools/new-game.ps1`: a new game project. The script **copies** crates/recomp,
  win32 and tools into it (no dependency on this repo; the kit's commit goes into its PLAN.md).
- `docs/GUIDE.md` the way through a port, `docs/LESSONS.md` the traps.

## Checks

- `cargo test --release --workspace` (snippet tests of the lifter, x87, SMC, discovery, images).
- `rtools missing <FCopLAPD.exe> --functions ../future_rust_cop/data/functions.json`: 2176
  functions, 0 unsupported (without `--functions`: 1778 found).
- fcop_plain with `FCOP_NO_VIDEO=1 FCOP_SHOT=... FCOP_INPUT="8000:click@484,303"`: main menu,
  then the first mission.
