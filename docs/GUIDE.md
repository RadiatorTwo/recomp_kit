# Guide: porting an old Windows game with recomp_kit

For games of the late 90s and early 2000s: a 32-bit Windows EXE (x86), DirectX 3 to 7 or
Glide, DirectSound, DirectInput. The game is not emulated. When the program starts, the
player's EXE is translated into machine code for their own computer (Cranelift), and Rust code
takes the place of Windows and DirectX. Every step has a **Done when ...**. Only then go on.

The new project gets a **copy** of the framework (`crates/recomp`, `crates/win32`,
`crates/tools`). The copy belongs to the game: change it wherever the game needs it. Tools:
`cargo build --release -p tools` in the game project gives `target/release/rtools`.

---

## 0. Ground rules (before anything else)

- Only **code** is shared. Players bring their own copy of the game.
- **Never commit** the EXE, data from it, translated code, textures, sounds or videos.
  `game_files/`, `*.iso` and `*.bin` are in `.gitignore`.
- Allowed: addresses, names and your own descriptions (`data/symbols.txt`), function bounds
  from Ghidra (`data/functions.json`), address maps between versions.

### Goal: better than the original, not the same

The port is not an emulator. It is not meant to run 100 % like the original on its hardware,
but **better**: faster, smoother, without the faults and limits of the technology of the time.
Three rules follow from that:

1. **Faulty game functions have no standing.** If they produce faults (wrong sorting,
   graphics glitches, crashes), they may be replaced by new Rust code instead of working
   around the fault with more and more effort.
2. **Game code may be changed when it brings an advantage and the result is identical.** A
   hot function (projection, polygon setup) runs in Rust if it gives the same result: the
   same operations in the same order, the same rounding, the original's quirks too. Every such
   replacement gets a check switch (`<PREFIX>_..._CHECK=1`: both run, differences are logged)
   and is only taken over when nothing differs over millions of calls (section 8a).
3. **Limits of the technology of the time may be lifted.** Whatever is only owed to the
   hardware of back then is made better, as long as the game stays the same game (its logic,
   its flow, its intended look): a fixed logic rate with pictures in between instead of 30/60
   fps (8b), widescreen (8c), high resolution, MSAA, texture filtering, a depth buffer instead
   of an ordering table that sorts wrong, locks to 640x480 (8d), vsync (8e), stutter from
   whole-number camera angles (8b point 7), idle loops that burn a core (LESSONS, Threads).
   After that come modern effects (section 11), as settings that can be switched off.

## 1. Create the project

```powershell
..\recomp_kit\tools\new-game.ps1 -Name gun_metal -Title "Gun Metal" -Target ..\gun_metal
```

The script copies the framework into `crates/` and notes in `PLAN.md` which recomp_kit commit
it came from. Then, in the new project:

1. Put the game's EXE into `game_files/<folder>/` (the CD's content or an installed folder).
2. Enter the EXE's SHA-256 in `VERSIONS` in `src/main.rs`:
   `certutil -hashfile GAME.EXE SHA256` (lower case, no spaces).
3. Fill in `Identity`: title, EXE name, folder, registry key and the CD label if the game
   checks for its CD. `rtools imports` shows whether `RegOpenKeyExA` is used. The key itself
   is plain text in the EXE (find it with `strings`, for example).

