//! Versions of the game's EXE. Everything known about a game is by the addresses of one
//! version (its functions.json, symbols.txt, hooks and the game data the platform reads); call
//! it the reference version. Other versions are the same code linked again (Future Cop's German
//! EXE: built a week later, functions and data a few bytes elsewhere, a few functions gone): a
//! map (`tools vermap` makes it from both EXEs; only addresses) gives each reference address's
//! place there. A reference address is a `Us`; `at` is where it is in the version that runs.

use std::sync::OnceLock;

/// A known version: SHA-256 of its EXE, name, address map (none: the reference version).
pub struct Version {
    pub sha256: &'static str,
    pub name: &'static str,
    /// lines "<reference address> <delta>" from there on, "-" = not in this version
    pub map: Option<&'static str>,
}

/// An address in the reference version.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// (named after Future Cop, whose reference version is the US one)
pub struct Us(pub u32);

impl Us {
    /// Where it is in the version that runs.
    pub fn at(self) -> u32 {
        at(self.0)
    }
}

/// (reference address, delta) runs, sorted; empty for the reference version
static MAP: OnceLock<Vec<(u32, i32)>> = OnceLock::new();
/// the starts and ends of what the version that runs has not (functions taken out)
static GONE: OnceLock<Vec<(u32, bool)>> = OnceLock::new();

/// The version with this SHA-256 (lower-case hex), if it is one of `versions`.
pub fn known<'a>(versions: &'a [Version], sha256: &str) -> Option<&'a Version> {
    versions.iter().find(|v| v.sha256 == sha256)
}

/// SHA-256 of a file's bytes, lower-case hex.
pub fn sha256(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect()
}

/// The version of this EXE, if it is one of `versions`.
pub fn of_exe<'a>(versions: &'a [Version], data: &[u8]) -> Option<&'a Version> {
    known(versions, &sha256(data))
}

/// Run this version (before anything asks `at`; the first choice stays).
pub fn select(v: &Version) {
    if MAP.get().is_some() {
        return;
    }
    let (mut runs, mut gone) = (Vec::new(), Vec::new());
    for l in v.map.unwrap_or("").lines().filter(|l| !l.starts_with('#') && !l.trim().is_empty()) {
        let mut w = l.split_whitespace();
        let a = u32::from_str_radix(w.next().unwrap(), 16).expect("address map");
        match w.next().unwrap() {
            "-" => gone.push((a, true)),
            d => {
                runs.push((a, d.parse().expect("address map")));
                gone.push((a, false));
            }
        }
    }
    let _ = MAP.set(runs);
    let _ = GONE.set(gone);
    if v.map.is_some() {
        eprintln!("the {} version", v.name);
    }
}

fn delta(runs: &[(u32, i32)], a: u32) -> i32 {
    match runs.partition_point(|r| r.0 <= a) {
        0 => 0,
        k => runs[k - 1].1,
    }
}

/// Where the reference address `us` is in the version that runs.
pub fn at(us: u32) -> u32 {
    let runs = MAP.get().map_or(&[][..], Vec::as_slice);
    us.wrapping_add_signed(delta(runs, us))
}

/// Is the reference address `us` in something the version that runs has not?
pub fn gone(us: u32) -> bool {
    let g = GONE.get().map_or(&[][..], Vec::as_slice);
    match g.partition_point(|r| r.0 <= us) {
        0 => false,
        k => g[k - 1].1,
    }
}
