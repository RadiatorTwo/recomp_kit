# PLAN — recomp_kit

## Done (2026-11-07)

- Core (`recomp`) and Windows layer (`win32`) extracted from future_rust_cop, game-independent:
  `Identity` + `Game` trait; fcop_plain reaches Future Cop's main menu and first mission.
- Fixes from the sibling ports: xlat, segment register moves, pushf/popf, bsf/bsr, AC/ID,
  fnstenv/fldenv/fnsave/frstor, CPUID register order (was wrong in every copy), jump tables
  end decoding, `find_functions`, DLL exports/rebase/combine.
- `rtools`, template, `new-game.ps1`, GUIDE.md, LESSONS.md.
- GitHub repository github.com/RadiatorTwo/recomp_kit, branch `main` (the template's git
  dependency points there).

- New games get a copy of crates/recomp, win32 and tools (`new-game.ps1`), no dependency on
  this repo. The existing ports keep their own cores; they do not move onto the kit.
- Speed from nuclear_rust_strike (its mission ~330 -> ~520 fps): x87 24-bit rounding inline,
  x87 registers cached along straight runs, dense `rt_lookup`, turn slice and quick Glide
  imports, `on_main` per thread, uniform lookups cached (window blit), sampling profiler
  (`SAMPLE`), `DUMP`. fcop_plain checked: menu and first mission.

- Speed from nuclear_rust_strike (2026-11-08, its mission +18% and +10% fps in A/B runs): 24-bit rounding's usual cases cover exact results and zeros, cut in a vector
  register, mode made once a run (`Cpu::x87_mode`); straight runs under 0xc7f lifted twice
  with the SSE rounding set to truncate; `rt_fpu_round` corrected for float stores beyond a
  float's range; `tests/x87_bench.rs`; sampler by offset with `DUMP`. LESSONS: the Glide
  alpha test as vertex data (the kit's Glide has none yet).

- Pictures between logic frames from nuclear_rust_strike: `platform::tween` (`LogicClock`,
  `Hist`, `mix_rotation`, `Statics`, `call_regs`), `hold_turn` / `without_turn`; GUIDE.md 8b,
  LESSONS. The game-specific part (which functions draw, what to interpolate) stays in ports.

- From nuclear_rust_strike (2026-11-09): code patches before compiling (`Image::patch`,
  `Game::code_patches`, test), `gl::set_vsync`; GUIDE 8c point 7, 8d, 8e; LESSONS (screens of
  their own stay 4:3, drawing once, game checks, vsync).

- Speed from nuclear_rust_strike (2026-11-13; it went ~175 -> ~790 fps without effects):
  guest memory at a fixed base (a constant in the code), hook index in the stub, thunks per
  thread, the turn by priority with Suspend/ResumeThread and POLL cleared while the slice
  lasts, Sleep(0) that sleeps, Glide's unchanged texture rows skipped, `tween::Statics` with
  one buffer and the write watch, `tween::Pacer` (sleep and spin; pictures only when shown
  before the next logic frame). GUIDE: goal "better than the original" and its rules
  (section 0), 8a speed before effects (measure, per-call costs, hot game functions in Rust
  checked identical), 11 modern effects. Not yet run on a game (fcop_plain).
- Present thread (2026-11-13): Glide pictures copied on the GPU into one of two textures at
  grBufferSwap and shown by a thread of ours with a shared context (`window::present_later`,
  `gl::Shared`, fences both ways; `<PREFIX>_PRESENT=0` off). The window's own drawing waits
  for it (`present_wait`, `PAINT`). Not yet run on a game.

## Next

Grow the kit for future games, one piece at a time, with tests:

1. From the sibling ports, when a game needs it:
   - black_white: ~90 kernel32 imports (INI, file mapping, Find*, CreateProcess, date/time),
     ~37 user32, advapi32 RegEnum*/RegCreate/RegDelete, DLL loading (`Module`, DllMain,
     `run_library`), winsock stub, small DLLs (IMM32, COMCTL32, SHELL32, VERSION), mscab/iscab;
   - rustschweine: gdi32 bitmap DCs, DirectDraw 7, Direct3D 7 (`d3d.rs`), CD audio from .cue;
   - rust_metal: Direct3D 9, COM passthrough, ole32 class registry, winmm timers, crash
     reports, sampling profiler, Media Foundation music; speed work in the lifter (flag
     crossings, cmp+jcc fusion, x87 values in variables, inline string ops, Direct dispatch);
   - nuclear_rust_strike: Watcom images, CodeView names, Miles Sound System.
   - future_rust_cop: the Glide effect points (vertex depth, `post_3d`, primitive census) if a
     Glide game wants its effects.
2. From nuclear_rust_strike: Glide's alpha test as vertex data and batches ended only by blend/chroma/filter/clamp/clip,
   uploads staged until a texture's next use, glFlush only while another thread draws, the
   sampler's 40-frame unwind; its effects (ao, lights, shadows, bloom, heat, zbuffer,
   particles) when a game wants them.
3. Difftest (Unicorn, GPL-2.0) as its own crate `crates/difftest` (still in future_rust_cop).
4. Video formats beyond MPEG-1 (Bink, Smacker, AVI) when a game needs them.
