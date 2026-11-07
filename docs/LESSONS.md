# Lessons

Traps found while porting Future Cop, Hogs of War, Gun Metal, Nuclear Strike and Black &
White. Most apply to any game of that time. Add new ones here.

## Threads

- Guest threads must take turns (`platform` turn lock), not run truly in parallel: games of
  the time assumed one CPU. Future Cop's async file reader and its callback share a one-slot
  mailbox and raced when run in parallel.
- Spin-waits: generated code checks `rt::POLL` at every loop head (target of a backward branch)
  and calls `rt_yield`, so a thread spinning on a flag lets the others run.

## x87

- The C runtime sets 53-bit precision, but renderers load their own control word (Future Cop:
  0xc7f, 24 bits, truncating). Arithmetic under any control word other than 53/64 bits to
  nearest goes through `rt_fpu_round` (exact rounding).
- `fild qword` / `fistp qword` is the compilers' 64-bit memcpy idiom: an exact integer shadow
  (`st_int`) keeps it bit-exact.

## CPU

- CPUID: report family 5/6 and no MMX unless the lifter does all of MMX (it does only
  movd/movq). Libraries (Bink, QMixer, D3DX) take their plain paths then.
- Keep AC and ID through pushfd/popfd: the CPUID presence test flips ID.
- `in` from the gameport reads 0xff (nothing connected); `out` does nothing.

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
