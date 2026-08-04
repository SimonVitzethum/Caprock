# SEL4Lake

Faehigkeitsbasierter Mikrokern in Rust, von Grund auf geschrieben — kein seL4-Fork.
Stand dieser Notiz: 2026-08-03. Nur Geprueftes.

## Wo die Wahrheit steht

Diese Datei ist eine Einstiegshilfe, keine Quelle. Fuer alles Inhaltliche gilt:

| Datei | Inhalt |
|---|---|
| `todo.md` | **ausschliesslich Offenes** — die Quelle fuer "was ist noch zu tun" |
| `done.md` | Erledigtes |
| `docs/invariants.md` | die tragenden Zusicherungen, u. a. §2a–2e zu DMA |
| `docs/00-overview.md` | Aufbau |
| `README-X86.md` | x86-Besonderheiten |

Offene Punkte **nicht aus dem Gedaechtnis beantworten**. `todo.md` lesen.

## Aktueller Stand

Zweig `arch/x86_64` (2026-08-03).

**Gemessen, nicht erinnert:**

| | |
|---|---|
| x86_64 | **2300 von 2300** mit identischer Signatur, `== ALL PASS ==` (2026-08-03: 200 im Leerlauf + 600 + 1500 unter Last in je 5 parallelen Stroemen, 20 vCPU auf 20 Kernen; alle Stroeme auch **untereinander** deckungsgleich, `e419003d625f`, ueber beide Aufrufe hinweg) |
| x86_64 RAM-Reihe | `== ALL PASS ==` bei **512M · 2560M · 3G · 6G**, Haupt- **und** Lade-Suite (2026-08-04). Vor E-Rest 3 starb ab 3G der Boot mit `#PF cr2=0x70_0000_0014` — der Zweig „RAM oberhalb 4 GiB" war nie gelaufen |
| x86_64 Lade-Suite | `== ALL PASS ==` (2026-08-03: 39 Pruefungen — 5 Module, **zwei** Treiber-PDs, Austausch, A-5.3/A-5.4, dazu **drei** verkettete Boots fuer Z4 Stufe 2, inkl. sieben Negativfaellen) |
| aarch64 | `RUNS=6` → **6 von 6** mit identischer Signatur, `== ALL PASS ==` (2026-08-02, **mit Root-Task**; davor 16/16 ohne, s. D6/D5) |
| Host-Tests | `mem · part · fat · cycles · loader · cap · virtio · typestate · ipctreue` → `== HOST-TESTS: ALL PASS ==` (`tools/host-tests.sh`, 2026-08-03) |
| Verus | **16 Beweisdateien, 0 errors** — dazu **drei** Modell-Treue-Waechter (cap_space, IPC, Scheduler) mit 28 · 28 · 30 Selbsttestfaellen (2026-08-03) |
| Scheduler-Messung | `tools/sched-erschoepfung-messen.sh`: 208 Messwerte, Positivkontrolle bestanden, vier Fassungen (echt/V0/H-a/H-b) — belegt D8, D9 und D10 |

**Was diese 2300 Läufe heißen — und was nicht.** Die noch offene Hälfte von D0 (Hänger ab `sched`)
hatte eine Grundlinie von rund **0,5 %** (2/400 unter Last, 1/200 im Leerlauf). Die ist jetzt
ausgeschlossen: `0,995²³⁰⁰ ≈ 1·10⁻⁵`. Eine Rate von **0,1 % ist es nicht** (`0,999²³⁰⁰ ≈ 10 %`);
die obere 95-%-Schranke liegt bei `3/2300 ≈ 0,13 %`.

**D0 bleibt trotzdem offen, und das ist der Punkt.** Niemand hat diesen Hänger behoben — die letzte
D0-Arbeit beseitigte das *Farbrennen*, nicht ihn. Er ist unter die Messschwelle gefallen, nicht
repariert. Ein Fehler ohne bekannte Ursache, der aufhört sich zu zeigen, ist damit auch nicht mehr
**debuggbar**; das macht die Lage schlechter, nicht besser. Was ihn wirklich schlösse, steht in
`todo.md` D0.

Zwei Vorbehalte, die zur Zahl gehören: die alte Serie lief parallel zur **Lade-Suite**, die neue
gegen fünf Kopien ihrer selbst — beides ist Last, aber nicht dieselbe. Und `pprobe` meldet unter
KVM grundsätzlich `SKIP` (`CPUID.1:ECX[31]`), urteilt in dieser Reihe also nicht mit.

Neu an der Messung ist der **Quervergleich**: bis dahin verglich jeder Lauf nur gegen den *ersten
Lauf seines eigenen Stroms*. Fünf Ströme mit je einer in sich stimmigen, untereinander aber
verschiedenen Signatur hätten so grün gemeldet.

Früher stand hier `RUNS=100` → 96 von 100. Diese Zahl war aus einem anderen Grund wertlos, als sie
aussah: drei Schichten verdeckten einander (bedingungsloses `SELFTEST COMPLETE`, `color` druckte
`FAIL` statt `FAILURES`, `-no-shutdown` machte `rc=124` immer wahr). Erst nachdem alle drei weg
waren, konnte eine Messung überhaupt etwas aussagen — s. todo D0.

Ein früher hier stehendes „aarch64 66/66, x86_64 24/24" war eine Momentaufnahme ohne Datum. Zahlen
in dieser Datei brauchen einen Stand **und eine Stichprobengröße**, sonst werden sie stillschweigend
falsch.

## Was am 2026-08-04 dazukam

* **D11 behoben: der Ueberlauf einer Endpoint-Warteschlange ist BENANNT.** Neuer ABI-Code
  `ERR_EP_FULL = 9`; `TidQueue::enqueue` gibt `bool` und ist `#[must_use]`. `call`/`recv` weisen
  ab **ohne zu blockieren**, `bind_receiver` und `migrate_owner` melden Misserfolg statt Erfolg,
  und `migrate_owner` prueft **vor** dem `take()` — die Antwortpflicht bleibt beim alten Besitzer,
  statt geloescht zu werden und den Aufrufer zu verlieren.
  **Eine fuenfte Fundstelle, die der Befund nicht nannte:** `Notification::wait` hatte dieselbe
  Form bei Kapazitaet 1 — ein zweiter `WAIT` ueberschrieb den Wartenden.
  Das Verus-Modell ist **dem Code gefolgt** (`dropped_*` -> `rejected_*`, `send_gate`/`recv_gate`
  mit Code 3, **25 -> 30 Beweise**), der Modelltreue-Waechter faehrt 99 Faelle / 35
  Selbsttestfaelle und fuehrt ein **Hauptbuch der Gestrandeten**; die Positivkontrolle sind fuenf
  Mutationen, die D11 einzeln wiederherstellen. Im Kernel: Pruefzeile `epfull`.
  Bemerkenswert: der alte Beweis `send_drops_above_cap` **bewies den Verlust**. Er war richtig —
  der Code war falsch. Ein Beweis, der dem Code treu ist, kann das Falsche beweisen.
* **E-Rest 3b behoben: die Freiliste kennt den Zonenwunsch.** `alloc_below`/`alloc_colored_below`
  nehmen eine Obergrenze (Farbe und Zone in EINER Entscheidung); die drei Stellen mit benannter
  GiB-0-Bedingung suchen jetzt, statt einmal zu fragen und aufzugeben. Der Behelf im Speicherplan
  ist weg — hoher Speicher geht **vollstaendig** in die Freiliste (bei `-m 3G` vorher 0 von
  1024 MiB).
  **Der Befund war groesser als der Eintrag:** „unten zuerst" war ueberhaupt ein **Zufall der
  Groessenrelation**. Best-Fit nimmt das kleinste passende Fragment; solange der obere Bereich
  zufaellig groesser war (4G, 6G), landete alles Unbenannte unten. Bei 3G kehrt sich das um, und
  der ganze Ladepfad faellt aus. „Unten zuerst" ist jetzt eine ausgesprochene Politik.
  Zwei eigene Fehler dabei, beide in der Fallenliste unten.
