# Slot-0-Kollision: Entscheidungsvorlage (2026-09-10)

## Befund (verifiziert, nicht geglaubt)

Slot-Konvention der Loader-ABI (`kernel/src/loader.rs`, Doku an `endow_from_manifest`):
`0` = Loader-Cap, `1` = Notification, `2` = Endpoint, `3–8` = Geräte-/Dienst-Caps.

- **Manifest-Seite:** `endow_from_manifest` legt bei `CAP_LOADER`-Bit die Loader-Cap fest
  auf Slot 0 (`loader.rs:668–674`; Aufgabenstellung nannte 660–666 — um ~8 Zeilen
  gedriftet, derselbe Block: `install_loader_cap(0, …)` → `out[0]`).
- **`init`-Seite:** `programs/trusted/init/src/main.rs:158–159` delegiert IMMER die
  gebadgte Notification-Kopie nach Ziel-Slot 0 (`&[(NTFN_CHILD_SLOT, 0)]`, Doku: „Slot 0
  der neuen PD — die Loader-ABI-Konvention L2"). `LOADER_SLOT = 0` (Z. 95).
- **SYS_LOAD-Seite:** `load_by_index` (`loader.rs:3036–3052`) legt erst das Aufrufer-Angebot
  in `slots` und prüft jede Manifest-Cap dagegen: derselbe Slot zweimal → `collision`,
  kein Start, delegierte Kopien werden zurückgenommen (fail-closed, benannt im Log).
- **LXPD-Seite:** `lxpddrv_laden` (`loader.rs:1696–1701`) kombiniert Manifest-Caps +
  `caller_endow` in einen Puffer: derselbe Slot zweimal → `LxpdAbsage::EndowKollision`
  (mit Aufräumen frisch geprägter Caps, Z. 1698–1700), zu viele → `EndowZuViel`.
- **Dispatch-Seite:** `sammle_endow` (`crates/caprock-microkit/src/lib.rs:1788–1845`,
  eine Stelle für `LOAD` und `LOAD_IMAGE`) lehnt doppelte Ziel-Slots schon im Angebot ab.

**Wann kollidiert es wirklich? Nur bei Doppelbelegung von Slot 0:** Manifest vergibt
`CAP_LOADER` (Slot 0) UND der Aufrufer delegiert nach Slot 0. Jede Seite allein ist
kollisionsfrei — ein einzelnes Cap auf Slot 0 belegt ihn genau einmal.

**Fordern Treiber-Manifeste `CAP_LOADER` an? I. d. R. nein** — ein Treiber braucht
MMIO/DMA/IRQ/ntfn/ep, keine Lade-Autorität. Die einzige Gegenprobe im Baum ist
absichtlich andersherum gebaut: `tools/lxpd-e2e.sh --mit-lader` gibt der *Dienst*-PD
selbst das `loader`-Bit (`CAPS="loader,ntfn,ep,shared"`, Z. 89) — und erwartet genau die
Absage (init meldet Index 6 als gescheitert, Bit 7 im Root-Badge, kein `lxpdimg`).
Das belegt die Sperre mit Zeilen statt Worten.

**Was delegiert der LXPD-Dienst wohin? Nichts.** `AnstossKontext`
(`programs/lxpd-runtime/src/lib.rs:242–255`) reicht nur durch — der Dienst rät keine
Slots („Der Dienst erfindet keine Übergabe, er reicht sie nur durch", Z. 910–911).
`lxpdrv` (`programs/lxpd-runtime/src/bin/lxpdrv.rs:196–203`) stößt mit
`deleg_liste = 0, deleg_anzahl = 0` an → `caller_endow` ist leer → **keine Kollision,
egal was das Treiber-Manifest in Slot 0 hat** (eine Quelle, ein Cap, kein Gegner).

**Pfad-Durchspiel mit echten Slots (Treiber ohne Slot 0 + leere Delegation):**
Manifest `{ntfn→1, ep→2, mmio→3/4/5/6}` + Angebot `{}` → `alle = [1,2,3,4,5,6]`, kein Slot
doppelt → Start. Gegenprobe (Dienst MIT `loader`-Bit über init): Manifest `{loader→0,…}`
+ Angebot `{(child→0)}` → Slot 0 doppelt → Absage, Kind startet nicht, init setzt das
Bit — exakt das `--mit-lader`-Bild. Die Absage ist damit auf beiden Pfaden der
benannte Ausgang, kein stiller.

Nebenbefund (nur gelesen, nicht angefasst — fremder Besitz): `tools/lx_driver_manifest.py:40`
trägt noch den FNV-Tippfehler `...2325` statt Standard `...25c5` (s. AGENTS.md-Mitteilung 14).

## Option 1 — Absage genügt (fail-closed, kein Umbau)

`EndowKollision`/`collision` bleiben der benannte Ausgang. Begründung: Die Kollision
erfordert eine Doppelbelegung, und kein realer Pfad erzeugt sie — init-Kinder ohne
`CAP_LOADER`, LXPD-Dienst ohne Delegation. Wer je delegieren will, liest heute schon am
Manifest, ob Slot 0 frei ist. Kosten: null Zeilen.

## Option 2 — Umlenkung (Manifest-Slot 0 oder Delegation weicht aus)

Z. B. Loader-Cap auf einen Ausweich-Slot legen, Delegation umbiegen oder `CAP_LOADER`
für Treiber-Einträge verbieten. Kosten: eine stille Wahl („welches Cap landet auf 0?") —
genau die Form, die A-5.4/`service_id` abgeschafft hat — plus neue Konvention an drei
Stellen (Manifest, Dispatch, Dienst), die wieder auseinanderaltern können.

## Empfehlung: Option 1

Die E2E-Befundlage („Absage ausreichend") hält der Nachprüfung stand: verifiziert an
`endow_from_manifest` (Slot-0-Quelle), `init` (Slot-0-Angebot), `load_by_index`/
`lxpddrv_laden` (Kollisionsprüfung) und `lxpdrv`/`AnstossKontext` (leeres Angebot).
Die `--mit-lader`-Erwartung im E2E-Skript belegt die Sperre positiv. Kein Handlungsbedarf;
bei der ersten echten Delegation eines Laufzeit-Dienstes neu bewerten.
