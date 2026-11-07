# recomp_kit

A framework for native ports of late-1990s Windows games: the player's own 32-bit x86 EXE is
recompiled to native code with Cranelift when the program starts, and Windows, DirectX and
Glide are implemented in Rust (winit, OpenGL, cpal, gilrs). Ports run on 64-bit Windows and
Linux. Extracted from [Future Cop: L.A.P.D. in Rust](https://github.com/RadiatorTwo/future_rust_cop).

- `crates/recomp` — discovery, x86 → Cranelift lifter (x87 included), self-modifying code, JIT
  with a cache, runtime
- `crates/win32` — kernel32, user32, gdi32, advapi32, DirectDraw, DirectSound, DirectInput,
  DirectShow (MPEG-1), DirectPlay, Glide; dialogs, gamepad, setup from CD images
- `crates/tools` — `rtools`: imports, unsupported instructions, disassembly, SMC, dialogs, CD
  images, movies, version maps
- `template/` — a new game project (`tools/new-game.ps1`)
- `docs/GUIDE.md` — how to port a game step by step (German), `docs/LESSONS.md` — known traps

A game describes itself with `win32::Identity` and adds what it needs through the
`win32::Game` trait:

```rust
static GAME: MyGame = MyGame;
fn main() {
    win32::run(&GAME)
}
```

Only code is here. Games and their files belong to their owners; players bring their own copy.

MIT license. The MPEG-1 decoder is a Rust port of pl_mpeg by Dominic Szablewski (MIT).