* **E-Rest 3d zur Haelfte: die Allokationsstellen sind AUFGEZAEHLT, und der Speicher oberhalb
  4 GiB traegt gemessen.** Die Klassifikation steht als `enum Zone` an EINER Stelle:
  `KernelOnly` (Kerneltabellen, Kernel-Stacks, Segmente **und** Stacks geladener Programme, alle
  L3-Tabellen, AP-Stacks) bevorzugt **oben** — bei 3G/6G liegen alle 28 dieser Allokationen
  oberhalb 4 GiB; `PdMappable` muss tief, und das ist strukturell (`vspace_map_page_at` weist
  `va >= GIB1_END` ab); drei Stellen tragen eine **harte** Bedingung.
  **Die Gegenprobe ist der Beleg:** `system::alloc` auf `KernelOnly` gestellt reisst die
  **Lade-Suite** bei 3G — waehrend die **Hauptsuite gruen bleibt**. Dieselbe Form wie D8/D9/D11.
  Dabei zwei eigene Vermutungen widerlegt: geladene Segmente brauchen GiB 0 **nicht** (VA und PA
  sind getrennt), und das Geraet erreicht Speicher oberhalb 4 GiB sehr wohl.
* **Der GiB-0-Deckel fuer isolierte PDs ist WEG — und er war eine Zahl: 504.** Die private
  Region einer isolierten PD wird nicht mehr identisch abgebildet, sondern in ein **VA-Fenster
  ausserhalb der Identitaetskarte** (x86 `PML4[1]`, aarch64 `L1[9]` — Bereiche, in denen der
  Kernel nie identisch zugreift; sonst verdeckte eine User-VA seine eigene Sicht auf physisches
  RAM). Belegt: `isohigh : ALL PASS` bei 3G/4G/6G, Regionen bei 4,04 GiB, Farbtrennung
  unveraendert — die Farbbedingung liegt auf der **Phys**adresse. `SKIP` bei 512M/2560M, weil die
  Frage dort nicht entscheidbar ist. Gegenprobe gefahren.
  Zwei eigene Annahmen widerlegt: der 2-MiB-Block-Fastpath geht **nicht** verloren (er hing an
  der Ausrichtung der VA, nicht an der Identitaet), und A1/B-4.1 ist gar nicht betroffen.
* **VA == PA steht jetzt im TYP.** `addr::Va` hat **keinen** Konstruktor aus `u64` oder `Pa` --
  der einzige Weg ist `Va::identity(reason, pa)` mit einer Variante des geschlossenen Enums
  `IdentityReason`. Die Liste IST damit der Quelltext. Die erste Fassung war ein Skript mit einer
  Liste im Kopf und hat sich am selben Tag selbst widerlegt: `vspace_map_dma` stand gar nicht
  darin, und der Grundtext zu `SYS_MAP` war **falsch** (`sys::MAP` traegt kein Adressargument --
  der Aufrufer nennt eine Cap, die Basis kommt aus der Cap-Aufloesung im Kernel; die Identitaet
  ist also behebbar, ohne die ABI anzufassen). **Ein Waechter prueft die Existenz eines Grundes,
  nie seine Wahrheit** -- deshalb tragen die Gruende jetzt, wo moeglich, einen **Falsifikator**.
  Der neue Waechter fand sofort eine Stelle, die der alte nicht sah (`unmap_dma_from_thread`).
* **VA == PA, erste Fassung: eine LISTE, keine Gewohnheit** (ueberholt, s. o.) Bestandsaufnahme nach dem Fenster-Umbau:
  neun Aufrufstellen in acht identisch abbildenden HAL-Funktionen, in drei Klassen.
  **Entfernt:** `spawn_isolated_native` bildete Code und Stack identisch ab und nahm die
  Physadresse des Code-Frames als **Einsprungadresse** -- beides geht jetzt ins Fenster,
  `vspace_map_region`/`vspace_map_code_region` sind ohne Aufrufer und geloescht.
  **Unmoeglich gemacht:** `Scheduler::spawn_user` (ein Wert fuer EL0-SP UND Reap-Region) ist
  **geloescht**, nicht repariert; es gibt nur noch `spawn_user_at`. Der letzte Aufrufer schreibt
  beide Werte hin, obwohl sie dort gleich sind -- die Gleichheit ist ein Zufall der Umgebung.
  **Benannt:** die uebrigen neun stehen mit Grund in `tools/identitaet.sh` (SYS_MAP/SYS_UNMAP ist
  die ABI, Geraetefenster sind physisch, zwei globale Abbildungen haben kein Subjekt).
  Der Waechter haelt die Liste gegen den Quelltext, mit Selbsttest in **beide** Richtungen.
* **Gemessen:** RAM-Reihe 512M · 2560M · 3G · 4G · 6G (Hauptsuite) und 512M · 3G · 6G
  (Lade-Suite), alle `== ALL PASS ==`; x86 `RUNS=8` mit identischer Signatur; Host-Tests, Verus,
  drei Modelltreue-Waechter, Kerngrenze, Identitaets-Waechter, Typestate gruen.
  **aarch64 mit Vorbehalt:** 11 gruene Laeufe (`RUNS=6` identische Signatur + 5 Einzellaeufe),
  **ein** Fehlschlag unter dreifacher paralleler QEMU-Last. Passt zum offenen Haenger aus D6 und
  trat auch vor diesen Aenderungen auf -- auseinandergehalten habe ich es nicht.

## Was am 2026-08-03 dazukam

* **Z4 Stufe 2 steht: ein Thread ueberlebt eine BOOTGRENZE.** Derselbe Kernel speichert und
  stellt wieder her und **entscheidet selbst welches** — er liest einen Sektor (32710, ausserhalb
  beider Partitionen), prueft Magie, Formatversion und `kernel_code_hash` und handelt danach. Der
  Transport ist der vorhandene Blockdienst; der Kern ist Client, `virtio-blk` blieb unveraendert.
  Format in `crates/sel4lake-cap/src/checkpoint.rs` (feste Breiten, LE, CRC-32, abhaengigkeitsfrei,
  host-getestet), und `Image::build` ruft `classify_all`: **eine nicht uebertragbare Cap verhindert
  den Checkpoint vor dem Schreiben.** Belegt ueber drei verkettete Boots (244 → 345 → 446, Epochen
  1 → 2 → 3) und zwei Negativfaelle. Details in `done.md`.
* **Vier Fehler im Kern gefunden, drei davon behoben — alle vier von Prüfern, die es gestern
  noch nicht gab.** D8 (ein erschöpfter Thread lief über `unblock` auf leerem Konto, ohne jede
  Cap), D9 (fünf Befunde im Donee-Zweig, darunter: die verschachtelte Spende `fs → Blockdienst →
  Treiber` ließ Client **und** Server dauerhaft hängen), D10 (O(n²) im Timer-Interrupt,
  erreichbar über den Lastausgleich) — behoben und gemessen. **D11 offen:** der 33. Sender an
  einem Endpoint hängt für immer, `is_quiescent()` meldet ihn als ruhig, `audit` und
  `purge_thread` sehen ihn nicht. Details in `todo.md`.
  Das Muster dahinter ist wichtiger als die Fehler: **keiner war über die Testsuite auffindbar.**
  Die Signatur der x86-Suite ist über alle drei Behebungen hinweg **byte-identisch geblieben**
  (`e419003d625f`, 500 Läufe je Stand) — die Suite hat nie einen davon ausgelöst.
* **D0 nachgemessen: 2300 Läufe, keine Abweichung — und trotzdem nicht zu.** Die alte Quote von
  0,5 % ist ausgeschlossen, 0,1 % nicht. Wichtiger als die Zahl: **niemand hat diesen Hänger
  behoben.** Er ist unter die Messschwelle gefallen, nicht repariert — und damit auch nicht mehr
  debuggbar. Details in `todo.md` D0.
  Dabei fiel ein Loch im Prüfer auf: **jeder Lauf verglich nur gegen den ersten Lauf seines
  eigenen Stroms** — fünf parallele Ströme mit je eigener, in sich stimmiger Signatur hätten
  fünfmal grün gemeldet. Der Quervergleich ist jetzt Teil der Messung.