**Done when** `cargo build --release` works and the program finds the EXE ("Compiling the
game..." in the splash).

## 2. Find the functions

The recompiler translates one function at a time. There are two ways:

- **Without Ghidra** (the template's default): `find_functions` follows calls from the entry
  point and looks for code addresses in data. For Future Cop that gives 1778 of 2176. Watcom
  EXEs (sections BEGTEXT/DGROUP) work too (Motorhead: about 2200). With DLLs of the game
  (`Game::dlls`), `platform::module::seeds()` belong to the entry points.
- **With Ghidra** (more exact, recommended once it gets serious): analyse the EXE in Ghidra,
  export the functions as JSON (`[{"addr": "0x401000", "size": 123, "name": "FUN_00401000"}, ...]`)
  to `data/functions.json` and use `load_functions(include_str!(...))` in `functions()`.

To check: `rtools missing game_files\X\GAME.EXE` counts the functions and lists unsupported
instructions. `rtools dis <exe> <address>` shows one function the way discovery sees it.

**Done when** `rtools missing` reports a plausible number of functions.

## 3. Missing instructions

`rtools missing` lists instructions the lifter (`crates/recomp/src/lift.rs`) cannot handle yet.
Each one goes into the copy in the game project, with a test in `crates/recomp/tests/`
(examples: `misc.rs`, `x87.rs`). Such a test builds a small piece of x86 with iced, translates
it and compares it with the expected result. FPU instructions go in `lift/x87.rs`. New
instructions help every game, so also bring them back into recomp_kit.

**Done when** `rtools missing` reports "0 instructions not supported" and
`cargo test --release -p recomp` passes.

## 4. Imports: until the window opens

The game calls Windows functions (imports). At start the program lists the ones still missing
("N imports not implemented yet: ..."). If the game calls one of them, it stops with a message.

- `rtools imports <exe>`: all imports, by DLL.
- Implement all of the EXE's own imports early, not only when the game stops at them. The rare
  ones are called late (Recoil: CompareFileTime only at the end of the first level). Those of
  the DLLs loaded along with it (MFC, MSVCRT) mostly stay unused.
- Generally useful imports go into `crates/win32/src/platform/<dll>.rs` (table `IMPORTS`:
  name, number of stdcall arguments or -1 for cdecl, Rust function). Before writing one, look
  in the sibling ports (see the framework's PLAN.md: black_white has about 90 more kernel32
  functions, rust_metal Direct3D 9, rustschweine Direct3D 7 and CD audio, nuclear_rust_strike
  the Miles Sound System).
- DLLs that only concern this game: `Game::import` in the game project.
- Debugging: `<PREFIX>_TRACE_API=1` (every import call with its caller),
  `<PREFIX>_TRACE_MSG=1` (window messages), `<PREFIX>_WATCH=1` (where the game hangs, every
  2 s), `<PREFIX>_TRACE_REG=1` (registry).

**Done when** the game's window opens.

## 5. Self-modifying code

Old software renderers write into their own code (constants, jump distances). `Smc::find`
finds most of it by itself (`mov [code address], x`). `rtools smc <exe>` shows what was found.
If the game rewrites code from a table (Future Cop: 555/565 pixel formats), enter the table in
`Game::smc` (`PatchTable { at, variants }`) and check it with
`rtools smc <exe> --patch <address>:<n>`.

**Done when** the game no longer crashes at places that change their own code (crash messages
name the function).

## 6. Picture

- 2D menus through DirectDraw (`ddraw.rs`) mostly work right away.
- 3D: Glide (`glide.rs`, OpenGL) is done. Direct3D 7 and 9 are in the sibling ports
  (rustschweine `d3d.rs`, rust_metal `d3d9/`). Bring them into the framework when a game
  needs them.
- Screenshots: `<PREFIX>_SHOT=shot.png` (every 0.5 s. `{t}` in the name is replaced by the
  milliseconds, so every picture is kept), `<PREFIX>_WINDOW=1280x960`.

**Done when** the main menu on the screenshot looks like the original.

## 7. Scripted input (repeatable tests)

- `<PREFIX>_INPUT="5000:click@320,240 8000:Enter 9000:Up*2000"`: input at fixed times
  (ms after the game starts). Mouse positions are in 640x480 game pixels, `*n` holds for n ms,
  `text@Name` types, `pad@A*500` is the gamepad and `@file.txt` reads the list from a file.
- `<PREFIX>_RECORD=file.txt`: record your own input and play it back later with
  `INPUT=@file.txt`. F11 sets a mark with a screenshot.
- `<PREFIX>_NO_VIDEO=1`: videos end at once.

**Done when** a script leads into the first level the same way every time.

## 8. Hooks: your own Rust functions instead of the game's

For everything beyond "rebuilding Windows" (widescreen, frame rate, cheats, gamepad layout,
faults of the original): replace a game function with Rust.

**Rule** (section 0): original functions have no standing when they produce faults. They may
also be replaced for an advantage (speed) if the result is identical. Faithfulness to the
original does not extend to its faults and limits.

- In `src/hooks.rs`: `(Us(0x406a30), my_function)`. Addresses are those of the reference
  version (the first in `VERSIONS`). `.at()` converts them for other versions.
- `call_original(c, address)` calls the original function. Arguments come from
  `win32::platform::arg(c, k)`.
- Whatever you learn about a function goes into `data/symbols.txt` right away (address, name,
  description, "?" for guesses). Crash messages and `rtools dis` then show names.
- Debug commands: `Game::cheats` (name, Shift+F key, function). A hook on the frame wait
  function calls `cheats::run_pending(c)` and `savestate::frame()`.

## 8a. Speed: before modern effects

Fast first, then pretty. Effects (SSAO, shadows, lights, bloom) each cost 10 to 30 % of the
frame rate, and whatever was slow before does not get faster with them. That way Nuclear
Strike went from about 175 to about 790 fps without effects (mission 1). The goal was 500.

**Rule: with these old games the GPU is never the limit.** Their scenes have a few thousand
triangles, and any GPU of today draws them in a fraction of a millisecond. If the frame rate is
low, the cause is on the CPU side: translated game code, your own Rust code per call, too many
draw calls or state changes, uploads, waiting for the turn, the driver (SwapBuffers, glFlush,
synchronisation). Never explain it with "the GPU can't keep up" and do not start by cutting
shaders or resolution. Measure (point 1) and look on the CPU side.

In this order:

1. **Measure, don't guess.** `<PREFIX>_SAMPLE=1` (samples of the game thread every 2 s: game
   function or host module), `<PREFIX>_PROF=1` (time in imports and waiting for the turn). To
   resolve your own Rust code, use `<PREFIX>_SAMPLE_RAW=<file>` against a link map
   (`cargo rustc --release --bin <game> -- -C link-arg=/MAP:<file>`, with its own target
   folder) or `CARGO_PROFILE_RELEASE_DEBUG=line-tables-only` and llvm-symbolizer. Measure
   every change as A/B. Runs of the same build vary by about 5 % (LESSONS, Threads:
   hyperthreads).
2. **What the kit already brings** (nothing to do, but know that it is there): guest memory at
   a fixed address (`rt::FIXED_BASE`, the base is a constant in the code: +11 %), x87 in
   registers and 24-bit rounding inline (`lift/x87.rs`), a dense table for indirect calls, the
   hook index in the stub, thunks per thread, the turn with a time slice and priorities (loop
   heads do not call `rt_yield` while the slice lasts), Sleep(0) without spinning, texture rows
   that did not change are not uploaded again, and pictures in between only write back the
   pages they changed (`tween::Statics`, write watch).
   grLfbUnlock only draws the bands of rows the game wrote to (Recoil's HUD: the whole buffer
   cost about 0.6 ms per picture), the font file is looked up once per font (`Path::exists`
   opens the file on Windows), and `<PREFIX>_SAMPLE=1` also logs the pictures per second. For
   measuring, add a switch to the game that turns pacing off (Recoil: `RUSTCOIL_UNPACED=1`)
   and a replay that leads into the mission.
   If the profile by function is flat, measure by guest instruction:
   `<PREFIX>_SAMPLE=insn` logs the hottest instructions (the lifter remembers the guest
   address for every piece of machine code, `jit::guest_at`). `<PREFIX>_DUMP=<hex>` shows a
   function's machine code with the guest addresses (`<PREFIX>_DUMP_CLIF=1` the IR as well).
   With Recoil the time was then almost never in the game code itself but in what the lifter
   made of it. x87 runs now have a version of their own for the usual control words (53/64
   bit, round to nearest, `<PREFIX>_NO_USUAL87=1` turns it off), and flags are only computed
   where they are read (`<PREFIX>_FLAGS_ALL=1` turns that off). Unpaced, `LogicClock::frame`
   allows 64 pictures per logic frame. A bubble sort in the game (Recoil 0x4b13b0) is a case
   for point 4: sort stably in Rust, then carry on in the game code behind it (that address
   as an entry point of its own, with stack and registers as the game has them there).
3. **Cost per call in your own code.** At hundreds of thousands of calls a second everything
   counts: no locks, no `Arc` clones, no hash maps with SipHash, no `Vec` per primitive, no
   environment variable per call (read it once), no system calls (`glGetUniformLocation`, and
   `Instant::now` in hot loops: use `tsc`). Use thread-local copies, tables indexed by address,
   open addressing.
4. **Hot game functions in Rust (rule 2, section 0).** If the profile shows a game function
   with a large share (Nuclear Strike: two projections about 11 %, three polygon renderers
   about 15 %), write it as a hook in Rust, **identical**: the same operations in the same
   order, the same x87 rounding (under 0xc7f the SSE rounding set to truncate and every result
   cut to 24 bits, under 0x27f double, rounded to float when stored), and keep the original's
   quirks (registers it sets on the side, state it writes to guest memory). A check switch runs
   both and compares them (results, memory written, registers). Only take it over when nothing
   differs over millions of calls. Cases you do not rebuild (data not loaded yet) call the
   original.
   Not worth it: functions whose cost is the algorithm (inserting into a sorted list), not the
   translated code.
5. **Batch the drawing.** State the game changes per primitive (alpha test) belongs in the
   vertex data, not in uniforms that end a batch. A batch only ends at changes that really
   need it (blend, chroma key, filter, clip). glFlush only when a second thread draws. Collect
   uploads instead of doing them row by row.
6. **Take the buffer swap off the game thread.** SwapBuffers cost 0.4 to 0.7 ms per picture in
   the driver (about 15 % at 200 fps). That is why the kit shows Glide pictures from a present
   thread of its own (`window::present_later`, shared context). At the buffer swap the picture
   is copied on the GPU into one of two textures of its own, a fence is set and the picture is
   handed over. The present thread lets the GPU wait for the fence, draws into the window and
   swaps. To switch it off for comparison: `<PREFIX>_PRESENT=0`.
7. **Pacing with sleeping and spinning** (`tween::Pacer`, `sleep_until`). A sleep lasts up to
   1 ms longer than asked for, so the last 1.5 ms are spun (240 fps: gaps between pictures of
   4.0 to 4.4 ms instead of 3.7 to 4.7).

**Done when** the game runs well above the screen's refresh rate without effects (the goal is
a multiple of it, so the effects have room), all check switches run without differences and the
profile no longer shows a single item over about 10 % that could sensibly be replaced. Only
then go to section 11.

