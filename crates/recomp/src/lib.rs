//! The recompiler core: a 32-bit x86 Windows program (EXE and DLLs) decoded with iced-x86,
//! every function lifted to Cranelift IR and compiled to host code; game pointers stay 32-bit
//! in a 4 GB guest address space. Game-independent: what a game adds comes as parameters
//! (function lists, `smc::SmcSpec`, hooks) and through `config`.

pub mod config;
pub mod discover;
pub mod image;
pub mod jit;
pub mod lift;
pub mod rt;
pub mod smc;
pub mod ver;