* **Alle drei CI-Gates (Kani, Loom, Verus) sind seit ihrer Anlage NIE gelaufen.** Sie lagen in
  `.gitea/workflows/`; der Server ist GitLab. Gemessen über die Pipelines-API: zwei Pipelines
  insgesamt, beide vom 2026-05-23, beide mit GitLabs Auto-DevOps-Vorgabejobs. Seit 2026-08-03
  liegt `.gitlab-ci.yml` dort, wo der Server liest — die Jobs entstehen, warten aber auf einen
  Runner. Siehe Fallenliste unten.
* **Der Befund, der dabei den ganzen Entwurf umgebaut hat — und der allgemein gilt:** der
  Fortschrittszaehler ist unter KVM **reproduzierbar** (gemessen 133…155 an derselben Stelle des
  Hochlaufs). Der erste Aufbau verglich einfach „gespeicherter Wert == gefundener Wert"; eine
  Mutation, die den Wert LAS und MELDETE, ohne ihn zu setzen, traf ihn **exakt**, und die Suite
  blieb gruen. Siehe unten in der Fallenliste.

## Was am 2026-08-02 dazukam

* **B-6.2: ein Kernel-Panic reisst den Knoten NICHT mit — und das ist die schlechtere Nachricht.**
  Gemessen: der Panic-Pfad haltet den Kern **ohne IRQ-Maskierung**, der naechste Timer-Tick holt
  ihn zurueck in den Scheduler. Der Knoten lief 61 s weiter und meldete `ipc`, `ring3`, `iommu`,
  `dmatok` als ALL PASS — mit einer nachweislich verletzten Invariante. Vier verschiedene Ausgaenge
  je nachdem **wo** der Panic auftritt, darunter ein stiller Totalausfall (Panic unter der
  MEM-Sperre) und ein Zustand, der von aussen nicht von einem Deadlock zu unterscheiden ist.
  Festlegung in `docs/fehlerdomaene.md`, normativ als `docs/invariants.md` §14.
* **Z4 angefangen — die zwei Stufen, die heute prüfbar sind.** **Z4a**: `freeze_thread` hält an
  einer *benennbaren* Grenze (nicht auf einem Kern **und** keine offene IPC-Beziehung), geprüft
  über die **Wirkung** — Zähler bewegt sich, steht, läuft wieder. **Z4b**: die Verweigerungsregel
  (`crates/sel4lake-cap/src/checkpoint.rs`, host-getestet) — was drüben nicht dasselbe bezeichnen
  kann, wandert nicht mit, und die Entscheidung braucht den **Umfang** des Checkpoints, nicht nur
  die Cap. Drei Fehler im eigenen Entwurf gemessen, darunter ein Beobachtungsfenster kürzer als
  ein Tick und ein `all_done()`-Konjunkt, das erst im Bericht entsteht.
* **A-5.4 ist zu: das Geraet der einen Treiber-PD erreicht die DMA-Region der anderen nicht**
  (`dmaiso : ALL PASS`). Vier Zahlen, keine reicht allein — Positivkontrolle ueber denselben
  Treiber und dieselbe Deskriptorkette (nur **eine** Adresse wandert), keine Daten beim
  Fremdversuch, das Opfer **vom Kernel** nachgeprueft, und ein VT-d-Fault als aktiver Beleg. Zwei
  Fehler im eigenen Entwurf gefunden: `arp_probe` nullte nur acht Byte, also las die zweite Probe
  die Antwort der ersten; und `rx_used` sagt, dass das Geraet *gehandelt* hat, nicht dass Daten
  ankamen.
* **Dabei: zwei Treiber-PDs laufen gleichzeitig**, jede bekommt ihr im Manifest **benanntes**
  Geraet. Der Weg dorthin legte vier versteckte Politiken frei — „die erste benutzte Zuteilung",
  „der zuletzt geladene Dienst", eine geteilte Notification-Ablage und eine geteilte
  Uebertragungsflaeche. Alle vier sind jetzt ueber die `program_id` aus dem Manifest verschluesselt;
  wo eine Wahl mehrdeutig waere, wird **abgewiesen statt geraten**. Der Client benennt seinen Dienst
  im Manifest (`service_id`, die letzten 4 reservierten Bytes).
* **`pprobe` urteilte unter Emulation.** Auf aarch64 unter Last "trug" die Positivkontrolle und die
  Verteilungen waren "trennbar" — bei **0,6 Zyklen je Kettenglied**. Eine abhaengige Ladeoperation
  kostet auf echter Hardware mindestens die L1-Trefferlatenz; darunter misst der Test die
  Befehlszahl der Emulation, nicht den Cache. Jetzt eine Untergrenze, die von der Messung
  unabhaengig ist.
* **A1 gilt jetzt auch auf aarch64 — und war dort vorher NICHT ausgehängt, sondern auf `true`
  verdrahtet.** `spawn_demo` setzte `COLOR_OK`/`STRIPE_ALLOC_OK`/`PPROBE_OK` hart auf `true`: drei
  dauerhaft wahre Konjunkte in `all_done()`, keine Berichtszeile, kein Check — die Abwesenheit war
  nicht bloß unbelegt, sie war **unsichtbar**. Die Lösung war der **Platz** (ans Ende der Kette,
  hinter `cross`/`strand`/`loadstop`), nicht das Weglassen. Damit läuft A1 zum ersten Mal auf der
  **16-Farben**-Aufteilung — genau dem Fall, den die `MASK_BITS`-Verwechslung falsch machte.
* **B-4.5 reproduzierbar — und die Ursache war NICHT der Allokator.** Der Aufbau war
  byte-identisch (gleiche Physadressen, Fragment-Höchststand 419/1024); die Streuung lag in der
  **Messung**: der Angreifer lief linear (Positivkontrolle 3/10 → bit-umgekehrt 10/10), das
  Minimum war der falsche Schätzer (4/10 → Median 10/10), und „Opfer > L2" war eine feste Zahl.
  Dazu: **QEMU meldete eine erfundene Cache-Geometrie.** Mit `host-cache-info=on` sieht der Kernel
  512 echte Farben statt 256 gedachter — und 512 ist keine 256, genau daran hing der
  A1-Rest-Fehler.
* **A-5.3: die Geräte-Zuteilung stand im Enumerator, jetzt im Manifest.** Der Eintrag trägt einen
  Selektor (`vendor`/`device`/`class`) in den reservierten Bytes — Format bleibt eingefroren, und
  Nullen heißen „beliebig", nicht „passt auf nichts". Fail-closed: passt keines, gibt es keines.
  Belegt durch zwei Negativfälle; der stärkere zeigt auf die **Netzkarte** und bekommt sie, obwohl
  das Blockgerät in der Angebotsliste davor steht.
* **B-5.1: die Abrechnung hing am Tick — und zwar ganz.** Nicht „bis zu 10 ms Verzerrung":
  `block_current`, `switch_to` und YIELD belasteten **gar nichts**. Wer kurz vor dem Tick
  blockiert, zahlte null. Jetzt wird jede Umplanung gestempelt (`crates/sel4lake-sched/src/cycles.rs`,
  abhängigkeitsfrei, host-geprüft), geprüft als `cycacct`: **Proben > Ticks** ist die Aussage.
* **D5: der aarch64-Kernel hat einen Root-Task** — und der Weg dorthin fand zwei Fehler, die mit
  dem Manifest nichts zu tun hatten: `boot_arg` gab die **Archiv**-Größe statt der Startmenge
  (jetzt `StartSetNotPrefix`, fail-closed), und `loadstop` maß eine **globale** Baseline mit einer
  **lokalen** IRQ-Sperre.
* **B-5.5: begrenzt war der Prüfer, nicht `revoke`.** `cdt_audit`-Code 9, Höchststände als
  Operationszahl im Bericht.
