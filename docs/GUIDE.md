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

### Ziel: besser als das Original, nicht gleich

Der Port ist kein Emulator. Er soll nicht zu 100 % so laufen wie das Original auf seiner
Hardware, sondern **besser**: schneller, ruhiger, ohne die Fehler und Grenzen der damaligen
Technik. Drei Regeln folgen daraus:

1. **Fehlerhafte Spiel-Funktionen haben keinen Bestand.** Produzieren sie Fehler (falsche
   Sortierung, Grafikfehler, Abstürze), dürfen sie durch neuen Rust-Code ersetzt werden,
   statt den Fehler mit immer mehr Aufwand zu umgehen.
2. **Spiel-Code darf verändert werden, wenn es einen Vorteil bringt und das Ergebnis
   identisch ist.** Eine heiße Funktion (Projektion, Polygon-Aufbau) läuft in Rust, wenn sie
   dasselbe liefert: dieselben Operationen in derselben Reihenfolge, dieselbe Rundung, auch
   die Eigenheiten des Originals. Jede solche Ersetzung bekommt einen Prüf-Schalter
   (`<PREFIX>_..._CHECK=1`: beide laufen, Unterschiede werden geloggt) und wird erst
   übernommen, wenn über Millionen Aufrufe nichts abweicht (Abschnitt 8a).
3. **Grenzen der damaligen Technik dürfen behoben werden.** Was nur der Hardware von damals
   geschuldet ist, wird besser gemacht, solange das Spiel dasselbe Spiel bleibt (seine Logik,
   sein Ablauf, sein Aussehen in der Absicht): feste Logik-Rate mit Zwischenbildern statt
   30/60 fps (8b), Breitbild (8c), hohe Auflösung, MSAA, Texturfilter, ein Tiefenpuffer statt
   einer Ordering Table, die falsch sortiert, Sperren auf 640x480 (8d), Vsync (8e), Ruckeln
   durch ganzzahlige Kamera-Winkel (8b Punkt 7), Leerlauf-Schleifen, die einen Kern verheizen
   (LESSONS, Threads). Danach moderne Effekte (Abschnitt 11), als Einstellungen, die man
   ausschalten kann.

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

**Regel** (Abschnitt 0): Original-Funktionen haben keinen Bestand, wenn sie Fehler
produzieren; sie dürfen auch für einen Vorteil (Tempo) ersetzt werden, wenn das Ergebnis
identisch ist. Treue zum Original gilt nicht für seine Fehler und Grenzen.

- In `src/hooks.rs`: `(Us(0x406a30), meine_funktion)`. Adressen sind die der
  Referenz-Version (erste in `VERSIONS`); `.at()` rechnet für andere Versionen um.
- `call_original(c, adresse)` ruft die Original-Funktion; Argumente mit
  `win32::platform::arg(c, k)`.
- Was man über eine Funktion lernt, sofort in `data/symbols.txt` (Adresse, Name, Beschreibung,
  "?" bei Vermutungen). Absturzmeldungen und `rtools dis` zeigen dann Namen.
- Debug-Befehle: `Game::cheats` (Name, Shift+F-Taste, Funktion); ein Hook auf die
  Frame-Warte-Funktion ruft `cheats::run_pending(c)` und `savestate::frame()`.

## 8a. Tempo: bevor moderne Effekte kommen

Erst schnell, dann schön: Effekte (SSAO, Schatten, Licht, Bloom) kosten jeder 10–30 % der
Bildrate, und was vorher langsam war, wird mit ihnen nicht schneller. Nuclear Strike kam so
von ~175 auf ~790 fps ohne Effekte (Mission 1); das Ziel waren 500. Reihenfolge:

1. **Messen, nicht raten.** `<PREFIX>_SAMPLE=1` (Stichproben des Spiel-Threads alle 2 s:
   Spiel-Funktion oder Host-Modul), `<PREFIX>_PROF=1` (Zeit in Imports und beim Warten auf den
   Turn). Eigenen Rust-Code auflösen: `<PREFIX>_SAMPLE_RAW=<datei>` gegen eine Link-Map
   (`cargo rustc --release --bin <spiel> -- -C link-arg=/MAP:<datei>`, eigenes
   target-Verzeichnis) oder mit `CARGO_PROFILE_RELEASE_DEBUG=line-tables-only` und
   llvm-symbolizer. Jede Änderung als A/B messen: Läufe desselben Builds schwanken um ~5 %
   (LESSONS, Threads: Hyperthreads).