## 8b. Frame rate: fixed logic, pictures in between

Old games compute their logic for a fixed frame rate (often 30 or 60, PlayStation ports 60).
Never simply unlock the frame rate. A lot of logic counts frames instead of time, or takes
whole frames out of a time step (`dt >> 8`, `dt / step` with the rest thrown away). Above the
intended rate, explosions in Nuclear Strike never stopped, missiles left twice as much smoke
and the hook's autopilot never took a step (`256 / 0x114 = 0`). Instead
(`win32::platform::tween`, examples: Future Cop's `fps.rs`, Nuclear Strike's `pictures.rs`):

1. **Fixed logic.** Find the game loop (reads the clock, waits for the frame) and call
   `LogicClock::frame` at the start of every logic frame (a hook on the clock function, only
   from the loop: check the return address). The game's clock then moves exactly one frame
   per pass. With that the game runs like the original. **Test first**, then go on.
2. **Find the drawing part of the loop.** What is logic, what is drawing? Often they are mixed
   (Nuclear Strike: one pass moves the objects of a tile, the next draws them). Drawing code
   sometimes does logic too (a rotor keeps turning while it is drawn).
3. **Interpolate, one frame late.** Every picture, including the one of the logic frame
   itself, shows the game between the last two logic frames (`LogicClock::alpha`). Camera and
   objects (position, rotation matrix) are only put there *while drawing* (hooks on the
   drawing functions) and put back afterwards, so the logic only sees its own values. Whatever
   moves an object itself while drawing (a load on the hook) keeps its position. Rotations go
   through quaternions (`mix_rotation`. Mixed element by element, a fast rotor turns into a
   fan). Do not interpolate jumps (respawn, camera cut).
