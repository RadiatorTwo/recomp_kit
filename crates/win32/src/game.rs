//! What a game tells the platform layer: who it is (`Identity`: names, paths, registry key,
//! the CD it checks for) and where it goes beyond plain Windows (`Game`: hooks, its own
//! imports, menus by gamepad, dialog extensions, Glide extras, scripted-input names).
//!
//! A game crate implements `Game` on a static and calls `run(&GAME)` from its `main`. Every
//! method has a default that does nothing, so a new port starts with only `identity`.

use crate::platform::Import;
use crate::platform::dialog::Dialog;
use crate::platform::dialog::template::Template;
use recomp::rt::{Cpu, Hook};
use recomp::smc::SmcSpec;
use recomp::ver::Version;
use std::sync::OnceLock;

/// Who the game is: names, paths and what its installer and CD leave behind.
pub struct Identity {
    /// shown in the window's and the splash's title: "Future Cop: L.A.P.D."
    pub title: &'static str,
    /// prefix of the environment switches without "_": "FCOP" (<PREFIX>_INPUT, <PREFIX>_SHOT...)
    pub env_prefix: &'static str,
    /// the EXE's file name: "FCopLAPD.exe"
    pub exe: &'static str,
    /// the known versions of the EXE (SHA-256, map); the first is the reference version
    pub versions: &'static [Version],
    /// where the installer put the game, as the game sees it: "C:\\FCopLAPD"
    pub install_dir: &'static str,
    /// the game's folder on its CD ("D:\\FCopLAPD\\Missions\\..." is the game folder), if any
    pub cd_folder: Option<&'static str>,
    /// the CD's volume label and serial number (games check them), if the game has a CD
    pub cd: Option<(&'static str, u32)>,
    /// GetComputerNameA
    pub computer_name: &'static str,
    /// the game's registry key: "HKEY_LOCAL_MACHINE\\Software\\Electronic Arts\\Future Cop"
    pub registry_key: &'static str,
    /// REG_SZ values the installer wrote into that key (Future Cop: Group = 3Dfx)
    pub registry_seed: &'static [(&'static str, &'static str)],
    /// the game folder's name in `game_files` (and on the CD): "FCopLAPD"
    pub folder: &'static str,
    /// files and folders next to the EXE the game cannot run without: "febmp.bin", "missions"
    pub required: &'static [&'static str],
    /// other files `setup` keeps in the game folder (saves, the game's settings): "save.cfg"
    pub game_files: &'static [&'static str],
    /// a file in the CD's video folder (`setup` copies that folder), if the game has one
    pub video_marker: Option<&'static str>,
    /// 8 bytes each: the network discovery question and answer (UDP)
    pub net_magic: (&'static [u8; 8], &'static [u8; 8]),
    /// the log file next to the program in release builds: "fcop.log"
    pub log_file: &'static str,
}

/// A game's own behaviour on top of the platform layer. Addresses are in the version that
/// runs (`recomp::ver::Us::at`).
pub trait Game: Sync {
    fn identity(&self) -> &'static Identity;

    /// The game's functions: (start, end, name), sorted. Default: none (the caller must give
    /// a Ghidra export or run `recomp::discover::find_functions`).
    fn functions(&self, _img: &recomp::image::Image) -> Vec<(u32, u32, String)> {
        Vec::new()
    }

    /// A division by zero goes on (divided by 1, logged once per place) instead of stopping
    /// the game: for ports of PlayStation games, whose division does not trap.
    fn div_by_zero_goes_on(&self) -> bool {
        false
    }

    /// Self-modifying code beyond `mov [code], x` (patch tables, rewritten immediates).
    fn smc(&self) -> SmcSpec {
        SmcSpec::default()
    }

    /// Changes to the game's code before it is compiled: (address, its bytes, the new ones).
    /// Code is compiled ahead, so a change written into guest memory later (`on_start`) does
    /// not reach it. For checks the port does not want (a mode the game allows only at one
    /// resolution, ...): a jump over the check, nops. The old bytes are checked: another
    /// version of the EXE stops with an error instead of being patched wrong.
    fn code_patches(&self) -> Vec<(u32, &'static [u8], &'static [u8])> {
        Vec::new()
    }

    /// Game functions replaced by Rust functions (`rt::call_original` runs the original).
    fn hooks(&self) -> Vec<(u32, Hook)> {
        Vec::new()
    }

