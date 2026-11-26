//! The player's game files: the folder `game_files` next to our program (in a checkout: in the
//! current folder). The player puts an image of the CD into it or next to the program (.cue
//! with its .bin files, .iso, .img, .mdf), or copies the CD or an installed game into it; at
//! every start we find the game's EXE (`Identity::exe`), check it is a version we know, and
//! keep what the game needs in this layout (Future Cop's as the example):
//!
//! ```text
//! game_files/FCopLAPD/   FCopLAPD.exe, febmp.bin, missions/ (`Identity::required`), and
//!                        registry.txt, controls.txt, save.cfg... (`Identity::game_files`)
//! game_files/Video/      the videos (optional; the folder with `Identity::video_marker`)
//! game_files/Textures/   replacement textures (optional, `platform::hd`)
//! ```
//!
//! No EXE yet: it comes from a CD image (copied out of it) or, for a game whose CD holds it
//! only in InstallShield cabinets (`Identity::cab_left_out`), is installed from the cabinets
//! on the image or on a copied CD. Nothing is ever removed.

use crate::iso::Iso;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

use crate::game::id;

const VIDEO: &str = "Video";
/// The game folder's name in `game_files`.
fn folder() -> &'static str {
    id().folder
}

/// What is moved into the game folder from where the game was found: the game's files and ours.
fn kept() -> Vec<&'static str> {
    let i = id();
    let mut v = vec![i.exe, "registry.txt", COMPILED, SAVESTATE];
    v.extend(i.required);
    v.extend(i.game_files);
    v
}

/// Our program's file name, for messages.
fn program() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| Some(p.file_name()?.to_string_lossy().into_owned()))
        .unwrap_or("this program".into())
}
/// our savestate (`platform::savestate`)
pub const SAVESTATE: &str = "savestate.bin";
/// the game's compiled code, kept for the next start (`jit::compile_cached`)
pub const COMPILED: &str = "compiled.bin";

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
    std::env::current_exe()
        .ok()?
        .parent()
        .map(Path::to_path_buf)
}

/// Set up the game files in `root` and return the EXE's path, or what the player must do.
pub fn game_exe(root: &Path) -> Result<PathBuf, String> {
    let (title, exe_name, me) = (id().title, id().exe, program());
    let copy_in = format!(
        "Put an image of your {title} CD (.cue with its .bin files, .iso, .img or .mdf) into this folder or next to {me}, or copy the CD (or your installed game folder) into this folder:\n\n{}\n\nThen start {me} again.",
        root.display()
    );
    let game = root.join(folder());
    // the first EXE of a version we know: a CD can have others of the same name (Recoil's
    // Westwood launcher, Westwood\RECOIL\Recoil3Dfx.exe)
    let found: Vec<PathBuf> = find_all(&game, exe_name, 0)
        .into_iter()
        .chain(find_all(root, exe_name, 6))
        .collect();
    let mut exe = found.iter().find(|p| check(p).is_ok()).cloned();
    if exe.is_none() && install(root)? {
        exe = find(&game, exe_name, 0);
    }
    let Some(exe) = exe else {
        if let Some(other) = found.first() {
            check(other)?;
        }
        return Err(format!(
            "The game files of {title} were not found.\n\n{copy_in}"
        ));
    };
    check(&exe)?;
    let from = exe.parent().unwrap().to_path_buf();
    if !same(&from, &game) {
        std::fs::create_dir_all(&game).map_err(|e| format!("{}: {e}", game.display()))?;
        for name in kept() {
            move_in(&from, name, &game.join(name))?;
        }
    }
    let missing = || id().required.iter().find(|n| named(&game, n).is_none());
    // an installed game whose installer put some files elsewhere (the MFC DLLs into Windows'
    // folder): what is missing comes from the CD, when it is there too
    if missing().is_some() && id().cab_left_out.is_some() {
        install(root)?;
    }
    if let Some(need) = missing() {
        return Err(format!(
            "{need} is missing next to {exe_name} in {}.\n\n{copy_in}",
            from.display()
        ));
    }
    let video = root.join(VIDEO);
    if !video.is_dir() {
        match id()
            .video_marker
            .and_then(|m| find(root, m, 6))
            .and_then(|p| p.parent().map(Path::to_path_buf))
        {
            Some(v) if !same(&v, &game) => move_into(&v, &video)?,
            _ if id().video_marker.is_some() => {
                eprintln!("setup: the game's videos were not found; the game runs without them")
            }
            _ => {}
        }
    }
    writable(&game);
    writable(&video);
    // Nothing is ever removed: the folder holds the player's files (the CD image, a copied game).
    // An earlier cleanup ("what the game does not need") put a game's files and its CD image
    // into the recycle bin when the game's own files were not all listed in `Identity`.
    Ok(game.join(named(&game, exe_name).unwrap()))
}

