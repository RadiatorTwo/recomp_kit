# PLAN — recomp_kit

## Done (2026-11-07)

- Core (`recomp`) and Windows layer (`win32`) extracted from future_rust_cop, game-independent:
  `Identity` + `Game` trait; fcop_plain reaches Future Cop's main menu and first mission.
- Fixes from the sibling ports: xlat, segment register moves, pushf/popf, bsf/bsr, AC/ID,
  fnstenv/fldenv/fnsave/frstor, CPUID register order (was wrong in every copy), jump tables
  end decoding, `find_functions`, DLL exports/rebase/combine.
- `rtools`, template, `new-game.ps1`, GUIDE.md, LESSONS.md.
- GitHub repository github.com/RadiatorTwo/recomp_kit, branch `main` (the template's git
  dependency points there).

- New games get a copy of crates/recomp, win32 and tools (`new-game.ps1`), no dependency on
  this repo. The existing ports keep their own cores; they do not move onto the kit.
- Speed from nuclear_rust_strike (its mission ~330 -> ~520 fps): x87 24-bit rounding inline,
  x87 registers cached along straight runs, dense `rt_lookup`, turn slice and quick Glide
  imports, `on_main` per thread, uniform lookups cached (window blit), sampling profiler
  (`SAMPLE`), `DUMP`. fcop_plain checked: menu and first mission.

## Next

Grow the kit for future games, one piece at a time, with tests:

1. From the sibling ports, when a game needs it:
   - black_white: ~90 kernel32 imports (INI, file mapping, Find*, CreateProcess, date/time),
     ~37 user32, advapi32 RegEnum*/RegCreate/RegDelete, DLL loading (`Module`, DllMain,
     `run_library`), winsock stub, small DLLs (IMM32, COMCTL32, SHELL32, VERSION), mscab/iscab;
   - rustschweine: gdi32 bitmap DCs, DirectDraw 7, Direct3D 7 (`d3d.rs`), CD audio from .cue;
   - rust_metal: Direct3D 9, COM passthrough, ole32 class registry, winmm timers, crash
     reports, sampling profiler, Media Foundation music; speed work in the lifter (flag
     crossings, cmp+jcc fusion, x87 values in variables, inline string ops, Direct dispatch);
   - nuclear_rust_strike: Watcom images, CodeView names, Miles Sound System.
   - future_rust_cop: the Glide effect points (vertex depth, `post_3d`, primitive census) if a
     Glide game wants its effects.
2. Difftest (Unicorn, GPL-2.0) as its own crate `crates/difftest` (still in future_rust_cop).
3. Video formats beyond MPEG-1 (Bink, Smacker, AVI) when a game needs them.
