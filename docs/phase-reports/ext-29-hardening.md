# ext-29 — Härtung aus dem Sicherheits-Review (Abschlussbericht)

Invarianten: [invariants.md §1a, §10](../invariants.md).

## Ziel

Ein Review des Gesamtkerns (Capability-System, IPC, MMU, Krypto, Nebenläufigkeit) hat sechs Befunde
ergeben. Fünf davon sind hier behoben (einer ist kein Befund, s. u.). Leitlinie: **Mechanismus statt
Konvention** — jede Invariante soll dort erzwungen werden, wo sie strukturell nicht umgangen werden
kann, statt an der Disziplin der Aufrufstellen zu hängen.

| # | Befund | Klasse | Status |
|---|---|---|---|
| 1 | Cap-Leck beim IPC-Grant → geteilte Cap-Tabelle erschöpfbar | Cross-PD-DoS | behoben + Regressionstest `grantlk` |
| 2 | Keine Nullung bei (Re-)Allokation → Datenremanenz zwischen Subjekten | Vertraulichkeit | behoben + Test `zerotest` |
| 3 | `RwSpinLock` maskiert keine IRQs (Konvention statt Mechanismus) | Liveness/Deadlock | behoben |
| 4 | Kein Cap-Budget je PD → eine PD kann die geteilte Tabelle monopolisieren | Fairness/DoS | behoben + Test `budget` |
| 5 | Keine Spekulations-Härtung an der EL0-Grenze | Seitenkanal | teilweise behoben (s. Grenzen) |
| 6 | TrustedSAS-PDs teilen einen Adressraum | Vertrauensmodell | **kein Befund** — bewusst so |
| 7 | Fünf `transmute`s in den Trap-Hooks | Robustheit | behoben (typisierter Slot) |

Zu **6**: TrustedSAS ist ausschließlich für *safe* Rust gedacht, und safe Rust kann keinen Zeiger auf
fremden Speicher erzeugen. Der geteilte Adressraum ist damit kein Isolationsloch, sondern die
Konsequenz der intralingualen Isolation (ADR 0002/0007). Das Zertifikats-Gate (ext-28) erzwingt
genau diese Voraussetzung: `unsafe_status == ALL_PASS`.

## 1. Cap-Leck beim IPC-Grant

**Problem.** `grant_cap` legte eine Ableitung der Server-Cap an und schrieb sie in
`GRANT_RECV_SLOT` des Aufrufers. Lag dort schon eine Cap, wurde sie nur *überschrieben*: sie blieb
als Kind im CDT und als belegter Slot in der **systemweit geteilten** Tabelle zurück, für keine PD
mehr erreichbar. Ein Server, der wiederholt mit `GRANT_FLAG` antwortet, erschöpft so `NSLOTS`
(bzw. `NOBJECTS`) und verwehrt **allen** PDs jede weitere Cap-Installation. Der Angreifer ist ein
kompromittierter Server — genau im Bedrohungsmodell.

**Warum nicht einfach in `grant_cap` löschen?** Die Finalisierung kann Speicher freigeben (`MEM`)
oder einen ausstehenden Call abbrechen (`EPS` → `SCHEDS`). `grant_cap` läuft aber unter gehaltenem
`CAPS` **und** `EPS[ep]` — beides wäre dort sperrordnungswidrig.

**Lösung.** `grant_cap` **meldet** die verdrängte Cap als Rückgabewert. `dispatch` gibt erst `CAPS`
(vor dem Rendezvous, wie bisher) und dann `EPS` frei und löscht sie danach über den neuen
`delete_cap`-Callback (kernelseitig `cap_delete` → `CAPS`+`MEM` in korrekter Ordnung + Abbruch
finalisierter Reply-Calls). Zwischen Installation der neuen und Freigabe der alten Cap liegt kein
Lock und kein Kontextwechsel des Aufrufers — die alte Cap ist in diesem Fenster bereits von keiner
PD mehr referenziert.

**Nebeneffekt.** Der Empfänger sieht weiterhin genau eine Cap im Slot; die Semantik „letzter Grant
gewinnt" bleibt (anders als bei seL4, wo ein belegter Empfangs-Slot den Transfer scheitern lässt —
das hätte wiederholte Grants funktional gebrochen, da es keinen User-Syscall zum Leeren eines Slots
gibt).

**Test `grantlk`.** 65 Grants derselben Quell-Cap in denselben Slot → die Quell-Cap hat genau **1**
lebende Ableitung, die transferierte Cap funktioniert weiterhin (`3*7 == 21`), `cap_audit_cdt() == 0`.
Gemessen wird der **Kindzähler der Quell-Cap**, nicht die globale Slot-Zahl: er zählt genau die
Ableitungen dieser Cap und ist damit immun gegen die parallel laufenden übrigen Demos.
**Sensitivitätsgeprüft:** mit deaktiviertem Fix meldet der Test `65` → `FAILURES`.

## 2. Datenremanenz

**Problem.** Der Allokator vergab Regionen ohne Nullung. Ein neuer Thread-Stack, eine neue isolierte
PD-Region, ein DMA-Puffer oder die Segmente eines geladenen Prozesses konnten Restdaten eines
beendeten Subjekts enthalten. seL4 nullt bei `Retype`; hier fehlte das Äquivalent.

**Lösung.** Genau **eine** Stelle vergibt RAM: `system::mem_alloc` (allokieren + nullen). Alle 17
bisherigen `MEM.lock().alloc(...)`-Stellen laufen darüber.

