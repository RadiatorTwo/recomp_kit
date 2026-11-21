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

- With these old games the GPU is never the limit: a few thousand triangles a picture take a
  modern GPU a fraction of a millisecond. A low frame rate is on the CPU side (translated game
  code, our per-call cost, draw calls and state changes, uploads, the turn, the driver's
  SwapBuffers / glFlush / synchronisation). Never blame the GPU, never cut shaders or
  resolution first: measure and find the CPU cost.
- Measure before guessing: `<PREFIX>_SAMPLE=1` samples the thread that swaps Glide's buffers
  (recompiled function, or host module unwound to our own frame), `<PREFIX>_SAMPLE_RAW=<file>`
  all samples; symbolise our own addresses with a build with
  `CARGO_PROFILE_RELEASE_DEBUG=line-tables-only` and llvm-symbolizer. `<PREFIX>_DUMP=<hex>`
  disassembles one function's compiled code; with both set the sampler counts that
  function's samples by offset in its machine code (the dump's offsets). Hooked functions
  count by their lifted originals (`Dispatch::hooks`): left out, they counted for whatever
  function lay before them in memory (Future Cop: a 3-instruction function at 21%).
- The frame rate varies by ~10% from run to run (the same scene, the same build): a gain of a
  few percent does not show in fps. Time the code itself (a timer around the path, both
  variants in one build behind a switch, alternating runs). `<PREFIX>_STATS=1` shows frames a
  second and frame times in the window (no RivaTuner hooking our GL).
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
  Reserve it first thing (`run`), before other allocations take the place. Future Cop, without
  such large functions, still gained ~5% (with the hook index in the stub).
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
  else to do: show pictures from a present thread of one's own (`window::present_later`;
  Nuclear Strike ~202 -> ~243 fps). The game may draw on into its buffer at once (and some
  draw only what changes): the present thread gets a GPU copy, two copies take turns, and
  fences both ways keep the GPU from reading a copy being made or overwriting one not yet
  drawn. Not a gain everywhere: Future Cop (~650 fps, one GPU copy a picture) was slower with
  it (692 -> 638); measure per game, `<PREFIX>_PRESENT=0` turns it off.
- NVIDIA's "Vulkan/OpenGL present method: prefer layered on DXGI swapchain" (driver setting)
  made a windowed OpenGL game slower, not faster: Future Cop 792 -> 517 fps, its thread waiting
  ~39% in SwapBuffers instead of ~14%. Leave it on Auto.
- Hot game functions rewritten in Rust (projections, polygon set-ups) gained 6-20% each.
  Kept exact (same operations, order and rounding) and checked by a switch that runs both:
  not one difference in millions of calls. Future Cop's Glide display list walker (~1000
  instructions, ~25% of its game thread) in Rust: ~490 -> ~645 fps; its locals kept in the
  game's stack frame, so even the values the game leaves unset come out the same, and the
  check compares the Glide calls of both (traced, ours not done). Its terrain pass (projection,
  culling, display list records; ~6% of the game thread) was integer code only: exact by
  following the instructions, checked by comparing the outputs (the vertex buffer, the records
  appended, byte for byte), ~675 -> ~729 fps. Port a hooked callee inside its caller: the
  hook's round trip per call (thousands a picture) goes too.
- Before porting the kit's x87 work (rounding inline, register cache), look for `rt_fpu_round`
  in the profile: in Future Cop it was not among the top 25, the work was not worth it.
- Each framebuffer switch cost NVIDIA's driver ~7 µs of the calling thread. A bloom chain with a
  framebuffer per level (14 switches a picture): one framebuffer, the levels attached in turn,
  ~135 -> ~114 µs a picture. Lifting longer x87 runs under 0xc7f (over
  branches) gained nothing: the renderers' cost was their size, not the rounding.
- Upload each batch's vertices with glBufferSubData, not by mapping the buffer's range: every
  map made NVIDIA's driver thread catch up first (Future Cop with its effects on: 23-31% of the
  frame; 354 -> ~555 fps together with the next two points).
- No set lookups inside a sort's comparison: Future Cop's walker sorted the display list by
  "is terrain" / "is cast-only" with a std HashSet looked up in every comparison (~15% of the
  frame). Look each record's key up once, then one stable sort by it; sets of guest addresses
  take a fast hash (Fibonacci / multiply-shift), not SipHash.