2. **Was das Kit schon mitbringt** (nichts zu tun, aber wissen, dass es da ist): Gastspeicher
   an fester Adresse (`rt::FIXED_BASE`, die Basis ist eine Konstante im Code: +11 %), x87 in
   Registern und 24-Bit-Rundung inline (`lift/x87.rs`), dichte Tabelle für indirekte Aufrufe,
   Hook-Index im Stub, Thunks pro Thread, Turn mit Zeitscheibe und Prioritäten (Loop-Köpfe
   rufen `rt_yield` nicht, solange die Scheibe läuft), Sleep(0) ohne Spinnen, unveränderte
   Texturzeilen werden nicht neu geladen, Zwischenbilder schreiben nur die Seiten zurück, die
   sie geändert haben (`tween::Statics`, Write-Watch).
3. **Kosten pro Aufruf im eigenen Code.** Bei Hunderttausenden Aufrufen pro Sekunde zählt
   alles: keine Locks, keine `Arc`-Klone, keine Hash-Maps mit SipHash, kein `Vec` pro
   Primitiv, keine Umgebungsvariable pro Aufruf (einmal lesen), keine Systemaufrufe
   (`glGetUniformLocation`; `Instant::now` in heißen Schleifen: `tsc`). Thread-lokale Kopien,
   Tabellen nach Adresse indiziert, offene Adressierung.
4. **Heiße Spiel-Funktionen in Rust (Regel 2, Abschnitt 0).** Zeigt das Profil eine Spiel-
   Funktion mit großem Anteil (Nuclear Strike: zwei Projektionen ~11 %, drei Polygon-Renderer
   ~15 %), sie als Hook in Rust schreiben, **identisch**: dieselben Operationen in derselben
   Reihenfolge, dieselbe x87-Rundung (unter 0xc7f: SSE-Rundung auf Abschneiden und jedes
   Ergebnis auf 24 Bit gekürzt; unter 0x27f: Double, beim Speichern zu Float gerundet), die
   Eigenheiten des Originals behalten (Register, die es nebenbei setzt; Zustände, die es in
   Gastspeicher schreibt). Ein Prüf-Schalter lässt beide laufen und vergleicht (Ergebnisse,
   geschriebener Speicher, Register): erst übernehmen, wenn über Millionen Aufrufe nichts
   abweicht. Fälle, die man nicht nachbaut (noch nicht geladene Daten), rufen das Original.
   Nicht lohnend: Funktionen, deren Kosten der Algorithmus ist (Einfügen in eine sortierte
   Liste), nicht der übersetzte Code.
5. **Zeichnen bündeln.** Zustand, den das Spiel pro Primitiv ändert (Alpha-Test), gehört in
   die Vertex-Daten, nicht in Uniforms, die einen Batch beenden. Ein Batch endet nur bei
   Wechseln, die es wirklich brauchen (Blend, Chroma-Key, Filter, Clip). glFlush nur, wenn ein
   zweiter Thread zeichnet. Uploads gesammelt, nicht zeilenweise.
6. **Den Tausch der Puffer vom Spiel-Thread nehmen.** SwapBuffers kostete im Treiber 0,4 bis
   0,7 ms pro Bild (~15 % bei 200 fps). Zeichnet das Spiel auf einem eigenen Thread mit
   geteiltem Kontext, zeigt ein eigener Present-Thread die Bilder (Fence setzen, übergeben,
   dort auf die GPU warten, blitten, tauschen): nuclear_rust_strike `window::present_later`
   (im Kit noch nicht: PLAN).
7. **Takten mit Schlafen und Spinnen** (`tween::Pacer`, `sleep_until`): ein Sleep dauert bis
   1 ms länger als verlangt; die letzten 1,5 ms werden gespinnt (240 fps: Bildabstände
   4,0–4,4 ms statt 3,7–4,7).

**Fertig, wenn** das Spiel ohne Effekte deutlich über der Bildwiederholrate des Bildschirms
läuft (Ziel: ein Mehrfaches, damit die Effekte Platz haben), alle Prüf-Schalter ohne
Abweichung laufen und das Profil keinen einzelnen Posten über ~10 % mehr zeigt, der sich
sinnvoll ersetzen lässt. Erst dann Abschnitt 11.

## 8b. Bildrate: Logik fest, Zwischenbilder dazu

