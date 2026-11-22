//! Menus (USER32): kept as items so a game (an MFC frame) can load, change and ask about its
//! menu. They are not shown: the games draw their own screens.

use super::{Import, ResId, arg, cstr, mem, put_cstr};
use recomp::rt::Cpu;
use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard};

#[derive(Clone, Debug, Default)]
struct Item {
    /// MF_ flags: grayed 1, disabled 2, checked 8, popup 0x10, separator 0x800, ...
    flags: u32,
    /// the command id, or the submenu's handle for a popup
    id: u32,
    text: String,
}

struct Menus {
    items: BTreeMap<u32, Vec<Item>>,
    next: u32,
}

static MENUS: Mutex<Menus> = Mutex::new(Menus {
    items: BTreeMap::new(),
    next: 0x7700,
});

fn menus() -> MutexGuard<'static, Menus> {
    MENUS.lock().unwrap_or_else(|e| e.into_inner())
}

fn new_menu(items: Vec<Item>) -> u32 {
    let mut m = menus();
    m.next += 4;
    let h = m.next;
    m.items.insert(h, items);
    h
}

const MF_POPUP: u32 = 0x10;
const MF_END: u32 = 0x80;
const MF_BYPOSITION: u32 = 0x400;
const MF_SEPARATOR: u32 = 0x800;
/// flags MF_STRING / MF_BITMAP / MF_OWNERDRAW: the item's content
const CONTENT: u32 = 0x4 | 0x100;

/// A MENU resource's items from `at` (one level, up to MF_END); returns the end.
fn parse(mut at: u32) -> (Vec<Item>, u32) {
    let m = mem();
    let mut items = Vec::new();
    loop {
        let flags = m.rd16(at) as u32;
        at += 2;
        let id = if flags & MF_POPUP == 0 {
            at += 2;
            m.rd16(at - 2) as u32
        } else {
            0
        };
        let mut text = Vec::new();
        loop {
            let u = m.rd16(at);
            at += 2;
            if u == 0 {
                break;
            }
            text.push(u);
        }
        let mut item = Item {
            flags: flags & !MF_END,
            id,
            text: String::from_utf16_lossy(&text),
        };
        if flags & MF_POPUP != 0 {
            let (sub, end) = parse(at);
            at = end;
            item.id = new_menu(sub);
        }
        items.push(item);
        if flags & MF_END != 0 || items.len() > 4096 {
            return (items, at);
        }
    }
}

/// LoadMenuA(module, name)
fn load_menu(c: &mut Cpu) -> u32 {
    let (h, name) = (arg(c, 0), arg(c, 1));
    let Some((at, _)) = super::resource_in(h, ResId::Id(4), ResId::from_guest(name, cstr)) else {
        return 0;
    };
    // MENUHEADER: version, offset to the items
    let items = parse(at + 4 + mem().rd16(at + 2) as u32).0;
    if super::trace_api() {
        log_items(&items, 0);
    }
    new_menu(items)
}

fn log_items(items: &[Item], depth: usize) {
    for it in items {
        super::log!(
            "menu {:indent$}{:?} id {:#x} flags {:#x}",
            "",
            it.text,
            it.id,
            it.flags,
            indent = 2 * depth
        );
        if it.flags & MF_POPUP != 0 {
            let sub = menus().items.get(&it.id).cloned().unwrap_or_default();
            log_items(&sub, depth + 1);
        }
    }
}

fn create_menu(_: &mut Cpu) -> u32 {
    new_menu(Vec::new())
}

fn destroy_menu(c: &mut Cpu) -> u32 {
    menus().items.remove(&arg(c, 0)).is_some() as u32
}

