//! Game functions replaced by Rust functions. A hook gets the guest's registers (`Cpu`); its
//! arguments are on the guest stack above the return address (`win32::platform::arg(c, k)`).
//! `call_original(c, address)` runs the game's own function (it pops the return address
//! itself); a hook that does not call it must pop the return address (and, for stdcall, the
//! arguments) and set eax itself.
//!
//! Addresses are the reference version's (`Us(0x...)`), `.at()` gives their place in the
//! version that runs. Write what you learn about each function into data/symbols.txt.

use recomp::rt::{Cpu, Hook};
#[allow(unused_imports)]
use recomp::rt::call_original;
#[allow(unused_imports)]
use recomp::ver::Us;

/// Every hook: (address in the version that runs, function).
pub fn all() -> Vec<(u32, Hook)> {
    let table: &[(Us, Hook)] = &[
        // (FRAME_WAIT, frame_wait),
    ];
    table.iter().map(|&(a, h)| (a.at(), h)).collect()
}

// Example: the game's frame wait, where debug commands and savestates can run.
//
// const FRAME_WAIT: Us = Us(0x406a30);
//
// fn frame_wait(c: &mut Cpu) {
//     win32::platform::savestate::frame();
//     win32::platform::cheats::run_pending(c);
//     call_original(c, FRAME_WAIT.at());
// }

#[allow(dead_code)]
fn _example(_c: &mut Cpu) {}
