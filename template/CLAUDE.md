# CLAUDE.md — GAMETITLE in Rust

Hand-over for future sessions. What is done and what is open: **PLAN.md** (keep it current).

## Goal

A native port of GAMETITLE (YEAR) with recomp_kit: the player's own EXE recompiled to native
code (Cranelift), Windows and DirectX in Rust (`win32` crate), playable on Windows and Linux.
Only code is shared; players supply their own game files. **Never commit anything derived
from the EXE** (translated code, extracted data). `game_files/` is git-ignored. Function
boundaries (`data/functions.json`) and our notes by address (`data/symbols.txt`) are fine.

## The user

- New to reverse engineering; explain in plain words, give a recommendation, not a survey.
- Speaks German; works in short sessions and watches usage limits.
- Confirm before anything outward-facing or destructive (pushing, deleting).
- Clean Rust first, performance later.

## How it works

- The framework, copied from recomp_kit into `crates/` (`recomp`: discovery, lifter, JIT,
  runtime; `win32`: the Windows layer; `tools`: `rtools`). This project's own copy: change it
  freely, with a test for the lifter (`crates/recomp/tests/`). recomp_kit's docs/GUIDE.md is
  the way through a new port, docs/LESSONS.md the known traps. A fix that other games need
  too: also into recomp_kit (ask the user).
- `src/main.rs`: `Identity` (names, paths, registry key, CD) and `impl Game` (functions,
  self-modifying code, hooks, extensions).
- `src/hooks.rs`: game functions replaced by Rust (`call_original` runs the game's own).
- `data/symbols.txt`: what we know about the game's functions and data, by address.
- Switches (prefix `MYGAME_`): `TRACE_API=1` every import call, `TRACE_MSG=1` window
  messages, `WATCH=1` where the game is every 2 s, `SHOT=file.png` screenshots,
  `INPUT="5000:click@320,240 8000:Enter"` scripted input, `RECORD=file.txt` records input,
  `NO_VIDEO=1`, `WINDOW=1280x960`, `COUNT=file` calls per function.

## Known facts about the game

(addresses, threads, renderer, CD check, file formats: add as they are learned)

## Next steps

See PLAN.md.
