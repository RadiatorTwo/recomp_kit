# PLAN — recomp_kit

## Done (2026-11-07)

- Core (`recomp`) and Windows layer (`win32`) extracted from future_rust_cop, game-independent:
  `Identity` + `Game` trait; fcop_plain reaches Future Cop's main menu and first mission.
- Fixes from the sibling ports: xlat, segment register moves, pushf/popf, bsf/bsr, AC/ID,
  fnstenv/fldenv/fnsave/frstor, CPUID register order (was wrong in every copy), jump tables
  end decoding, `find_functions`, DLL exports/rebase/combine.
- `rtools`, template, `new-game.ps1`, GUIDE.md, LESSONS.md.

## Next

1. **GitHub repository** for recomp_kit (the template's git dependency points at
   github.com/RadiatorTwo/recomp_kit; ask the user before creating or pushing).
2. **Move future_rust_cop onto the framework**: its game modules (fps, camera, cheats, walker,
   wide, light, lights, ao, shadows, heat, bloom, effects, gamepad, dialog/extend, the hook
   half of dinput, controller parts of user32, dsound volume hook, savestate mission hook)
   become its crate; needs:
   - `GlideExt` to grow the effect points (vertex depth, `post_3d`, primitive census) or
     Future Cop keeps its own glide.rs (decide when moving);
   - `GuestLayout` if a game needs another memory layout (all five use the same today).
3. Bring over from the siblings, one at a time, with tests:
   - black_white: ~90 kernel32 imports (INI, file mapping, Find*, CreateProcess, date/time),
     ~37 user32, advapi32 RegEnum*/RegCreate/RegDelete, DLL loading (`Module`, DllMain,
     `run_library`), winsock stub, small DLLs (IMM32, COMCTL32, SHELL32, VERSION), mscab/iscab;
   - rustschweine: gdi32 bitmap DCs, DirectDraw 7, Direct3D 7 (`d3d.rs`), CD audio from .cue;
   - rust_metal: Direct3D 9, COM passthrough, ole32 class registry, winmm timers, crash
     reports, sampling profiler, Media Foundation music; speed work in the lifter (flag
     crossings, cmp+jcc fusion, x87 values in variables, inline string ops, Direct dispatch);
   - nuclear_rust_strike: Watcom images, CodeView names, Miles Sound System.
4. Difftest (Unicorn, GPL-2.0) as its own crate `crates/difftest` (still in future_rust_cop).
5. Move the siblings onto the framework.
6. Video formats beyond MPEG-1 (Bink, Smacker, AVI) when a game needs them.
