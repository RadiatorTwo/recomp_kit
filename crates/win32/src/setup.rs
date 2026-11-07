//! The player's game files: the folder `game_files` next to our program (in a checkout: in the
//! current folder). The player copies the CD, an image of it or an installed game into it (or
//! puts the image next to the program: what the game needs is copied out of it); at every
//! start we find the game's EXE (`Identity::exe`), check it is a version we know, and keep only
//! what the game needs, in this layout (Future Cop's as the example):
//!
//! ```text
//! game_files/FCopLAPD/   FCopLAPD.exe, febmp.bin, missions/ (`Identity::required`), and
//!                        registry.txt, controls.txt, save.cfg... (`Identity::game_files`)
//! game_files/Video/      the videos (optional; the folder with `Identity::video_marker`)
//! game_files/Textures/   replacement textures (optional, `platform::hd`)
//! ```
//!
//! Everything else in `game_files` (the CD's installer, DirectX...) goes to the recycle bin
//! (Linux: deleted), without asking.

use crate::iso::Iso;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

use crate::game::id;

const VIDEO: &str = "Video";
/// replacement textures (`platform::hd`), the player's
const TEXTURES: &str = "Textures";
/// The game folder's name in `game_files`.
fn folder() -> &'static str {
    id().folder
}

/// What stays in the game folder: the game's files and ours.
fn kept() -> Vec<&'static str> {
    let i = id();
    let mut v = vec![i.exe, "registry.txt", COMPILED, SAVESTATE];
    v.extend(i.required);
    v.extend(i.game_files);
    v
}

/// Our program's file name, for messages.
fn program() -> String {
    std::env::current_exe().ok().and_then(|p| Some(p.file_name()?.to_string_lossy().into_owned())).unwrap_or("this program".into())
}
/// our savestate (`platform::savestate`)
pub const SAVESTATE: &str = "savestate.bin";
/// the game's compiled code, kept for the next start (`jit::compile_cached`)
pub const COMPILED: &str = "compiled.bin";
/// our note in `game_files` (in the release)
const NOTE: &str = "README.txt";

/// The folder with the game files: `game_files` next to our program, else in the current folder.
/// When there is neither, the one next to our program (made when a CD image is found there).
pub fn root() -> PathBuf {
    match home().map(|d| d.join("game_files")) {
        Some(d) if d.is_dir() || !Path::new("game_files").is_dir() => d,
        _ => PathBuf::from("game_files"),
    }
}

/// The folder our program is in.
fn home() -> Option<PathBuf> {
    std::env::current_exe().ok()?.parent().map(Path::to_path_buf)
}

/// Set up the game files in `root` and return the EXE's path, or what the player must do.
pub fn game_exe(root: &Path) -> Result<PathBuf, String> {
    let (title, exe_name, me) = (id().title, id().exe, program());
    let copy_in = format!(
        "Put an image of your {title} CD (.iso or .bin) next to {me}, or copy the CD (or your installed game folder) into this folder:\n\n{}\n\nThen start {me} again.",
        root.display()
    );
    let game = root.join(folder());
    let mut exe = find(&game, exe_name, 0).or_else(|| find(root, exe_name, 6));
    if exe.is_none() {
        // a CD image in game_files (it goes to the recycle bin afterwards), or next to our program
        // (it stays there)
        if let Some(image) = find_image(root, 6).or_else(|| home().and_then(|h| find_image(&h, 0))) {
            from_image(&image, root)?;
            exe = find(&game, exe_name, 0);
        }
    }
    let Some(exe) = exe else {
        return Err(format!("The game files of {title} were not found.\n\n{copy_in}"));
    };
    check(&exe)?;
    let from = exe.parent().unwrap().to_path_buf();
    if !same(&from, &game) {
        std::fs::create_dir_all(&game).map_err(|e| format!("{}: {e}", game.display()))?;
        for name in kept() {
            move_in(&from, name, &game.join(name))?;
        }
    }
    for need in id().required {
        if named(&game, need).is_none() {
            return Err(format!("{need} is missing next to {exe_name} in {}.\n\n{copy_in}", from.display()));
        }
    }
    let video = root.join(VIDEO);
    if !video.is_dir() {
        match id().video_marker.and_then(|m| find(root, m, 6)).and_then(|p| p.parent().map(Path::to_path_buf)) {
            Some(v) if !same(&v, &game) => move_into(&v, &video)?,
            _ if id().video_marker.is_some() => eprintln!("setup: the game's videos were not found; the game runs without them"),
            _ => {}
        }
    }
    writable(&game);
    writable(&video);
    // what the game does not need
    let mut rest = others(root, &[folder(), VIDEO, TEXTURES, NOTE]);
    rest.extend(others(&game, &kept()));
    for p in rest {
        eprintln!("setup: not needed, removed: {}", p.display());
        if let Err(e) = trash(&p) {
            eprintln!("setup: cannot remove {}: {e}", p.display());
        }
    }
    Ok(game.join(named(&game, exe_name).unwrap()))
}

