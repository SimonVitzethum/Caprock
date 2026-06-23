# ADR 0003 — Capability-System und physisches Speichermodell

**Status:** umgesetzt (Kern) · **Datum:** 2026-06-23

> **Umsetzungsstand:**
> - *Phase 2* (`crates/sel4lake-mem`): physisches Speichermodell — `MemoryCap`
>   als lineare (move-only) Capability, `PhysAllocator` (koaleszierende Freiliste).
> - *Phase 3* (`crates/sel4lake-cap`): der **CDT-Kern** ist implementiert —
>   `CapSpace` (typsichere Slots + Derivation-Tree), Objekt-Tabelle mit Refcount
>   und Finalisierung, Generations-Handles (`CapPtr`), Operationen `copy`/`mint`/
>   `move`/`delete`/`revoke` (Rechte nur einschränkbar). Alles sicheres Rust
>   (0 unsafe). Siehe `docs/phase-reports/phase-3.md`.
> - **Noch offen** (spätere Phasen): weitere `ObjectKind`s (Endpoint/Notification/
>   TCB/Device), mehrere cspaces + mehrstufige CNode-Adressierung (Guards/Radix),
>   dynamischer Backing-Store für Caps/Objekte. Das unten skizzierte Design bleibt
>   die Richtschnur.

## Motivation

Capabilities sind die einzige Autoritätsquelle im System: ein Prozess darf genau
das, wofür er eine gültige Capability besitzt (Speicher, IPC, Geräte, Dienste,
Prozessverwaltung). Im SAS-Design (ADR 0002) ersetzen sie die fehlende
MMU-Isolation als Autoritätsschicht.

## Referenz: seL4-Modell

seL4 speichert Capabilities in *CTEs* (Cap + MDB-Knoten), organisiert in *CNodes*
(radix-baumartig adressiert), und verwaltet Ableitungen über einen *Capability
Derivation Tree* (CDT/MDB). Kernoperationen: `copy`, `mint` (Rechte
einschränken + Badge), `move`, `delete`, `revoke` (rekursiv alle Abkömmlinge).
Speicher entsteht aus *Untyped*-Regionen via `retype`. Siehe
[`../seL4-architecture-map.md`](../seL4-architecture-map.md).

## Analysierte Lösungsansätze (Repräsentation)

### A) seL4-getreue, zeigerbasierte CTE/CNode-Struktur in `unsafe` Rust
- **+** Bekannt, fastpath-tauglich.
- **−** Viel `unsafe` (rohe Zeiger, intrusive Listen) — widerspricht der
  Unsafe-Minimierung. Verworfen als Default.

### B) Typsichere CNode als Slot-Tabellen + CDT als Arena mit Indizes (gewählt)
CNodes sind `Slot`-Arrays; der Derivation-Tree ist eine **Arena** (`Vec`-artiger
Pool im kernel-eigenen, capability-verwalteten Speicher) mit **Index-Handles**
statt roher Zeiger. Capabilities sind ein Rust-`enum` mit Typ-Tag + Rechtemaske
+ optionalem Badge.

- **+** Speichersicher ohne `unsafe`: Index-Handles statt Zeiger, der
  Borrow-Checker schützt die Strukturen.
- **+** `enum` macht Cap-Typen explizit und erschöpfend matchbar.
- **+** Revocation = Baum-Traversierung über Indizes; deterministisch begrenzbar
  (seL4-Zombie-Technik für lange Revokes übernehmbar).
- **−** Arena-Indizes brauchen Generationszähler gegen Stale-Handles
  (gut beherrschbar, kostet ein Wort pro Slot).

### C) Reine Capability-Pointer-Maschine ohne CDT
- **−** Ohne Ableitungsbaum keine korrekte rekursive Revocation. Verworfen.

## Entscheidung

**Ansatz B.** Capabilities als Rust-`enum` mit Rechtemaske und Badge; CNodes als
typsichere Slot-Tabellen; CDT als generationsgesicherte Arena mit Index-Handles.
Operationen `copy/mint/move/delete/revoke` als sichere Methoden; `unsafe` nur
dort, wo Cap-verwalteter Rohspeicher initial typisiert wird (Retype-Grenze).

**Capability-Klassen (geplant):** `Memory` (physische Region + Rechte
RWX/Owner), `Endpoint`, `Notification`, `Reply`, `Tcb`/`Process`, `Device`
(MMIO-Region/IRQ), `Service` (benannter Dienst-Endpoint), `CNode`.

## Physisches Speichermodell (Phase 2)

- Beim Boot erhält der Root-Prozess **Memory-Capabilities** über das gesamte
  freie physische RAM (eine oder wenige große Regionen, aus DTB ermittelt).
- Ein **capability-basierter Allokator** vergibt Unterregionen durch *Splitten*
  einer Memory-Cap (analog seL4-`retype`, aber auf physische Regionen statt
  Untyped-Objekte). Splits sind Kinder im CDT der Eltern-Cap.
- Prozesse können RAM **anfordern** (Split aus einer besessenen Region),
  **zurückgeben** (Merge/Delete der Kind-Cap), **übertragen** (Move/Copy der Cap
  über IPC) und **von Eltern erhalten** (Delegation beim Spawn).
- Da Adresse == Phys-Adresse (ADR 0002), beschreibt eine Memory-Cap direkt einen
  realen Bereich `[base, base+len)` mit Rechten — kein Mapping nötig.

## Sicherheitsauswirkungen

Least Privilege per Konstruktion; Delegation und Revocation über den CDT; keine
Ambient Authority. Korrektheit der Revocation ist sicherheitskritisch und wird
mit Tests (Phase 3) und perspektivisch mit Modellprüfung abgesichert.

## Performanceauswirkungen

Cap-Lookup ist ein Array-Index in CNode-Slots (O(Tiefe), klein). Der Fastpath
(ADR 0004) cached aufgelöste Endpoint-Caps, um Lookups im Hot-Path zu vermeiden.
Arena statt Allokationen pro Cap hält die Operationen alloc-frei und
deterministisch.
