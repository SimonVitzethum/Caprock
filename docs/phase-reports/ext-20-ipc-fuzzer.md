# Ausbaustufe 20 — IPC-State-Machine-Fuzzer (Audit Bereich H, Teil 2)

**Datum:** 2026-06-25 · **Status:** umgesetzt & in QEMU verifiziert (Oracle-sensitiv);
1 reale Härtung (eager-purge) mit Sensitivitätsnachweis.

Gezielter Audit des bislang **am schwächsten abgedeckten Bereichs**: die
**IPC-Zustandsmaschinen** (Endpoint/Notification) unter Nebenläufigkeit + adversariale
Ereignisse zu ungünstigen Zeiten — Death-during-IPC, Hot-Reload-during-IPC,
MCS-Erschöpfung-während-IPC, Cap-Ops-während-IPC, cross-core.

## Härtung (Fund: toter TCB in IPC-Queue / Corpse-Fill-DoS)

Der frühere Fix (ext-18) ließ tote Einträge in `senders`/`receivers` **liegen**
(lazy-skip in `recv`/`call`). Bei fester Queue-Größe (`QCAP = 32`) füllen gekillte,
blockierte Threads die Queue mit **Leichen** → echte Sender werden verdrängt (DoS);
zudem würde ein `REPLY`/`SIGNAL` in einen recycelten Frame schreiben.

**Fix — EAGER PURGE beim Thread-Tod.** `system::purge_ipc_queues(tid)` entfernt den
sterbenden Thread aus **allen** Endpoint-Strukturen (`senders`/`receivers`/`caller`)
und Notification-`waiter`n. Aufgerufen aus **allen** Todespfaden (`KernelSched::kill`,
`exit_current`, `el0_fault`, `kill_local`, `destroy_isolated`, `kill_remote`) —
**ohne** gehaltenen `SCHEDS`-Lock (Ordnung `EPS/NTFNS < SCHEDS`, nie zwei Objekte
gleichzeitig). Bausteine: `Endpoint::purge_thread`, `Notification::purge_thread`,
`TidQueue::remove`. Damit gilt die Invariante **„keine toten TCBs in IPC-Queues"**
(zusätzlich zum lazy-skip als Defense-in-Depth).

## Der Fuzzer

**Aktoren** (EL1, über cores 1..7; Controller auf core 0 → Cross-Core-IPC inhärent):
2 Server (RECV/REPLY), 4 Clients (CALL), 1 Notifier (SIGNAL), 2 Waiter (WAIT), 1
**Fast-Signaller** (async SIGNAL ohne Waiter — hohe Op-Rate). Jeder Aktor wählt seine
Operationen per eigenem PRNG; gelegentlich Selbst-`EXIT` mitten im Betrieb.

**Controller** injiziert je Epoche Meta-Events zu **ungünstigen Zeiten**:

- **KILL** eines zufälligen Aktors (cross-core via `kill_remote`) — Death-during-IPC,
  inkl. Kill eines in CALL/RECV/WAIT blockierten Threads (eager-purge + Oracle prüfen
  die Queue-Konsistenz).
- **Hot-Reload-Modell**: einen Server `retire_receiver` + killen → respawn bringt die
  neue Instanz auf demselben stabilen Endpoint (Clients CALLen währenddessen).
- **MCS-Budget-Bind** an Nicht-Server (Erschöpfung/Refill während IPC).
- **balancierte Cap-Churn** auf einer Kopie eines aktiv genutzten Endpoint-Caps
  (CDT/Refcount während IPC; baseline-neutral).
- tote Aktoren werden **respawnt** (IPC-Last bleibt erhalten).

**ORACLE** je Epoche (`system::ipc_audit`, per-Objekt unter Lock, Liveness verschachtelt
`EPS/NTFNS → SCHEDS`): keine **toten** TCBs in Endpoint-/Notification-Queues, keine
**Duplikate**, kein toter `caller`/`waiter`; `Scheduler::audit` je Kern: Bitmap-
Konsistenz, keine doppelt/dead/falsch-priorisiert eingeplanten und **keine verlorenen**
Threads (lauffähig, aber in keiner Queue — fängt die Stranding-Klasse).