**Warum bei der Vergabe, nicht bei der Rückgabe?** Nullen bei der Vergabe deckt zusätzlich
fabrikfrisches RAM mit Firmware-Resten ab und ist robust gegen Pfade, die eine Region ohne `free`
verlieren. Genullt wird **außerhalb** des `MEM`-Locks (die Region gehört bereits exklusiv dem
Aufrufer) — der Allokator bleibt für andere Kerne frei.

**Kosten.** Der `churn`-Test (tausende spawn/destroy à 64 KiB Stack) und der Rest der Suite laufen
ohne messbare Laufzeitänderung unter QEMU-TCG durch.

**Test `zerotest`.** Frische Allokation genullt → Muster schreiben → freigeben → dieselbe Region
erneut allozieren (der Allokator gibt sie nachweislich wieder aus) → wieder vollständig genullt.

## 3. IRQ-Sicherheit des `RwSpinLock`

Siehe [invariants.md §1a](../invariants.md). Kurz: `SpinLock` maskierte selbst, `RwSpinLock`
(= `CAPS`) nicht — mit der Begründung, er werde nicht im IRQ-Pfad genommen. Der gefährliche Fall ist
aber der **preemptierbare EL1-Threadkontext**: wird ein `CAPS`-Halter dort vom Timer verdrängt und
läuft auf demselben Kern ein Syscall an (IRQs im Trap maskiert), der `CAPS` nimmt, spinnt der Kern
für immer. Die Deadlockfreiheit hing an der Konvention, dass jede der ~35 Aufrufstellen selbst
`local_irq_disable()` klammert. Jetzt maskieren `read()`/`write()` selbst; die bestehenden Klammern
bleiben gültig (Save/Restore ist nesting-sicher).

## 4. Cap-Budget je PD

`NCAPS = 16` begrenzte nur den lokalen Index-Adressraum, nicht den Verbrauch an globalen Slots
(`NPDS * NCAPS = 4096` ≫ Tabellengröße). Neu: `CAP_BUDGET_PER_PD = 8` als harte Obergrenze der
gleichzeitig belegten Slots einer PD, erzwungen an **beiden** Eintragspfaden — `install_cap_checked`
und `grant_cap` (sonst wäre der Grant das Schlupfloch um die Schranke). Ein **Ersetzen** eines
belegten Slots verbraucht nichts und bleibt erlaubt. Die vorhandenen PDs nutzen 1–4 Slots.

**Test `budget`.** Bis zum Budget füllen → gelingt; ein weiterer leerer Slot → abgewiesen; belegten
Slot ersetzen → erlaubt; Teardown → CDT konsistent, Speicher-Baseline wiederhergestellt.

## 5. Spekulations-Härtung

**Umgesetzt.**
- `cpu::array_index_nospec` (arithmetische Maske + `CSDB`, Linux-Manier) an jedem Tabellenindex, den
  EL0 beeinflusst: Cap-Slot (`PdTable::cap_at`), Endpoint- und Notification-Id im Dispatch. Die
  architektonische Schranke bleibt; die Maske härtet den **spekulativen** Pfad (Spectre-v1).
- `cpu::speculation_barrier()` beim VSpace-Wechsel (`SB` bei FEAT_SB, sonst `dsb sy; isb`) — nur im
  tatsächlichen Wechselfall, der All-Trusted-Pfad bleibt unbelastet.
- Boot-Report `spec : CSV2=… CSV3=… FEAT_SB=…` (was die HW von sich aus garantiert).

**Grenzen (offen, bewusst).**
- **Cache-/Timing-Seitenkanäle zwischen PDs** sind *nicht* adressiert. Dafür bräuchte es
  Cache-Partitionierung/Coloring — eine Architekturänderung (Allokator + VSpace-Layout), kein Patch.
- **Spectre-v2 auf HW ohne FEAT_CSV2**: die Gegenmaßnahme wäre Predictor-Invalidierung per
  Firmware-Call (`SMCCC_ARCH_WORKAROUND_1`), den QEMU `virt` nicht anbietet. Unter QEMU meldet die
  HW `CSV2=0 CSV3=0 FEAT_SB=0` → der portable Barriere-Pfad ist im Test aktiv.

## 6. Typisierte Trap-Hooks

Die fünf Trap-Hooks lagen als `AtomicUsize` und wurden an jeder Lesestelle einzeln per
`transmute::<usize, KonkreterHookTyp>` zurückgewandelt — fünf `unsafe`-Stellen, bei denen ein
Vertipper einen Hook als **falschen Funktionstyp** aufgerufen hätte (UB, vom Compiler ungeprüft).
Neu: `AtomicHook<F>` bindet den Slot per `PhantomData` an genau einen Funktionszeigertyp;
`store`/`load` sind typgeprüft, die Verwechslung ist strukturell unmöglich, und die
Roh-Konvertierung existiert nur noch **einmal** (const-geprüft auf Zeigergröße). Netto: **−4**
`unsafe`-Stellen im Trap-Dispatch.

## Verifikation

- `./test-qemu.sh` (Release, ohne Fuzzer): **ALL PASS**, inkl. der drei neuen Checks `zerotest`,
  `budget`, `grantlk`.
- `KERNEL_FUZZ=1 ./test-qemu.sh`: **ALL PASS** inkl. aller fünf Fuzzer — insbesondere der
  Domänen/HW-Fuzzer (Cap-Churn gegen Policy + CDT-/Ressourcen-Oracle) und der IPC-State-Machine-
  Fuzzer laufen gegen die neue Budget- und Grant-Semantik.
- Sensitivitätsprüfung des `grantlk`-Tests gegen den ursprünglichen Fehler (s. o.).