Alte Spiele rechnen ihre Logik für eine feste Bildrate (oft 30 oder 60, PlayStation-Ports
60). Die Bildrate nie einfach freigeben: viel Logik zählt Frames statt Zeit oder nimmt ganze
Frames aus einem Zeitschritt (`dt >> 8`, `dt / schritt` mit verworfenem Rest). Über der
gedachten Rate hörten in Nuclear Strike Explosionen nie auf, Raketen zogen doppelt so viel
Rauch, und der Autopilot des Hakens machte nie einen Schritt (`256 / 0x114 = 0`). Stattdessen
(`win32::platform::tween`, Vorbilder: Future Cops `fps.rs`, Nuclear Strikes `pictures.rs`):

1. **Logik fest.** Die Spielschleife finden (Uhr lesen, auf den Frame warten) und am Anfang
   jedes Logik-Frames `LogicClock::frame` aufrufen (Hook auf die Uhr-Funktion, nur von der
   Schleife aus: Rücksprungadresse prüfen). Die Uhr des Spiels geht dann genau einen Frame
   pro Durchlauf weiter. Damit läuft das Spiel wie im Original; **erst testen**, dann weiter.
2. **Zeichen-Teil der Schleife finden.** Was ist Logik, was Zeichnen? Oft vermischt (Nuclear
   Strike: ein Durchgang bewegt die Objekte einer Kachel, der nächste zeichnet sie). Auch
   Zeichen-Code macht manchmal Logik (ein Rotor dreht sich beim Zeichnen weiter).
3. **Interpolieren, einen Frame zu spät.** Jedes Bild, auch das des Logik-Frames selbst,
   zeigt das Spiel zwischen den letzten zwei Logik-Frames (`LogicClock::alpha`). Kamera und
   Objekte (Position, Drehmatrix) werden nur *während des Zeichnens* dorthin gesetzt (Hooks
   der Zeichenfunktionen) und danach zurück: die Logik sieht nur ihre eigenen Werte. Wer
   beim Zeichnen ein Objekt selbst bewegt (eine Last am Haken), behält seine Position.
   Drehungen über Quaternionen (`mix_rotation`; elementweise gemischt wird ein schnell
   drehender Rotor zum Fächer), Sprünge (Respawn, Kameraschnitt) nicht interpolieren.
4. **Zwischenbilder.** In `LogicClock::frame` läuft vor der Wartezeit nur der Zeichen-Teil
   (Bildtausch, Gelände, Objekte, HUD, Ordering Table) als eigenes Bild.
5. **Nebenwirkungen zurücksetzen.** Nach dem Bildtausch `Statics::save`, nach dem Zeichnen
   `restore`: die statischen Daten des Spiels (Zufallsgenerator, Hilfsmatrizen, Zähler,
   HUD-Zustand). Ausnahmen in `left`: Listen eines Speicher-Managers, deren Einträge woanders
   liegen (zurückgesetzt zerstörten sie den Heap). Während des Zeichnens `hold_turn(true)`:
   sonst laufen andere Threads (Audio-Timer) und ihre Änderungen gehen mit verloren.
6. **Daten, die das Zeichnen für den nächsten Frame vorbereitet** (ein Gelände mit zwei
   Puffern): Zwischenbilder zeichnen aus dem Stand vor dem Zeichnen des Logik-Frames
   (vorher sichern, im Zwischenbild einsetzen, danach zurück). Sonst schmiert das Bild.
7. **Kamera aus den feinsten Werten des Spiels.** Viele Spiele führen Winkel intern genauer
   (Festkomma 16.16) als die Kamera, die sie daraus bauen (ganze 1/4096 Umdrehungen). In
   einer Kurve dreht die Kamera dann ungleichmäßig (7, 8, 6, 8 pro Frame), und eine Kamera,
   die im Abstand um den Spieler kreist, springt mit: das Bild zittert in Kurven, auch bei
   perfekter Interpolation (Nuclear Strike: `StrkCam_Update_` hält `[0x52301c]..` in
   16.16, `Camera_SetYPR_` bekommt `>> 16`). Abhilfe nur für die Anzeige: pro Logik-Frame
   den verfolgten Punkt merken (Kamera-Ort minus ihr Versatz, mit der *Matrix des Spiels*
   gerechnet, nicht einer eigenen: deren Rundung mal Abstand rauscht wieder), im Bild die
   feinen Winkel interpolieren, die Matrix selbst in Gleitkomma bauen (Formel aus der
   Matrix-Funktion des Spiels abschreiben, gegen ihr Ergebnis prüfen) und die Kamera vom
   gemerkten Punkt aus setzen. Erkennen: pro Logik-Frame Kamera- und Spieler-Ort loggen;
   schwanken die Schritte der Kamera, die des Spielers nicht, liegt es in der Logik.
