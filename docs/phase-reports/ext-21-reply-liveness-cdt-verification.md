# Ausbaustufe 21 — Reply-Liveness, CDT-Property-Oracle & Verifikations-Audit

**Datum:** 2026-06-25 · **Status:** 1 reale Liveness-Lücke behoben, 1 Property-Oracle
ergänzt, beide sensitivitätsgeprüft; Langzeit-Stress + Property-Invarianten dokumentiert.

Verifikations-Audit (nicht: neue Features) mit drei Zielen. Zusammenfassung der
Ergebnisse, getesteten Szenarien, Property-Invarianten und Restrisiken.

---

## Ziel A — Reply-Liveness (dokumentierte Lücke behoben)

**Befund (real):** Stirbt der Server **nach `RECV`, vor `REPLY`** (KILL/EXIT/Fault),
blieb der blockierte `CALL`-Aufrufer **dauerhaft** hängen — das `Endpoint.caller`-Token
wurde nie konsumiert, und niemand kannte den schuldenden Server.

**Fix (Reply-Token mit Owner-Bindung):**
- `Endpoint.reply_owner: Option<ThreadId>` — der Server, der `caller` empfangen hat und
  ihm eine Antwort schuldet. Gesetzt beim Rendezvous in `call()`/`recv()`, **einmalig**
  konsumiert in `reply()` (kein Doppel-Reply), beim Caller-Tod via `purge_thread`
  verworfen.
- `Endpoint::owner_died(owner)` — stirbt der `reply_owner` mit ausstehendem `caller`,
  gibt es diesen zurück. `system::purge_ipc_queues` (aus **allen** Todespfaden:
  `KernelSched::kill`/`exit_current`, `el0_fault`, `kill_local`, `destroy_isolated`,
  `kill_remote`) sammelt verwaiste Caller und entblockt sie über `unblock_with_error`
  mit dem neuen ABI-Code **`ERR_SERVER_GONE` (5)** — der Client kehrt aus `CALL` mit
  einem Fehler zurück und kann reagieren, statt zu hängen.
- `ipc_audit` prüft jetzt zusätzlich `reply_owner`-Liveness (kein toter Owner mit Caller).

**Abgedeckte „Break"-Szenarien:**
| Szenario | Verhalten |
|----------|-----------|
| mehrfache Replies | `caller.take()` + `reply_owner=None` → 2. REPLY ist No-Op |
| verlorene Replies | lebender Caller wird via `frame_of` bedient |
| Reply auf toten Thread | `frame_of`==None → übersprungen |
| Reply während EXIT/KILL/Fault | **`owner_died` → Caller bekommt `ERR_SERVER_GONE`** |
| Reply während Hot-Reload (kill-basiert) | wie KILL (Todespfad) |
| Cross-Core-Reply | `unblock_with_error`/`reply` + IPI, kern-übergreifend |

**Regressionstest `rgone`:** Server `RECV`t einen Client-CALL und parkt ohne zu
antworten; der Manager killt ihn; der Client-CALL **muss** mit `ERR_SERVER_GONE`
zurückkehren. **Sensitivität:** `owner_died` deaktiviert → `DBG pending` zeigt
`rgone=false`, kein `SELFTEST COMPLETE` (Client hängt). Verifiziert: mit Fix
Ergebnis=5, ALL PASS.

**Bewusst NICHT umgesetzt (ehrlich):** ein **voller first-class Reply-Cap** (eigene
`ObjectKind::Reply` im CapSpace, per-PD-Reply-Slot mit grant-on-RECV, Revocation über
den CDT, echte **Budget-Donation** der MCS-Scheduling-Contexts) ist ein großer,
querschneidender Umbau, der alle bestehenden IPC-Server migrieren müsste (hohes
Regressionsrisiko). Umgesetzt wurde der **sicherheitskritische Kern** (Reply-Owner-Token
+ Tod/Revoke-Behandlung), der die dokumentierte Liveness-Lücke schließt. **Restrisiko:**
Reply nach **reiner Cap-Revocation** (Endpoint-/Recv-Cap entzogen, Server-Thread lebt
aber capless) entblockt den Caller noch nicht (die Cap-Finalisierung ruft nicht ins IPC).

---

## Ziel C — Property-basierte Verifikation (Oracle-Erweiterung)

Neu: **`CapSpace::audit_cdt()`** — read-only Property-Checker des Capability-Systems.
Prüft je `0`/Anomalie-Code:

1. toter CDT-Knoten (belegter Slot → unbelegtes Objekt),
2. **Refcount == Anzahl auf das Objekt zeigender Caps** (keine negativen/falschen Refcounts),
3. verlorenes/inkonsistentes Objekt (belegt ohne Cap / unbelegt mit Caps),
4. Eltern-Verkettung kaputt **oder Ableitung auf fremdes Objekt** (Rechteeskalation),
5. Geschwister nicht reziprok, 6. `first_child` kaputt, 7. Zyklus/keine Baumform.

**Damit abgesichert:** keine verlorenen Objekte, keine negativen Refcounts, keine toten
CDT-Knoten, keine Rechteeskalation über die Ableitung, Revocation/CDT bleiben baumförmig
und konsistent, Generationen über `resolve` konsistent.

**Integration:** `system::cap_audit_cdt` in (a) `captest` — 3 deterministische Checks
(abgeleiteter Baum / nach revoke / nach Teardown); (b) Ressourcen-Fuzzer — **mid-epoch**
(Caps maximal abgeleitet, Code 40+) **und** post-teardown (`fuzz_check`, Code 20+);
(c) IPC-Fuzzer — `ipc_audit` (Cap-Churn während IPC, Code 20+). **Sensitivität:**
`copy()` ohne `refcount += 1` → `audit_cdt` Code 2, captest CDT-Checks FAIL.

