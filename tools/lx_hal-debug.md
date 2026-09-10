# hal::debug-Entwurf (Strang 10, Patch-Text für HAL-Besitz)

Stand 2026-09-10: DEBUG 33 (WRITE_MEM) im Kernel verdrahtet, 34 (SINGLE_STEP)
und 35 (HWBREAK) fail-closed (`ERR_BADSYS`, „Antrag ok, Pfad fehlt"). Was fehlt,
ist die HAL-Seite. NICHT angewendet (HAL ist B-Besitz) — zur Review/Anwendung
durch B. Voller Code im Strang-10-Bericht (Session-Protokoll 2026-09-10).

## Ablage (Vorschlag)

NEU `crates/caprock-hal/src/x86_64/debug.rs` + `aarch64/debug.rs`, Fassade
arch-neutral (`pub mod debug`, `lib.rs`-Re-Export wie `cpu`/`exception`).
Begründung: DRx vs. DBGBVR/CR verschieden, API-Fläche gemeinsam.

## Inhalt x86 (Skizze)

- `RFLAGS_TF = 1<<8` (SDM Vol.1 §17.3 prüfen!), `HWBREAKS = 4`,
  `DR7_L = [1<<0, 1<<2, 1<<4, 1<<6]` (nur Lx, SDM Vol.3 §18.2 prüfen!).
- `single_step_scharf/unscharf(frame)`: TF-Bit im GESPEICHERTEN TrapFrame
  (NIEMALS live — TF im Kernel finge den Kernel selbst).
- `hwbreak_setzen/löschen(fach, addr)`: DR0-3 + DR7, Start nur exec/Länge-1.
- `dr6_lesen()` (B0-B3 vs. BS/Bit-14 prüfen!), `sync_zu_thread(addrs, maske)`
  (Diff gegen Kernpuffer, Muster `sync_tls`).
- `DbHook`-Typ + `set_db_hook` (Muster FAULT_HOOK); `#DB`-Zweig in
  `handle_exception()` VOR Fault-Zweig (Vektor 1, nur EL0, sonst fatal wie #NM).

## Inhalt aarch64 (Spiegel)

PSTATE.SS, MDSCR_EL1.SS, DBGBVRn/DBGBCRn n=0..3, nur exec. Bitnummern prüfen!

## Offene Prüfpunkte

1. Alle SDM/ARM-Bitnummern gegen Handbuch verifizieren (aus dem Gedächtnis!).
2. IDT Vektor 1 wirklich instanziiert?
3. IST für #DB nötig oder Thread-Stack ok?
4. TCB-Heimat (`dbg_addr/maske`, Migration!) + Vergabe-Tabelle + ERR_NOSPACE.
5. aarch64 EC-Codes + VENTRY-Haken.
