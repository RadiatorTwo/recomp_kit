//! The game's own DLLs (`Game::dlls`: files in the game folder, such as the MFC and C runtime
//! DLLs a game shipped with): loaded at their bases (moved by their relocations if the game
//! wants another), compiled with the EXE, their imports bound like the EXE's, their DllMain
//! run before the EXE starts (each after the DLLs it imports, as Windows does), or when the
//! game loads one with LoadLibraryA.

use super::{guest_call, log};
use recomp::image::Image;
use recomp::rt::Cpu;
use std::path::Path;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

/// A game DLL in guest memory.
pub struct Module {
    /// its file name in the game folder, as in `Game::dlls`
    pub file: &'static str,
    pub base: u32,
    pub entry: u32,
    /// (address, name) and (address, ordinal)
    exports: Vec<(u32, String)>,
    ordinals: Vec<(u32, u32)>,
    /// whether its DllMain has run
    attached: AtomicBool,
    /// the DLLs it imports
    imports: Vec<String>,
}

impl Module {
    /// An export by name or `#ordinal`: its address (0 if there is none).
    pub fn export(&self, name: &str) -> u32 {
        if let Some(n) = name.strip_prefix('#').and_then(|n| n.parse::<u32>().ok()) {
            return self.ordinals.iter().find(|e| e.1 == n).map_or(0, |e| e.0);
        }
        self.exports.iter().find(|e| e.1 == name).map_or(0, |e| e.0)
    }

    /// The path the game sees (in the install folder).
    pub fn guest_path(&self) -> String {
        format!("{}\\{}", crate::game::id().install_dir, self.file)
    }
}

static MODULES: OnceLock<Vec<Module>> = OnceLock::new();

/// The game's DLLs from its folder, each at the base `Game::dlls` gives. Called once, before
/// the code is found.
pub fn load(dir: &Path) -> std::io::Result<Vec<Image>> {
    let mut images = Vec::new();
    let mut modules = Vec::new();
    for &(file, base) in crate::game::game().dlls() {
        let mut img = Image::load(&dir.join(file))?;
        if img.base != base {
            img.rebase(base);
        }
        modules.push(Module {
            file,
            base: img.base,
            entry: img.entry,
            exports: img.exports(),
            ordinals: img.export_ordinals(),
            attached: AtomicBool::new(false),
            imports: img.imports().into_iter().map(|i| i.dll).collect::<std::collections::BTreeSet<_>>().into_iter().collect(),
        });
        images.push(img);
    }
    let _ = MODULES.set(modules);
    Ok(images)
}

/// Where finding the DLLs' code starts: their entry points and exports.
pub fn seeds() -> Vec<(u32, String)> {
    let mut s = Vec::new();
    for m in MODULES.get().into_iter().flatten() {
        let stem = m.file.rsplit_once('.').map_or(m.file, |p| p.0);
        s.push((m.entry, format!("{stem}_DllMain")));
        s.extend(m.exports.iter().cloned());
        s.extend(m.ordinals.iter().map(|&(a, n)| (a, format!("{stem}_{n}"))));
    }
    s
}

/// A game DLL by file name (with or without folder or `.dll`, any case).
pub fn module(name: &str) -> Option<&'static Module> {
    let file = name.rsplit(['\\', '/']).next().unwrap_or(name);
    let has_ext = file.rsplit_once('.').is_some_and(|(_, e)| !e.is_empty());
    MODULES.get()?.iter().find(|m| {
        if has_ext { m.file.eq_ignore_ascii_case(file) } else { m.file.rsplit_once('.').is_some_and(|(s, _)| s.eq_ignore_ascii_case(file)) }
    })
}

/// The game DLL at `base`.
pub fn module_at(base: u32) -> Option<&'static Module> {
    MODULES.get()?.iter().find(|m| m.base == base)
}

/// Whether a game DLL's DllMain has run (GetModuleHandleA finds only those).
pub fn attached(m: &Module) -> bool {
    m.attached.load(Ordering::Relaxed)
}

/// Run a game DLL's DllMain(hinstDLL, DLL_PROCESS_ATTACH, 0) once, after the DLLs it imports.
/// False when it failed.
pub fn attach(c: &mut Cpu, m: &'static Module) -> bool {
    if m.attached.swap(true, Ordering::Relaxed) {
        return true;
    }
    for d in &m.imports {
        if let Some(dep) = module(d) {
            attach(c, dep);
        }
    }
    log!("{}: DllMain", m.file);
    let ok = guest_call(c, m.entry, &[m.base, 1, 0]) != 0;
    if !ok {
        log!("{}: its DllMain failed", m.file);
    }
    ok
}

/// The system DLLs we implement, as GetModuleHandleA and LoadLibraryA give them (a module
/// handle each, no memory behind it).
const SYSTEM_DLLS: [&str; 14] = [
    "KERNEL32.DLL",
    "USER32.DLL",
    "GDI32.DLL",
    "ADVAPI32.DLL",
    "DDRAW.DLL",
    "DSOUND.DLL",
    "WINMM.DLL",
    "OLE32.DLL",
    "DINPUT.DLL",
    "COMCTL32.DLL",
    "SHELL32.DLL",
    "COMDLG32.DLL",
    "WSOCK32.DLL",
    "WS2_32.DLL",
];
const SYSTEM_MODULE: u32 = 0x7e00_0000;

/// A system DLL's module handle.
pub fn system_module(name: &str) -> Option<u32> {
    let file = name.rsplit(['\\', '/']).next().unwrap_or(name).to_ascii_uppercase();
    let file = if file.contains('.') { file } else { file + ".DLL" };
    SYSTEM_DLLS.iter().position(|&d| d == file).map(|k| SYSTEM_MODULE + 0x1_0000 * k as u32)
}

/// GetProcAddress in a system DLL: a thunk for one of our functions (0 if we have none).
pub fn system_proc(module: u32, name: &str) -> u32 {
    use std::collections::BTreeMap;
    use std::sync::Mutex;
    static MADE: Mutex<BTreeMap<(u32, String), u32>> = Mutex::new(BTreeMap::new());
    let Some(dll) = module.checked_sub(SYSTEM_MODULE).map(|k| k / 0x1_0000).and_then(|k| SYSTEM_DLLS.get(k as usize)) else {
        return 0;
    };
    let mut made = super::lock(&MADE);
    if let Some(&a) = made.get(&(module, name.to_string())) {
        return a;
    }
    let Some(&(n, nargs, f)) = super::find_import(dll, name) else { return 0 };
    let a = super::thunk(n, nargs, f);
    made.insert((module, name.to_string()), a);
    a
}

/// Attach the game DLLs that `img` imports.
pub fn attach_imports(c: &mut Cpu, img: &Image) {
    let wanted: std::collections::BTreeSet<String> = img.imports().into_iter().map(|i| i.dll).collect();
    for m in wanted.iter().filter_map(|d| module(d)) {
        attach(c, m);
    }
}
