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
- From nuclear_rust_strike (2026-11-13): Glide from any thread (global state, a shared
  context per thread made ahead by the window's thread, vertex arrays and framebuffers per
  thread, glFlush only while two threads draw, pictures of other threads to the present
  thread), Glide's alpha test as vertex data, DirectInput keyboard and mouse (GetDeviceState,
  buffered GetDeviceData, raw mouse motion), VirtualAlloc 64 KB aligned, Watcom's jump tables
  `jmp cs:[reg + table]`, `Game::div_by_zero_goes_on` (divided by 1, logged once), the
  sampler's 40-frame unwind. Checked with fcop_plain.
- From nuclear_rust_strike (2026-11-13): Glide downloads staged on the CPU and uploaded
  together when the texture is next drawn with (`Texture::staged`, `pending`, `commit`; no
  flush per download). With HD replacements on, uploaded at once (they read the original's
  layer). Not yet run on a game.
- From future_rust_cop (2026-11-13): `<PREFIX>_STATS` (frames a second and a frame time graph
  in the window, `stats.rs`); the sampler counts hooked functions by their lifted originals;
  LESSONS: fps varies ~10% between runs (time the code), the present thread is not a gain in
  every game, a game's display list walker in Rust. Checked with fcop_plain.
- LESSONS from future_rust_cop (2026-11-14): the fixed base's gain without large functions,
  integer-only game functions ported exactly (terrain pass), check `rt_fpu_round` in the profile
  before the x87 work, framebuffer switches' driver cost (bloom).
- LESSONS / GUIDE from future_rust_cop (2026-11-21): batch uploads with glBufferSubData, no set
  lookups in sort comparisons, frame pacing (late frames keep their length, pictures keep their
  schedule, frame logs with the shown game time), vsync near the refresh, gamepad trigger rest,
  savestates and new threads' stacks, depth buffer, shadow map and its ring, tone mapping
  targets; the user's rules (glitches fixed regardless of the original, no environment
  variables for hand tests, replays with the user's settings).
- From rustcoil (Recoil, 2026-11-21): game DLLs loaded and compiled with the EXE
  (`Game::dlls`, `platform/module.rs`: MFC 4.2, MSVCRT), the MFC window layer in user32 (WH_CBT
  hook in CreateWindowExA, classes, properties, menus from MENU resources: `platform/menu.rs`),
  gdi32 DC state, DIB sections, GetObjectA of them, TrueType text (`platform/text.rs`,
  ab_glyph), DrawTextA; Glide for real 3D games (perspective-correct textures, depth buffer,
  culling, fog, color mask, Get/SetState, polygon lists, the linear frame buffer); mouse
  positions in Glide's screen pixels (`window::set_screen`); AVI stubs (open fails, the game
  goes on); InstallShield 5/6 cabs (`iscab.rs`, `rtools iscab`), `rtools iso --extract`.
- From rustcoil (2026-11-22): Glide texture arrays grow (64 .. 2048 layers), mipmaps made per
  texture on the CPU, s/t scaled by the texture's size / 256, tmu oow 0 falls back to the
  vertex's for the texture rectangle too, repeated textures wrapped inside the texture with
  mipmaps; DirectInput reads fetch the window's input; a scripted run does not catch the mouse.
  Then: the texture layer in 12 bits (black textures past 256 layers), clamp at the texture's
  edge, a scripted run ignores the user's mouse and keys over the window. Open: with SHOT and the
  present thread pictures sometimes stop while the game goes on (PRESENT=0 meanwhile).
  And: setup never removes anything (its cleanup put a game's files and CD image into the
  recycle bin), DeleteFileA only inside the game folder, no writes outside it; a hidden cursor
  the game draws itself is not caught (`window::cursor_drawn`); ShowOwnedPopups, WinHelpA.

- From future_rust_cop (2026-11-25): gamepad triggers pressed at 30% beyond each pad's rest
  (`dinput::trigger_down`); a new thread's TIB and stack made in CreateThread / timeSetEvent
  (`thread_stack`, `thread_cpu`); 2D frames with vsync end with glFinish (menu highlight lag);
  the Linux start window reads no settings (`Gl::settings`, it has no game folder) and its error
  has an Open Folder button. LESSONS: a busy menu loop, teleports undone by putting places back,
  blob shadows without the depth test, effect lists cleared every picture, macOS facts. Built on
  Windows; the Linux start window not yet run.

## Open code fixes (known from future_rust_cop, not yet in the kit)

- `glide.rs`: check how batches are uploaded (glBufferSubData, no map per batch; the kit
  orphans the buffer with glBufferData now).
- macOS (Apple Silicon): game on the main thread, stack size from `build.rs`, 16 KB pages,
  the guest base read at run time, the app bundle's folder (future_rust_cop 2026-11-24).

## Next

Grow the kit for future games, one piece at a time, with tests:

1. From the sibling ports, when a game needs it:
   - black_white: ~90 kernel32 imports (INI, file mapping, Find*, CreateProcess, date/time),
     ~37 user32, advapi32 RegEnum*/RegCreate/RegDelete, DLL loading (`Module`, DllMain,
     `run_library`), winsock stub, small DLLs (IMM32, COMCTL32, SHELL32, VERSION), mscab/iscab;
   - rustschweine: gdi32 bitmap DCs, DirectDraw 7, Direct3D 7 (`d3d.rs`), the mixer* CD volume (CD audio from
     .cue is in the kit since Recoil: `cdaudio.rs`, MCI and aux*);
   - rust_metal: Direct3D 9, COM passthrough, ole32 class registry, winmm timers, crash
     reports, sampling profiler, Media Foundation music; speed work in the lifter (flag
     crossings, cmp+jcc fusion, x87 values in variables, inline string ops, Direct dispatch);
   - nuclear_rust_strike: Watcom images, CodeView names, Miles Sound System.
   - future_rust_cop: the Glide effect points (vertex depth, `post_3d`, primitive census) if a
     Glide game wants its effects.
2. From nuclear_rust_strike: its effects (ao, lights, shadows, bloom, heat, zbuffer,
   particles) when a game wants them.
3. Difftest (Unicorn, GPL-2.0) as its own crate `crates/difftest` (still in future_rust_cop).
4. Video formats beyond MPEG-1 (Bink, Smacker, AVI) when a game needs them.
