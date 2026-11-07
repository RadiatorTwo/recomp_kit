//! Debug commands (`Game::cheats`): by name in `<PREFIX>_INPUT` ("30000:win") and, with
//! `<PREFIX>_CHEATS=1`, on Shift+F<n> (the game does not see those keys). A command only asks;
//! it runs at the game's next `run_pending` (the game calls it from a hook between two frames
//! of its logic, e.g. its frame wait). Test-only commands belong behind a Cargo feature, never
//! in a release.

use super::log;
use crate::game::{Cheat, game};
use recomp::rt::Cpu;
use std::sync::Mutex;

static PENDING: Mutex<Vec<usize>> = Mutex::new(Vec::new());

fn all() -> &'static [Cheat] {
    game().cheats()
}

/// The command with this name.
pub fn find(name: &str) -> Option<usize> {
    all().iter().position(|c| c.name == name)
}

/// Shift+F<n> when `<PREFIX>_CHEATS` is set (the first call lists them).
pub fn by_key(f: u8) -> Option<usize> {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let on = *ON.get_or_init(|| {
        let on = recomp::config::env_on("CHEATS");
        if on {
            for c in all().iter().filter(|c| c.key != 0) {
                log!("cheat: Shift+F{} {}", c.key, c.name);
            }
        }
        on
    });
    if on && f != 0 { all().iter().position(|c| c.key == f) } else { None }
}

/// Ask for command k (it runs at the next `run_pending`).
pub fn request(k: usize) {
    PENDING.lock().unwrap().push(k);
}

/// The names, for an unknown one in `<PREFIX>_INPUT`.
pub fn names() -> String {
    all().iter().map(|c| c.name).collect::<Vec<_>>().join(", ")
}

/// Run the commands asked for (on the game's thread, between two frames of its logic).
pub fn run_pending(c: &mut Cpu) {
    let pending = std::mem::take(&mut *PENDING.lock().unwrap());
    for k in pending {
        log!("cheat: {}", all()[k].name);
        (all()[k].run)(c);
    }
}