**Teardown**: `STOP` → laufende Aktoren beenden sich selbst (Schleifenkopf-Check),
blockierte werden per `kill_remote` getötet; danach muss die **Ressourcen-Baseline**
(MEM/TCB/VSpace/kstack/Cap-Slots/Cap-Objekte über alle Kerne) **exakt** zurückkehren.

## Verifikation & Sensitivität

`./test-qemu.sh` → **ALL PASS** (31 Checks, neuer `ipcfuzz`-Check). Auf einem ruhigen
Host endet ein erfolgreicher Lauf in **~4 s** (s. Infra unten). Beispiel:

```
ipcfuzz : 8/8 Epochen, ~500 IPC-Ops, ~24 KILLs (8 Kerne); Oracle-Anomalie=0; Teardown-Baseline=true
ipcfuzz : ALL PASS
```

**Oracle-Sensitivität bewiesen:** mit absichtlich deaktiviertem eager-purge meldet der
Oracle bereits in **Epoche 1** **Anomalie=3** (toter `waiter` in einer Notification) →
`FAILURES`. Danach zurückgerollt. Das belegt: der Oracle fängt genau die Klasse, die
der eager-purge verhindert.

**Keine weiteren neuen Bugs**: über die nebenläufigen Aktoren + adversarialen Events
blieb der Oracle (außer im Sensitivitätstest) sauber, und die Baseline kehrte stets
zurück.

## Infrastruktur: `system_off`

`hal::psci::system_off` (PSCI SYSTEM_OFF via HVC): der Kernel fährt QEMU nach
bestandenem Selbsttest sauber herunter (`== SELFTEST COMPLETE ==`). Ein erfolgreicher
Lauf endet, **sobald er fertig ist** (auf ruhigem Host wenige Sekunden), statt bis zum
Timeout im Idle zu warten — verhindert ein Abschneiden des Berichts und erlaubt
umfangreichere Fuzz-Läufe. Bei einem Fehlschlag bleibt der Kernel im Idle → Timeout.

## Grenzen & Restrisiken

- **Roher IPC-Durchsatz ist TCG-limitiert.** Unter single-threaded QEMU-TCG teilen sich
  8 vCPUs eine Host-CPU; jede blockierende Cross-Core-IPC ist teuer (2 IPIs +
  Kontextwechsel, seriell emuliert), und die **eine globale `CAPS`-Sperre serialisiert
  alle Cap-Lookups** → der aggregierte IPC-Op-Durchsatz ist begrenzt. Pro Lauf entstehen
  einige hundert bis tausend IPC-Ops + dutzende adversariale KILLs über 8 Kerne, je
  Epoche oracle-geprüft. Die Op-Zahl **skaliert mit `IPCF_DELAY`** (größeres Lauffenster
  → mehr Kern-Wechsel → mehr Ops); „zehntausende" in einem CI-Lauf sind unter TCG
  unpraktisch, auf schnellerer/realer HW aber erreichbar. *Empfehlung:* die einzelne
  globale `CAPS`-Sperre ist auch funktional ein Skalierungs-Engpass (per-PD-Cap-Cache
  oder feinere Cap-Locks).
- **Kein Reply-Timeout**: stirbt ein Server **nach** RECV, **vor** REPLY, bleibt der
  Client bis zum nächsten KILL blockiert (Liveness, nicht Korruption; vom Controller
  über KILL+respawn aufgelöst). Reply-Caps (seL4-Stil) wären die Härtung.
- **Host-Last-Sensitivität (nur Testumgebung)**: unter schwerer Host-CPU-Last emuliert
  TCG die gesamte Suite drastisch langsamer; ein Lauf kann dann das (großzügige)
  Timeout überschreiten — kein Kernel-Hang (auf ruhigem Host ~4 s, ALL PASS).
