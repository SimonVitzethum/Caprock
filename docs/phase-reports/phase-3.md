# Phasenbericht 3 — Capability-System-Kern

**Datum:** 2026-06-23 · **Status:** abgeschlossen, in QEMU verifiziert (8 Kerne)

## Was umgesetzt wurde

Das konzeptionelle Herzstück: ein Capability-System mit Derivation-Tree,
Revocation und Speicher-Finalisierung — vollständig in **sicherem Rust**.

### `crates/caprock-cap`

- **`CapSpace`** — flache Tabelle typsicherer Capability-Slots (CTE-artig:
  Capability + MDB-/CDT-Knoten) + Objekt-Tabelle. Feste Kapazität
  (256 Slots / 128 Objekte) → allokationsfrei, deterministisch.
- **CDT (Capability-Derivation-Tree):** jeder Slot trägt
  `parent/first_child/next_sibling/prev_sibling`-Verkettungen. Abgeleitete Caps
  sind Kinder ihrer Quelle → Basis für rekursive Revocation.
- **Objekt-Tabelle mit Referenzzählung:** mehrere Caps können dasselbe Objekt
  referenzieren; das Objekt (Phase 3: `Memory(PhysRegion)`) wird **finalisiert**
  (Speicher an den Allokator zurück), sobald die *letzte* Cap gelöscht wird.
- **Generations-Handles (`CapPtr`):** Slot-Index + Generation; gelöschte/
  verschobene Slots erhöhen die Generation → stale Handles werden erkannt.
- **Operationen:** `install_memory`, `copy`, `mint` (Rechte + Badge), `move_cap`,
  `delete` (blatt-only; sonst `HasChildren`), `revoke` (löscht alle Abkömmlinge,
  iterativ statt rekursiv). Rechte können bei Ableitung **nur eingeschränkt**
  werden (`intersect` → keine Privilege-Escalation).

### Kernel-Integration — `kernel/src/mm.rs`

Der globale Allokator wurde aus dem Phase-2-Selbsttest in ein sauberes
`mm`-Modul gehoben: `static PHYS: SpinLock<PhysAllocator>` +
`static CSPACE: SpinLock<CapSpace>`, mit dünnen Wrappern, die die Lock-Reihenfolge
(CSPACE vor PHYS) kapseln, wo die Finalisierung beide Locks braucht.

### Test — `kernel/src/selftest.rs`

`captest` exerziert den vollen Lebenszyklus; `test-qemu.sh` prüft
`captest : ALL PASS`.

## Verifiziertes Ergebnis

`./test-qemu.sh` → **ALL PASS**. captest (14 Checks):

```
captest: PASS  install belegt keinen zusaetzlichen Speicher
captest: PASS  Wurzel-Cap: RW, refcount 1
captest: PASS  refcount 4 nach copy/mint/copy
captest: PASS  Wurzel hat 2 Kinder
captest: PASS  mint: Rechte READ + Badge gesetzt
captest: PASS  keine Rechte-Eskalation bei copy
captest: PASS  altes Handle nach move ungueltig
captest: PASS  neues Handle nach move gueltig
captest: PASS  delete mit Kindern -> HasChildren
captest: PASS  revoke entfernt alle Abkoemmlinge (refcount 1)
captest: PASS  Abkoemmling-Handles ungueltig nach revoke
captest: PASS  Speicher noch gehalten (Wurzel lebt)
captest: PASS  Wurzel-Handle nach delete ungueltig
captest: PASS  Finalisierung gibt Speicher zurueck
captest : ALL PASS
```

Plus weiterhin: MMU+Caches, memtest ALL PASS, 8/8 Kerne online + Ticks.

## Getroffene Entscheidungen

- **Trennung CDT vs. Refcount:** Der Derivation-Tree (parent/child) dient der
  *Revocation* (Abkömmlinge finden); ein separater Objekt-**Refcount** bestimmt
  die *Finalisierung*. Das macht „letzte Referenz?“ eindeutig, unabhängig von
  Re-Parenting-Feinheiten — einfacher und robuster als die kombinierte
  seL4-MDB-Logik, gleiche Semantik.
- **`delete` ist blatt-only**, `revoke` räumt Abkömmlinge: klare, komponierbare
  Semantik (ganzen Teilbaum löschen = `revoke` + `delete`).
- **Rechte-Monotonie:** Ableitung schränkt Rechte nur ein (`intersect`).
- **Flacher Cap-Space (eine Tabelle):** demonstriert CDT/Revocation vollständig;
  mehrstufige CNode-Adressierung (Guards/Radix) und mehrere cspaces sind bewusst
  zurückgestellt.
- **Lineare `MemoryCap` (Phase 2) wird beim `install` konsumiert:** der Besitz
  geht ins Capability-System über; dort sichert der Refcount die genau-einmalige
  Freigabe (`free_region`).

## Unsafe-Bilanz

- `caprock-cap`: **0 unsafe** (reine Datenstruktur-Logik über feste Arrays).
- Kernel-Crate weiterhin **0 `unsafe`-Blöcke**.
- Gesamtsystem: `unsafe` ausschließlich in `caprock-hal` (Low-Level, erlaubt),
  `caprock-sync` (Lock-Primitive) und 2 Zeilen Boot-Assembler.

## Risiken / offene Punkte

- **Feste Kapazitäten (256 Slots / 128 Objekte):** ausreichend für Bring-up;
  ein dynamischer Backing-Store (Caps/Objekte aus Cap-verwaltetem RAM) ist eine
  spätere Erweiterung.
- **Nur `Memory`-Objekte:** Endpoint/Notification/Reply/TCB/Device kommen als
  weitere `ObjectKind`-Varianten in den Phasen 4–5 hinzu (Modell ist erweiterbar).
- **Ein flacher Cap-Space:** noch keine per-Subjekt-cspaces / CNode-Caps /
  Adress-Auflösung mit Guards. Für Multi-Prozess-Delegation (Phase 4+) nötig.
- **Finalisierungs-Korrektheit** liegt jetzt am Refcount/CDT-Bookkeeping (nicht
  mehr am linearen Typ); durch `captest` abgedeckt, perspektivisch Modellprüfung.

## Nächste Schritte (Phase 4 — Threads + Scheduler)

1. `ObjectKind::Tcb` + Thread-Control-Block; Kontextwechsel (aarch64, FP lazy).
2. Deterministischer Pro-Kern-Bitmap-Scheduler mit fester Affinität (ADR 0005);
   Timer-Tick treibt Preemption.
3. Idle-Thread pro Kern; erste lauffähige Kernel-Threads.
4. Tests: Scheduler (mehrere Threads, deterministische Reihenfolge), Preemption.