* **`tools/host-tests.sh`** existiert — `sel4lake-cap` hatte gar keinen Host-Test-Pfad, seine
  sechs Tests wären nirgends gelaufen.

## Was am 2026-08-01 dazukam

* **D0 zur Hälfte zu.** Das Farbrennen ist weg (500/500): die Kernelseite wird nicht mehr
  *zurückgelesen*, sondern vom Spawn geliefert — ein Wert, der an der Lebendigkeit eines Threads
  hängt, der sterben darf, taugt nicht als Messgröße. Der Hänger bleibt offen (s. o.).
* **Ein echtes Leck nebenbei behoben:** `record_user_kstack` lief *hinter* dem kritischen
  Abschnitt; traf der Einsammler das Fenster, wurde der Kernel-Stack nie freigegeben.
* **A-5.2 ist zu:** virtio-**Transport**, **Blockgerät** und **Netzkarte** auf x86, alle drei
  kernfrei in `crates/sel4lake-virtio`. Wichtiger als die Geräte ist, was sie belegen: der RNG
  zeigte nur, dass ein Gerät in unseren Speicher **schreibt** — `blk` schickt eine Deskriptorkette,
  deren erstes Glied das Gerät **lesen** muss, und belegt damit die andere Richtung. Auch im
  Negativtest: nach dem VT-d-Aufbau bleibt das Statusbyte auf `0xff`, das Gerät hat den Anfragekopf
  nicht einmal gesehen. Details in `done.md`.
* **B-4.5 (Prime+Probe) steht** — mit einem negativen Ergebnis, s. unten.
* **Die Kerngrenze ist prüfbar:** `tools/kernel-grenze.sh`.
* **A-5.1 ist zu (2026-08-02): ein Treiber laeuft als DIENST ausserhalb des Kerns — und wird
  ausgetauscht, ohne dass der Kernel weiss, was er treibt.** `programs/hardware/virtio-blk` loest
  sein Geraet auf seiner **eigenen Konfigurationsraum-Seite** auf und wartet dann in `recv`; der
  Kernel ist **Client**. Zwischen zwei Anfragen tauscht er den Empfaenger am laufenden Endpoint aus
  (A-4.1, ohne Empfaengerluecke); der Bedienungszaehler 1 → 2 in der DMA-Region belegt, dass die
  neue Fassung dieselbe Region **geerbt** hat. In `loader::reload_driver` kommt „virtio" nicht vor
  — das ist die Abnahmebedingung, nicht ein Zufall.
  Der Einwand, der unterwegs wegfiel: der Konfigurationsraum ist geraeteweit — stimmt fuer das
  ECAM-Fenster als Ganzes, nicht fuer **eine Funktion** (ECAM bildet jede auf 4 KiB ab, also auf
  eine Seite). **Nicht dabei:** `CAP_IRQ` — der Treiber pollt, s. unten. Details in `done.md`.
* **B-3.3 ist zu — und die Todo-Notiz war nur die halbe Wahrheit.** Aggregiert war schon einiges;
  **nicht** aggregiert waren drei Stellen mit Zaehnen: `init()` stellte nur Einheit 0 scharf (die
  uebrigen blieben mit `TE=0` — das heisst *keine Uebersetzung*, nicht *blockiert*),
  `invalidate_context_cache()` ebenso, und `flush_entry`/`slpt_map` trafen **Politik** (`clflush`,
  `SNP`) nach den Faehigkeiten von Einheit 0. Dazu neu: eine Sprechprobe je Einheit.
  Nebenbefund behoben: `detach` kehrte bei nicht lesbaren Faehigkeiten **still** zurueck — ein
  ausgefallener Teardown laesst eine Uebersetzung stehen. Jetzt `dma_audit` Code 8.
* **B-5.5 ist zu — und stand genau verkehrt herum.** Begrenzt war ausgerechnet der **Pruefer**
  (`audit_cdt`); `revoke`, `move_cap` und `child_count` liefen unbegrenzt — auf Mandantenwunsch und
  unter der CAPS-Sperre. Schranke jetzt hergeleitet (`slots.len()`), Ueberlauf gezaehlt und als
  `cdt_audit`-Code 9 geprueft, Hoechststaende als **Operationszahl** im Bericht.
  Nebenertrag: `sel4lake-cap` hatte gar keinen Host-Test-Pfad — `tools/host-tests.sh` sammelt jetzt
  **62 Tests** (mem, part, fat, cap) an einem Ort.
* **B-7.1 ist zu — und die Notiz war ueberholt.** Kani deckte `sync` laengst ab; die CI beschrieb
  sich nur falsch (`Job: „…(Loader-Parser)"`, tatsaechlich alle vier Ziele) — und genau daraus war
  der „Befund" entstanden. Die **echte** Luecke: die Beweise liefen mit KONKRETEN Werten, also in
  derselben Groessenordnung wie Loom. Fuenf neue Harnesses nehmen den Zustand **symbolisch**
  (2^31 Leserzahlen, u32-Ticketueberlauf); `sync` steht bei 8 statt 3 Beweisen.
* **B-7.2 ist zu — und die Kopie war nicht der Grund.** Loom prueft jetzt den **echten**
  `sel4lake-sync`-Quelltext (das Skript kopiert ihn unveraendert, Beweise in derselben Datei), und
  die alten Kopien sind geloescht. Der Fund dabei: mit `core::cell::UnsafeCell` prueft Loom nur das
  **Atomic-Protokoll** — eine abgeschwaechte Ordnung im Ticket-Release lief durch ALLE Beweise
  durch. Erst mit `loom::cell::UnsafeCell` fallen 2 von 10. Selbst nachgemessen.
* **D6: die aarch64-Suite hat jetzt eine Wiederholungsmessung** (`RUNS`, Signaturvergleich, Logs
  bei Abweichung). Die scheinbare Sporadik von ~30 % war zur Hauptsache die **Mechanik** — Pipe
  statt Datei, danach eine gemeinsame Datei fuer alle Laeufe. Ein echter Haenger bleibt offen und
  ist jetzt erstmals messbar.
* **B-3.4 ist zu:** `0xFEE0_0000..0xFEF0_0000` ist als IOVA unbenutzbar (VT-d liest DMA dorthin als
  Interrupt-Nachricht und uebersetzt gar nicht — kein Fault, keine Fehlerzeile, nur Daten, die
  nirgends ankommen). Die Fensterbasis liegt jetzt **strukturell** darueber. Die Gegenprobe zeigt:
  das Fenster lag vorher wirklich darin.
* **A-6 ist zu: ueber dem Sektor liegt ein Speicherstapel — vollstaendig ausserhalb des Kerns.**
  **A-6.1** Blockdienst (Auskunft, Lesen, **Schreiben**, Flush, Bereichsfehler mit eigenem Status);
  **A-6.2** `crates/sel4lake-part` liest GPT (14/14 Host-Tests, drei kaputte Tabellen mit
  unterscheidbaren Gruenden abgewiesen); **A-6.3** `crates/sel4lake-fat` + `programs/trusted/fs`
  lesen eine Datei ueber GPT → FAT16 → Blockdienst → Treiber (16/16 Host-Tests). Beide Parser sind
  abhaengigkeitsfrei und `forbid(unsafe_code)` — fremde Plattenbytes werden nirgends mit
  Kernprivileg interpretiert. **A-6.4** die PD SCHREIBT auch (zweiter Cluster, beide FAT-Kopien,
  Flush, jedes Byte zurueckgelesen), und `tools/checkfat.py` liest das Abbild **unabhaengig** nach.
  Details in `done.md`.
* **Die Lade-Suite war rot und ist es nicht mehr** — und die Ursache ist lehrreich:
  `test-qemu-x86-load.sh` startete `virtio-rng-pci` ohne `iommu_platform=on`, das Gerät war damit
  transitional, der Treiber brach korrekt ab, `virtio` fiel durch, `all_done()` wurde nie wahr,
  Watchdog. Sah aus wie ein Hänger, war eine Gerätekonfiguration. **Zwei Suiten, die dasselbe
  Gerät verschieden aufsetzen, sind ein Riss, durch den genau so etwas fällt.**