/// The first CD image (.iso, .bin, .img with an ISO 9660 file system) in `dir` or below.
fn find_image(dir: &Path, depth: u32) -> Option<PathBuf> {
    let mut entries: Vec<_> = std::fs::read_dir(dir).ok()?.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    let image = |p: &Path| {
        let ext = p.extension().map(|e| e.to_string_lossy().to_ascii_lowercase());
        matches!(ext.as_deref(), Some("iso" | "bin" | "img")) && Iso::open(p).is_some()
    };
    let mut files = entries.iter().filter(|e| e.file_type().is_ok_and(|t| t.is_file())).map(|e| e.path());
    if let Some(p) = files.find(|p| image(p)) {
        return Some(p);
    }
    if depth == 0 {
        return None;
    }
    entries.iter().filter(|e| e.file_type().is_ok_and(|t| t.is_dir())).find_map(|e| find_image(&e.path(), depth - 1))
}

/// Take what the game needs from a CD image into `root`'s layout: the videos, the game folder's
/// files, the EXE last (an interrupted start does it again). The image itself goes to the
/// recycle bin with the rest after that.
fn from_image(path: &Path, root: &Path) -> Result<(), String> {
    let fail = |e: std::io::Error| format!("Cannot read the CD image {}: {e}", path.display());
    let mut iso = Iso::open(path).ok_or_else(|| format!("{} is not a CD image", path.display()))?;
    let top = iso.root();
    let (title, exe_name) = (id().title, id().exe);
    let Some((dir, exe)) = iso.find(&top, exe_name, 6) else {
        return Err(format!("{} holds no {exe_name}: is it the {title} CD?", path.display()));
    };
    let mut bytes = Vec::new();
    iso.read(&exe, &mut bytes).map_err(fail)?;
    // what is copied, for the progress shown
    let video_dir = id().video_marker.and_then(|m| iso.find(&top, m, 6)).map(|(v, _)| v);
    let mut total = exe.size as u64;
    let parts: Vec<_> = id().required.iter().map(|n| iso.get(&dir, n)).chain([video_dir.clone()]).collect();
    for e in parts.into_iter().flatten() {
        total += if e.dir { iso.size(&e).map_err(fail)? } else { e.size as u64 };
    }
    let total = total.max(1);
    iso.copied = 0;
    iso.progress = Some(Box::new(move |n| {
        crate::splash::status(&format!("Copying the game from the CD image... {}%", n * 100 / total))
    }));
    let hash = hex(&Sha256::digest(&bytes));
    if recomp::ver::known(id().versions, &hash).is_none() {
        return Err(unknown(&format!("{exe_name} on {}", path.display()), &hash));
    }
    eprintln!("setup: taking the game from the CD image {}", path.display());
    let game = root.join(folder());
    std::fs::create_dir_all(&game).map_err(fail)?;
    let video = root.join(VIDEO);
    if !video.is_dir() {
        if let Some(v) = &video_dir {
            iso.extract(v, &video).map_err(fail)?;
        }
    }
    for &name in id().required {
        match iso.get(&dir, name) {
            Some(e) if e.dir => iso.extract(&e, &game.join(name)).map_err(fail)?,
            Some(e) => {
                let mut out = std::io::BufWriter::new(std::fs::File::create(game.join(name)).map_err(fail)?);
                iso.read(&e, &mut out).map_err(fail)?;
            }
            None => return Err(format!("{name} is missing next to {exe_name} on {}.", path.display())),
        }
    }
    std::fs::write(game.join(exe_name), &bytes).map_err(fail)
}

fn hex(d: &[u8]) -> String {
    d.iter().map(|b| format!("{b:02x}")).collect()
}

