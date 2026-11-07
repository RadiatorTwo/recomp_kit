//! A check of the framework: Future Cop: L.A.P.D. (1998) with only its `Identity`, its
//! self-modifying code's patch table and its function list, no hooks. It should reach the main
//! menu and play the first mission as the original does (4:3, the game's own input).
//!
//! The function list (a Ghidra export) is read at run time from `FCOP_FUNCTIONS` (default:
//! future_rust_cop's data/functions.json next to this repository); nothing derived from the
//! game is in this repository.
//!
//! ```text
//! cargo run --release -p fcop_plain -- <path to FCopLAPD.exe>
//! ```

use recomp::smc::{PatchTable, SmcSpec};
use recomp::ver::{Us, Version};
use win32::{Game, Identity};

static VERSIONS: [Version; 1] =
    [Version { sha256: "2dcdd1e78b99947d125712e1a159548e6f5f3929020c29af9e156314ed1c7e5e", name: "US", map: None }];

static ID: Identity = Identity {
    title: "Future Cop: L.A.P.D.",
    env_prefix: "FCOP",
    exe: "FCopLAPD.exe",
    versions: &VERSIONS,
    install_dir: r"C:\FCopLAPD",
    folder: "FCopLAPD",
    cd_folder: Some("FCopLAPD"),
    cd: Some(("FCOP_CD", 0x1998_0001)),
    computer_name: "FCOP",
    registry_key: r"HKEY_LOCAL_MACHINE\Software\Electronic Arts\Future Cop",
    // the installer's renderer choice: Glide
    registry_seed: &[("Group", "3Dfx")],
    required: &["febmp.bin", "missions"],
    game_files: &["controls.txt", "save.cfg"],
    video_marker: Some("intro.mpg"),
    net_magic: (b"FCOPNET?", b"FCOPNET!"),
    log_file: "fcop_plain.log",
};

struct FutureCop;

impl Game for FutureCop {
    fn identity(&self) -> &'static Identity {
        &ID
    }

    fn functions(&self, _img: &recomp::image::Image) -> Vec<(u32, u32, String)> {
        let path = std::env::var("FCOP_FUNCTIONS").unwrap_or("../future_rust_cop/data/functions.json".into());
        let json = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e} (set FCOP_FUNCTIONS)"));
        recomp::image::load_functions(&json)
    }

    fn smc(&self) -> SmcSpec {
        // the software renderer rewrites code for the 555/565 pixel formats from this table
        SmcSpec { patch_tables: vec![PatchTable { at: Us(0x48ea20).at(), variants: 2 }], imm_fields: Vec::new() }
    }
}

static GAME: FutureCop = FutureCop;

fn main() {
    win32::run(&GAME)
}