## Das Entwurfsprinzip, das alles zusammenhaelt

**Ein Pruefer, der ueber Abwesenheit entscheidet, muss belegen koennen, dass er ueberhaupt
sprechfaehig ist. Ein leerer Lauf ist kein Testergebnis.**

Das ist keine Stilfrage, sondern im Code verankert: `config_errors()` prueft
beobachtungsunabhaengig, `evtq_liveness` weist nach, dass die Ereigniswarteschlange antworten
koennte, und die Audit-Codes 6 (IOMMU-Konfigurationsfehler) und 7 (haengende Isolierung)
existieren, damit ein Schweigen nicht als Erfolg durchgeht.

Wer hier etwas aendert, weicht das leicht versehentlich auf. Vor jeder Aenderung an einem
Pruefpfad: Kann dieser Test noch fehlschlagen, wenn die gepruefte Sache kaputt ist?

Dasselbe gilt fuer Abnahmekriterien. Ein frueher Kriterium lautete "die vorhandenen Tests
hoeren auf zu skippen" — und setzte damit voraus, dass es sie gibt. Auf x86 gab es sie nicht;
sie skippten nicht, sie fehlten. Daher liegen die DMA-Tests jetzt architekturneutral in
`kernel/src/dmatests.rs` und werden von **beiden** Hochlaufwegen gefahren.

## Zwei Achsen, die nie vermischt werden duerfen

`addr::Pa` und `addr::Iova` sind getrennte Typen. `DmaRegion::identity` wurde **absichtlich
entfernt** — es gibt keinen bequemen Weg mehr, eine PA als IOVA auszugeben.

IOVA-Fenster liegen oberhalb von `RAM_TOP`, mit 2-MiB-Schutzbaendern, und werden nicht
wiederverwendet. Oberhalb von `RAM_TOP` kann eine IOVA nie zufaellig eine gueltige PA sein —
darauf beruht die Trennung.

**Die Falle dabei:** Eine Funktion, die vollstaendig in `u64` rechnet, ist keine Kante, sondern
ein Loch. Genau so las `dmagen` einmal Stage-1-Blaetter mit der PA statt der IOVA — die
Newtypes konnten das nicht sehen, weil sie nirgends vorkamen. Wo Adressarithmetik passiert,
gehoeren die Typen mit hinein.

## Fallen, die dieses Projekt bereits bezahlt hat

Alle behoben. Sie stehen hier, weil die Bedingung dahinter weiterhin gilt.

* **SMMUv3 `STE.S1STALLD`** darf nur gesetzt werden, wenn `IDR0.STALL_MODEL == 0b10`. Sonst
  `C_BAD_STE`, und der Strom wird nie uebersetzt.
* **SMMUv3 CD** braucht die Bits `A` (terminate) und `R` (record). Ohne `R` ist die
  Ereigniswarteschlange **strukturell** leer — und ein leerer Puffer sieht aus wie "keine
  Fehler".
* **QEMU-virtio umgeht die SMMU**, solange `VIRTIO_F_ACCESS_PLATFORM` fehlt. Der Treiber
  verlangt es jetzt; der Lauf braucht `iommu_platform=on`.
* **x86 `GCMD` ist kein Read-Modify-Write.** Ein zurueckgeschriebenes `TE=0` gibt DMA frei.
* **x2APIC**: `EN` und `EXTD` in einem Schreibvorgang ist ein verbotener Zustandsuebergang —
  #GP, Triple Fault, zurueck ins BIOS. Zwei Schritte, oder gar nicht. Unter TCG faellt das
  nicht auf, weil `qemu64` kein x2APIC hat.
* **`cpuid` in einem heissen Pfad** ist unter KVM ein bedingungsloser VM-Exit. In `cycles()`
  kostete das 3556 statt 51 Zyklen. Merkmale werden einmal ermittelt und zwischengespeichert.
* **Faerbung wirkt nur auf Blech.** Gemessen: als Gast schreibt der Wirt die Farbbits um (sie
  liegen oberhalb des Seitenoffsets, die zweite Uebersetzungsstufe zerstoert sie). `disjunkt=234`
  gegen `gleichfarbig=210` — kein Schutz. Wer „Isolation ohne VMs" auf gemieteten VMs betreibt,
  hat sie nicht.
* **Eine „arch-neutrale" Barriere ist keine.** `core::sync::atomic::fence(SeqCst)` wird auf aarch64
  zu `dmb ish` — Device-Memory liegt nicht in dieser Domaene, dort braucht es `dsb sy`. Beim
  Entkoppeln von `sel4lake-virtio` waere das der bequeme Weg gewesen und haette die Semantik still
  abgeschwaecht.
* **Ein Test, der Speicher belegt, kippt baseline-empfindliche Tests.** Der Farbtest am Anfang von
  `threads::spawn_demo` liess auf aarch64 mal `captest`, mal `sched` durchfallen. Er ist dort
  deshalb ausgehaengt (x86 laeuft ihn). Ein Test, der andere Tests kippt, macht das GESAMTE
  Ergebnis unbrauchbar.
* **`MASK_BITS` ist nicht die Farbanzahl.** `region_bytes()` rechnete mit 64 statt mit `count()` —
  auf x86 (256 Farben) zufaellig richtig, auf aarch64 (16) falsch. `sel4lake_mem::stripe` hatte
  denselben Fehler; **behoben am 2026-08-02**, und er war schlimmer als gedacht: bei 16 Farben
  bekam Streifen 0 ALLE Farben und die Streifen 1..3 KEINE — und weil leere Mengen sich nicht
  schneiden, meldete der Selbsttest „disjunkt". Gruen, ohne dass etwas getrennt war.
  `stripe` nimmt jetzt die Farbanzahl als Parameter; bei 256 Farben bit-identisch zu vorher.
* **Kern-Uebergabe**: `CR3` per 32-Bit-Schreibzugriff ist ein abgeschnittener Zeiger, sobald
  die Tabellen ueber 4 GiB liegen. Seitentabellen mit `GFP_DMA32` anfordern und pruefen.
* **Geteilte Seitenverzeichnisse vertragen keine PD-spezifischen Eintraege.** `vspace_create_base`
  haengt GiB 1..3 **jeder** isolierten x86-PD an dieselben statischen Tabellen (`ISO_PD_HIGH`).
  Ein Geraetefenster dort einzutragen gaebe es JEDER isolierten PD -- lautlos, denn die
  Cap-Pruefung liefe korrekt durch. Seit A-5.1 entsteht beim ersten Geraetefenster eine private
  Kopie; beim Abbau werden nur die privaten freigegeben (die geteilten sind Kernel-Speicher).
* **Ein Geraet, das nur schreibt, belegt nur das Schreiben.** `virtio-rng` galt als Beleg fuer "der
  DMA-Pfad traegt" — er liest nie etwas von uns, die Leserichtung kam in seinem Testfall gar nicht
  vor. Das trug bis in den Negativtest: dass der VT-d-Default-Block auch Lesezugriffe sperrt, war
  eine ANNAHME. `virtio-blk` prueft sie (A-5.2). Dieselbe Form wie die leere Event-Queue ohne
  `CD.R` — eine Aussage sieht wahr aus, weil der Fall, der sie widerlegen koennte, nie laeuft.
* **Ein Schreiber, der sein eigenes Ergebnis bestaetigt, bestaetigt nichts.** Die
  Dateisystem-PD las nach dem Schreiben zurueck und meldete Erfolg — mit derselben Sicht, mit der
  sie geschrieben hatte. Dass sie nur EINE der zwei FAT-Kopien fortgeschrieben hatte, sah nur ein
  **unabhaengiger** Leser (`tools/checkfat.py`, andere Sprache, Muster dort noch einmal
  hingeschrieben statt importiert).
