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
- A message loop of PeekMessage and Sleep(0) must not spin: as `yield_now` Nuclear Strike's
  WinMain thread (priority LOWEST) took the turn ~340 000 times a second while the game
  thread was in host code, kept a core busy and made the game thread ~10% slower. The kit's
  Sleep(0) sleeps 1 ms when another guest thread runs or the caller is below normal priority
  (SetThreadPriority is kept per guest thread). Count turn takes per thread when the frame
  rate swings.
- The turn goes to waiters by priority (a higher one gets it at the holder's next loop head,
  as Windows preempts), else after the slice, then politely (behind all waiters: no
  starvation). SuspendThread stops a thread at its next loop head or before guest code after
  an import, never in host code (it would hold our locks).
- Loop heads must not call into the host while nothing can change: with a lower thread
  waiting, `rt_yield` ran at every loop head (4.5 million calls a second, ~3% of the game
  thread). The holder clears `POLL` while its slice lasts or it holds the turn; waiters wait
  with a timeout of half a slice and set it again.
- The rest of such swings (phases of seconds, same work a frame) is where Windows puts the
  game thread: on a core's second hyperthread beside another process the recompiled code was
  ~35% slower; pinning it to one core helped (~375 -> ~425 fps) but cannot keep others off.

## x87

- The C runtime sets 53-bit precision, but renderers load their own control word (Future Cop,
  Nuclear Strike: 0xc7f, 24 bits, truncating). The generated code rounds 24 bits to nearest or
  truncating itself (`round24_between`, then `round24` with the error's sign from TwoSum/fma
  when the double lands on a 24-bit number or a middle); only other modes, subnormals and
  infinities call `rt_fpu_round` (it took 24% of Nuclear Strike's game thread). Results are
  often exact 24-bit numbers: without the error's sign that case went to the slow call.
  The usual cases cover them too: on a 24-bit number with no error (the error from fma/TwoSum
  only a branch's condition), and exact zeros (x * 0); in Nuclear Strike ~20 million
  operations a second had gone to the slow `round24`, its branch mispredicted. Keep the cut
  in a vector register (`vpaddq`/`vpand`): moving the result to an integer register and back
  put ~8 cycles on every operation of a chain.
- Cranelift makes a pure value where it is first used: values made once for a run (the
  rounding mode's tests) were made again in every operation's block, as those do not
  dominate each other. A store of them (`Cpu::x87_mode`, read by nobody) pins them down.
- Under 0xc7f a straight run with 4 or more x87 operations is lifted twice (`rz_runs`,
  `versions`): if the control word truncates to 24 bits, `rt_mxcsr_rz` sets the SSE rounding
  to truncate as well, and each operation is the double one cut to 24 bits (truncating twice
  is truncating once), tested only for a normal result (`arith_rz`); a float store is a
  plain fdemote. Nuclear Strike's projection routines: 16.6% -> 7.1% of the game thread.
  Traps: Cranelift moves and shares pure float operations, so write the cache back before
  the switch (nothing computed under the old rounding reaches the run) and let each result
  feed a branch (made after the switch); our own Rust helpers (`rt_fpu_round`) must set the
  default rounding themselves; keep out what the SSE rounding would change wrongly (fild
  qword's conversion) and what changes the control word (fldcw, helpers).
- The reference had two wrong answers the versioned code showed: a float store beyond a
  float's range under truncation gives the largest float (masked overflow), not infinity;
  and a truncated double overflow gives the largest double, which must go to the slow path.
- `tests/x87_bench.rs` (`--ignored --nocapture`) times a projection-like loop under 0xc7f
  and 0x27f: a steady number, unlike a game's frame rate.
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
  disassembles one function's compiled code; with both set the sampler counts that
  function's samples by offset in its machine code (the dump's offsets).
- Count before optimising a path: a temporary counter in the slow call (`rt_fpu_round`, the
  Glide flush's reasons) told what a profile could not (which operations, which state change).
- A Glide renderer drew ~250 batches a picture because the game changed the alpha test
  (grAlphaTestFunction / ReferenceValue) between almost all primitives: state a game changes
  per primitive belongs in the vertex data, not in uniforms that end a batch (the driver's
  draw calls were ~10% of the game thread). A mapped ring buffer and cached uniforms gained
  ~3% beside it. (The kit's Glide has no alpha test yet: add it that way.)
- NVIDIA's glGetUniformLocation makes system calls: ~10% of a game thread at a few lookups a
  frame. Ask once (`gl::uniform`).
- Indirect calls (function pointers, Glide through GetProcAddress) look the target up:
  `rt_lookup` reads a dense table, thunks miss at once; a binary search there cost 7%.
- The guest memory's base as a register: large functions (~50 KB of machine code) spilled it
  and reloaded it from the stack at nearly every access. Reserved at `rt::FIXED_BASE`
  (0x7fff0000, below 2 GB) it is a constant folded into each access's displacement (+11%).
  Reserve it first thing (`run`), before other allocations take the place.
- Per-call costs at ~2 million hooked calls and ~400 000 imports a second: a binary search
  for the hook (now its index in the stub), the thunk table's lock (now a copy per thread),
  an `Arc` clone of the thread's record at every loop head, the thread id from the system,
  four SipHash maps per primitive. Each was 1-4%.
- A game downloads the same texture rows again and again (Nuclear Strike marks a page's rows
  dirty, its picture restore marks them again: ~30 000 single-row uploads a second, 87%
  unchanged): compare with the kept raw texels and skip; upload the rest together.
- A picture between logic frames saving and restoring 625 KB of static data: a fresh `Vec`
  each time came from the system (page faults); keep one, and copy only the pages written
  (`MEM_WRITE_WATCH`, `Memory::written`).
- SwapBuffers costs 0.4 to 0.7 ms of the calling thread in the driver, also with nothing
  else to do: show pictures from a present thread of one's own (nuclear_rust_strike
  `window::present_later`).
- Hot game functions rewritten in Rust (projections, polygon set-ups) gained 6-20% each.
  Kept exact (same operations, order and rounding) and checked by a switch that runs both:
  not one difference in millions of calls. Lifting longer x87 runs under 0xc7f (over
  branches) gained nothing: the renderers' cost was their size, not the rounding.

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

## Widescreen

- Look for a separate x scale of the projection before hooking every projection: Nuclear
  Strike's `View_SetWindow_` makes x and y scales from the view window and every projection
  (fixed point and x87 assembly) reads them. Narrowing that one value by (4/3)/aspect (and
  what the game derives from it: clip margin and bound) was the whole Hor+ projection.
- The HUD may share the 3D picture's ordering table: no window switch or draw call to tell
  them apart. Tag the primitives allocated while the HUD function runs and switch the view
  per primitive at render time (nuclear_rust_strike `wide.rs`). Clear the tags when the HUD
  begins, not at the frame's end: the allocator reuses addresses every frame.
- Give the HUD the 4:3 projection scale while it runs: it sizes sprites and places marks with
  it, and marks projected that way and drawn unstretched land on the stretched 3D picture.
- Visible-area tables sized at level start (Nuclear Strike's tile count for TileMgr) cannot
  grow later: allocate for at least 16:9 at the start, widen up to that afterwards.
- `window_size()` called from the game thread returned a default (1280x960) in an earlier
  version: the Glide target never followed the real window. Read the size the window's thread
  last drew at.
- Screens with a panel of their own (Nuclear Strike's cockpit HUD modes: a full-screen frame
  around a smaller 3D window; mission menus) stay 4:3: stretched, the panel came out wide and
  its parts tagged as HUD were drawn again in the middle. Switch to 4:3 before the screen
  draws (hook the mode switch), not at the next swap.
- Before widening HUD marks, check whether the HUD projects anything at all: Nuclear Strike's
  HUD never calls a projection (its 3D sprites are sized by the x scale and stay round), so
  there was nothing to move.

## Drawing once

- Many HUDs draw a panel whole once (after a mode change) and then only the parts that
  change, trusting the card's buffers to keep the rest. Every time the port makes its render
  target anew (aspect, window size, MSAA samples) that panel is gone: find the game's "draw
  everything" flag (Nuclear Strike `[0x51f1b9]`, read by HUDUserDraw_) and set it for the next
  two HUD frames (one per buffer). A test passed at 640x480 only by luck: an extra picture
  between logic frames happened to draw the panel a second time.

## Game checks

- A mode the game allows at one resolution only (Nuclear Strike: cockpit HUD at 640x480)
  may well work at any: its layout was scaled by the screen size already. Patch the check
  out before compiling (`Game::code_patches`, the old bytes are checked), look for all of them
  (Nuclear Strike had two: one made the panel at level start, one let the key choose the
  mode; with only the second the game read a null panel and crashed).
- Patches written into guest memory after the start do not reach code: it was compiled
  ahead. Code patches go into the image before compiling, and so into the cache key.

## Vsync

- With vsync the swap does not tell when the refresh is: while the game is slower than the
  display the driver queues the picture and returns at once (Nuclear Strike at 480 Hz: swap
  and glFinish after it never waited). Keep the port's own pacing and only set the swap
  interval; when the pace runs ahead, the swap waits and the pace starts again from then.
- In a window the desktop's compositor already keeps tearing away; vsync matters in
  (borderless) fullscreen. With G-Sync/FreeSync there is no tearing below the display's
  maximum anyway: make vsync a setting of its own (off: no cap), not tied to a frame rate.

## Modern effects

- **Rule: tell the game's effects apart by the game's own functions, never by colour.**
  Hook the function that draws (or creates) an effect, note which effect is drawing, and tag
  the primitives allocated meanwhile (allocator hook: primitive -> effect; renderer hook: the
  one drawn now). Colour filters failed twice: Future Cop's heat shimmer, and Nuclear
  Strike's dynamic lights, where a boat's wake and water spray (additive, grey-blue) lit the
  water like fire; a "warm only" rule left them half in. Nuclear Strike: all effects are
  pyros drawn by one function (0x430970), the pyro's sequence at +0x38 says which effect;
  a list of sequences gives light (nuclear_rust_strike `lights.rs`).
- Find that function by logging which code allocates the effect primitives (the allocator's
  return address, counted by blend mode), then read its record for a kind/sequence field.
  Debug names help (Nuclear Strike's `pyro*`); the effect kinds usually come from one data
  file and are the same in every level.
- Keep a log of the effects by kind with their counts (`NSTRIKE_LIGHT_LOG=1`) and an
  environment override of the list: the user names what misbehaves, the log says its kind.

## Process

- Back up the game's settings files (key bindings!) before tests that start the game; some
  games rewrite them when they are missing.
- A savestate (`platform::savestate`) is the fastest way back to a bug deep in a level.
