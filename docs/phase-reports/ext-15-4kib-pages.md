# Ausbaustufe 15 — 4-KiB-Seiten (Ziel 1 der Härtung)

**Datum:** 2026-06-24 · **Status:** umgesetzt & in QEMU verifiziert.

Erweitert den VMM (bisher nur 2-MiB-Blöcke) um echte **4-KiB-Seiten-Mappings** (L3),
ohne die 2-MiB-Pfade zu verlieren — sie bleiben als Fastpath für große Regionen
(isolierte Stacks/Code). Damit: feingranulare W^X-Rechte, Guard Pages und kleine
Frames.

## Mechanismus

- **mmu** (`vspace_map_page`/`vspace_unmap_page`/`vspace_collect_l3s`,
  `enum UserPerm { Rw, Rx, Ro }`): Wird ein 2-MiB-Block erstmals seitenweise belegt,
  legt der Kernel eine **L3-Tabelle** an (alle 512 Seiten zunächst EL1-only, den
  Block spiegelnd → Kernel behält Sicht aufs RAM) und hängt sie in die L2 ein;
  einzelne Seiten werden dann auf EL0 mit `Rw`/`Rx`/`Ro` gesetzt. `Rx` = `AP_RO_EL0`
  + ausführbar (W^X). Alle User-Seiten `nG` (ASID-getaggt).
- **system** (`vspace_map`/`vspace_unmap`): mappen eine Region `[base,len)`
  seitenweise; 2-MiB-ausgerichtete 2-MiB-Regionen mit RW/RX nutzen den Block-Fastpath.
  `vspace_teardown` gibt jetzt zuerst alle per-PD-**L3-Tabellen** (via
  `vspace_collect_l3s`) und dann L1+L2 an `MEM` zurück.
- **MAP/UNMAP-Syscalls**: unterstützen 4-KiB-Granularität (Frame-Caps können einzelne
  Seiten repräsentieren); das **Recht der Cap** bestimmt die Seitenrechte
  (EXEC→RX, WRITE→RW, READ→RO). `map_into_thread` erlaubt dem Kernel das Vorab-Mappen
  feingranularer Seiten (z. B. RW/RO/Guard) in eine PD-VSpace.
- `PER_CORE` 32 → **64** (die Demo erzeugt ~33 Threads auf core 0; sonst scheitert der
  letzte Spawn an der TCB-Partition).

## Verifiziertes Ergebnis

`./test-qemu.sh` → **ALL PASS** (25 Checks).

- **`vmm`** (jetzt 4 KiB): die VMM-Probe mappt einen **4-KiB**-Frame per `MAP`,
  schreibt/liest ihn, `UNMAP`t **die einzelne Seite**, liest erneut → Fault
  (korrektes Einzel-Seiten-`UNMAP`).
- **`pages4k`** (neu): die Seiten-Probe bekommt eine 12-KiB-Region feingranular
  gemappt — P (RW), P+4 KiB (RO, vorbefüllt), P+8 KiB (**Guard**, ungemappt). Sie
  schreibt P (RW), liest P+4 KiB (RO) → Badge PAGES, schreibt dann die Guard-Seite →
  **Fault** (`FAR=…+8 KiB`). Belegt: mehrere einzelne Seiten, gemischte RW/RO-Rechte,
  Guard-Page-Fault.

5/5 gespacete Läufe stabil. `kernel_end` ≈ 1,875 MiB (< 2 MiB W^X-Grenze; FP_STATES
wächst mit NTHREADS — künftig ggf. auf einen kleineren FP-Kontext-Pool umstellen).

## Erfüllte Test-Items (Ziel 1)

- ✅ Mapping mehrerer einzelner Seiten (P, P+4 KiB; vmm 4-KiB-Frame).
- ✅ gemischte RX/RO/RW-Seiten (RW + RO in `pages4k`; RX via nativem Loader/Code-Block).
- ✅ Guard-Page-Faults (`pages4k` Guard-Seite).
- ✅ korrektes `UNMAP` einzelner Seiten (`vmm` 4-KiB).

## Offene Punkte

- ELF-Segmente mit unterschiedlichen Rechten (Loader nutzt aktuell einen 2-MiB-
  Code-Block; mit 4-KiB-Seiten ließen sich .text=RX / .rodata=RO / .data=RW trennen).
- FP_STATES-Sizing (s. o.).