* **Wer eine Fassung ersetzt, muss ihr ALLES geben, was die alte hatte.** Beim Hot-Reload fehlte
  der neuen Fassung ein Endowment-Slot; sie brach korrekt ab, und der Austausch meldete
  `NotReady` — was nach einem Zeitproblem aussieht und ein fehlendes Cap war. Eine
  Endowment-Liste, die beim Ersetzen von der beim Erstladen abweicht, ist ein Riss.
* **Rollen, die sich melden, brauchen getrennte Ablagen.** Root-Task, Treiber und Client teilten
  sich eine Notification-Ablage; die zuletzt geladene PD ueberschrieb sie, und der Kernel wartete
  auf ein Signal am falschen Objekt.
* **Ein Urteil, das in `all_done()` steht, darf nicht erst im Bericht entstehen.** Sonst kann es
  den Bericht nicht ausloesen: der Lauf laeuft in den Watchdog und druckt das Ergebnis trotzdem.
  Im Log sieht das aus wie „gruen, aber gehangen". Zweimal an einem Tag passiert (A-6.1).
* **Ein Test, der nirgends laeuft, ist kein Test.** `sel4lake-cap` hatte `#[cfg(test)]`-Module und
  keinen Weg, sie auszufuehren (`cargo test -p` scheitert am erzwungenen Custom-Target). Seit
  2026-08-02: `tools/host-tests.sh`.
* **Wer eine Schleife begrenzt, pruefe zuerst, WELCHE begrenzt ist.** Hier war es der Pruefer und
  nicht der Pfad, den ein Mandant ausloest.
* **Eine Beschriftung, die neben der Sache herlaeuft, erzeugt Arbeit, die es nicht braucht.** Der
  CI-Job hiess „Kani — Tier-1-Beweise (Loader-Parser)" und fuhr in Wahrheit alle vier Ziele. Daraus
  wurde ein Todo-Eintrag ueber eine Luecke, die es nicht gab — waehrend die echte Luecke (Beweise
  mit konkreten statt symbolischen Werten) unbenannt blieb.
* **Ein Pruefer, der die gepruefte Groesse NACHRECHNET statt sie zu lesen, prueft eine zweite
  Wirklichkeit.** `iova_window_clear_of_msi` rechnete die Fensterlage selbst aus und bildete
  dabei nur den starken Zweig ab; im schwachen gab er `true` zurueck, obwohl das Fenster dort
  `[0, 512 GiB)` ist und den Sperrbereich ENTHAELT. Zuteiler und Pruefer brauchen EINE Quelle.
* **Was auf q35 nicht vorkommt, ist damit nicht abwesend, sondern ungeprueft.** RMRR faerbte das
  Geraet statt der ACS-Gruppe. q35 hat 0 RMRRs, also konnte die QEMU-Suite das nie zeigen -- auf
  echter Hardware (Legacy-USB, BMC, Grafik) ist es der Normalfall. Wo die Emulation eine
  Eigenschaft gar nicht hat, gehoert ein Host-Test mit synthetischer Topologie hin.
* **Eine flaechige Identitaetskarte ist bequem und macht jeden verirrten Zeiger gueltig.** Beim
  Hochziehen ueber 4 GiB waeren 512 GiB mit 1-GiB-Blaettern 512 Eintraege gewesen -- billiger als
  der gewaehlte Weg. Dann ist aber auch alles praesent, wo nichts ist: ein Zeiger nach 200 GiB
  traefe eine gueltige, beschreibbare Seite statt eines Faults. Abgebildet wird, was der
  Speicherplan deckt; darueber wird abgewiesen, nicht geraten.
* **Ein `if cap { .. }` ohne `else` verwirft still — und der Aufrufer merkt es nicht.**
  `TidQueue::enqueue` nahm 32 Sender; der 33. wurde TROTZDEM blockiert, bekam keinen
  Ergebniscode, stand in keiner Struktur des Endpoints, wurde nie geweckt — und
  `is_quiescent()` meldete ihn als RUHIG. `audit` sagte `(false,false)`, `purge_thread` `false`.
  Ein Faden haengt dauerhaft, und JEDER Pruefer meldet Ordnung. Schlimmer als die leere
  Event-Queue ohne `CD.R`, weil die Ruhemeldung zusaetzlich einen Hot-Reload freigaebe.
  Wer eine Kapazitaet einfuehrt, muss den Ueberlauf **benennen** (Rueckgabewert, eigener
  Fehlercode) — sonst ist die Schranke kein Schutz, sondern ein Loch.
* **Ein Beweis, der die Wunschform beweist, ist schlechter als keiner.** `send_no_loss` galt am
  echten Endpoint NICHT (die Kapazitaetsschranke kam im Modell nicht vor), `ep_inv` hielt nicht
  der Typ, sondern die Aufrufdisziplin. Beides sah gruen aus. Die Behebung war nicht, mehr zu
  beweisen, sondern das Modell auf das abzuschwaechen, was HAELT — und den Bruch der starken
  Fassung ausdruecklich mitzubeweisen.
* **Eine Iterationszahl ist eine Eigenschaft des Programms, eine Zeitmessung nicht.** Bei D10
  war „wie teuer ist der Refill" auf einer 20-Kern-Maschine unter Last mit einer Stoppuhr nicht
  zu beantworten. Gezaehlte Iterationen ergaben eine Tabelle, die sich vorher/nachher und ueber
  vier Fassungen vergleichen laesst — und eine Sprechprobe (Tabelle 32 gegen 10 000).
* **Ein Bit, das zwei Gruende traegt, macht den Wecker unbestimmbar.** Im Scheduler hiess
  `blocked` gleichzeitig „pausiert", „wartet in IPC" und „wartet auf Konto-Refill". Wer die
  Blockade aufhebt, hebt damit auch eine auf, deren Grund er nicht kennt — und wer sie stehen
  laesst, laesst einen Thread liegen, dessen Wecker weggefallen ist. Vier der fuenf D9-Befunde
  hingen daran (gemessen: 0 Ticks in 3 Perioden, `audit() == 0`). Die Behebung war nicht ein
  weiterer Waechter, sondern ein eigenes Bit fuer den GRUND.
* **Ein Zeiger auf „den" Empfaenger einer Spende ist falsch, sobald Spenden schachteln.**
  `sc_donee` hielt genau einen Slot. Der zweite CALL ueberschrieb ihn, der innere REPLY loeschte
  ihn — danach wartete der mittlere Server auf einen Wecker, den es nicht mehr gab. Genau die
  Kette `fs -> Blockdienst -> Treiber`, die dieses Projekt selbst faehrt, und ohne jedes
  Privileg: zwei CALLs und ein REPLY. Eine Spende ist ein STAPEL; `sc_donee` war nur die Spitze.
* **Die naheliegende Fassung eines Waechters kann schlimmer sein als der Fehler.** D8: `unblock`
  reihte einen ERSCHOEPFTEN Thread bedingungslos ein. Die offensichtliche Behebung
  `if blocked && !depleted { .. }` ueberspringt aber auch `blocked = false` — das RESUME wird
  verschluckt, und zusammen mit dem noetigen zweiten Waechter im Refill verhungert der Thread
  **vollstaendig** (gemessen: 0 Ticks mit Budget statt 6). Richtig ist der Waechter INNERHALB des
  Rumpfes. Wer eine Behebung nicht mit derselben Schaerfe misst wie den Fehler, tauscht ihn nur
  gegen einen schlechteren.
* **Ein Waechter, der nach seiner eigenen Behebung weiterschreit, wird abgeschaltet.** Die
  B2-Veraltungsmeldung im Scheduler-Waechter feuerte, sobald `audit` ueberhaupt `depleted` prueft
  — also ab dem Tag, an dem der Befund behoben war, fuer immer. Danach schweigt sie auch beim
  naechsten echten Fall. Solche Meldungen gehoeren an das Register gekoppelt, nicht an die
  Beobachtung.
