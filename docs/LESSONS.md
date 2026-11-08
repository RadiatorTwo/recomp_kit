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
  frames with interpolated positions instead (`platform::tween`, Future Cop's `fps.rs`,
  Nuclear Strike's `pictures.rs`; GUIDE.md 8b).
- Rounding the game's time to whole frames is not enough: code that counts frames (a trail
  piece a frame) still runs per picture, and deadlines set with one time and compared with
  another break. Fix the loop's rate; give pictures only drawing.
- Logic may assume a frame longer than the fixed one: `dt / 0x114` (1/55.6 s) with the rest
  dropped is 0 at 60 frames of 256: carry the rest (Nuclear Strike's autopilot).
- Pictures: interpolate a frame late, only while drawing; quaternions for rotations; put back
  static data (not a memory manager's lists), hold the turn meanwhile; draw from the data a
  double-buffered renderer had before the frame's own drawing.
- Shaking only in turns, with interpolation working: the game's camera may be built from
  whole angle units (1/4096 turn) cut from finer ones it keeps (16.16), so it turns unevenly
  per frame and, orbiting the player at a distance, its place jumps. Log camera and player
  per logic frame: uneven camera steps with smooth player steps mean the logic. For the
  pictures only: note the point followed (camera less its offset, computed with the game's
  own matrix: one's own rounding times the distance is noise again), interpolate the fine
  angles, build the matrix in floats (checked against the game's), place the camera from
  that point (Nuclear Strike's `pictures.rs` `cam_update`/`view`).
- A pause of one's own (an overlay menu stopping a fixed-rate logic clock) must leave that
  clock even with the game's real clock afterwards: the game reads both. Nuclear Strike's
  Esc menu set the mission start from the real clock on resuming (`strikeClockPause_`); with
  the logic clock lagging by the pause the mission time went back, a negative time step ran
  `Slerp_` some 4 billion times and the game froze. Do as the game's own pause: move the
  logic clock up to the real one (whole game ticks) and the start times the game subtracts
  (mission start, last tick) by the pause (nuclear_rust_strike's `frames.rs`
  `logic_frame_us`). Test: own menu open and closed, then the game's pause menu.
- A PlayStation port may divide by zero where the PlayStation did not trap (a point at the
  camera's depth): report it once and divide by 1 rather than stop (nuclear_rust_strike's
  `lift.rs` `div` and its event handler; the kit's lifter still stops).
- Generated code must not hold host addresses as constants if it is cached: statics move from
  run to run.

## Process

- Back up the game's settings files (key bindings!) before tests that start the game; some
  games rewrite them when they are missing.
- A savestate (`platform::savestate`) is the fastest way back to a bug deep in a level.
