//! The Windows layer for recompiled games: the Windows and DirectX functions a game of the
//! late 1990s imports (kernel32, user32, gdi32, advapi32, DirectDraw, DirectSound, DirectInput,
//! DirectShow, DirectPlay, Glide), implemented in Rust on guest memory and shown through winit
//! and OpenGL; plus setting up the game files (from a CD image too) and a splash window.
//!
//! A game crate describes itself with `game::Game` and calls `run`.

pub mod game;
pub mod iso;
pub mod mpeg;
pub mod platform;
pub mod run;
pub mod setup;
pub mod splash;

pub use game::{After, Cheat, DialogExt, Game, GlideExt, Identity, Menu};
pub use run::run;