* **Ein Gate im Format des falschen Servers ist kein schwaeches Gate, sondern keins.** Kani, Loom
  und Verus lagen in `.gitea/workflows/`; der Server ist GitLab und liest das nicht. Zwei
  Pipelines in der ganzen Projektgeschichte, beide vom 2026-05-23, beide mit der
  Auto-DevOps-Vorlage. Der rote Verus-Beweis fiel deshalb 37 Tage niemandem auf — nicht weil
  niemand hinsah, sondern weil es nichts zu sehen gab. **Ein CI-Gate ist erst dann eines, wenn man
  eine Pipeline-ID vorzeigen kann.** Und danach zaehlen drei unterscheidbare Faelle: keine Pipeline
  (Konfiguration nicht gefunden), Pipeline mit `pending`-Jobs (kein Runner), gruen.
* **Eine lokale Datei in `.git/info/exclude` sieht versioniert aus.** `CLAUDE.md` stand dort. Ein
  Commit mit dem Titel „CLAUDE.md auf den Stand von heute" enthielt ausschliesslich `todo.md`, und
  jede Aenderung an ihr lag auf genau einer Platte. `.gitignore` faellt beim Lesen auf,
  `.git/info/exclude` nicht — es wird nicht mitversioniert und steht in keinem Diff.
* **Ein Nebenlaeufigkeitsbeweis ohne verfolgte Zellen prueft nur die Atomics.** Loom sah eine
  abgeschwaechte Speicherordnung im Ticket-Release nicht, solange `data` in einem
  `core::cell::UnsafeCell` lag — die Veroeffentlichung der Nutzlast war gar nicht im Modell.
  Gemessen: 0 von 6 gegen 2 von 6.
* **Eine Suite, die einmal laeuft, misst nicht.** Die ARM-Seite hatte bis 2026-08-02 keine
  Wiederholungsmessung, und ihre Ausgabe hing an einer **Pipe**, die beim SIGKILL verlorenging.
  Das sah wie Kernel-Nichtdeterminismus aus (~30 %, jedes Mal eine andere Pruefung) und war die
  Mechanik. **Zweite Schicht desselben Fehlers:** danach teilten sich alle Laeufe EINE Logdatei —
  dann fehlten gelegentlich fruehe Bootzeilen, waehrend die Ergebniszeilen (und damit die
  Signatur) vollstaendig blieben.
* **Nur den Treiberteil einer Virtqueue zu nullen, reicht nicht.** `used` gehoert dem Geraet — aber
  bei einer **wiederverwendeten** Region (Treiber-Austausch, A-5.1) steht dort noch der Endstand der
  vorigen Fassung. Das Geraet faengt nach dem Reset wieder bei 0 an, die neue Fassung wartet auf
  einen Fortschritt, der schon eingetreten ist, und laeuft in ihre Poll-Schranke. Sieht aus wie ein
  stummes Geraet. Der Treiber initialisiert die **ganze** Queue, bevor er sie freigibt.
* **Zwei Suiten, die dasselbe Geraet verschieden aufsetzen**, sind ein Riss: `test-qemu-x86.sh`
  bekam `iommu_platform=on`, `test-qemu-x86-load.sh` nicht — die Lade-Suite lief in den Watchdog,
  und es sah aus wie ein Haenger.
* **Eine lokale IRQ-Sperre ist kein Fenster über eine geteilte Größe.** `loadstop` verglich
  globale Zähler mit `local_irq_disable()` — ein Kern still, sieben laufen. Das ging gut, solange
  nichts sonst passierte; mit dem Root-Task fiel ein fremder `free` (16 KiB) mitten hinein und
  meldete FAILURES. Erst Ruhe feststellen, dann messen — und „nicht messbar" ist kein bestandener
  Test.
* **Zwei Zahlen, die aus derselben Hand kommen, sind keine zwei Quellen.** `boot_arg` gab dem
  Root-Task die Archivgröße statt der Startmenge; auf x86 stimmten beide überein, weil dasselbe
  Skript Archiv und Manifest erzeugte. Auf aarch64 (zehn Fremdmodule im Archiv) wäre die
  Startmenge falsch gewesen.
* **Ein reproduzierbarer Wert kann nicht belegen, dass er geerbt wurde.** Z4 Stufe 2 verglich
  zuerst nur „gespeicherter Fortschritt == gefundener Fortschritt". Unter KVM ist der Hochlauf
  deterministisch: derselbe Kernel erreicht an derselben Stelle 133…155 Runden, Streuung rund 20.
  Eine Mutation, die den Wert **las und meldete, ohne ihn zu setzen**, traf ihn exakt (151 gegen
  151) — gruene Suite, nichts wiederhergestellt. Die Nonce half nicht: sie belegt die **Herkunft
  der Bytes**, nicht die **Wirkung** des Wiederherstellens. Zwei verschiedene Fragen, dieselbe
  Unterscheidung wie `rx_used` gegen „Daten angekommen". Die Loesung war eine **wachsende Kette**
  (jeder Lauf arbeitet +100 Runden weiter, Epoche + 1), damit der geerbte Wert strukturell
  ausserhalb dessen liegt, was ein Lauf allein erreicht.
* **Der Puffer eines Treibers gehoert dem LETZTEN Client, nicht der Aussage.** `drv : ALL PASS`
  las den Datenpuffer der Treiber-PD **im Bericht** und verglich ihn mit der Plattenmagie. Das ging
  gut, solange es genau einen Client gab; mit einem zweiten stand dort dessen Sektor, und die Zeile
  meldete `FAILURES` fuer einen Treiber, der alles richtig gemacht hatte. Ein Wert wird dort
  **erfasst, wo die Aussage gilt** — nicht dort, wo sie gedruckt wird.
* **`match lock() { .. None => lock() }` ist ein Selbst-Deadlock im seltenen Zweig.** Der
  Guard des Scrutinees lebt bis zum Ende des `match`; ein zweites `lock()` im `None`-Arm
  blockiert auf einem Spinlock, den derselbe Faden haelt. Gebaut beim Ausweichpfad der
  Zonenpolitik (E-Rest 3b), gemessen als stehende Lade-Suite. Ein Fehler im seltenen Zweig sieht
  aus wie ein Haenger und nicht wie ein Fehler.
* **Ein Zaehler, der VERSUCHE zaehlt, beantwortet die Frage nach der WIRKUNG nicht.** Der
  Ausweichzaehler von E-Rest 3b meldete `1x` auf einer 512-MiB-Maschine, auf der es oberhalb
  4 GiB gar keinen Speicher gibt — gezaehlt hatte er eine absichtlich uebergrosse Anforderung,
  die NIRGENDS passte. „Unten war kein Platz" und „es wurde oben genommen" sind zwei Aussagen;
  dieselbe Verwechslung wie `rx_used` gegen „Daten sind angekommen".
* **Ein Waechter prueft die EXISTENZ eines Grundes, nie seine WAHRHEIT -- ein falscher Grund ist
  damit unsterblich.** Der Eintrag zu `SYS_MAP` sagte „das ist die ABI"; tatsaechlich traegt
  `sys::MAP` kein Adressargument, und die Identitaet entsteht erst in der Cap-Aufloesung des
  Kernels. Der falsche Grund liess den Punkt als unbehebbar erscheinen. Abhilfe: wo ein Grund
  widerlegbar ist, gehoert ein **Falsifikator** dazu -- und der muss selbst pruefen, dass sein
  Anker existiert, sonst liest er ins Leere.
* **Eine Liste im Pruefskript ist eine Textflaeche; eine Liste im Typ ist eine Bedingung.** Die
  erste Fassung des Identitaets-Waechters fuehrte ihre Funktionsliste von Hand -- `vspace_map_dma`
  fehlte, und damit sah sie den DMA-Pfad einer Treiber-PD nie. Die zweite liest die Funktionen aus
  der HAL und stuetzt sich im Uebrigen auf einen Typ ohne zweiten Konstruktor.
