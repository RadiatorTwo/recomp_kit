//! What the core needs to know about the game that runs: the prefix of its environment
//! switches (`FCOP_` for Future Cop: `FCOP_COUNT`, `FCOP_TRACE_API`...). Set once at start.

use std::sync::OnceLock;

static PREFIX: OnceLock<&'static str> = OnceLock::new();

/// The switches' prefix, e.g. "FCOP" (without the underscore). Before the first `env`.
pub fn set_env_prefix(p: &'static str) {
    let _ = PREFIX.set(p);
}

/// The prefix ("RECOMP" if none was set).
pub fn env_prefix() -> &'static str {
    PREFIX.get().copied().unwrap_or("RECOMP")
}

/// The full name of a switch: `env_name("COUNT")` = "FCOP_COUNT".
pub fn env_name(name: &str) -> String {
    format!("{}_{name}", env_prefix())
}

/// A switch's value, if set.
pub fn env(name: &str) -> Option<String> {
    std::env::var(env_name(name)).ok()
}

/// Is the switch set to something other than "" and "0"?
pub fn env_on(name: &str) -> bool {
    env(name).is_some_and(|v| !v.is_empty() && v != "0")
}
