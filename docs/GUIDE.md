# Anleitung: ein altes Windows-Spiel mit recomp_kit portieren

Für Spiele der späten 90er / frühen 2000er: 32-Bit-Windows-EXE (x86), DirectX 3–7 oder Glide,
DirectSound, DirectInput. Das Spiel wird nicht emuliert: Beim Start wird die EXE des Spielers
in Maschinencode des eigenen Rechners übersetzt (Cranelift), und Windows/DirectX ersetzt
Rust-Code. Jeder Schritt hat ein **Fertig, wenn …**. Erst dann weiter.

Das neue Projekt bekommt eine **Kopie** des Frameworks (`crates/recomp`, `crates/win32`,
`crates/tools`). Sie gehört dem Spiel: ändern, wo das Spiel es braucht. Werkzeuge:
`cargo build --release -p tools` im Spiel-Projekt gibt `target/release/rtools`.

---

## 0. Spielregeln (vor allem anderen)

- Nur **Code** wird geteilt. Spieler bringen ihre eigene Spiel-Kopie mit.
- **Nie committen**: die EXE, Daten aus ihr, übersetzten Code, Texturen, Sounds, Videos.
  `game_files/`, `*.iso`, `*.bin` sind in `.gitignore`.
- Erlaubt: Adressen, Namen und eigene Beschreibungen (`data/symbols.txt`), Funktionsgrenzen
  aus Ghidra (`data/functions.json`), Adress-Karten zwischen Versionen.

## 1. Projekt anlegen

```powershell
C:\!Code2\recomp_kit\tools\new-game.ps1 -Name gun_metal -Title "Gun Metal" -Target C:\!Code2\gun_metal
```

Das Skript kopiert das Framework nach `crates/` und notiert in `PLAN.md`, von welchem
recomp_kit-Commit. Dann im neuen Projekt:

1. Spiel-EXE nach `game_files/<Ordner>/` legen (CD-Inhalt oder installierter Ordner).
2. SHA-256 der EXE in `VERSIONS` in `src/main.rs` eintragen:
   `certutil -hashfile GAME.EXE SHA256` (Kleinbuchstaben, ohne Leerzeichen).
3. `Identity` ausfüllen: Titel, EXE-Name, Ordner, Registry-Schlüssel (Strings in der EXE:
   `rtools imports` zeigt, ob `RegOpenKeyExA` benutzt wird; der Schlüssel steht im Klartext in
   der EXE, z. B. mit `strings`), CD-Label falls das Spiel die CD prüft.

