# Ausbaustufe 12 — Allgemeiner VMM (Frame-Caps + map/unmap + VSpace-Teardown)

**Datum:** 2026-06-24 · **Status:** umgesetzt & in QEMU verifiziert.

Erweitert die per-Prozess-VSpaces (ext-11) zu einem **allgemeinen VMM**: isolierte
PDs können Frames **zur Laufzeit** dynamisch und **cap-gated** in ihre VSpace mappen
und wieder entmappen; VSpaces werden beim PD-Ende **abgebaut** (Tabellen
zurückgegeben).

## Mechanismus

- **Frame-Caps**: keine neue Cap-Art — `ObjectKind::Memory(PhysRegion)` *ist* die
  Frame-Cap (benennt eine physische Region + Rechte). `MAP`/`UNMAP` lösen sie im
  PD-Cspace auf; das `WRITE`-Recht autorisiert das Mapping.
- **Syscalls** `MAP`/`UNMAP` (ABI 10/11): mappen den über eine Memory-Cap (lokaler
  Slot) bezeichneten Frame **identity** als EL0-RW in die **eigene** VSpace bzw.
  entfernen ihn. Nur in einer isolierten VSpace sinnvoll (trusted PDs sehen im SAS
  ohnehin alles).
- **VSpace-Verwaltung** (`kernel/src/system.rs`): `VSPACES[asid]` (L1/L2 je VSpace);
  `create_vspace` (leere VSpace), `vspace_map_region`/`vspace_unmap_region` (setzen
  einen L2-Block auf EL0-RW bzw. EL1-only + ASID-Flush), `vspace_teardown` (gibt
  L1+L2 an `MEM` zurück, flusht die ASID). `spawn_isolated` nutzt jetzt diesen Pfad
  (create + map der Stack-Region).
- **mmu** stellt die Primitive: `vspace_create_base`, `vspace_map_block`,
  `vspace_unmap_block`, `flush_asid` (TLB nach ASID — global getaggte Kernel-/
  Code-Einträge bleiben gültig).
- **Teardown** beim Thread-Ende: der `el0_fault`-Hook baut die VSpace der gefaulteten
  isolierten PD ab — **sicher**, weil `sync_vspace` `TTBR0` zuvor auf den nächsten
  Thread umgeschaltet hat (nicht mehr die tote VSpace).

**Granularität:** dieses Inkrement mappt **2-MiB-Frames** (ein L2-Block je Frame) —
kein on-demand-L3/Splitting, daher minimale MMU-Komplexität. 4-KiB-Granularität
(L3-Tabellen) ist die nächste Verfeinerung. Sperrordnung bleibt zyklenfrei:
`VSPACES` und `MEM` werden nie verschachtelt gehalten (Tabellen-Frames werden vor
dem Eintrag alloziert bzw. nach dem Austrag freigegeben).

## Verifiziertes Ergebnis

`./test-qemu.sh` → **ALL PASS** (22 Checks). Neuer `vmm`-Check: eine isolierte PD
(`vmm_probe`) führt den vollen Zyklus aus:
1. `MAP` (Slot 0 = Memory-Cap für Frame G) → G wird EL0-RW.
2. schreibt `0xDEADBEEF` nach G, liest zurück → Roundtrip ok → `SIGNAL` Badge MAPPED.
3. `UNMAP` (Slot 0) → G wieder EL1-only.
4. liest G erneut → **Fault** (`FAR=0x40800000`) → Kernel beendet sie + baut die
   VSpace ab.

Beobachtet: `VMM-Probe mappte+beschrieb Frame=true; isolierte Faults gesamt=2`
(`iso_probe` faultete bei Fremdzugriff, `vmm_probe` nach dem UNMAP) → `map` macht den
Frame zugänglich, `unmap` entzieht ihn wieder, alles cap-gated in der eigenen VSpace.
5/5 gespacete Läufe stabil.

## Offene Punkte

- **4-KiB-Granularität**: on-demand-L3-Allokation + Block-Splitting (Frame-Caps für
  kleine Frames). Aktuell 2-MiB-Frames.
- `MAP` mappt identity (VA==PA); freie Wahl der VA bräuchte echte Übersetzung.
- EXIT-isolierter PDs: Teardown läuft heute über den Fault-Pfad; der seltene Fall
  „isolierte PD ruft `EXIT`" könnte die Tabellen leaken (Demo faultet stattdessen).
- Nächste geplante Schritte: **Shared-Memory-IPC** (denselben Frame in zwei VSpaces
  mappen) und **natives Code-Laden** je isolierter VSpace.
