# GAMETITLE in Rust

GAMETITLE (YEAR) recompiled to native code: your own copy of the game's EXE is translated to
the machine code of your computer when the program starts, and Windows and DirectX are
replaced by Rust code (OpenGL, winit, cpal, gilrs). Runs on 64-bit Windows and Linux.

Built with [recomp_kit](https://github.com/RadiatorTwo/recomp_kit).

## Playing

You need the PC version of the game. Download the release, unpack it, and put your CD image
(.iso or .bin) next to `mygame.exe` or copy the CD into `game_files` (see
`game_files/README.txt`). Start `mygame.exe`.

## Building

```text
cargo build --release
```

## License

MIT (the code here). The game and its files belong to their owners and are not part of this
project.