fn unknown(what: &str, hash: &str) -> String {
    let known: Vec<String> = id().versions.iter().map(|v| format!("{}: {}", v.name, v.sha256)).collect();
    format!(
        "{what} is a version of {} this port does not know (the versions with these SHA-256 are \
         supported:\n{}).\n\nIts SHA-256: {hash}",
        id().title,
        known.join("\n")
    )
}

/// Which version of the game's EXE this is, if we know it.
pub fn check(exe: &Path) -> Result<&'static recomp::ver::Version, String> {
    let bytes = std::fs::read(exe).map_err(|e| format!("{}: {e}", exe.display()))?;
    let hash = hex(&Sha256::digest(&bytes));
    recomp::ver::known(id().versions, &hash).ok_or_else(|| unknown(&exe.display().to_string(), &hash))
}

/// The entry of `dir` called `name`, in any case.
fn named(dir: &Path, name: &str) -> Option<std::ffi::OsString> {
    std::fs::read_dir(dir).ok()?.flatten().map(|e| e.file_name()).find(|n| n.to_string_lossy().eq_ignore_ascii_case(name))
}

/// The first file called `name` in `dir` or below it, at most `depth` folders down (links are
/// not followed).
fn find(dir: &Path, name: &str, depth: u32) -> Option<PathBuf> {
    let mut dirs = Vec::new();
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let Ok(t) = e.file_type() else { continue };
        if t.is_dir() {
            dirs.push(e.path());
        } else if t.is_file() && e.file_name().to_string_lossy().eq_ignore_ascii_case(name) {
            return Some(e.path());
        }
    }
    if depth == 0 {
        return None;
    }
    dirs.sort();
    dirs.iter().find_map(|d| find(d, name, depth - 1))
}

fn same(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Move `from`/`name` (any case) to `to`, unless it is not there or `to` already is.
fn move_in(from: &Path, name: &str, to: &Path) -> Result<(), String> {
    match named(from, name) {
        Some(n) if !to.exists() => move_into(&from.join(n), to),
        _ => Ok(()),
    }
}

fn move_into(from: &Path, to: &Path) -> Result<(), String> {
    eprintln!("setup: {} -> {}", from.display(), to.display());
    std::fs::rename(from, to).map_err(|e| format!("Cannot move {} to {}: {e}", from.display(), to.display()))
}

/// The entries of `dir` other than `keep` (any case).
fn others(dir: &Path, keep: &[&str]) -> Vec<PathBuf> {
    let Ok(d) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut v: Vec<PathBuf> = d
        .flatten()
        .filter(|e| !keep.iter().any(|k| e.file_name().to_string_lossy().eq_ignore_ascii_case(k)))
        .map(|e| e.path())
        .collect();
    v.sort();
    v
}

/// Files copied from a CD are read-only; the game writes into its folder.
fn writable(dir: &Path) {
    let Ok(d) = std::fs::read_dir(dir) else { return };
    for e in d.flatten() {
        let p = e.path();
        if let Ok(m) = std::fs::symlink_metadata(&p) {
            let mut perm = m.permissions();
            if perm.readonly() {
                #[allow(clippy::permissions_set_readonly_false)]
                perm.set_readonly(false);
                let _ = std::fs::set_permissions(&p, perm);
            }
            if m.is_dir() {
                writable(&p);
            }
        }
    }
}

/// Move a file or folder to the recycle bin (elsewhere: delete it).
#[cfg(windows)]
fn trash(p: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::Shell::*;
    // a list of paths, each NUL-terminated, the list ending with another NUL
    let from: Vec<u16> = p.as_os_str().encode_wide().chain([0, 0]).collect();
    let mut op = SHFILEOPSTRUCTW {
        wFunc: FO_DELETE,
        pFrom: from.as_ptr(),
        fFlags: (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_NOERRORUI | FOF_SILENT) as u16,
        ..Default::default()
    };
    match unsafe { SHFileOperationW(&mut op) } {
        0 if op.fAnyOperationsAborted == 0 => Ok(()),
        e => Err(format!("error {e:#x}")),
    }
}

#[cfg(not(windows))]
fn trash(p: &Path) -> Result<(), String> {
    let r = if p.is_dir() { std::fs::remove_dir_all(p) } else { std::fs::remove_file(p) };
    r.map_err(|e| e.to_string())
}