8. **Eigene Pause (Overlay-Menü) wie die Pause des Spiels.** Hält ein eigenes Menü die
   Logik an, darf die feste Logik-Uhr danach nicht um die Pause hinter der Uhr des Spiels
   (`clock`, GetTickCount) zurückbleiben. Das Spiel liest beide: Nuclear Strikes Esc-Menü
   setzt beim Fortsetzen den Missionsstart aus der echten Uhr (`strikeClockPause_`), die
   Missionszeit aus der nachgehenden Logik-Uhr lief dann rückwärts, ein Zeitschritt unter 0
   ließ `Slerp_` rund 4 Milliarden Mal laufen: das Spiel fror ein. Richtig: die Pause so
   nachbilden wie das Spiel selbst. Die Logik-Uhr springt nach der Pause auf die echte Uhr
   vor (in ganzen Ticks des Spiels), und die Startzeiten, von denen das Spiel seine Zeit
   abzieht (Missionsstart, letzter Tick), rücken um dieselbe Pause weiter. Testen: eigenes
   Menü auf und zu, danach das Pausenmenü des Spiels auf und zu.

Fehlerbilder: Schlieren (etwas wird in Zwischenbildern nicht gezeichnet, das Spiel löscht den
Bildschirm nicht), Fächer (Matrix elementweise gemischt), fehlende Töne (Änderungen anderer
Threads zurückgesetzt), Absturz nach Speicheranforderung 0 (Heap-Listen zurückgesetzt), die
Steuerung hängt (Logik bekommt interpolierte Werte, oder ein Zeitschritt wird zu 0), Zittern
nur in Kurven (Kamera aus ganzzahligen Winkeln: Punkt 7), Einfrieren nach eigenem Menü und
Pausenmenü des Spiels (Logik-Uhr geht nach: Punkt 8).

## 8c. Breitbild (Hor+)

Ziel: links und rechts mehr sehen, nichts verzerrt, das HUD bleibt unverzerrt (in der
4:3-Mitte oder an den Rändern). Vorbilder: future_rust_cop `wide.rs`/`walker.rs`,
nuclear_rust_strike `wide.rs`.

1. **Die x-Skala der Projektion finden.** Viele Spiele haben getrennte Skalen für x und y
   (Nuclear Strike: `View_SetWindow_` rechnet sie aus dem Sichtfenster, alle Projektionen
   lesen sie). Diese x-Skala mal k = (4/3) / Seitenverhältnis, dazu Werte, die das Spiel aus
   ihr ableitet (Clip-Rand, rechte Clip-Grenze). Gibt es keine solche Skala: die
   Kamera-x vor jeder Projektion und jedem Sichtbarkeitstest mal k (Future Cop).
2. **Breit zeichnen.** Das Glide-Ziel bekommt das Seitenverhältnis des Fensters
   (`glide.rs` `resize(aspect, h)`), das 3D-Bild wird über die ganze Breite gestreckt
   (`View::Full`). Weil die x-Skala um k schmaler ist, stimmen die Proportionen wieder.
3. **HUD erkennen.** Primitive des HUD in `View::Hud(0.5)` zeichnen (4:3-Mitte). Wie man sie
   erkennt, hängt vom Spiel ab: Future Cop über die Displayliste (`walker.rs`), Nuclear Strike
   über die Primitive, die während der HUD-Funktion angelegt werden (Allokator gehookt,
   Adressen in einer Menge; Menge zu Beginn des HUD leeren), und die unter einem anderen
   Sichtfenster (Meldungsfenster). Umschalten beim Zeichnen jedes Primitivs.