* **Ein Fehlschlag ohne Protokoll ist ein verlorener Fehlschlag.** Zwei Ausfaelle am 2026-08-04
  (aarch64 unter Last, x86-Lade-Suite bei 512M) liessen sich nicht untersuchen, weil die
  Sammelschleife nur `tail -1` festhielt. Die Suiten legen bei Abweichung selbst ein volles Log
  ab; wer darueber schleift, muss es auch tun. Bei einer Rate um 1/20 kostet jeder verlorene
  Fehlschlag Stunden.
* **Ein Parameter, der zwei Bedeutungen traegt, ist so lange harmlos, wie die beiden zufaellig
  gleich sind.** `Scheduler::spawn_user` nahm EINEN Wert fuer den EL0-Stackzeiger UND die
  Reap-Region, die beim Thread-Tod an den Allokator zurueckgeht. Solange die private Region
  identisch abgebildet war (VA == PA), war das dieselbe Zahl. Nach dem Fenster-Umbau nicht mehr:
  `#PF cr2=0x0000008000000000` im KERNEL, weil der Reap-Pfad eine virtuelle Adresse als
  Physadresse freigab. Dieselbe Form wie das `blocked`-Bit im Scheduler (D9). Das Gegenstueck
  `spawn_user_at` gab es laengst -- der Ladepfad benutzt es seit A-2.
* **Ein User-VA-Fenster gehoert dorthin, wo der Kernel NIE identisch zugreift.** Der Kernel
  laeuft beim Syscall im Adressraum der PD und erreicht physisches RAM ueber die
  Identitaetskarte. Eine User-VA in GiB 0, die auf eine andere PA zeigt, verdeckt genau diese
  Sicht -- der Kernel laese dort den Speicher der PD statt den eigenen. Deshalb `PML4[1]` (x86)
  bzw. `L1[9]` (aarch64) und nicht „irgendwo in GiB 0".
* **Eine Messung an einem kaputten Aufbau ist keine Messung.** Der Befund „geladene
  Programmsegmente brauchen GiB 0" stand einen halben Tag als Tatsache im Kopf -- er kam aus
  einem Lauf, in dem gleichzeitig ein Selbst-Deadlock steckte. Nach dessen Behebung war er
  widerlegt: `vspace_map_page_at` nimmt VA und PA getrennt, die Physadresse ist frei. Wer
  waehrend einer Fehlersuche misst, muss den Aufbau zuerst gesundmachen.
* **Eine Klassifikation, die nur gegen die HAUPTSUITE geprueft ist, prueft die Haelfte.** Stellt
  man `system::alloc` auf „reiner Kernel-Speicher", bleibt die Hauptsuite gruen und die
  Lade-Suite faellt bei 3G aus. Dieselbe Form wie D8/D9/D11: die Suite loest den Fall nicht aus,
  den sie zu decken scheint.
* **„Unten zuerst" war jahrelang ein Zufall der Groessenrelation, kein Entwurf.** Best-Fit nimmt
  das kleinste passende Fragment. Solange der Speicherbereich oberhalb 4 GiB zufaellig groesser
  war als der untere, landete alles Unbenannte unten — und Dutzende Stellen kamen ohne
  Zonenwunsch aus. Bei `-m 3G` (1024 gegen 2032 MiB) kehrt sich die Relation um, und der ganze
  Ladepfad faellt aus. Wo eine Eigenschaft aus einer Groessenrelation folgt statt aus der
  Struktur, verschwindet sie beim naechsten Messwert.
* **`wrapping_sub` auf einer Zeitdifferenz ist die teuerste bequeme Zeile.** Ein Zähler, der um
  100 Zyklen zurückspringt, ergäbe rund `2^64` — ein Konto, das so belastet wird, ist sofort und
  dauerhaft erschöpft. Rückwärts heisst **verworfen**, nicht „fast einmal herum".

## Aufbau, grob

| Ort | Inhalt |
|---|---|
| `kernel/src/system.rs` | Kern der Faehigkeitsverwaltung, IOVA-Fenster, Teardown-Token, Audit |
| `kernel/src/addr.rs` | `Pa`, `Iova`, `DmaRegion` |
| `kernel/src/dmatests.rs` | architekturneutrale DMA-Tests, von beiden Hochlaufwegen gefahren |
| `kernel/src/arch/x86_64/bootinfo.rs` | `HandoverInfo` — eine Struktur, zwei Herkuenfte |
| `kernel/src/arch/x86_64/dmar_selftest.rs` | synthetisches DMAR fuer den Selbsttest |
| `crates/sel4lake-hal/` | `vtd`, `dmar`, `intc`, `timer`, `fault`, `iommu`-Fassade |
| `crates/sel4lake-cap/src/space.rs` | `Finalized`, CDT, `delete_leaf` |
| `crates/sel4lake-cap/src/checkpoint.rs` | Z4: die Verweigerungsregel (`classify`) **und** das Checkpoint-Format (`Image`, CRC-32). Abhaengigkeitsfrei, ohne `unsafe`, host-getestet — ein Checkpoint ist Eingabe, kein Zustand |
| `crates/sel4lake-virtio/` | virtio: `Transport` + `Queue`, darauf `rng`/`blk`/`net`, plus `probe_ecam`. **Ohne jede Abhaengigkeit** — wird von der Treiber-PD gelinkt (A-5.1) |
| `programs/hardware/virtio-blk/` | **der erste Treiber ausserhalb des Kerns** (A-5.1): loest sein Geraet selbst auf, bedient Anfragen ueber seinen Kanal, austauschbar im Betrieb; seit A-6 auch Blockdienst + GPT-Scan |
| `crates/sel4lake-part/` | GPT-Parser (A-6.2). Abhaengigkeitsfrei, `forbid(unsafe_code)`, host-getestet — fremde Plattenbytes gehoeren nicht in den Kern |
| `crates/sel4lake-fat/` | FAT16-Parser (A-6.3), ebenso |
| `programs/trusted/fs/` | **Dateisystem-PD** (A-6.3): faehrt kein Geraet, ruft den Blockdienst |
| `tools/mkgpt.py` | baut die GPT-Testabbilder, auch **kaputte** (`--break`) — beide Suiten benutzen dasselbe Werkzeug |
| `kernel/src/colors.rs` | Farbzuteilung, `run_color`, Prime+Probe (B-4.5) — arch-neutral |
| `crates/sel4lake-sched/src/cycles.rs` | Zyklenabrechnung (B-5.1) — **ohne jede Abhaengigkeit**, damit die Fallen mit Literalen statt mit einer Maschine ausloesbar sind |
| `tools/kernel-grenze.sh` | prueft, dass keine Treiber in die HAL wandern; mit Selbsttest |
| `tools/host-tests.sh` | die Host-Tests der reinen Crates an **einem** Ort (`sel4lake-cap` lief vorher nirgends) |
| `tools/handover/` | Linux-Kernelmodul fuer die Kern-Uebergabe (Variante B) |

## Wenn du hier auf dem Server arbeitest

Diese Kopie enthaelt **kein `target/`** und keine grossen Testdaten — siehe `../MEMORY.md`.
Ein fehlendes Bauverzeichnis ist Absicht, kein Defekt. Der erste Build dauert entsprechend.

## Der naechste Schritt

**Strang A-5 ist zu.** Zwei Treiber-PDs, jede mit ihrem im Manifest benannten Geraet, und die
Trennung zwischen ihnen ist gemessen statt behauptet (A-5.4).

Alles Weitere in `todo.md` — lesen, nicht raten. Zwei Dinge, die unmittelbar anschliessen:

* **`CAP_IRQ` fuer Treiber** braucht eine **IRTE-Vergabe** — die Interrupt-Remapping-Tabelle steht
  seit B-3.2 auf lauter „not present", und das ist Absicht. Bis dahin pollt jeder Treiber. Das ist
  B-3-Arbeit, nicht A-5.
* **Das Geraete-ANGEBOT ist noch virtio-gebunden**: der Kernel findet Kandidaten ueber
  `hal::pcie::find(VIRTIO_VENDOR, …)`, weil die BAR-Bestimmung durch `probe_transport` geht. Die
  Auswahl steht seit A-5.3 sauber im Manifest, das Angebot nicht.
