//! An on/off setting: a REG_DWORD in the game's key, an environment switch over it
//! (`<PREFIX>_<env>`, "0" off, anything else on). Kept in memory after the first read.

use super::advapi32;
use std::sync::atomic::{AtomicU8, Ordering};

/// 0 off, 1 on, 2 not read yet.
pub struct Setting {
    on: AtomicU8,
    value: &'static str,
    env: &'static str,
    default: bool,
}

impl Setting {
    /// `value`: the registry value's name; `env`: the switch without the prefix.
    pub const fn new(value: &'static str, env: &'static str, default: bool) -> Setting {
        Setting { on: AtomicU8::new(2), value, env, default }
    }

    pub fn get(&self) -> bool {
        let e = self.on.load(Ordering::Relaxed);
        if e != 2 {
            return e == 1;
        }
        let on = match recomp::config::env(self.env) {
            Some(v) => v != "0",
            None => advapi32::value("", self.value).map_or(self.default, |(_, d)| d.first().is_some_and(|&b| b != 0)),
        };
        self.on.store(on as u8, Ordering::Relaxed);
        on
    }

    /// Saved, and used from now on.
    pub fn set(&self, on: bool) {
        self.on.store(on as u8, Ordering::Relaxed);
        advapi32::set_value("", self.value, 4, &(on as u32).to_le_bytes());
    }
}