/// The position of item `which` (by position, or by command id: also inside submenus, then
/// with the submenu that holds it).
fn find(m: &Menus, h: u32, which: u32, flags: u32) -> Option<(u32, usize)> {
    let items = m.items.get(&h)?;
    if flags & MF_BYPOSITION != 0 {
        return ((which as usize) < items.len()).then_some((h, which as usize));
    }
    for (k, it) in items.iter().enumerate() {
        if it.flags & MF_POPUP == 0 && it.id == which {
            return Some((h, k));
        }
    }
    items
        .iter()
        .filter(|it| it.flags & MF_POPUP != 0)
        .find_map(|it| find(m, it.id, which, flags))
}

fn get_sub_menu(c: &mut Cpu) -> u32 {
    let (h, k) = (arg(c, 0), arg(c, 1) as usize);
    menus()
        .items
        .get(&h)
        .and_then(|v| v.get(k))
        .filter(|it| it.flags & MF_POPUP != 0)
        .map_or(0, |it| it.id)
}

fn get_menu_item_count(c: &mut Cpu) -> u32 {
    menus()
        .items
        .get(&arg(c, 0))
        .map_or(u32::MAX, |v| v.len() as u32)
}

fn get_menu_item_id(c: &mut Cpu) -> u32 {
    let (h, k) = (arg(c, 0), arg(c, 1) as usize);
    match menus().items.get(&h).and_then(|v| v.get(k)) {
        Some(it) if it.flags & MF_POPUP != 0 => u32::MAX,
        Some(it) => it.id,
        None => u32::MAX,
    }
}

/// GetMenuState(menu, item, flags): the item's flags (a popup's count of items in the high
/// byte), -1 if there is none.
fn get_menu_state(c: &mut Cpu) -> u32 {
    let (h, which, flags) = (arg(c, 0), arg(c, 1), arg(c, 2));
    let m = menus();
    let Some((h, k)) = find(&m, h, which, flags) else {
        return u32::MAX;
    };
    let it = &m.items[&h][k];
    if it.flags & MF_POPUP != 0 {
        let n = m.items.get(&it.id).map_or(0, |v| v.len() as u32);
        return n << 8 | (it.flags & 0xff);
    }
    it.flags & 0xffff
}

/// Sets `mask` of an item's flags to `value`; returns the old ones of `mask`, -1 if none.
fn set_flags(h: u32, which: u32, flags: u32, mask: u32, value: u32) -> u32 {
    let mut m = menus();
    let Some((h, k)) = find(&m, h, which, flags) else {
        return u32::MAX;
    };
    let it = &mut m.items.get_mut(&h).unwrap()[k];
    let old = it.flags & mask;
    it.flags = it.flags & !mask | value & mask;
    old
}

/// EnableMenuItem(menu, item, flags): MF_GRAYED (1) and MF_DISABLED (2).
fn enable_menu_item(c: &mut Cpu) -> u32 {
    let (h, which, flags) = (arg(c, 0), arg(c, 1), arg(c, 2));
    set_flags(h, which, flags, 3, flags)
}

/// CheckMenuItem(menu, item, flags): MF_CHECKED (8).
fn check_menu_item(c: &mut Cpu) -> u32 {
    let (h, which, flags) = (arg(c, 0), arg(c, 1), arg(c, 2));
    set_flags(h, which, flags, 8, flags)
}

/// RemoveMenu and DeleteMenu(menu, item, flags): DeleteMenu also destroys a submenu.
fn remove(c: &mut Cpu, delete: bool) -> u32 {
    let (h, which, flags) = (arg(c, 0), arg(c, 1), arg(c, 2));
    let mut m = menus();
    let Some((h, k)) = find(&m, h, which, flags) else {
        return 0;
    };
    let it = m.items.get_mut(&h).unwrap().remove(k);
    if delete && it.flags & MF_POPUP != 0 {
        m.items.remove(&it.id);
    }
    1
}

fn remove_menu(c: &mut Cpu) -> u32 {
    remove(c, false)
}

fn delete_menu(c: &mut Cpu) -> u32 {
    remove(c, true)
}