4. **Pictures in between.** In `LogicClock::frame`, before the wait, only the drawing part
   runs (buffer swap, terrain, objects, HUD, ordering table) as a picture of its own.
5. **Undo side effects.** After the buffer swap `Statics::save`, after drawing `restore`: the
   game's static data (random generator, helper matrices, counters, HUD state). Exceptions go
   in `left`: lists of a memory manager whose entries live elsewhere (put back, they destroyed
   the heap). While drawing, `hold_turn(true)`. Otherwise other threads run (audio timers) and
   their changes are lost too.
6. **Data that drawing prepares for the next frame** (terrain with two buffers): pictures in
   between draw from the state before the logic frame's picture was drawn (save it before,
   put it in for the picture in between, put it back afterwards). Otherwise the picture smears.
7. **Camera from the game's finest values.** Many games keep angles more exactly inside (16.16
   fixed point) than the camera they build from them (whole 1/4096 turns). In a curve the
   camera then turns unevenly (7, 8, 6, 8 per frame), and a camera circling the player at a
   distance jumps along: the picture shakes in curves, even with perfect interpolation
   (Nuclear Strike: `StrkCam_Update_` keeps `[0x52301c]..` in 16.16, `Camera_SetYPR_` gets
   `>> 16`). The fix, for display only: per logic frame remember the point being followed
   (the camera's place minus its offset, computed with *the game's matrix*, not one of your
   own, whose rounding times the distance adds noise again), interpolate the fine angles in the
   picture, build the matrix yourself in floating point (copy the formula from the game's
   matrix function and check it against its result) and place the camera from the remembered
   point. To recognise it: log the camera's and the player's place per logic frame. If the
   camera's steps vary and the player's do not, the cause is in the logic.