### Vollständige Property-Invarianten je Subsystem (durch Oracles abgesichert)

- **Capability-System** — `audit_cdt`: verlorene Objekte / negative Refcounts / tote
  CDT-Knoten / Rechteeskalation / Revocation-Vollständigkeit / Baumform.
- **Scheduler** — `Scheduler::audit` (ext-19): genau ein Zustand je Thread, keine
  Doppel-Einplanung, keine verlorenen Threads (lauffähig aber in keiner Queue), Bitmap
  ↔ Queues konsistent; MCS-Budgets über `set_budget`/Refill geprüft (ext-17/strand).
- **IPC** — `ipc_audit` (ext-20) + `reply_owner`-Liveness (ext-21): keine toten/
  duplizierten Threads in senders/receivers/caller/waiter, kein toter Reply-Owner,
  Endpoint-/Notification-Zustände konsistent.
- **VMM** — über die Ressourcen-Baseline: keine VSpace-/ASID-/Page-Table-/Mapping-Leaks
  (`free_vspaces`, `total_free` kehren zur Baseline zurück); Guard-Pages/W^X durch den
  `pages4k`-/`vspace`-Test + die isolierten Proben (Fremdzugriff faultet) abgesichert.
- **Ressourcen** — nach jedem Teardown exakt Baseline: MEM, TCBs (alle Kerne), VSpaces,
  ASIDs, Kernel-Stacks, Cap-Slots, Cap-Objekte (Churn-/Fuzzer-Oracles).

---

## Ziel B — Langzeit-Stress (Stand & Skalierung)

Die bestehende Test-Infrastruktur **ist** die Stress-Harness: der **Ressourcen-Fuzzer**
(Cap-CDT/MEM/VSpace/Spawn-Kill/Map-Unmap + SMP-Cap/MEM-Churn über 8 Kerne) und der
**IPC-Fuzzer** (10 nebenläufige Aktoren über 8 Kerne, Controller injiziert KILL/Reload/
MCS-Bind/Cap-Churn während IPC). Beide:

- **deterministisch** (fester Seed je Fuzzer),
- **Per-Epoche-Oracle** über alle Subsysteme (Ressourcen-Baseline + CDT + IPC-Queues +
  Scheduler), **ein einziger Verstoß beendet den Lauf** (mit Anomalie-Code + Seed),
- **skalierbar**: die Op-Zahl ist linear in `FUZZ_EPOCHS`/`FUZZ_OPS_PER_EPOCH`/
  `FUZZ_SMP_ITERS`/`IPCF_EPOCHS`. Die SMP-Churn-Phase fährt bereits **zehntausende**
  konkurrierende Cap/MEM-Ops je Lauf.

**Ehrliche Grenze:** „Millionen Ops / Stunden" ist **rein eine Frage der Konstanten** —
durch die **single-thread-TCG-Wall-Clock** begrenzt, nicht durch das Design. Ein
*echter* mehrstündiger Lauf wurde in dieser Sitzung nicht gefahren. Die zwei Fuzzer
laufen **sequenziell** (jeder mit voller Oracle-Abdeckung); ein *gleichzeitiger*
Mega-Lauf aller Subsysteme in einer Schleife würde gemeinsame Baselines erfordern
(Cross-Fuzzer-Interferenz sonst → Falsch-Positive) — dokumentierter nächster Schritt.

---

## Geprüft & als korrekt befunden (keine neuen Bugs)

Über die nebenläufigen Fuzzer + adversarialen Events + die neuen Property-Oracles blieb
das System konsistent (außer in den Sensitivitätstests, die die Oracles absichtlich
brechen). Insbesondere: das Capability-System hält Refcounts/CDT/Baumform über zufällige
copy/mint/move/delete/revoke-Sequenzen; der Scheduler hält seine Zustands-Invarianten
über tausende Spawn/Kill/Block/Unblock; IPC-Queues bleiben frei von toten/duplizierten
Threads über KILL/EXIT/Reload/MCS-während-IPC.

## Verbleibende Restrisiken & Empfehlungen (nächste Härtungsstufe)

1. **Voller first-class Reply-Cap** (eigene Objektart, per-PD-Slot, CDT-Revocation,
   **Budget-Donation**) — der hier umgesetzte Owner-Token-Kern ist die Grundlage.
2. **Reply nach reiner Cap-Revocation** (ohne Thread-Tod): Cap-Finalisierung sollte
   ausstehende Caller des Endpoints mit `ERR_SERVER_GONE` entblocken.
3. **Globale CAPS-Sperre** serialisiert alle Cap-Lookups → IPC-Durchsatz-Engpass
   (funktional + Skalierung); per-PD-Cap-Cache / feinere Cap-Locks.
4. **Echter Mehrstunden-Lauf** (Mio. Ops) auf realer HW / KVM statt TCG; + ein
   vereinheitlichter, gleichzeitiger Mega-Fuzzer mit gemeinsamer Baseline.
5. **VMM-Tiefenprüfung**: ein expliziter „keine verwaisten Page-Tables / keine
   doppelten Mappings"-Walker (derzeit indirekt über die Ressourcen-Baseline).
6. **u32-Generationen** (ABA nach 2³² Recyclings) — 64-bit, falls extreme Churn-Lauf-
   zeiten erwartet werden.

**Ergebnis:** `./test-qemu.sh` = **ALL PASS (32 Checks, ~5 s)**. Reply-Liveness-Lücke
geschlossen + regressionsgetestet; CDT-/Refcount-Property-Oracle ergänzt + in alle
Fuzzer integriert + sensitivitätsgeprüft; Property-Invarianten je Subsystem dokumentiert.