/// Take the game from a CD image (in `root` or next to our program) or from a copied CD's
/// InstallShield cabinets (in `root`) into `root`'s layout. False when there is neither.
fn install(root: &Path) -> Result<bool, String> {
    if let Some(image) = find_image(root, 6).or_else(|| home().and_then(|h| find_image(&h, 0))) {
        from_image(&image, root)?;
        return Ok(true);
    }
    if id().cab_left_out.is_none() {
        return Ok(false);
    }
    let game = root.join(folder());
    let cab = find(root, "data1.cab", 6);
    match cab.as_deref().and_then(Path::parent) {
        Some(dir) if !same(dir, &game) => {
            eprintln!("setup: installing the game from {}", dir.display());
            let mut read = |name: &str| std::fs::read(in_folder(dir, name)?).ok();
            from_cab(&mut read, &dir.display().to_string(), &game, "the CD")?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

/// `dir`'s file at `rel` ("video\x.avi"), each part in any case.
fn in_folder(dir: &Path, rel: &str) -> Option<PathBuf> {
    rel.split(['\\', '/'])
        .filter(|s| !s.is_empty())
        .try_fold(dir.to_path_buf(), |p, s| {
            let n = named(&p, s)?;
            Some(p.join(n))
        })
}

/// The first CD image (.iso, .bin, .img, .mdf with an ISO 9660 or UDF file system) in `dir` or
/// below. A .cue's data track is its first .bin.
fn find_image(dir: &Path, depth: u32) -> Option<PathBuf> {
    let mut entries: Vec<_> = std::fs::read_dir(dir).ok()?.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    let image = |p: &Path| {
        let ext = p
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase());
        matches!(ext.as_deref(), Some("iso" | "bin" | "img" | "mdf")) && Iso::open(p).is_some()
    };
    let mut files = entries
        .iter()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .map(|e| e.path());
    if let Some(p) = files.find(|p| image(p)) {
        return Some(p);
    }
    if depth == 0 {
        return None;
    }
    entries
        .iter()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .find_map(|e| find_image(&e.path(), depth - 1))
}

/// Take what the game needs from a CD image into `root`'s layout: the videos, then the game
/// folder's files (from the image, or installed from its cabinets), the EXE last (an
/// interrupted start does it again). The image stays where it is.
fn from_image(path: &Path, root: &Path) -> Result<(), String> {
    let fail = |e: std::io::Error| format!("Cannot read the CD image {}: {e}", path.display());
    let mut iso = Iso::open(path).ok_or_else(|| format!("{} is not a CD image", path.display()))?;
    let top = iso.root();
    let (title, exe_name) = (id().title, id().exe);
    let mut unknown_exe = None;
    let mut on_image = None;
    for (dir, e) in iso_find_all(&mut iso, &top, exe_name, 6) {
        let mut bytes = Vec::new();
        iso.read(&e, &mut bytes).map_err(fail)?;
        let hash = hex(&Sha256::digest(&bytes));
        if recomp::ver::known(id().versions, &hash).is_some() {
            on_image = Some((dir, e));
            break;
        }
        unknown_exe.get_or_insert(hash);
    }
    let cab = match (&on_image, id().cab_left_out) {
        (None, Some(_)) => iso.find(&top, "data1.cab", 6).map(|(dir, _)| dir),
        _ => None,
    };
    if on_image.is_none() && cab.is_none() {
        if let Some(hash) = unknown_exe {
            return Err(unknown(&format!("{exe_name} on {}", path.display()), &hash));
        }
        return Err(format!(
            "{} holds no {exe_name}: is it the {title} CD?",
            path.display()
        ));
    }
    eprintln!(
        "setup: taking the game from the CD image {}",
        path.display()
    );
    let video = root.join(VIDEO);
    if !video.is_dir() {
        if let Some((v, _)) = id().video_marker.and_then(|m| iso.find(&top, m, 6)) {
            let total = iso.size(&v).map_err(fail)?.max(1);
            iso.copied = 0;
            iso.progress = Some(Box::new(move |n| {
                crate::splash::status(&format!(
                    "Copying the videos from the CD image... {}%",
                    n * 100 / total
                ))
            }));
            // under another name until it is whole: an interrupted start copies it again
            let part = root.join("Video.part");
            let _ = std::fs::remove_dir_all(&part);
            iso.extract(&v, &part).map_err(fail)?;
            std::fs::rename(&part, &video).map_err(fail)?;
            iso.progress = None;
        }
    }
    let game = root.join(folder());
    match (on_image, cab) {
        (Some((dir, exe)), _) => files_from_image(&mut iso, path, &dir, &exe, &game),
        (None, Some(dir)) => {
            let mut read = |name: &str| {
                let mut at = dir.clone();
                for part in name.split(['\\', '/']).filter(|s| !s.is_empty()) {
                    at = iso.get(&at, part)?;
                }
                let mut bytes = Vec::with_capacity(at.size as usize);
                iso.read(&at, &mut bytes).ok()?;
                Some(bytes)
            };
            from_cab(&mut read, &path.display().to_string(), &game, "the CD image")
        }
        (None, None) => unreachable!(),
    }
}

/// The game folder's files as they lie on the image beside the EXE (`Identity::required`).
fn files_from_image(
    iso: &mut Iso,
    path: &Path,
    dir: &crate::iso::Entry,
    exe: &crate::iso::Entry,
    game: &Path,
) -> Result<(), String> {
    let fail = |e: std::io::Error| format!("Cannot read the CD image {}: {e}", path.display());
    let exe_name = id().exe;
    let mut bytes = Vec::new();
    iso.read(exe, &mut bytes).map_err(fail)?;
    let hash = hex(&Sha256::digest(&bytes));
    if recomp::ver::known(id().versions, &hash).is_none() {
        return Err(unknown(&format!("{exe_name} on {}", path.display()), &hash));
    }
    // what is copied, for the progress shown
    let mut total = 0;
    let parts: Vec<_> = id().required.iter().map(|n| iso.get(dir, n)).collect();
    for e in parts.into_iter().flatten() {
        total += if e.dir {
            iso.size(&e).map_err(fail)?
        } else {
            e.size as u64
        };
    }
    let total = total.max(1);
    iso.copied = 0;
    iso.progress = Some(Box::new(move |n| {
        crate::splash::status(&format!(
            "Copying the game from the CD image... {}%",
            n * 100 / total
        ))
    }));
    std::fs::create_dir_all(game).map_err(fail)?;
    for &name in id().required {
        match iso.get(dir, name) {
            Some(e) if e.dir => iso.extract(&e, &game.join(name)).map_err(fail)?,
            Some(e) => {
                let mut out =
                    std::io::BufWriter::new(std::fs::File::create(game.join(name)).map_err(fail)?);
                iso.read(&e, &mut out).map_err(fail)?;
            }
            None => {
                return Err(format!(
                    "{name} is missing next to {exe_name} on {}.",
                    path.display()
                ));
            }
        }
    }
    iso.progress = None;
    std::fs::write(game.join(exe_name), &bytes).map_err(fail)
}

/// Install the game into `game` from InstallShield cabinets, as the CD's setup would: every
/// file of every group but `Identity::cab_left_out`, at its folder below the game folder.
/// `read` gives a file beside the cabinets ("data1.hdr", "data2.cab"); `source` names them in
/// messages, `what` in the progress shown. Files already in `game` are kept (each is written
/// under another name until it is whole), the EXE comes last: an interrupted start goes on.
fn from_cab(
    read: &mut dyn FnMut(&str) -> Option<Vec<u8>>,
    source: &str,
    game: &Path,
    what: &str,
) -> Result<(), String> {
    use crate::iscab::{self, Header, INVALID};
    let (title, exe_name) = (id().title, id().exe);
    let left_out = id().cab_left_out.unwrap_or(&[]);
    crate::splash::status(&format!("Reading the installer on {what}..."));
    // the header: data1.hdr, or the start of data1.cab (InstallShield 5)
    let (hdr, in_cab) = match read("data1.hdr") {
        Some(h) => (h, false),
        None => (
            read("data1.cab").ok_or_else(|| format!("{source}: cannot read data1.cab"))?,
            true,
        ),
    };
    let head = Header::parse(&hdr).map_err(|e| format!("{source}: {e}"))?;
    let mut files: Vec<iscab::File> = Vec::new();
    for g in &head.groups {
        if left_out.iter().any(|l| l.eq_ignore_ascii_case(&g.name)) {
            continue;
        }
        for k in g.first..=g.last {
            let Some(f) = head.files.get(k as usize) else {
                continue;
            };
            let new = !files.iter().any(|o| o.name == f.name && o.dir == f.dir);
            if f.flags & INVALID == 0 && !f.name.is_empty() && new {
                files.push(f.clone());
            }
        }
    }
    let rel = |f: &iscab::File| {
        if f.dir.is_empty() {
            f.name.clone()
        } else {
            format!("{}\\{}", f.dir, f.name)
        }
    };
    let Some(at) = files
        .iter()
        .position(|f| f.dir.is_empty() && f.name.eq_ignore_ascii_case(exe_name))
    else {
        return Err(format!("{source} holds no {exe_name}: is it the {title} CD?"));
    };
    let exe = files.remove(at);
    // in the cabs' order: each volume is read once
    files.sort_by_key(|f| (f.volume, f.offset));
    // the volume read last (data1.cab: the header's bytes when it holds the header)
    let mut cab: (u16, Vec<u8>) = if in_cab { (1, hdr) } else { (0, Vec::new()) };
    let mut contents = |f: &iscab::File| -> Result<Vec<u8>, String> {
        if f.beside() {
            return read(&rel(f)).ok_or_else(|| format!("{source}: cannot read {}", rel(f)));
        }
        if cab.0 != f.volume {
            let name = format!("data{}.cab", f.volume);
            // the last one goes before the next is read
            cab = (0, Vec::new());
            let bytes = read(&name).ok_or_else(|| format!("{source}: cannot read {name}"))?;
            cab = (f.volume, bytes);
        }
        let packed = usize::try_from(f.offset)
            .ok()
            .zip(usize::try_from(f.offset + f.packed).ok())
            .and_then(|(a, b)| cab.1.get(a..b))
            .unwrap_or(&[]);
        iscab::unpack(f, packed).map_err(|e| format!("{source}: {e}"))
    };
    let bytes = contents(&exe)?;
    let hash = hex(&Sha256::digest(&bytes));
    if recomp::ver::known(id().versions, &hash).is_none() {
        return Err(unknown(&format!("{exe_name} in {source}"), &hash));
    }
    let fail = |p: &Path, e: std::io::Error| format!("Cannot write {}: {e}", p.display());
    let write = |rel: &str, data: &[u8]| -> Result<(), String> {
        let to = rel
            .split('\\')
            .filter(|s| !s.is_empty())
            .fold(game.to_path_buf(), |p, s| p.join(s));
        std::fs::create_dir_all(to.parent().unwrap()).map_err(|e| fail(&to, e))?;
        let part = to.with_file_name(format!(
            "{}.part",
            to.file_name().unwrap().to_string_lossy()
        ));
        std::fs::write(&part, data).map_err(|e| fail(&part, e))?;
        std::fs::rename(&part, &to).map_err(|e| fail(&to, e))
    };
    let total = files.iter().map(|f| f.size).sum::<u64>().max(1);
    let mut done = 0;
    for f in &files {
        let rel = rel(f);
        if in_folder(game, &rel).is_none() {
            write(&rel, &contents(f)?)?;
        }
        done += f.size;
        crate::splash::status(&format!(
            "Installing the game from {what}... {}%",
            done * 100 / total
        ));
    }
    if in_folder(game, &exe.name).is_none() {
        write(&exe.name, &bytes)?;
    }
    eprintln!(
        "setup: installed {} files into {}",
        files.len() + 1,
        game.display()
    );
    Ok(())
}

fn hex(d: &[u8]) -> String {
    d.iter().map(|b| format!("{b:02x}")).collect()
}

fn unknown(what: &str, hash: &str) -> String {
    let known: Vec<String> = id()
        .versions
        .iter()
        .map(|v| format!("{}: {}", v.name, v.sha256))
        .collect();
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
    recomp::ver::known(id().versions, &hash)
        .ok_or_else(|| unknown(&exe.display().to_string(), &hash))
}

/// Every file called `name` (any case) on the image in `dir` or below, at most `depth` folders
/// down, with its folder: `dir`'s first.
fn iso_find_all(
    iso: &mut Iso,
    dir: &crate::iso::Entry,
    name: &str,
    depth: u32,
) -> Vec<(crate::iso::Entry, crate::iso::Entry)> {
    let list = iso.list(dir).unwrap_or_default();
    let mut out: Vec<_> = list
        .iter()
        .filter(|e| !e.dir && e.name.eq_ignore_ascii_case(name))
        .map(|e| (dir.clone(), e.clone()))
        .collect();
    if depth > 0 {
        for d in list.iter().filter(|e| e.dir) {
            out.extend(iso_find_all(iso, d, name, depth - 1));
        }
    }
    out
}

/// Every file called `name` (any case) in `dir` or below it, at most `depth` folders down
/// (links are not followed): `dir`'s first, then its folders' in order.
fn find_all(dir: &Path, name: &str, depth: u32) -> Vec<PathBuf> {
    let Ok(d) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let (mut out, mut dirs) = (Vec::new(), Vec::new());
    for e in d.flatten() {
        let Ok(t) = e.file_type() else { continue };
        if t.is_dir() {
            dirs.push(e.path());
        } else if t.is_file() && e.file_name().to_string_lossy().eq_ignore_ascii_case(name) {
            out.push(e.path());
        }
    }
    if depth > 0 {
        dirs.sort();
        for d in dirs {
            out.extend(find_all(&d, name, depth - 1));
        }
    }
    out
}

/// The entry of `dir` called `name`, in any case.
fn named(dir: &Path, name: &str) -> Option<std::ffi::OsString> {
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.file_name())
        .find(|n| n.to_string_lossy().eq_ignore_ascii_case(name))
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
    std::fs::rename(from, to)
        .map_err(|e| format!("Cannot move {} to {}: {e}", from.display(), to.display()))
}

/// Files copied from a CD are read-only; the game writes into its folder.
fn writable(dir: &Path) {
    let Ok(d) = std::fs::read_dir(dir) else {
        return;
    };
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
