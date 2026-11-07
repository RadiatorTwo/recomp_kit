//! GAMETITLE recompiled to native code with recomp_kit.
//!
//! ```text
//! mygame [path to the EXE]     the game (default: game_files next to the program)
//! ```

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod hooks;

use recomp::smc::SmcSpec;
use recomp::ver::Version;
use win32::{Game, Identity};

/// The versions of the game's EXE this port knows; the first is the reference version: every
/// address in this crate (hooks, symbols.txt) is one of its addresses (`Us(0x...)`).
static VERSIONS: [Version; 1] = [Version {
    // TODO: `certutil -hashfile GAME.EXE SHA256` (Linux: sha256sum), lower case
    sha256: "0000000000000000000000000000000000000000000000000000000000000000",
    name: "US",
    map: None,
}];

static ID: Identity = Identity {
    title: "GAMETITLE",
    // TODO: the prefix of the debug switches (MYGAME_INPUT, MYGAME_SHOT, ...)
    env_prefix: "MYGAME",
    exe: "GAME.EXE",
    versions: &VERSIONS,
    // where the original installer put the game (GetModuleFileName answers with it)
    install_dir: r"C:\Games\GAMEFOLDER",
    // the game folder's name in game_files (and on the CD, if the CD has one)
    folder: "GAMEFOLDER",
    cd_folder: None,
    // the CD's volume label and serial, if the game checks for its CD (GetVolumeInformationA)
    cd: None,
    computer_name: "PC",
    // TODO: the key the game's installer made (look for RegOpenKeyExA in `rtools imports` and
    // the strings in the EXE)
    registry_key: r"HKEY_LOCAL_MACHINE\Software\COMPANY\GAMETITLE",
    registry_seed: &[],
    // files and folders next to the EXE without which the game cannot start
    required: &[],
    // other files kept in the game folder (saves, the game's own settings)
    game_files: &[],
    // a file in the CD's video folder, if the game has videos there
    video_marker: None,
    // network discovery (DirectPlay over UDP): 8 bytes of the game's own, question and answer
    net_magic: (b"GAMENET?", b"GAMENET!"),
    log_file: "mygame.log",
};

struct MyGame;

impl Game for MyGame {
    fn identity(&self) -> &'static Identity {
        &ID
    }

    fn functions(&self, img: &recomp::image::Image) -> Vec<(u32, u32, String)> {
        // a Ghidra export, when there is one (only function boundaries: fine to commit):
        // recomp::image::load_functions(include_str!("../data/functions.json"))
        let mut code = recomp::discover::Code::new(img);
        recomp::discover::find_functions(&mut code, &[(img.entry, "entry".into())])
    }

    fn smc(&self) -> SmcSpec {
        // self-modifying code beyond `mov [code], x` (see `rtools smc`)
        SmcSpec::default()
    }

    fn hooks(&self) -> Vec<(u32, recomp::rt::Hook)> {
        hooks::all()
    }

    fn on_start(&self) {
        recomp::image::set_symbols(include_str!("../data/symbols.txt"));
    }
}

static GAME: MyGame = MyGame;

fn main() {
    win32::run(&GAME)
}
