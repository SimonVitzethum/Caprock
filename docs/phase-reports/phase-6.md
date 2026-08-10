# Phasenbericht 6 — Microkit-Runtime + cap-gesicherte IPC

**Datum:** 2026-06-23 · **Status:** abgeschlossen, in QEMU verifiziert (8 Kerne)

## Was umgesetzt wurde

Die Microkit-Runtime im Kernelimage: **Protection Domains** mit eigenem
Capability-Space und **cap-gesicherte IPC** — schließt die Phase-5-Lücke
(IPC war per Endpoint-ID adressiert, ohne Zugriffskontrolle).

### Endpoints als Capabilities (`crates/caprock-cap`)

`ObjectKind::Endpoint(id)` ergänzt das eine Capability-System (kein zweites!).
`install_endpoint` prägt einen Endpoint-Cap; `lookup(ptr) -> (ObjectKind,
Rights)` löst einen Cap für die Invokation auf. `inspect`/Finalisierung wurden
auf mehrere Objektarten erweitert (Endpoints halten keinen Allokator-Speicher).

### Microkit-Runtime (`crates/caprock-microkit`)

- **Protection Domain** = Thread + eigener **Capability-Space**: eine Tabelle,
  die *lokale* Cap-Indizes auf *globale* `CapPtr`s abbildet. Ein Thread kann nur
  Endpoints invozieren, für die seine PD eine Cap hält.
- **`dispatch`** (cap-gesicherter Syscall): liest Syscall-Nr + lokalen
  Cap-Index, löst die Cap im PD-Cspace → globalen `CapSpace` auf, prüft
  **Objekttyp** (Endpoint) und **Rechte** (`CALL`→WRITE/senden, `RECV`/`REPLY`→
  READ/empfangen) und ruft dann die IPC-Operation. Fehlende Cap → `ERR_BADCAP`,
  falsches Recht → `ERR_RIGHTS`. **0 `unsafe`**.

### Kernel-Konsolidierung (`kernel/src/system.rs`)

Aller veränderlicher Kernzustand (Allokator, CapSpace, Scheduler, Endpoints,
PDs) liegt nun hinter **einem** `SYSTEM`-Lock → kein Lock-Ordering-Problem
zwischen Timer- und Syscall-Pfad. Operationen mit mehreren Teilzuständen
(cap-delete, IPC-Dispatch) nutzen disjunkte Feld-Borrows.

## Verifiziertes Ergebnis

`./test-qemu.sh` → **ALL PASS**. Demo: Client-PD (Send-Cap) ruft Server-PD
(Recv-Cap):

```
ipc     : call(10..40) -> 20..80   (alle korrekt)   ipc     : ALL PASS
mk      : CALL ohne Cap        -> result=1 (verweigert: true)   [ERR_BADCAP]
mk      : RECV mit Send-only-Cap -> result=3 (verweigert: true) [ERR_RIGHTS]
mk      : ALL PASS
```

Plus weiterhin: MMU, memtest, captest, sched (Preemption), 8/8 Kerne. Die
Negativtests beweisen die Zugriffskontrolle: **ohne passende Cap bzw. ohne
passendes Recht wird IPC verweigert** — die Sicherheitslücke aus Phase 5 ist zu.

## Getroffene Entscheidungen

- **Ein Capability-System, kein zweites:** Endpoints sind Objekte im globalen
  `CapSpace` (mit Derivation-Tree); der PD-Cspace ist nur eine Indirektion
  (lokaler Index → globaler `CapPtr`). Eine über den CDT widerrufene Cap wird im
  PD-Cspace automatisch ungültig (stale `CapPtr`). Vermeidet „parallele Systeme
  mit gleicher Funktion".
- **Rechte als Send/Recv:** `Rights::WRITE` = senden (`CALL`), `Rights::READ` =
  empfangen (`RECV`/`REPLY`). Delegation erzeugt eingeschränkte Caps (`mint`).
- **Ein gemeinsamer `SYSTEM`-Lock:** maximale Einfachheit/Korrektheit; Per-Kern-
  Aufteilung ist die spätere Optimierung (ADR 0004/0005).
- **PD-gebundene Threads:** Threads ohne PD (die Worker) können keine IPC
  ausführen (`ERR_NOPD`) — nur PD-Threads haben Autorität.

## Unsafe-Bilanz

- `caprock-microkit`: **0 unsafe** (reine Orchestrierung). `caprock-cap`-
  Erweiterung: 0 neue unsafe. Kernel-Crate weiterhin **0 `unsafe`-Blöcke**.
- Gesamtsystem-`unsafe` unverändert nur in `caprock-hal` (Low-Level),
  `caprock-sync` (Lock) und dem Boot-Assembler.

## Risiken / offene Punkte

- **PD-Cspace mit fester Größe (16 Caps/16 PDs):** ausreichend für Bring-up;
  dynamische cspaces später.
- **Statische PDs/Channels:** PDs + Caps werden im Code aufgesetzt (wie ein
  Microkit-Systembild). Ein deklaratives Systembeschreibungs-Format (`.system`-
  artig) ist eine spätere Ergänzung.
- **Kein Capability-Transfer in Nachrichten:** Rechte-/Memory-Cap-Übergabe per
  IPC (ADR 0004) und Zero-Copy-Nutzdaten via Memory-Cap weiterhin offen.
- **Keine Notifications, kein nicht-blockierendes `Send`:** folgen.
- **Integer-only Kontext + globaler Lock:** wie zuvor (Lazy-FP, Per-Kern-Locks TODO).

## Nächste Schritte (Phase 7 — Hot-Reload)

1. Komponenten-Lebenszyklus: Quiesce → Drain/Checkpoint → Swap (Cap-Revoke/
   -Delegate) → Resume über stabile Endpoint-Cap-Identität (ADR 0006).
2. Endpoint-Quiesce/-Rebind als Kernel-Primitive; Reload-Manager-PD (privilegiert).
3. Beispiel: einen „Treiber"-PD im laufenden System stoppen, ersetzen, fortsetzen
   — ohne Kernel-Neustart, Clients (über stabile Cap) unverändert.
4. Tests: Hot-Reload eines Server-PDs während ein Client weiterläuft.