/// An item as AppendMenuA / InsertMenuA / ModifyMenuA give it: flags, id (or submenu), and a
/// string pointer when the content is a string.
fn item(flags: u32, id: u32, p: u32) -> Item {
    let text = if flags & (CONTENT | MF_SEPARATOR) == 0 && p != 0 {
        cstr(p)
    } else {
        String::new()
    };
    Item {
        flags: flags & !MF_BYPOSITION,
        id,
        text,
    }
}

fn append_menu(c: &mut Cpu) -> u32 {
    let (h, flags, id, p) = (arg(c, 0), arg(c, 1), arg(c, 2), arg(c, 3));
    menus()
        .items
        .get_mut(&h)
        .map(|v| v.push(item(flags, id, p)))
        .is_some() as u32
}

/// InsertMenuA(menu, before, flags, id, text): at the end when `before` is not found.
fn insert_menu(c: &mut Cpu) -> u32 {
    let (h, before, flags, id, p) = (arg(c, 0), arg(c, 1), arg(c, 2), arg(c, 3), arg(c, 4));
    let mut m = menus();
    let at = find(&m, h, before, flags).filter(|f| f.0 == h).map(|f| f.1);
    let Some(v) = m.items.get_mut(&h) else {
        return 0;
    };
    let k = at.unwrap_or(v.len());
    v.insert(k, item(flags, id, p));
    1
}

fn modify_menu(c: &mut Cpu) -> u32 {
    let (h, which, flags, id, p) = (arg(c, 0), arg(c, 1), arg(c, 2), arg(c, 3), arg(c, 4));
    let mut m = menus();
    let Some((h, k)) = find(&m, h, which, flags) else {
        return 0;
    };
    m.items.get_mut(&h).unwrap()[k] = item(flags, id, p);
    1
}

/// GetMenuStringA(menu, item, buf, n, flags): the length of the text (copied when buf is set).
fn get_menu_string(c: &mut Cpu) -> u32 {
    let (h, which, buf, n, flags) = (arg(c, 0), arg(c, 1), arg(c, 2), arg(c, 3), arg(c, 4));
    let m = menus();
    let Some((h, k)) = find(&m, h, which, flags) else {
        return 0;
    };
    let text = m.items[&h][k].text.clone();
    drop(m);
    if buf == 0 {
        text.len() as u32
    } else {
        put_cstr(buf, &text, n)
    }
}

fn ret0(_: &mut Cpu) -> u32 {
    0
}

fn ret1(_: &mut Cpu) -> u32 {
    1
}

/// GetMenuCheckMarkDimensions: 13 x 13.
fn check_mark_dimensions(_: &mut Cpu) -> u32 {
    13 << 16 | 13
}

pub const IMPORTS: &[Import] = &[
    ("LoadMenuA", 2, load_menu),
    ("CreateMenu", 0, create_menu),
    ("CreatePopupMenu", 0, create_menu),
    ("DestroyMenu", 1, destroy_menu),
    ("GetSubMenu", 2, get_sub_menu),
    ("GetMenuItemCount", 1, get_menu_item_count),
    ("GetMenuItemID", 2, get_menu_item_id),
    ("GetMenuState", 3, get_menu_state),
    ("EnableMenuItem", 3, enable_menu_item),
    ("CheckMenuItem", 3, check_menu_item),
    ("RemoveMenu", 3, remove_menu),
    ("DeleteMenu", 3, delete_menu),
    ("AppendMenuA", 4, append_menu),
    ("InsertMenuA", 5, insert_menu),
    ("ModifyMenuA", 5, modify_menu),
    ("GetMenuStringA", 5, get_menu_string),
    ("SetMenuItemBitmaps", 5, ret1),
    ("DrawMenuBar", 1, ret1),
    ("TrackPopupMenu", 7, ret0),
    ("GetSystemMenu", 2, ret0),
    ("GetMenuCheckMarkDimensions", 0, check_mark_dimensions),
];