    /// An import of the game's own (checked before ours): e.g. a DLL only it uses.
    fn import(&self, _dll: &str, _name: &str) -> Option<&'static Import> {
        None
    }

    /// Once guest memory holds the EXE, before its first instruction (patch tables, defaults).
    fn on_start(&self) {}

    /// The main menu for the gamepad (`menupad`): None when no menu is up.
    fn menu(&self) -> Option<Menu> {
        None
    }

    /// Whether a video plays (the pad's A is Space then: skips it).
    fn video_playing(&self) -> bool {
        false
    }

    /// The key a WH_KEYBOARD hook procedure `hook` gets for window message (msg, wp, lp), if
    /// it gets one. Default: key messages only, their virtual key.
    fn keyboard_hook_key(&self, msg: u32, wp: u32, _lp: u32, _hook: u32) -> Option<u32> {
        matches!(msg, 0x100 | 0x101 | 0x104 | 0x105).then_some(wp)
    }

    /// Debug commands by name in `<PREFIX>_INPUT` ("30000:win") and on Shift+F<n>.
    fn cheats(&self) -> &'static [Cheat] {
        &[]
    }

    /// Dialog extensions: the game's dialogs changed or our own controls added.
    fn dialogs(&self) -> Option<&'static dyn DialogExt> {
        None
    }

    /// Glide extras (widescreen, texture recolouring, frame pacing).
    fn glide(&self) -> Option<&'static dyn GlideExt> {
        None
    }

    /// Whether a savestate may be made now (Future Cop: not in a network game).
    fn savestate_ok(&self) -> Result<(), &'static str> {
        Ok(())
    }

    /// A savestate was loaded (forget what was kept of earlier frames).
    fn on_savestate_load(&self) {}

    /// The host address for joining a network game (empty: the local network). Default: the
    /// REG_SZ `JoinAddress` in the game's key.
    fn join_address(&self) -> String {
        crate::platform::advapi32::value("", "JoinAddress")
            .map(|(_, d)| d.iter().take_while(|&&b| b != 0).map(|&b| b as char).collect())
            .unwrap_or_default()
    }

    /// A subcommand of the program (`fcop export ...`): Some(exit code) if it is one.
    fn command(&self, _args: &[String]) -> Option<i32> {
        None
    }
}

/// A debug command: its name and what it does (in the frame wait, on the game's thread).
pub struct Cheat {
    pub name: &'static str,
    /// Shift+F<n> runs it with `<PREFIX>_CHEATS=1` (0: no key)
    pub key: u8,
    pub run: fn(&mut Cpu),
}

/// The menu shown, for the gamepad: what the mouse can click and where the mouse is (frame
/// pixels, 640x480).
pub struct Menu {
    pub items: Vec<(i32, i32, i32, i32)>,
    pub mouse: (i32, i32),
}

/// What the game adds to dialogs (Future Cop: its Preferences gets our settings).
pub trait DialogExt: Sync {
    /// Change a template before it is built.
    fn adjust(&self, _template: u32, _t: &mut Template) {}
    /// After the game's WM_INITDIALOG: fill our controls.
    fn init(&self, _d: &mut Dialog) {}
    /// One of our controls (ids from 0x7E00) was used; may name a dialog to open on top.
    fn command(&self, _d: &mut Dialog, _id: u32) -> Option<After> {
        None
    }
    /// OK: save our controls.
    fn ok(&self, _d: &mut Dialog) {}
    /// The gamepad's A on item k (a pad user cannot type).
    fn pad_click(&self, _d: &mut Dialog, _k: usize) {}
    /// A dialog of our own (not in the EXE), by its number.
    fn template(&self, _id: u32) -> Option<Template> {
        None
    }
    /// A game-defined action after a command (`After::Game`).
    fn after(&self, _c: &mut Cpu, _what: u32) {}
}

/// What follows a command in a dialog.
#[derive(Clone, Debug)]
pub enum After {
    /// open the game's dialog template with its procedure
    Open(u32, u32),
    /// call a game function with these arguments
    Call(u32, Vec<u32>),
    /// open our own dialog (`DialogExt::template`)
    Ours(u32),
    /// `DialogExt::after(what)`
    Game(u32),
}

/// What the game adds to the Glide renderer.
pub trait GlideExt: Sync {
    /// The window's aspect for the next frame (None: Glide closes); returns the aspect drawn
    /// (4/3 unless the game does widescreen).
    fn aspect(&self, _window: Option<f32>) -> f32 {
        4.0 / 3.0
    }
    /// A texture download (RGBA) with its hash (`hd::hash`), before it is stored.
    fn recolour(&self, _key: u64, _rgba: &mut [u8]) {}
    /// The gamma a frame is shown with, from the game's.
    fn gamma(&self, game: f32) -> f32 {
        game
    }
    /// A frame was shown (grBufferSwap).
    fn frame_shown(&self) {}
}

static GAME: OnceLock<&'static dyn Game> = OnceLock::new();

/// The game that runs.
pub fn game() -> &'static dyn Game {
    *GAME.get().expect("no game set (win32::run)")
}

/// The game's identity.
pub fn id() -> &'static Identity {
    game().identity()
}

/// Set the game (once, first thing).
pub fn set(g: &'static dyn Game) {
    if GAME.set(g).is_ok() {
        recomp::config::set_env_prefix(g.identity().env_prefix);
    }
}