- Ask the driver nothing per picture: a blit's uniform locations asked every picture waited for
  the driver too (even a few calls). Cache them (`gl::uniform`).

## Windows

- VirtualAlloc hands out 64 KB aligned blocks (Windows' allocation granularity): games' own
  heaps may rely on it (Nuclear Strike's SSMemAllocAlign corrupted its free list at 4 KB).
- A game may draw from more than one thread (Nuclear Strike's text before a mission): GL
  objects that contexts do not share (vertex arrays, framebuffers) are made per thread, and
  a thread's GL work must be flushed before another context uses it (only while two draw:
  flushing every call cost ~7%). The window's thread may be busy running the game: it makes
  a shared context ahead, so a thread drawing first does not wait for it.
- PlayStation ports may divide by zero on purpose-free paths (a depth of exactly 0): the
  PlayStation does not trap. `Game::div_by_zero_goes_on` divides by 1 and logs once.

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
- Gamepad triggers do not all rest at 0: a "HID-compliant game controller" through gilrs rests
  at ~0.45, and a fixed threshold (0.3) fired the heavy weapon all the time. Note each pad's
  lowest trigger value seen (the first read to start with) and count 30% of the travel beyond
  that rest as pressed (future_rust_cop `dinput.rs` `trigger_down`; the kit's `dinput.rs` still
  has the fixed 0.3).
- DirectInput gets input without the game's message loop: a game may wait for a key with
  Sleep and GetDeviceData only (Recoil's briefing). DirectInput reads fetch the window's input
  (`user32::poll_input`), else such a wait never ends.
- Scripted runs (`<PREFIX>_INPUT`) never catch the cursor: the window takes the focus and would
  lock the user's mouse while Claude tests.
- Glide textures: s and t go 0..256 over the longer side for every texture size (scale by
  size / 256); a tmu's oow may be 0 (use the vertex's, also for the texture rectangle); small
  textures repeat over a primitive (wrap inside the texture, its layer is larger). A game may
  download hundreds of small textures at once (Recoil 621 at a mission's start): the layer
  array grows. Never glGenerateMipmap a large array per download: hundreds of them at a
  mission's start took the NVIDIA driver down ("Unable to recover from a kernel exception",
  exit 0xC0000409, in the Application event log); mipmaps per texture on the CPU.
- Glide draws at the window's resolution, so a mouse position in the shown picture's pixels
  is not in the game's (Recoil: 1280x960 against 640x480). grSstWinOpen calls
  `window::set_screen`: input positions are then in the game's screen pixels.
- Games that move their own cursor by DirectInput's mouse motion (times a sensitivity: Recoil
  ~1.2) drift away from the system pointer in a window. Hook the cursor update: in the menus
  put the cursor where the pointer is (`user32::mouse_pos`) and keep the motion for the game.
- Videos drawn as textures (Recoil's menu) that cannot be opened (no decoder) may have zero
  frames, and the game's `time * rate % frames` divides by zero: leave such textures out.
- MFC 4.2 games: load the game's own MFC42 / MSVCRT / MSVCP50 as guest DLLs and compile them
  with the EXE (`Game::dlls`, `platform/module.rs`) instead of writing MFC in Rust. MFC
  subclasses windows in a WH_CBT hook called inside CreateWindowExA before WM_NCCREATE.

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
- A logic frame found late (a picture or a sleep ran past its start) must not become a longer
  frame: Future Cop's frame wait took the 9 ticks it found as frame time and game time, but
  its logic moves player and camera as far in every frame, so things went slower for that
  frame (~1% of frames, a stutter every few seconds). Return the due tick for frames 1...7 ticks
  late; the next frames catch up. Leave larger delays (a whole frame or more) to the game.
- Keep the pictures' own schedule across logic frames: started again at each frame, only whole
  pictures fit into a frame (60 fps gave 75, 100 gave 112.5 at 37.5 logic frames). The frame's
  own picture counts as one of the rate's and waits for its slot (else at 100 fps pictures came
  1, 5, 3, 3, 2, 4 ticks apart); the schedule runs at most 2 periods ahead, since the game's
  clock goes back after a pause and starts again with each level.
- Log a line per picture shown with the game time it shows, not only its frame time: uneven
  steps of the shown time stutter even when every picture comes on time (future_rust_cop
  `framelog.rs`, `examples/framelog.rs`, `tools/framegraph.html`, which also reads CapFrameX
  captures). Count a hitch at 1.5 times the median and 4 ms over it (1.5 times alone counts
  noise at high rates).

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
- At a chosen rate near the display's refresh (97% of it or above) or unlimited, with vsync
  on, let the swap pace the pictures: draw one after another. Our clock beside the screen's
  missed refreshes: at 120 fps on 120 Hz a picture 1.4 ms late after the game's own frame was
  shown twice or replaced unseen, ~2 a second. Only CapFrameX (display changes) showed it; our
  own frame log showed 10 then 6.6 ms and looked fine.
- At 480 Hz Future Cop showed ~472 fps with vsync: our work between swaps was ~0.75 ms, the
  loss was inside SwapBuffers (~10 swaps a second waited two refreshes). Not the pacing's
  fault: do not tune it for that. Overlays (RTSS) hook SwapBuffers too.

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
- A primitive finds its display list record by its first vertex through a sorted index, not by
  looking a few records ahead: with the terrain moved to the front a window of 16 lost the
  fog's smoke (not lit, dark).

## Depth buffer

- Games of the time sort by depth buckets, not a depth buffer: ground tiles painted over
  decals, faces in the wrong order. A depth buffer of our own (Future Cop `zbuffer.rs`), depth
  from each vertex's z taken from the game's projections. Draw the terrain's records first,
  keeping their order and the rest's; semi-transparent decals (oil) go after the terrain, not
  among its tiles (they flickered there).
- Only solid primitives write depth, in a depth-only draw first that discards what is not
  opaque (alpha < 0.75: keyed holes, lattices, and translucent surfaces: the PlayStation's
  semi-transparency at alpha 0x80; a pipe dipping into a translucent pool was cut at its
  surface). Writers write a little farther (0.1%: inside it the game's order decides, coplanar
  surfaces fought under a strict test), test-only primitives (blended, darkening) are pulled
  0.4% nearer.
- A ground point and a model point can land on the same screen place: keep both z with their
  kind, and give a vertex the kind its primitive's other corners have (else a ground tile
  tipped forward over a pickup box).