4. **Während das HUD läuft, die 4:3-Skala.** Das HUD liest die Projektionsskala auch
   (Sprite-Größen, Marken). HUD-Marken, die aus 3D-Positionen projiziert und unverzerrt
   gezeichnet werden, landen mit der 4:3-Skala genau dort, wo das gestreckte 3D-Bild das
   Objekt zeigt. Wurden ihre Positionen schon im 3D-Durchgang (schmal) berechnet, muss man sie
   verschieben (Future Cop `to_screen`: x' = cx + (x − cx) / k).
5. **Sichtbarkeit verbreitern.** Was das Spiel vorab aussortiert (Kacheln vor der Kamera,
   Sichtkegel), um 1/k breiter machen. Achtung: Mengen, nach denen das Spiel beim Levelstart
   Speicher anlegt (Nuclear Strike: Kachelzahl für TileMgr), für das breiteste erwartete
   Verhältnis anlegen (mindestens 16:9), später nur bis dahin verbreitern.
6. **Nur im Spiel breit.** Menüs, Videos und Texte bleiben 4:3: breit nur, solange 3D/HUD
   gezeichnet wird (Nuclear Strike: HUD in den letzten 250 ms gezeichnet), Aspekt bei jedem
   Swap für das nächste Bild festlegen. Mit Zwischenbildern (8b): Änderungen an statischen
   Daten vor dem Sichern des Bildes machen, sonst setzt das Zurückschreiben sie zurück.

7. **Eigene Bildschirme bleiben 4:3.** Alles mit eigenem Rahmen über den ganzen Bildschirm
   (Missionsmenü, Cockpit-HUD mit kleinerem 3D-Fenster) auf 4:3 schalten, bevor es zeichnet:
   die Umschalt-Funktion hooken. Zeichnet das Spiel den Rahmen nur einmal und danach nur
   geänderte Teile, nach jedem Neuanlegen des Render-Ziels sein "alles zeichnen"-Flag setzen
   (LESSONS, Drawing once).

**Fertig, wenn** im 16:9-Fenster der Rotor/die Räder rund sind, Gelände bis an die Ränder
reicht, HUD und Meldungsfenster unverzerrt sind und Menüs 4:3 bleiben. Prüfen: an- und
ausschalten, Pausenmenü, Zielmarken über Gegnern (erst nachsehen, ob das HUD überhaupt
etwas projiziert), alle HUD-Modi des Spiels in jeder Spielauflösung, Fenstergröße und MSAA
im laufenden Spiel ändern.

## 8d. Sperren des Spiels umgehen

Erlaubt das Spiel etwas nur in einer Auflösung oder Konfiguration (Nuclear Strike: Cockpit-HUD
nur bei 640x480), ist der Rest oft schon allgemein (Positionen nach Bildschirmgröße). Dann die
Prüfung vor dem Kompilieren überspringen: `Game::code_patches` liefert (Adresse, alte Bytes,
neue Bytes), etwa ein `jmp` über die Prüfung und `nop`s. Alle Stellen suchen
(`rtools dis`, nach den Vergleichswerten wie `280h`/`1E0h`), sonst fehlt Vorbereitetes und
das Spiel liest Nullzeiger. Patches in den Speicher (`on_start`) erreichen den schon
kompilierten Code nicht.

## 8e. Vsync

`gl::set_vsync(true)` setzt das Swap-Intervall 1 (ab dem nächsten Swap). Eigene Einstellung,
nicht an die Bildrate gekoppelt (mit G-Sync will man es oft aus). Das Takten bleibt beim
Port: der Treiber puffert Bilder, der Swap kehrt sofort zurück, solange das Spiel langsamer
als der Bildschirm ist (LESSONS, Vsync).

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

## 11. Moderne Effekte (nach 8a)

Vorbilder: future_rust_cop und nuclear_rust_strike (`ao.rs`, `lights.rs`, `shadows.rs`,
`bloom.rs`, `heat.rs`, `zbuffer.rs`, `particles.rs`). Grundlage ist eine Tiefe pro Vertex
(Glide-Vertices haben meist keine: aus den Projektionen des Spiels holen). Effekte erkennt man
an den Funktionen des Spiels, nie an der Farbe (LESSONS, Modern effects). Jeder Effekt ist
eine Einstellung, die man ausschalten kann, und wird einzeln gemessen.

---

## Wenn etwas hängt oder abstürzt

1. Absturz: Meldung nennt Funktion und Adresse → `rtools dis <exe> <adresse>`.
2. Hängt: `<PREFIX>_WATCH=1` → letzter Import-Aufruf; oft ein Thread, der auf einen anderen
   wartet (siehe docs/LESSONS.md, "Threads").
3. Falsches Ergebnis einer Funktion: Differenztest, oder die Funktion als Test-Schnipsel
   nachbauen.
4. Grafikfehler: `<PREFIX>_SHOT` mit `{t}`, Bilder vergleichen.
5. docs/LESSONS.md durchsehen – viele Fallen sind dort schon beschrieben.