8. **Your own pause (overlay menu) like the game's pause.** If a menu of your own stops the
   logic, the fixed logic clock must not fall behind the game's clock (`clock`, GetTickCount)
   by the length of the pause. The game reads both. Nuclear Strike's Esc menu sets the mission
   start from the real clock when it resumes (`strikeClockPause_`). The mission time from the
   lagging logic clock then ran backwards, and a time step below 0 made `Slerp_` run about 4
   billion times: the game froze. The right way is to copy the pause the way the game does it.
   After the pause the logic clock jumps forward to the real clock (in whole ticks of the
   game), and the start times the game subtracts its time from (mission start, last tick) move
   on by the same pause. Test: open and close your own menu, then the game's pause menu.
9. **A late logic frame keeps its length.** If the game's wait loop finds its frame a few
   ticks late (a picture or a sleep ran over), the game takes the longer time as its time
   step but moves player and camera the same distance per frame: a stutter. For slightly late
   frames the clock returns the tick that was due, and the next ones catch up (Future Cop
   `fps.rs`: 1 to 7 ticks late).
10. **Pictures keep their own pace.** Do not restart the schedule of the pictures in between
    at every logic frame (otherwise only whole pictures fit in: 60 fps became 75). The logic
    frame's picture counts as one of the rate and waits for its slot. At most 2 periods ahead
    (the game's clock jumps back after a pause).
11. **Measure with a picture log.** For every picture shown: the picture time *and* the game
    time it shows. Uneven steps of the game time stutter even with pictures on time
    (future_rust_cop `framelog.rs`, `tools/framegraph.html`, which also reads CapFrameX).
12. **Measure what the renderer uses, not what you set yourself.** Your own interpolated
    values can run smoothly while the renderer draws from another copy. For every picture,
    after drawing (before putting things back), read the values it really used (the object's
    world matrix, the camera matrix the renderer built) and plot the object's place in the
    camera picture over display time. A saw tooth in it is the stutter. Counter tests with a
    shifted value can mislead: Recoil's tank did not vanish because it was drawn there, but
    because its bounding sphere was computed again and it was culled.
13. **World copies and hierarchies.** Many engines (Recoil: Zipper's, like MechWarrior 3)
    keep for each object its own placement *and* a world placement it is drawn from. The world
    placement of a root is often written by the logic, that of the child parts by the
    renderer, but only for nodes marked as changed, so only in the logic frame's picture. Then
    interpolate the root's world placement in every picture too (only where it equals its own:
    effects like exhaust trails, whose world placement the logic sets separately, otherwise
    became long streaks) and compute the children's yourself by the renderer's rule (check it
    against its result). After the picture, compute everything again from the newest values,
    do not write remembered world values back. A child's are one frame old when remembered,
    and written back it fell further behind frame by frame (object and picture drifted apart).
    Leave alone children whose world placement the logic moves itself (effects). You can tell
    them because they do not fit the rule or the logic changes them.
14. **Pictures on the screen's beat.** Wait first, then show (`GlideExt::before_shown`). If the
    pacing waits only after showing, the logic frame's picture, which takes longer to compute,
    is late every time. Your own clock drifts against the screen's: a picture for every two
    refreshes (240 on 480 Hz) landed near the boundary sometimes one refresh early, sometimes
    late. In a window the swap does not help (it does not wait for the refresh).
    `Pacer::pace_on` with `window::vblank()` (the compositor's times) puts every picture in the
    middle between two refreshes. The frame rate is a whole fraction of the refresh rate, at
    most what the game reliably manages.

What the faults look like: streaks (something is not drawn in pictures in between and the game
does not clear the screen), fans (a matrix mixed element by element), missing sounds (changes
of other threads put back), a crash after a memory request of 0 (heap lists put back), the
controls hang (the logic gets interpolated values, or a time step becomes 0), shaking only in
curves (camera from whole-number angles: point 7), freezing after your own menu and the game's
pause menu (the logic clock lags: point 8), an object lags behind and jumps ahead while its
surroundings do not, invisible on screenshots (it only moves in the logic frame's picture:
point 13, or pictures land on the wrong refresh: point 14).

## 8c. Widescreen (Hor+)

The goal: see more on the left and right, nothing stretched, and the HUD stays unstretched (in
the 4:3 middle or at the edges). Examples: future_rust_cop `wide.rs`/`walker.rs`,
nuclear_rust_strike `wide.rs`.

1. **Find the projection's x scale.** Many games have separate scales for x and y (Nuclear
   Strike: `View_SetWindow_` computes them from the view window, all projections read them).
   Multiply this x scale by k = (4/3) / aspect ratio, together with values the game derives
   from it (clip margin, right clip bound). If there is no such scale, multiply the camera x
   by k before every projection and every visibility test (Future Cop).
2. **Draw wide.** The Glide target gets the window's aspect ratio (`glide.rs`
   `resize(aspect, h)`), and the 3D picture is stretched over the full width (`View::Full`).
   Because the x scale is narrower by k, the proportions are right again.