## Shadows

- A shadow map of the game's own triangles (those that write depth) beats a screen-space ray
  march: in the ray march the casters' shapes could not be made out.
- Only what the game draws casts, so the map lacks: faces the game culls as turned away (hook
  its winding test, keep them as cast-only records while the display list has room), things
  just off the screen (draw them again with the projection zoomed out, cast-only: actors,
  static objects, terrain; test each with its sphere swept away from the sun against the
  view), and faces off the screen of things crossing the screen's edge (the game left them out:
  a shed's roof above the picture cast nothing until walked toward). Never cull cast-only
  records by the screen's box (every off-screen caster was lost), draw them after the rest
  (their state changes split batches: 250 -> 350 fps), keep the caster buffer's storage.
  Flat ground never casts.
- Casters out to 40000 units (12000 faded the upper half of the usual view: shadows faded in
  walking forward).
- Fit the map to what is seen (the casters' corners on the screen; depth from all casters):
  texels ~2.8 instead of ~4.5 units. Against crawling edges: the square's side in steps of
  2^(1/4), its corner on texels of a grid fixed in the world, its axes fixed in the world.

## HDR and tone mapping

- A render target made anew when a setting changes (RGBA16F for tone mapping, MSAA samples):
  the post passes' copies and passes must follow the format the target was made in, not the
  setting. A curve chosen mid-frame changed the copies before the target: a multisample blit
  between mismatched formats, one black frame.

## Process

- Back up the game's settings files (key bindings!) before tests that start the game; some
  games rewrite them when they are missing.
- A savestate (`platform::savestate`) is the fastest way back to a bug deep in a level.
- Savestates: give a new guest thread its TIB and stack in CreateThread / timeSetEvent, not when
  it first runs: that timing moved every heap block after them, and the check against the saved
  heap failed at random (future_rust_cop `thread_stack`; the kit's `new_thread_cpu` still runs
  in the new thread).
- Replay a user's recording (`<PREFIX>_RECORD`) with exactly their settings: another window
  size or renderer switch makes the replay diverge and a savestate not match.
- Things the user tests by hand (cheat keys, comparison switches) must work without an
  environment variable: a Cargo feature (`cheats`, never in a release), a menu box or the
  registry. Environment variables are for scripted tests only.