**Fertig, wenn** `cargo build --release` läuft und das Programm die EXE findet ("Compiling the
game..." im Splash).

## 2. Funktionen finden

Der Recompiler übersetzt Funktion für Funktion. Zwei Wege:

- **Ohne Ghidra** (Standard der Vorlage): `find_functions` folgt Aufrufen ab dem
  Einsprungpunkt und sucht Code-Adressen in Daten. Für Future Cop: 1778 von 2176.
- **Mit Ghidra** (genauer, empfohlen sobald es ernst wird): EXE in Ghidra analysieren, Funktionen
  als JSON exportieren (`[{"addr": "0x401000", "size": 123, "name": "FUN_00401000"}, ...]`)
  nach `data/functions.json`, in `functions()` `load_functions(include_str!(...))` nehmen.

Prüfen: `rtools missing game_files\X\GAME.EXE` (zählt Funktionen, listet nicht unterstützte
Instruktionen), `rtools dis <exe> <adresse>` (eine Funktion so, wie die Discovery sie sieht).

**Fertig, wenn** `rtools missing` eine plausible Zahl Funktionen meldet.

## 3. Fehlende Instruktionen

`rtools missing` listet Instruktionen, die der Lifter (`crates/recomp/src/lift.rs`) noch nicht
kann. Jede kommt in die Kopie im Spiel-Projekt, mit einem Test in
`crates/recomp/tests/` (Vorbild: `misc.rs`, `x87.rs`; der Test baut ein kleines x86-Stück mit
iced, übersetzt es und vergleicht mit dem erwarteten Ergebnis). FPU-Befehle: `lift/x87.rs`.
Neue Instruktionen helfen jedem Spiel: zusätzlich ins recomp_kit übernehmen.

**Fertig, wenn** `rtools missing` "0 instructions not supported" meldet und
`cargo test --release -p recomp` grün ist.

## 4. Imports: bis das Fenster aufgeht

Das Spiel ruft Windows-Funktionen (Imports). Beim Start listet das Programm die noch fehlenden
("N imports not implemented yet: ..."); ruft das Spiel eine davon, hält es mit Meldung an.

- `rtools imports <exe>`: alle Imports nach DLL.
- Allgemein nützliche Imports kommen in `crates/win32/src/platform/<dll>.rs` (Tabelle
  `IMPORTS`: Name, Anzahl stdcall-Argumente oder -1 für cdecl, Rust-Funktion). Vor dem
  Schreiben in den Geschwister-Ports schauen (siehe PLAN.md des Frameworks: black_white hat
  ~90 weitere kernel32-Funktionen, rust_metal Direct3D 9, rustschweine Direct3D 7 und
  CD-Audio, nuclear_rust_strike Miles Sound System).
- Nur dieses Spiel betreffende DLLs: `Game::import` im Spiel-Projekt.
- Fehlersuche: `<PREFIX>_TRACE_API=1` (jeder Import-Aufruf mit Aufrufer),
  `<PREFIX>_TRACE_MSG=1` (Fenster-Nachrichten), `<PREFIX>_WATCH=1` (wo das Spiel hängt, alle 2 s),
  `<PREFIX>_TRACE_REG=1` (Registry).

**Fertig, wenn** das Fenster des Spiels aufgeht.

## 5. Selbstmodifizierender Code

Alte Software-Renderer schreiben in ihren eigenen Code (Konstanten, Sprungweiten). Das meiste
findet `Smc::find` allein (`mov [code-adresse], x`). `rtools smc <exe>` zeigt, was gefunden
wurde. Schreibt das Spiel Code aus einer Tabelle um (Future Cop: 555/565-Pixelformate), die
Tabelle in `Game::smc` eintragen (`PatchTable { at, variants }`) und mit
`rtools smc <exe> --patch <adresse>:<n>` prüfen.

**Fertig, wenn** das Spiel nicht mehr an Stellen abstürzt, die ihren eigenen Code ändern
(Absturzmeldungen nennen die Funktion).

## 6. Bild

- 2D-Menüs über DirectDraw (`ddraw.rs`) laufen meist sofort.
- 3D: Glide (`glide.rs`, OpenGL) ist fertig. Direct3D 7/9 in den Geschwister-Ports
  (rustschweine `d3d.rs`, rust_metal `d3d9/`) – ins Framework holen, wenn ein Spiel es braucht.
- Screenshots: `<PREFIX>_SHOT=shot.png` (alle 0,5 s; `{t}` im Namen = Millisekunden, jedes Bild
  bleibt), `<PREFIX>_WINDOW=1280x960`.

**Fertig, wenn** das Hauptmenü auf dem Screenshot so aussieht wie im Original.

## 7. Eingabe skripten (reproduzierbare Tests)

- `<PREFIX>_INPUT="5000:click@320,240 8000:Enter 9000:Up*2000"`: Eingaben zu festen Zeiten
  (ms nach Spielstart; Mausposition in 640x480-Spielpixeln; `*n` = n ms halten;
  `text@Name` tippt; `pad@A*500` Gamepad; `@datei.txt` liest die Liste aus einer Datei).
- `<PREFIX>_RECORD=datei.txt`: eigene Eingaben aufnehmen, später mit `INPUT=@datei.txt`
  abspielen. F11 setzt eine Marke mit Screenshot.
- `<PREFIX>_NO_VIDEO=1`: Videos sofort zu Ende.

**Fertig, wenn** ein Skript reproduzierbar ins erste Level führt.

## 8. Hooks: eigene Rust-Funktionen statt Spiel-Funktionen

Für alles, was über "Windows nachbauen" hinausgeht (Widescreen, Bildrate, Cheats, Gamepad-
Belegung, Fehler des Originals): eine Spiel-Funktion durch Rust ersetzen.

- In `src/hooks.rs`: `(Us(0x406a30), meine_funktion)`. Adressen sind die der
  Referenz-Version (erste in `VERSIONS`); `.at()` rechnet für andere Versionen um.
- `call_original(c, adresse)` ruft die Original-Funktion; Argumente mit
  `win32::platform::arg(c, k)`.
- Was man über eine Funktion lernt, sofort in `data/symbols.txt` (Adresse, Name, Beschreibung,
  "?" bei Vermutungen). Absturzmeldungen und `rtools dis` zeigen dann Namen.
- Debug-Befehle: `Game::cheats` (Name, Shift+F-Taste, Funktion); ein Hook auf die
  Frame-Warte-Funktion ruft `cheats::run_pending(c)` und `savestate::frame()`.

## 9. Ton, Video, Netzwerk, Installation

- DirectSound und PlaySound: fertig (`dsound.rs`, Mischung auf cpal).
- MPEG-1-Videos über DirectShow: fertig (`dshow.rs`, `mpeg/`). Andere Formate (Bink, Smacker,
  AVI) fehlen noch.
- DirectPlay (TCP/UDP): fertig für das, was Future Cop braucht (`dplay.rs`).
- Installation: `Identity::required` (was neben der EXE liegen muss), `video_marker` (eine
  Datei im Video-Ordner der CD). Das Setup holt alles aus einem CD-Image (.iso/.bin, auch UDF).

## 10. Differenztest und Release

- Differenztest gegen Unicorn (prüft jede Funktion gegen einen echten x86-Emulator): noch im
  future_rust_cop-Repo (`src/bin/difftest.rs`), nicht im Framework (siehe PLAN.md).
- Release: Version in `Cargo.toml`, Tag `v0.1.0` pushen; `.github/workflows/build.yml` baut
  Windows und Linux und macht eine GitHub-Release.

---

## Wenn etwas hängt oder abstürzt

1. Absturz: Meldung nennt Funktion und Adresse → `rtools dis <exe> <adresse>`.
2. Hängt: `<PREFIX>_WATCH=1` → letzter Import-Aufruf; oft ein Thread, der auf einen anderen
   wartet (siehe docs/LESSONS.md, "Threads").
3. Falsches Ergebnis einer Funktion: Differenztest, oder die Funktion als Test-Schnipsel
   nachbauen.
4. Grafikfehler: `<PREFIX>_SHOT` mit `{t}`, Bilder vergleichen.
5. docs/LESSONS.md durchsehen – viele Fallen sind dort schon beschrieben.