3. **Recognise the HUD.** Draw the HUD's primitives in `View::Hud(0.5)` (the 4:3 middle). How
   to recognise them depends on the game. Future Cop does it through the display list
   (`walker.rs`). Nuclear Strike uses the primitives created during the HUD function (the
   allocator hooked, addresses in a set that is emptied when the HUD starts) and those under
   another view window (message window). Switch while drawing each primitive.
4. **While the HUD runs, the 4:3 scale.** The HUD reads the projection scale too (sprite sizes,
   markers). HUD markers projected from 3D positions and drawn unstretched land, with the 4:3
   scale, exactly where the stretched 3D picture shows the object. If their positions were
   already computed in the (narrow) 3D pass, they have to be moved (Future Cop `to_screen`:
   x' = cx + (x - cx) / k).
5. **Widen visibility.** Whatever the game sorts out in advance (tiles in front of the camera,
   view cone) is made wider by 1/k. Careful: amounts the game allocates memory for at level
   start (Nuclear Strike: the tile count for TileMgr) must be sized for the widest expected
   ratio (at least 16:9), and later only widened up to that.
6. **Wide only in the game.** Menus, videos and text stay 4:3. Wide only while 3D or HUD is
   drawn (Nuclear Strike: HUD drawn within the last 250 ms), and set the aspect at every swap
   for the next picture. With pictures in between (8b), make changes to static data before the
   picture is saved, otherwise writing back undoes them.
7. **Screens of their own stay 4:3.** Switch everything with its own frame over the whole
   screen (mission menu, cockpit HUD with a smaller 3D window) to 4:3 before it draws: hook the
   function that switches to it. If the game draws the frame only once and afterwards only the
   changed parts, set its "draw everything" flag after every time the render target is created
   again (LESSONS, Drawing once).

**Done when** in a 16:9 window the rotor or the wheels are round, the terrain reaches the edges,
HUD and message window are unstretched and menus stay 4:3. Check: switch on and off, the pause
menu, target markers over enemies (first see whether the HUD projects anything at all), all of
the game's HUD modes in every game resolution, and changing window size and MSAA while the game
runs.

## 8d. Getting around the game's locks

If the game allows something only in one resolution or configuration (Nuclear Strike: cockpit
HUD only at 640x480), the rest is often already general (positions by screen size). Then skip
the check before compiling: `Game::code_patches` gives (address, old bytes, new bytes), for
example a `jmp` over the check and `nop`s. Find every place (`rtools dis`, look for the compared
values like `280h`/`1E0h`), otherwise something prepared is missing and the game reads null
pointers. Patches to memory (`on_start`) do not reach code that is already compiled.

## 8e. Vsync

`gl::set_vsync(true)` sets the swap interval to 1 (from the next swap on). It is a setting of
its own, not tied to the frame rate (with G-Sync you often want it off). The pacing stays with
the port. The driver buffers pictures and the swap returns at once as long as the game is
slower than the screen (LESSONS, Vsync). The exception: if the chosen rate is at 97 % of the
refresh rate or above (or unlimited), draw pictures right after each other and let the swap do
the pacing. Otherwise your own clock next to the screen's misses refreshes (only visible in
CapFrameX).

