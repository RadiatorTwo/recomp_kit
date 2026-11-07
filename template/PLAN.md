# PLAN — GAMETITLE

The way through: recomp_kit's docs/GUIDE.md. Tick off what is done, note what was learned.

## 1. Start

- [ ] EXE's SHA-256 in `VERSIONS`, `Identity` filled in (title, exe, folder, registry key)
- [ ] functions found (`rtools missing <exe>` runs; or a Ghidra export in data/functions.json)
- [ ] every instruction supported (`rtools missing` lists none; new ones go into recomp with a test)
- [ ] imports listed (`rtools imports <exe>`); the missing ones noted at start

## 2. Window and menu

- [ ] the game opens its window (missing imports implemented in win32, or in this crate)
- [ ] the main menu shows (`MYGAME_SHOT`)
- [ ] the menu answers to mouse and keys (`MYGAME_INPUT`)

## 3. Playing

- [ ] the first level starts and plays
- [ ] sound, music, videos
- [ ] saving and loading
- [ ] the whole game played through

## 4. Port

- [ ] setup from the CD or a CD image (`required`, `video_marker`)
- [ ] high resolution, widescreen, settings
- [ ] gamepad
- [ ] release (tag `v*`)

## Learned

(what was found out and why, dead ends included)
