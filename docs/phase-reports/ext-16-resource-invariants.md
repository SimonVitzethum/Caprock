# Ausbaustufe 16 — Ressourcen-/Teardown-Invarianten (Härtung Ziel 2)

**Datum:** 2026-06-24 · **Status:** umgesetzt & in QEMU verifiziert.

Stellt sicher, dass eine isolierte PD nach ihrem Ende **keinerlei Ressourcen**
hinterlässt, und beweist das über **tausende spawn/destroy-Zyklen**, nach denen alle
Ressourcenstände exakt zur Baseline zurückkehren.

## Behobene Leaks

1. **ASID-Leak**: Die ASID-Vergabe war ein monoton wachsender Zähler
   (`ISO_ASID_NEXT`) → nach `MAX_VSPACES` isolierten PDs keine ASID mehr. Ersetzt
   durch eine **Free-List** über die `VSPACES`-Tabelle (`create_vspace` belegt einen
   freien Slot, `vspace_teardown` gibt ihn zurück → **ASID-Wiederverwendung**).
2. **Kernel-Stack-Leak** (latent, vom Churn-Test aufgedeckt): `spawn_isolated` und
   `spawn_isolated_native` riefen `record_user_kstack` **nicht** auf → der
   reclaim-Pfad fand den Pool-Slot nicht → jede endende isolierte PD leakte ihren
   16-KiB-Kernel-Stack-Slot. Behoben (beide Pfade registrieren den Slot jetzt; betraf
   auch die faultenden Demo-Proben).

## `destroy_isolated` — vollständiger Teardown

`system::destroy_isolated(tid)` gibt **alle** Ressourcen einer (nicht laufenden)
isolierten PD frei: Thread/TCB (`kill` → Zombie → frei), Stack-Region (`reap` → MEM),
VSpace-Tabellen L1/L2/L3 (`vspace_teardown` → MEM) + ASID-Slot, Kernel-Stack-Pool-Slot
(`reclaim_user_kstack`), `VSPACE_OF`-Eintrag. (Der Fault-/EXIT-Pfad nutzt dieselben
Bausteine.)

## Verifiziertes Ergebnis

`./test-qemu.sh` → **ALL PASS** (26 Checks). Neuer `churn`-Check: der Idle-Manager
fährt — nach Abschluss der isolierten Demos, mit maskierten IRQs für eine saubere
Messung — **2000 spawn/destroy-Zyklen** isolierter PDs. Vorher werden ausstehende
Zombies eingesammelt (Baseline-Snapshot), danach wird geprüft, dass **alle** Stände
exakt zurückkehren:

- `MEM.total_free()` (Stacks + Page-Tables),
- belegte TCB-Slots (`used_tcbs(0)`),
- freie ASID-/VSpace-Slots (`free_vspaces`),
- freie Kernel-Stack-Pool-Slots (`user_kstack_free_count`).

`churn : 2000 spawn/destroy-Zyklen … zurueck=Baseline: true; ALL PASS`. 5/5
gespacete Läufe stabil. Damit: keine Mapping-, ASID-, TCB- oder Speicher-Leaks über
tausende Zyklen.

## Abgedeckte / offene Ressourcen

- ✅ Threads/TCBs, Stacks, Page-Tables (L1/L2/L3), VSpaces, ASIDs, Kernel-Stack-Pool —
  über den Churn-Test verifiziert leckfrei.
- **Offen** (nicht vom Churn abgedeckt): per-PD-**Endpoints/Notifications** und
  **PD-Slots** (`create_pd`) haben noch keinen Destroy-Pfad (im Demo einmalig beim
  Setup erzeugt, nicht pro PD-Lebenszyklus); der native **Code-Frame** wird beim
  Teardown nicht zurückgewonnen (Stack schon). CDT-Konsistenz beim Cap-Revoke ist
  über den bestehenden `captest` abgedeckt. Diese sind die nächsten Schritte zur
  vollständigen PD-Destroy-Semantik.