## 8f. The port's own settings menu

`platform::options`: a menu of the port over the game (an overlay in the style of the dialogs),
on `Game::options_key` (F10. The game does not see the key, so choose one it does not use). The
game gives its pages with `Game::options`. Without pages there is no menu. Ready rows:
`fullscreen`, `window_size` (resolution), `anti_aliasing`, `textures`, `frame_rate_display`.
Your own with `Row::on_off` / `Row::choice` (use and save values at once, mostly with
`Setting`). What a game needs depends on the game: widescreen only with 8c, effects only once
there are any, the gamepad layout.

While it is open, `window::pump` waits in it. The game stands still and the guest's clock is
set back afterwards (GetTickCount, timeGetTime and QPC do not move on). The fixed logic clock
(`tween::LogicClock`) jumps to now afterwards, without catching up. If a game reads both clocks
in the mission, check the pause as in 8b, point 8 (mission time, timers). The game still gets
keys and buttons that are released, but not the mouse movement for DirectInput. Test: open and
close the menu in a mission and in the main menu. The game's mission clock must not move on by
the length of the pause.

Bindings: `Row::bind(name, note, text, bind, clear)` captures the next key after Enter
(`Bound::Key`, a DirectInput code, its name from `dinput::dik_name`) or the next pad button
(`Bound::Pad`, a number from `dinput::CONTROLS`). Delete clears it. Long pages scroll in smaller
text. It is best to change the keys in the game's own table (where it reads them, and save them
there: `advapi32::set_value_at` for a key other than the port's, see rustcoil `controls.rs`).
The pad works the menu itself (D-pad, A, B, X, LB/RB for pages, Start closes). Buttons in the
mission work as in rustcoil `gamepad.rs` (`Pad::buttons`, pressed as the command's key).

## 9. Sound, video, network, installation

- DirectSound and PlaySound: done (`dsound.rs`, mixed on cpal).
- MPEG-1 videos through DirectShow: done (`dshow.rs`, `mpeg/`). Smacker: compile the
  `SMACKW32.DLL` that comes with the game along with the EXE (`Game::dlls`, with a base
  outside our heap, see motorhead_rust). Its sound goes through waveOut (`waveout.rs`, into
  the mixer's stream). Bink and
  AVI are still missing.
- DirectPlay (TCP/UDP): done for what Future Cop needs (`dplay.rs`).
- Installation: `Identity::required` (what must lie next to the EXE), `game_files` (what moves
  along: sounds, fonts, save games), `video_marker` (a file in the CD's video folder). Setup
  takes a CD image (.cue/.bin, .iso, .img, .mdf, ISO 9660/Joliet or UDF, also 2448-byte sectors
  with subchannel), a copied CD or an installed game folder. If the CD holds the game only in
  an InstallShield archive (data1.cab, maybe data1.hdr), `cab_left_out` gives the file groups
  that are not installed (uninstaller, drivers, the videos if the CD also has them loose.
  `rtools iscab data1.cab` lists the groups). Setup then unpacks the archive itself (file by
  file under `.part`, the EXE last), also to supply files an installed game lacks (MFC DLLs in
  the Windows folder). Only an EXE with a known SHA-256 counts (launchers often have the same
  name, like Recoil's `Westwood\RECOIL\Recoil3Dfx.exe`). A CD check that looks for a video file
  is better passed with `code_patches`, so the game also runs without videos (Recoil
  0x42e78a). CD music (`cdaudio.rs`): the .cue in the game folder, in `game_files` (2 levels
  deep) or next to the program.

## 10. Difference test and release

- Difference test against Unicorn (checks every function against a real x86 emulator): still
  in the future_rust_cop repository (`src/bin/difftest.rs`), not in the framework (see
  PLAN.md).
- Release: set the version in `Cargo.toml` and push a tag `v0.1.0`.
  `.github/workflows/build.yml` builds Windows and Linux and makes a GitHub release.

## 11. Modern effects (after 8a)

Examples: future_rust_cop and nuclear_rust_strike (`ao.rs`, `lights.rs`, `shadows.rs`,
`bloom.rs`, `heat.rs`, `zbuffer.rs`, `particles.rs`). The basis is a depth per vertex (Glide
vertices mostly have none, so take it from the game's projections). Recognise effects by the
game's functions, never by colour (LESSONS, Modern effects). Every effect is a setting that can
be switched off, and each one is measured on its own.

The order in Future Cop: perspective-correct textures (from the same depth), a depth buffer
(the game only sorts roughly: terrain first, only opaque things write depth. LESSONS, Depth
buffer), SSAO, dynamic lights, shadows (a shadow map from the game's triangles, with a "ring"
for things just outside the picture. LESSONS, Shadows), heat shimmer, bloom, HDR with tone
mapping. Graphics glitches are fixed even if the original has them. Do not spend time proving
that it does.

Bloom is in the kit (`bloom.rs`, from rustcoil). Call `glide::bloom(rows)` once the 3D views are
drawn and before the HUD, with the view's rows (rustcoil `cut::bloom`). Add the row
`options::bloom()` to a page (setting `BloomLevel`: Off, Low, Medium, High). Only effects glow:
glide draws a glow mask beside the picture. A primitive that is blended and writes no depth
(fire, explosions, shots, the glow of lights) writes its alpha there, every other one writes 0.
Check first that this holds for your game: leave out exactly these primitives in a test
(`flush`) and look at what goes missing. In Recoil only the effects went, sky and sand stayed.

Settings the player (or a developer while testing) switches by hand do not need an environment
variable: a menu box, the registry or a Cargo feature for test builds (`--features cheats`).
Environment variables only for scripted tests.

---

## When something hangs or crashes

1. Crash: the message names the function and address → `rtools dis <exe> <address>`.
   "call to unknown address" with an address in the heap: the game jumps into code from its
   data files (Motorhead: fonts, sprites). Replace the game functions that jump there with
   hooks and interpret the loaded code (LESSONS, Discovery).
2. Hangs: `<PREFIX>_WATCH=1` → the last import call. Often a thread waiting for another one
   (see docs/LESSONS.md, "Threads").
3. Wrong result of a function: the difference test, or rebuild the function as a test snippet.
4. Graphics glitches: `<PREFIX>_SHOT` with `{t}`, compare the pictures.
5. Look through docs/LESSONS.md. Many traps are already described there.
