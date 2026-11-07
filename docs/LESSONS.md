# Lessons

Traps found while porting Future Cop, Hogs of War, Gun Metal, Nuclear Strike and Black &
White. Most apply to any game of that time. Add new ones here.

## Threads

- Guest threads must take turns (`platform` turn lock), not run truly in parallel: games of
  the time assumed one CPU. Future Cop's async file reader and its callback share a one-slot
  mailbox and raced when run in parallel.
- Spin-waits: generated code checks `rt::POLL` at every loop head (target of a backward branch)
  and calls `rt_yield`, so a thread spinning on a flag lets the others run.
- But not at every loop head: a thread keeps the turn for a slice (0.5 ms, `SLICE`, timed with
  the TSC; Instant::now there cost 8%). Nuclear Strike's game thread and its input thread
  traded the turn 60 000 times a second, 15% of the game thread waiting.
- Imports give up the turn while they run. Glide's drawing calls never wait: they keep it
  (`Thunk::quick`, `SLOW_GLIDE` lists the ones that may wait), else hundreds of thousands of
  hand-overs a second.

## x87

- The C runtime sets 53-bit precision, but renderers load their own control word (Future Cop,
  Nuclear Strike: 0xc7f, 24 bits, truncating). The generated code rounds 24 bits to nearest or
  truncating itself (`round24_between`, then `round24` with the error's sign from TwoSum/fma
  when the double lands on a 24-bit number or a middle); only other modes, subnormals and
  infinities call `rt_fpu_round` (it took 24% of Nuclear Strike's game thread). Results are
  often exact 24-bit numbers: without the error's sign that case went to the slow call.
- The register stack lives in Cranelift values along straight runs of instructions
  (`x87::Cache`), with the control word and its rounding tests; it is written back before
  branch targets, calls, helpers and anything not a plain integer instruction (renderers'
  projection loops are long fld/fmul/fxch/faddp chains). `tests/x87_cache.rs` compares random
  chains cached and uncached (`lift::X87_CACHE`).
- `fild qword` / `fistp qword` is the compilers' 64-bit memcpy idiom: an exact integer shadow
  (`st_int`) keeps it bit-exact.

## CPU

- CPUID: report family 5/6 and no MMX unless the lifter does all of MMX (it does only
  movd/movq). Libraries (Bink, QMixer, D3DX) take their plain paths then.
- Keep AC and ID through pushfd/popfd: the CPUID presence test flips ID.
- `in` from the gameport reads 0xff (nothing connected); `out` does nothing.

## Speed

- Measure before guessing: `<PREFIX>_SAMPLE=1` samples the thread that swaps Glide's buffers
  (recompiled function, or host module unwound to our own frame), `<PREFIX>_SAMPLE_RAW=<file>`
  all samples; symbolise our own addresses with a build with
  `CARGO_PROFILE_RELEASE_DEBUG=line-tables-only` and llvm-symbolizer. `<PREFIX>_DUMP=<hex>`
  disassembles one function's compiled code.
- NVIDIA's glGetUniformLocation makes system calls: ~10% of a game thread at a few lookups a
  frame. Ask once (`gl::uniform`).
- Indirect calls (function pointers, Glide through GetProcAddress) look the target up:
  `rt_lookup` reads a dense table, thunks miss at once; a binary search there cost 7%.

## Windows

- WM_PAINT: windows need a dirty flag; GetMessage/PeekMessage return WM_PAINT when the queue is
  empty. Menus that are redrawn only at WM_PAINT stay stale without it.
- Never send WM_ACTIVATEAPP 0: games pause themselves on it. Release held keys when the focus
  goes instead.
- Exit with TerminateProcess (`_exit` on Linux) after saving the registry: with audio, gamepad
  and GL threads killed mid-way a driver could hang in the DLLs' clean-up.
- ShowCursor keeps a counter; while hidden and focused, grab the cursor (confined, else locked).
- DDBLTFX: dwFillColor is at 0x50 (an earlier port read 0x48 and drew black).
- DirectDraw windowed: the primary surface is the whole desktop; show the window's rectangle.
  Show blits to the primary at the next message poll, a Flip, or 50 ms after the first unshown
  change: showing every blit makes menus flicker while they build up.
- Keyboard hooks (WH_KEYBOARD) get only the four key messages, not WM_CHAR.

## Discovery

- MSVC puts jump tables right after `_exit` calls: decoding must stop at a table's bytes.
- Code addresses stored in data (function pointer tables, a renderer's jump targets) must be
  entry points too.
- Other code jumps into the middle of functions: those places become extra entries.

## Game logic and frame rate

- Never shorten a game's logic frame to raise the frame rate: logic written as
  `(rate * ticks) >> 8` breaks (hits, effects, hovering). Draw extra pictures between logic
  frames with interpolated positions instead (Future Cop's `fps.rs`).
- Generated code must not hold host addresses as constants if it is cached: statics move from
  run to run.

## Process

- Back up the game's settings files (key bindings!) before tests that start the game; some
  games rewrite them when they are missing.
- A savestate (`platform::savestate`) is the fastest way back to a bug deep in a level.
