# Caprock

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
| x86_64 | **49 996 von 50 000** nach der D0-Behebung (2026-08-07 abends, `tools/d0-messen.sh`, 16 parallele Stroeme gegen EINE Referenz). **0 D0-Treffer** — davor 9 in 50 000. Die 4 Abweichungen sind Lastartefakte des Messstands (3,2-fache vCPU-Ueberbuchung), s. todo D13 |
| x86_64 RAM-Reihe | `== ALL PASS ==` bei **512M · 2560M · 3G · 6G**, Haupt- **und** Lade-Suite (2026-08-04). Vor E-Rest 3 starb ab 3G der Boot mit `#PF cr2=0x70_0000_0014` — der Zweig „RAM oberhalb 4 GiB" war nie gelaufen |
| x86_64 Lade-Suite | `== ALL PASS ==` (2026-08-03: 39 Pruefungen — 5 Module, **zwei** Treiber-PDs, Austausch, A-5.3/A-5.4, dazu **drei** verkettete Boots fuer Z4 Stufe 2, inkl. sieben Negativfaellen) |
| aarch64 | **`RUNS=6` → 6 von 6 mit identischer Signatur, `== ALL PASS ==`** (2026-08-13, nach der C9e-Behebung). Über **12** Läufe desselben Standes: **11 sauber, 1 D13** — die Rate gehört zur Zahl, und D13 ist hier unterscheidbar, weil die Farbzeile selbst auf `ALL PASS` steht und nur der `COLOR_DONE`-Store zu spät kommt. Davor **zehn Tage rot** (`color : FAILURES` mit lauter Nullen, 3 von 3), ohne dass es jemand sah: die Abnahme-Reihe **baute** aarch64 und **bootete** ihn nie. Seit dem 2026-08-13 fährt `tools/abnahme.sh` die Suite mit — **62 s je Lauf**, gemessen, gegen rund 950 s für die übrige Reihe |
| Host-Tests | `mem · part · fat · cycles · loader · cap · virtio · typestate · ipctreue` → `== HOST-TESTS: ALL PASS ==` (`tools/host-tests.sh`, 2026-08-03) |
| Verus | **16 Beweisdateien, 0 errors** — dazu **drei** Modell-Treue-Waechter (cap_space, IPC, Scheduler) mit 28 · 28 · 30 Selbsttestfaellen (2026-08-03) |
| Scheduler-Messung | `tools/sched-erschoepfung-messen.sh`: 208 Messwerte, Positivkontrolle bestanden, vier Fassungen (echt/V0/H-a/H-b) — belegt D8, D9 und D10 |

**D0 ist am 2026-08-07 gefangen und behoben worden — die Abnahme steht unter Vorbehalt.**
0 Treffer in 50 000 Läufen gegen 9 in 50 000 davor. **Aber:** zwischen beiden Reihen wurde der
Speicherregler berichtigt, die Parallelität war also eine andere, und bei einem Startrennen ist
genau die Last die Größe, die die Rate erzeugt. `P(0 | unverändert) ≈ 1,2·10⁻⁴` steht damit für
„behoben ODER weniger Druck" — die beiden sind nicht getrennt. Schlimmer: die Bedingung der
Fundmessung ist aus ihrem (in der Mitte abgeschnittenen) Protokoll **nicht mehr feststellbar**.
Seither nimmt `tools/d0-messen.sh` eine feste Arbeiterzahl und **nennt die Bedingung in der
Bilanz**.

Dazu: `pdbind` zählt auf x86 **3** Bindungen, auf aarch64 **70** — 50 000 x86-Läufe decken drei
Zulassungsstellen ab und lassen siebzig am unbeobachteten Ende. Der Messstand fährt deshalb jetzt
auch `ARCH=arm`. Details und die aarch64-Klassifikation in `done.md`.

Die Ursache und der Umbau stehen in `done.md`; hier bleibt, wie der Fehler aussah:

**Der Fund selbst — mit 50 000 Läufen.** 9 Abweichungen, alle neun
zeichengleich (gleiche MD5 über den Signaturdiff, verteilt über fünf Ströme). Rate **0,0180 %**,
95-%-Intervall **[0,0082 %, 0,0342 %]**, einer je **5556** Läufen.

**Damit ist auch die Lesart von gestern erledigt.** Die 2300 sauberen Läufe waren keine Behebung,
sondern eine zu kleine Stichprobe: bei 0,018 % ist `0,99982²³⁰⁰ ≈ 66 %` — ein Nullbefund war der
**wahrscheinlichste** Ausgang. Die alte Grundlinie von 0,5 % (2/400) hat nie gestimmt. Wer aus
„2300 grün" auf „behoben" geschlossen hätte, hätte 66 % Zufall für einen Beweis gehalten.

**Das Fehlerbild ist nicht der vermutete Hänger.** Der Knoten läuft die vollen 61 s durch
(`ticks=6104` gegen 52 in der Referenz, Worker-Runden 4182 gegen 27) und besteht jede andere
Prüfung. Blockiert ist **ein** Thread: der IPC-Client wartet auf eine Antwort, die nie kommt, weil
der Server seine `RECV`/`REPLY`-Schleife verlassen hat (`IPC-Rolle-abgewiesen=false`). Der Grund
des Ausstiegs fiel bis 2026-08-07 auf den Boden — ein `if m.result != OK { break }` ohne Ablage.
Seither hält `IPC_SERVER_EXIT` ihn fest und der Bericht druckt ihn. Details in `todo.md` D0.

Ein Vorbehalt, der zur Zahl gehört: `pprobe` meldet unter KVM grundsätzlich `SKIP`
(`CPUID.1:ECX[31]`) und urteilt in dieser Reihe nicht mit.

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
* **VA == PA: die BINDUNG, nicht nur die Liste.** Der Grund war ein **freies Argument**
  (`Va::identity(reason, pa)`) -- nichts hinderte `Va::identity(Mmio, dma_pa)`, und der Waechter
  haette einen gueltigen Grund gesehen. Jetzt gibt es einen Konstruktor **je Stelle**
  (`Va::for_*`), kein verwechselbares Argument, und die Engstellen nehmen den Konstruktor als
  Funktionswert. Dazu `IdentityClass::{Invariant, Debt}`: „die Identitaet IST die Zusicherung"
  und „behebbar, jemand sollte" standen ununterscheidbar nebeneinander -- die Schuld waere
  unsichtbar geworden. `IDENTITY_DEBTS = 3` ist eine **Ratsche** und darf nur fallen.
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

## Was am 2026-08-11 dazukam (C8: der Verifiziererthread)

* **Die Krypto steht nicht mehr auf dem Stack des Aufrufers — und die Zahl ist gemessen.**
  `SYS_LOAD` → `verify_image` → Ed25519 + SHA-2 lief auf dem 16-KiB-EL1-Stack des *aufrufenden*
  EL0-Threads und fuellte ihn zu **73,1 %** (11 992 von 16 384 B). Seit C8 laeuft der Pfad auf dem
  64-KiB-Stack eines dedizierten Verifiziererthreads. Lade-Suite, 512M:
  **EL0-Kstack 11 992 → 1 312 B (73,1 % → 8,0 %)**, Verifiziererstack **12 328 / 65 536 B (18,8 %)**.
  Die Hauptsuite bleibt bei 824 B — **das ist die Kontrolle**, sie hat kein Archiv und fuhr nie
  Krypto ueber `SYS_LOAD`.
* **Die `kstack`-Zeile misst seither etwas ANDERES**, und sie sagt es selbst. Vorher war sie eine
  Messung des Ladepfads, jetzt eine des **Restpfads**. Wer 1312 gegen 11 992 haelt, vergleicht ueber
  einen Bedeutungswechsel hinweg — die Umdefinition steht in der Zeile, in beiden Suiten-Skripten
  und in `done.md`.
* **Der Wartegrund ist ein GRUND, kein viertes Bit** (`BlockReasons::LOAD`, Z24, sechste Instanz
  derselben Klasse). Wecker ist ausschliesslich `load_reply`; `resume`/`unpark`/`unblock`/
  `handler_reply` entfernen ihn nicht, und eingereiht wird nur bei leerer Menge.
* **Die Serialisierung ist ein BENANNTER DoS-Kanal:** `AUFTRAEGE_MAX = 4`, Ueberlauf
  `ERR_LOAD_BUSY = 13`, und der Ueberlaeufer wird **gar nicht erst blockiert** — D11 woertlich.
  Gefahren, nicht behauptet: Pruefzeile `verif`, 5 Sonden gegen 4 Plaetze, `Fuellstand erreichte
  4/4`, Ueberlaeufer nicht blockiert und am Rundenzaehler nachweislich weitergelaufen. Zwei
  Gegenproben, beide mit **genau einem** offenen Konjunkt (`offen waren: verif`).
* **Kein verlorenes Wecken, und zwar ohne zweiten Zustand.** Der Aufrufer wird blockiert, **bevor**
  sein Auftrag sichtbar wird — beides unter EINER Sperrung der Auftragsschlange. Andersherum haette
  ein Verifizierer auf einem anderen Kern antworten koennen, bevor der Grund gesetzt ist. Fuer
  `PARK` faengt das eine Weckmarke ab; hier tut es die Sperrung (`SCHLANGE` R1.5 → `SCHEDS` R2,
  `docs/invariants.md` §1).
* **Drei Befunde, die nicht im Auftrag standen:** die **Platzierungspolitik** waere still
  mitgewandert (ein Programm ohne Manifest-Affinitaet landet auf `hal::cpu::core_id()` — bis C8 der
  Aufrufer, danach der Verifizierer); die abgeleiteten **Endowment-Caps lecken** auf den zwei neuen
  Abweispfaden, weil dort der Loader gar nicht erst drankommt; und eine **Marke auf Bit 63**
  kollidierte mit dem obersten Zahlenfeld des Ergebniswortes (`verloren` las sich als 128, Urteil
  fiel durch, Watchdog — bei jedem einzelnen gruenen Feld darueber).
* **Vorbestehend, nicht von mir:** der **aarch64-Kernel baut auf diesem Zweig nicht** (drei
  `E0425`: `hal::mmu::guard_unmap`/`guard_remap` gibt es nur in der x86-HAL, seit der Guard-Page-
  Arbeit vom 2026-08-10). Die aarch64-Seite von C8 (`verifizierer::starten()` in `main.rs`) ist
  damit **ungeprueft**.

## Was am 2026-08-07 dazukam (zweiter Teil: A1 + Z11c)

* **A1 wirkt jetzt auf dem REGULAEREN Weg — und die Entscheidung steht im Manifest.** Ein Programm
  mit `POLICY_EXCLUSIVE_STRIPE` wird stueckweise aus EINEM Farbstreifen geladen (Segmente, Stack,
  Seitentabellen, EL0-Kernel-Stack). Belegt als `pdcolor : ALL PASS` — 5 Seiten in 16 von 512
  Farben, gemessen an der **Teardown-Buchhaltung**, nicht am Ladepfad.
  Die alte Begruendung, warum das nicht gehe, war zur Haelfte falsch: „Segmente kommen
  zusammenhaengend aus `mem_alloc`" beschrieb die damalige ALLOKATION, nicht eine Notwendigkeit —
  gemappt wurde laengst seitenweise.
* **Die Politik des Manifests wird ANGEWANDT, nicht nur gedruckt** (`ladepol : ALL PASS`).
  `priority`/`core_affinity` eingehalten; `numa_node != 0`, `POLICY_PINNED` und `budget_us != 0`
  **abgewiesen** statt still ignoriert. Bei `budget_us` ist das eine Formatfrage: eine
  MCS-Reservierung braucht Budget UND Periode, das Manifest hat eine Zahl — aus einer Zahl eine
  Reservierung zu machen hiesse, die Periode zu erfinden.
* **Der Befund, der groesser ist als der Eintrag:** die Prioritaeten standen seit jeher im
  Test-Manifest (3/1/2/2/2) und wurden nie eingeloest — Platzhalter. Eingehalten REISST dieselbe
  Zuteilung die Lade-Suite: ein **pollender** Treiber auf hoeherer Prioritaet als sein Client
  laesst den Client verhungern. Ein Feld, das nie eingeloest wird, sammelt ungepruefte Werte an,
  und der Tag der Einloesung ist der Tag, an dem sie alle falsch sind.
* **Drei eigene Fehler im Pruefer, alle gemessen:** die erste Gegenprobe war NICHT ERFUELLBAR
  (sie verlangte von einer 2-Seiten-PD mehr Farben, als ein Streifen fasst — sie fiel durch,
  unabhaengig davon, ob die Faerbung traegt); die Messung stand zuerst in `all_done()`, das
  GEPOLLT wird; und die Prioritaet wurde im Bericht zurueckgelesen, wo der Thread schon tot sein
  darf. Den dritten hat die Zeile selbst gefangen — sie meldete **SKIP** statt ALL PASS, weil ich
  nur einen der beiden Ladepfade umgestellt hatte.
* **Way-Partitionierung (CAT/MPAM) bewertet, nicht gebaut, und der Grund ist eine Messung:** auf
  dem Entwicklungsrechner gibt es sie nicht (keine `cat_l3`/`rdt_a`-Flag, kein `resctrl`). Baubar,
  aber nicht pruefbar. Der Entwurfspunkt gilt trotzdem: Faerbung trennt SETS, CAT trennt WAYS —
  beide ohne gemeinsame Politik ist schlechter als eine.
* **Drei Eintraege standen offen und waren erledigt:** Z11b (Manifest signiert und ans Image
  gebunden), Z11e (`iface_version` durchgesetzt), Z11f (Negativliste in `docs/invariants.md` §13).
  Dazu D11 tags zuvor. Ein Register, das Erledigtes fuehrt, macht die Frage „was ist offen"
  unbeantwortbar — genau die Form, die der Modell-Treue-Waechter an sich selbst gemeldet hat.

## Was am 2026-08-07 dazukam

* **D0 ist gefangen UND behoben — nach zehn Tagen und vier Messreihen.** Die Ursache war keine
  der drei Hypothesen, die im Eintrag standen: **ein Thread war lauffaehig, bevor er seine PD
  hatte.** `spawn()` reihte ein, `bind_pd()` kam danach; faellt der IPC-Server in dieses Fenster,
  macht er sein erstes `RECV` mit LEEREM Cspace, bekommt `ERR_NOPD` und verlaesst seine Schleife
  fuer immer. Gemessen: 9 Treffer in 50 000 Laeufen (0,0180 %, einer je 5556), alle neun
  zeichengleich; danach 5 in 6895 mit dem Melder, **4 von 4 `ERR_NOPD`**.
  Behoben strukturell: der Scheduler trennt `spawn_parked` von `admit`, 61 Aufrufstellen
  umgestellt, `load_into_pd` mit dazu. **Abnahme: 0 Treffer in 50 000 Laeufen** am selben
  Messstand, der den Fehler vorher 9-mal gefangen hat. Details in `done.md`.
* **Die vier uebrig gebliebenen Abweichungen sind ein Befund ueber den MESSSTAND.** Zwei
  `cycles`, zwei `freeze` -- und entschieden hat nicht die Zahl, sondern die FORM: bei einem der
  `freeze`-Fehlschlaege fiel die **Positivkontrolle** durch (`laeuft-vorher=false`), gemessen
  bevor ueberhaupt eingefroren wird. Ein Fehler im Auftaupfad kann sie strukturell nicht
  verursachen. Gemeinsam ist beiden Bildern ein Fenster in **Wanduhrzeit**, und der Stand faehrt
  16 Gaeste zu je 4 vCPU auf 20 Kernen -- 3,2-fache Ueberbuchung; das `cycles`-Fenster mass
  Faktor 4,2. Steht als D13 in `todo.md`.
* **Warum niemand es frueher sah — und die Zahl, die dazugehoert.** Die 2300 sauberen Laeufe
  vom 2026-08-03 galten als „die alte Quote ist ausgeschlossen". Bei der wahren Rate ist
  `0,99982²³⁰⁰ ≈ 66 %` — ein Nullbefund war der **wahrscheinlichste** Ausgang. Die belastbare
  Groesse ist nicht die Stichprobe, sondern die **erwartete Trefferzahl**: 0,41 damals, 9,0 jetzt.
* **Der Riss steckte auch im Produktionspfad.** `load_into_pd` band die PD und installierte das
  Endowment NACH dem Lauffaehigmachen; gedeckt war das nur durch ein `local_irq_save` — also durch
  zwei Bedingungen, die nirgends festgeschrieben sind (kernlokale Ready-Queue, Lastausgleich aus).
  Und das Wissen war da: an **einer** von 53 Stellen stand seit jeher ein `local_irq_disable()`
  mit genau dieser Begruendung. Eine Gefahr, die an einer Stelle per Hand abgewehrt wird und an
  52 nicht, ist ein fehlender Mechanismus, keine Sorgfaltsfrage.
* **Der Waechter dazu zaehlt die GELEGENHEIT, nicht den Treffer** (`pdbind`). Bei 0,018 % waere
  ein Melder, der nur beim Unglueck spricht, in 5555 von 5556 Laeufen stumm. Die REIHENFOLGE
  dagegen ist in jedem Lauf pruefbar. Mit Sprechprobe, getrenntem `unklar` und **benannten**
  Ausnahmen (`SpaetbindungsGrund`) statt eines `if tid == ..`.
* **Der Modell-Treue-Waechter hat die Aenderung von selbst beanstandet** — und dabei einen
  veralteten Registereintrag gefunden, den sonst niemand bemerkt haette (`spawn` stand als
  zustandsschreibend, schreibt aber nichts mehr).
* **Die Luecke, die der Zaehler NICHT sieht — und der Typ, der sie schliesst.** `spaet == 0` zaehlt
  SPAETE BINDUNGEN, nicht AUSBLEIBENDE ZULASSUNGEN. Seit 2026-08-07 gibt `spawn_*_parked` ein
  **`Parked`** zurueck: `#[must_use]`, kein `Drop` (sonst liesse sich das Feld in `admit` nicht
  herausbewegen), **kein oeffentlicher Weg an die `ThreadId`**. Der Typ fand sofort eine fuenfte
  Stelle mit Autoritaet nach der Zulassung, die das Gegenlesen uebersehen hatte
  (`map_region_into_thread` an drei Geraete-Backends) -- und die naheliegende mechanische
  Umstellung nahm den Fehler mit. Bewacht von `tools/zulassung.sh` (7 von 7 im Selbsttest), dort
  auch der Ankertest fuer `ERLAUBTE_SPAETBINDUNGEN`.
* **OFFEN, und ernst: der Kernel springt nach Adresse 0** — **0,225 % gepoolt ueber 4000
  aarch64-Laeufe** (2026-08-08). Ein EL0-Thread faultet mit `EC=0x20 FAR=0` (Instruction Abort,
  sein PC stand auf 0), danach nimmt der KERNEL `EC=0x21` mit `ELR=0`. Die betroffenen Threads
  liegen auf **wiederverwendeten Slots** (Generation 1, im Bereich des 1024-Thread-`scale`-Tests).
  **Nicht durch den D0-Umbau entstanden** — gemessen, nicht vermutet: 3 in 2000 Laeufen auf dem
  Stand VOR dem Umbau gegen 6 in 2000 danach, `P(>=6 von 9 in einer Reihe) = 0,254` einseitig.
  Damit ist auch meine Hypothese widerlegt („der neue Zustand *geparkt* trifft auf den Reap-Pfad"
  kann nicht stimmen, wenn es das Bild ohne diesen Zustand genauso oft gibt).
  Details in `todo.md` D15, Protokolle in `docs/befunde/d15/`.
* **Der D0-Umbau hat eine Regression erzeugt, und der Kernel-Audit hat sie selbst gefunden.**
  Audit-Code 7 lautet „lauffaehig und in keiner Liste" -- woertlich der Zustand eines GEPARKTEN
  Threads, den es vor der Behebung nicht geben konnte. Gemessen in der aarch64-Reihe:
  `scale : FAILURES`, `sched_audit=7`, Lauf vollstaendig durchgelaufen, jedes andere Feld
  identisch. **1 von 600 aarch64-Laeufen -- und 0 von 56 895 x86-Laeufen**, weil das Fenster
  zwischen `spawn_parked` und `admit` auf x86 dreimal je Lauf existiert und auf aarch64
  siebzigmal. Behoben durch `t.admitted` in der Bedingung; die Schaerfe fuer D8 bleibt.
  **Die Lehre:** ein Umbau, der einen neuen Zustand einfuehrt, muss jede Stelle mitnehmen, die
  ueber Zustaende URTEILT -- nicht nur die, die sie erzeugen. Gefunden hat es weder ein Gegenlesen
  noch der Typ, sondern eine Messung unter Last auf der Architektur, wo der Zustand oft vorkommt.
* **Die Gegenprobe fand, dass der Waechter nichts gatterte.** Eine Mutation ergab
  `pdbind : FAILURES` -- und die Suite meldete `== ALL PASS ==`. x86s `all_done()` baute eine Liste
  fuer den BERICHT und gab eine getrennte `&&`-Kette zurueck: 21 Glieder gegen 24 Eintraege,
  `pdcolor`/`ladepol`/`pdbind` gatterten nichts. Jetzt ist die Liste das Urteil, auf beiden Zweigen.
* **Der aarch64-Watchdog nennt jetzt, was offen war.** Bis dahin versprach die Kopfzeile „offene
  Tests:" und druckte den vollen Bericht -- darin ist eine nie gesetzte Aussage von einer
  bestandenen nicht zu unterscheiden. Damit war mein eigener aarch64-Haenger unklassifizierbar.
  Nachgemessen: **9 von 9 Abweichungen `offen: color`**, Farbzeilen byte-identisch zur Referenz,
  Watchdog feuert zwischen Druck und `COLOR_DONE`-Store -- **D13**, kein geparkter Thread.
* **Was Verus dazu NICHT sagt:** das IPC-Modell kennt „Thread ohne PD" nicht (null Vorkommen von
  PD-Bindung oder `ERR_NOPD`). „16 Dateien, 0 errors" heisst hier nur, dass die vorhandenen
  Beweise weiter halten -- ueber die neue Eigenschaft sagt es nichts.
* **Die Speichermessung des D0-Reglers mass die falsche PID** (die Subshell statt QEMUs
  Prozessbaum). Der Wert fiel unter die Untergrenze, und die griff **still**: im Protokoll stand
  „je Lauf rund 192 MiB" — exakt `128 * 3/2`, also die Untergrenze und kein Messwert. Jetzt ueber
  den ganzen Baum (361 MiB), und statt der Untergrenze steht dort eine **Sprechprobe mit Abbruch**:
  eine Untergrenze, die einspringt, wenn die Messung nichts sieht, macht deren Ausfall unsichtbar.

## Was am 2026-08-03 dazukam

* **Z4 Stufe 2 steht: ein Thread ueberlebt eine BOOTGRENZE.** Derselbe Kernel speichert und
  stellt wieder her und **entscheidet selbst welches** — er liest einen Sektor (32710, ausserhalb
  beider Partitionen), prueft Magie, Formatversion und `kernel_code_hash` und handelt danach. Der
  Transport ist der vorhandene Blockdienst; der Kern ist Client, `virtio-blk` blieb unveraendert.
  Format in `crates/caprock-cap/src/checkpoint.rs` (feste Breiten, LE, CRC-32, abhaengigkeitsfrei,
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
  (`crates/caprock-cap/src/checkpoint.rs`, host-getestet) — was drüben nicht dasselbe bezeichnen
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
  blockiert, zahlte null. Jetzt wird jede Umplanung gestempelt (`crates/caprock-sched/src/cycles.rs`,
  abhängigkeitsfrei, host-geprüft), geprüft als `cycacct`: **Proben > Ticks** ist die Aussage.
* **D5: der aarch64-Kernel hat einen Root-Task** — und der Weg dorthin fand zwei Fehler, die mit
  dem Manifest nichts zu tun hatten: `boot_arg` gab die **Archiv**-Größe statt der Startmenge
  (jetzt `StartSetNotPrefix`, fail-closed), und `loadstop` maß eine **globale** Baseline mit einer
  **lokalen** IRQ-Sperre.
* **B-5.5: begrenzt war der Prüfer, nicht `revoke`.** `cdt_audit`-Code 9, Höchststände als
  Operationszahl im Bericht.
* **`tools/host-tests.sh`** existiert — `caprock-cap` hatte gar keinen Host-Test-Pfad, seine
  sechs Tests wären nirgends gelaufen.

## Was am 2026-08-01 dazukam

* **D0 zur Hälfte zu.** Das Farbrennen ist weg (500/500): die Kernelseite wird nicht mehr
  *zurückgelesen*, sondern vom Spawn geliefert — ein Wert, der an der Lebendigkeit eines Threads
  hängt, der sterben darf, taugt nicht als Messgröße. Der Hänger bleibt offen (s. o.).
* **Ein echtes Leck nebenbei behoben:** `record_user_kstack` lief *hinter* dem kritischen
  Abschnitt; traf der Einsammler das Fenster, wurde der Kernel-Stack nie freigegeben.
* **A-5.2 ist zu:** virtio-**Transport**, **Blockgerät** und **Netzkarte** auf x86, alle drei
  kernfrei in `crates/caprock-virtio`. Wichtiger als die Geräte ist, was sie belegen: der RNG
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
  Nebenertrag: `caprock-cap` hatte gar keinen Host-Test-Pfad — `tools/host-tests.sh` sammelt jetzt
  **62 Tests** (mem, part, fat, cap) an einem Ort.
* **B-7.1 ist zu — und die Notiz war ueberholt.** Kani deckte `sync` laengst ab; die CI beschrieb
  sich nur falsch (`Job: „…(Loader-Parser)"`, tatsaechlich alle vier Ziele) — und genau daraus war
  der „Befund" entstanden. Die **echte** Luecke: die Beweise liefen mit KONKRETEN Werten, also in
  derselben Groessenordnung wie Loom. Fuenf neue Harnesses nehmen den Zustand **symbolisch**
  (2^31 Leserzahlen, u32-Ticketueberlauf); `sync` steht bei 8 statt 3 Beweisen.
* **B-7.2 ist zu — und die Kopie war nicht der Grund.** Loom prueft jetzt den **echten**
  `caprock-sync`-Quelltext (das Skript kopiert ihn unveraendert, Beweise in derselben Datei), und
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
  **A-6.2** `crates/caprock-part` liest GPT (14/14 Host-Tests, drei kaputte Tabellen mit
  unterscheidbaren Gruenden abgewiesen); **A-6.3** `crates/caprock-fat` + `programs/trusted/fs`
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
  Entkoppeln von `caprock-virtio` waere das der bequeme Weg gewesen und haette die Semantik still
  abgeschwaecht.
* **Ein Test, der Speicher belegt, kippt baseline-empfindliche Tests.** Der Farbtest am Anfang von
  `threads::spawn_demo` liess auf aarch64 mal `captest`, mal `sched` durchfallen. Er ist dort
  deshalb ausgehaengt (x86 laeuft ihn). Ein Test, der andere Tests kippt, macht das GESAMTE
  Ergebnis unbrauchbar.
* **`MASK_BITS` ist nicht die Farbanzahl.** `region_bytes()` rechnete mit 64 statt mit `count()` —
  auf x86 (256 Farben) zufaellig richtig, auf aarch64 (16) falsch. `caprock_mem::stripe` hatte
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
* **Ein Test, der nirgends laeuft, ist kein Test.** `caprock-cap` hatte `#[cfg(test)]`-Module und
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
* **Der Lader meldet seinen eigenen Speicher als frei.** Die klassische GRUB/Multiboot-Falle:
  von 0 bis zum EBDA gilt als frei, dort liegen aber BIOS-Datenbereich, EBDA und die Ablagen des
  Laders. Hier dreifach gedeckt (nur Typ-1-Bereiche, `base >= 1 MiB` verworfen, Freiliste ab
  16 MiB, Module ausgeschnitten) -- **aber die Multiboot-Info-Struktur wird NICHT ausgeschnitten**.
  Ihr Schutz hing allein an `USER_RAM_MIN`; senkt jemand die Konstante, faellt er lautlos weg.
  Seit 2026-08-07 eine gemessene Zeile statt einer Annahme.
* **Ein Ausstiegskriterium, das Wiederholbarkeit verlangt, schliesst seltene Fehler per
  Definition aus.** Der erste D12-Ausstieg tat das -- und die Klasse, um die es geht (verpasstes
  WFE-Wakeup, Timer/IPI-Rennen bei 1 in 32), reproduziert gerade nicht. Ein Kriterium gehoert an
  die **Form des Artefakts** (vollstaendige Ausgabe, passender Exit-Code, inhaltlich abweichende
  Pruefzeile); Wiederholbarkeit gehoert in die Priorisierung.
* **rustc prueft HERSTELLBARKEIT, nicht NICHT-WEITERGABE.** Ein Zeuge mit privatem Feld ist
  ausserhalb nicht konstruierbar -- innerhalb seines Moduls aber beliebig, und ein
  `pub fn witness()`, ein `#[derive(Copy)]` oder ein oeffentliches Feld gibt die Bindung wieder
  frei. Was der Typ nicht abdeckt, muss der Waechter pruefen.
* **Ein Muster, das nur die uebliche Formatierung trifft, prueft den Stil und nicht die
  Eigenschaft.** Die Feldpruefung war zeilenanfangs verankert und uebersah
  `pub struct T { pub w: Witness }` in einer Zeile.
* **Ein Abschalter fuer eine Sicherheitseigenschaft wird gesetzt und nie zurueckgenommen.**
  `SAMMELLAUF_BEHALTEN=0` ist durch eine Rotation ersetzt: das Verhalten bleibt stabil, ohne dass
  jemand die Eigenschaft ganz abschalten muss.
* **Ein Loch im Pruefer beschaedigt die GRUEN-Bilanz, nicht nur die rote.** „Leere Schlusszeile
  galt als Erfolg" hat kein Fehlerbild verloren -- es hat einen **Erfolg erfunden**: ein
  abgebrochener Lauf wurde gruen gebucht, und weil bei Erfolg geloescht wurde, war nicht
  nachzaehlbar, wie oft. Wer ein solches Loch findet, muss die Gruen-Zahlen nachrechnen, nicht nur
  die Ausfaelle beklagen.
* **Ein Sammler darf nur `$?` lesen.** Erfolg ueber Textvergleich der Schlusszeile heisst: mit
  jeder neuen Suite waechst eine Formel, und jede Formel ist ein Loch. Der Schluessel des
  Registers ist der Exit-Code; gibt eine Suite bei Fehlschlag 0 zurueck, ist DAS der Fehler --
  einer in der Suite. Der Text wird gegengelesen, nicht befragt.
* **Ein Zeuge braucht keinen Vorfahren.** „`pub(in path)` verlangt einen Vorfahren" stimmt und ist
  trotzdem kein Blocker: ein `pub struct Witness(())` im Modul der Engstelle ist ausserhalb
  nennbar, aber nicht herstellbar -- damit prueft rustc die Bindung Stelle<->Grund, und die
  Namenstabelle im Waechter entfaellt. Eine Zeile je Stelle. Als „Entwurfsarbeit" eingestuft zu
  haben war dieselbe Fehleinstufung wie `tail -1`.
* **Eine Kardinalzahl, wo eine Menge gemeint ist, ist eine Ratsche mit einem Loch.** Zweimal am
  2026-08-05 im selben Werkzeug: die Stelligkeitspruefung zaehlte Aufrufe („ein- oder zweimal")
  statt sie zu binden -- zwei Aufrufe aus dem FALSCHEN Paar sind so von Abbilden+Gegenstueck
  nicht zu unterscheiden. Und `IDENTITY_DEBTS` war ein `usize`: eine Ratsche ueber einer Zahl
  greift nur gegen Zuwachs, nicht gegen **Austausch** -- und Austausch fuehlt sich beim Umbauen
  wie Fortschritt an. Beides sind jetzt Mengen von Namen.
* **Ein Sammler, der Erfolge als Ausfaelle ablegt, macht sein Verzeichnis unlesbar -- und eine
  LEERE Schlusszeile ist kein Erfolg.** `tools/sammellauf.sh` hatte binnen einer Stunde beide
  Fehler: Erfolg als „ALL PASS in der Schlusszeile" (die gruenen Waechter schliessen anders), und
  ein Lauf ohne Schlusszeile (SIGKILL, Zeitlimit) galt als gruen -- **Schweigen als Erfolg**, im
  eigenen Werkzeug.
* **Ein waehlbarer Grund ist ein Warnschild, keine Struktur.** `Va::identity(reason, pa)` schloss
  die Liste der Gruende, aber nicht die Zuordnung Stelle<->Grund: der falsche Grund war weiterhin
  tippbar, und der Pruefer haette ihn durchgewinkt. Ein Konstruktor je Stelle nimmt das Argument
  weg, das man verwechseln kann.
* **Eine Messung, die bei der erwarteten Effektgroesse nicht trennen KANN, ist teure Wandzeit.**
  Der aarch64-Bisect (0/6 vorher gegen 1/18 nachher) gibt Fisher p ≈ 1 -- bei gleicher Rate
  liefert ein 6-Lauf-Vorher zu 71 % null Fehlschlaege. Vor dem Start rechnen, nicht danach.
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
* **Ein Thread, der lauffaehig ist, bevor er seine Autoritaet hat.** `spawn()` reihte ein,
  `bind_pd()` kam danach -- dazwischen eine Speicherbelegung, ein `CAPS.write()`, ein Tick. Wer in
  dieses Fenster faellt, macht seinen ersten Syscall mit LEEREM Cspace: nicht *eine* Cap ist
  unsichtbar, sondern jede. Das war **D0**, zehn Tage lang unter dem Namen „Haenger ab `sched`",
  und die Rate war 0,018 %. Es gibt keine Reihenfolge, die traegt -- `bind_pd` braucht die `tid`,
  die `spawn` erst liefert. Die Luecke laesst sich verkleinern, nicht schliessen. Seit 2026-08-07
  trennt der Scheduler deshalb `spawn_parked` von `admit`.
* **„Ich habe das noch nie gesehen" ist ein Nullbefund ohne Groesse — und rutscht deshalb noch
  leichter durch als eine Zahl.** Bei D15 stand „in KEINEM aarch64-Protokoll vor dem Umbau
  aufgetaucht" als Indiz im Eintrag. Die Protokolle waren eine Handvoll `RUNS=6`-Laeufe; bei
  0,15 % ist die erwartete Trefferzahl darin **0,02**. „Nie gesehen" war die wahrscheinlichste
  Beobachtung, ganz gleich ob der Fehler da war. Die Messung hat es dann widerlegt (3 in 2000).
  Derselbe Fehlschluss wie bei D0 am 2026-08-03 -- ein zweites Mal, in derselben Sitzung, in der
  er als Lehre aufgeschrieben wurde. Eine Aussage ueber Abwesenheit braucht eine
  Stichprobengroesse, auch wenn sie als Erinnerung daherkommt.
* **Eine Messung, deren wahrscheinlichstes Ergebnis „nichts" ist, belegt nichts.** Die 2300
  sauberen Laeufe vom 2026-08-03 lasen sich wie ein Freispruch; bei der wahren Rate war
  `0,99982²³⁰⁰ ≈ 66 %`. Die Zahl, die zu einer Nullmessung gehoert, ist nicht die
  Stichprobengroesse, sondern die **erwartete Trefferzahl** (0,41 damals, 9,0 bei 50 000).
* **Eine Sprechprobe gehoert an den GEPRUEFTEN PFAD, nicht an eine Ausnahme darin.** Der erste
  Entwurf des `pdbind`-Waechters fragte „hat die erklaerte Ausnahme gefeuert?" -- auf aarch64 gibt
  es die gar nicht (sie sitzt im x86-Checkpoint-Aufbau), die Zeile waere dort durchgefallen,
  obwohl alles stimmt. Und auf x86 haette sie angefangen durchzufallen, sobald jemand die Ausnahme
  BESEITIGT, also als Antwort auf eine Verbesserung. Gefragt ist „wurde ueberhaupt eine PD
  gebunden?".
* **Ein Waechter, der die GELEGENHEIT zaehlt, schlaegt einen, der den Treffer zaehlt** -- wenn die
  Trefferquote klein ist. Bei 0,018 % ist ein Melder, der nur beim Unglueck spricht, in 5555 von
  5556 Laeufen stumm. Die REIHENFOLGE dagegen ist in jedem Lauf pruefbar.
* **Eine Untergrenze, die einspringt, wenn die Messung nichts sieht, macht deren Ausfall
  unsichtbar.** Der D0-Regler mass den RSS der Subshell statt QEMUs Prozessbaum; der Wert fiel
  unter die 128-MiB-Untergrenze, und im Protokoll stand „je Lauf rund 192 MiB" -- exakt
  `128 * 3/2`. Das sah wie ein Messwert aus und war die Untergrenze. Jetzt: Baum-Summe, und statt
  der Untergrenze eine Sprechprobe mit **Abbruch**.
* **Die naheliegende Fassung einer Behebung kann den Fehler mitnehmen.** Bei der D0-Umstellung
  bekam die `page_probe`-Sonde mit `admit_in_pd` ihre drei Mappings NACH der Zulassung -- sie waere
  losgelaufen, bevor die Seiten standen, und haette auf P statt auf P+8KiB gefaultet. Gruene Zeile,
  anderer Test. (Dieselbe Form wie D8: dort machte der offensichtliche Waechter den Thread
  vollstaendig verhungern.)
* **Ein Pruefer, der die falsche GROESSE liest, kann den Fehler, gegen den er gebaut ist,
  strukturell nicht sehen.** Die `park`-Zeile sollte belegen, dass `unpark` einen IPC-Wartenden
  NICHT weckt (der D9-Fehler). Sie las dafuer `is_parked` vorher und nachher -- an einem
  IPC-Wartenden ist dieses Bit aber in **beiden** Faellen falsch, ob geweckt wird oder nicht. Die
  Zeile stand auf ALL PASS und haette bei eingebautem Fehler weiter darauf gestanden. Gefunden hat
  es die Gegenprobe, nicht das Gegenlesen. Gemessen wird jetzt `blocked` -- die Groesse, die sich
  tatsaechlich aendert.
* **Eine Mutation, die zwei Dinge zugleich kaputtmacht, beweist nichts ueber das gemeinte.** Die
  erste D9-Gegenprobe ersetzte `if self.tcbs[s].parked` durch `if true` -- damit wurde die
  Weckmarke im selben Zug **immer sofort geloescht**. Die Suite fiel durch, aber an der ERSTEN
  Aussage (`marke-wirkt=false`), nicht an der gemeinten. Die saubere Fassung verschiebt nur den
  `unblock`-Aufruf aus dem `if` heraus; dann faellt genau ein Feld (`ipc-bleibt-liegen=false`) und
  alle uebrigen bleiben gruen. Eine Gegenprobe muss **isolieren**, sonst misst sie die Reihenfolge
  der Pruefungen und nicht die Eigenschaft.
* **Ring-3-Code gehoert in `.user_text`, und die Fehlermeldung dafuer sieht aus wie ein
  Kernelfehler.** Eine neue Ring-3-Sonde ohne `#[link_section = ".user_text"]` landet in `.text`,
  ist aus Ring 3 nicht ausfuehrbar und faultet **an ihrer eigenen Einsprungadresse**. Im Log stand
  nur „User-Thread 0x6 faultete (FAR=0x134930)" und eine Pruefzeile mit lauter `false` -- was wie
  ein kaputter Mechanismus aussieht und eine fehlende Zeile ist. Entschieden hat `nm`: die Adresse
  des Symbols war die Fault-Adresse. Ein mitgerufener Helfer muss `#[inline(always)]` sein, sonst
  liegt ER in `.text` und der Fehler wandert nur eine Ebene tiefer.
* **`wrapping_sub` auf einer Zeitdifferenz ist die teuerste bequeme Zeile.** Ein Zähler, der um
  100 Zyklen zurückspringt, ergäbe rund `2^64` — ein Konto, das so belastet wird, ist sofort und
  dauerhaft erschöpft. Rückwärts heisst **verworfen**, nicht „fast einmal herum".
* **`echo "$X" | grep -q MUSTER` unter `set -o pipefail` meldet „nicht gefunden", sobald die
  Ausgabe den Pipe-Puffer überschreitet.** `grep -q` steigt beim **ersten Treffer** aus, `echo`
  bekommt SIGPIPE, `pipefail` reicht rc=141 durch. Gemessen kippt es zwischen **66 und 70 KiB** —
  damit hing das Urteil dreier QEMU-Suiten an der **Größe ihrer eigenen Ausgabe**, und ein
  sechster Archiveintrag hat es über die Kante geschoben: **neun Prüfungen meldeten FAIL für
  Zeilen, die im Protokoll standen**. Das ist die schlimmere Richtung von „erfundene Erfolge":
  erfundene **Misserfolge** kosten kein Fehlerbild, sie **ertränken** es. Abhilfe:
  `grep -q MUSTER <<<"$X"` (keine Pipeline), und eine Sprechprobe des Prüfers an einer bewusst
  **großen** Eingabe — an einer kleinen wäre sie während des ganzen Fehlers grün gewesen.
  `bash -n` fängt das nicht: die kaputte Zeile ist syntaktisch gültig.
* **Eine Ablage je ROLLE ist eine Ablage zu wenig, sobald es zwei Instanzen der Rolle gibt.**
  „Drei Rollen, drei Badges, drei Ablagen" behob A-6.3 — und `Client` ist eine **Rolle**. Mit der
  zweiten Client-PD überschrieb sie die Ablage der ersten, ein Prüfablauf wartete auf ein Badge
  an einem fremden Objekt, und drei Prüfzeilen fielen aus, **ohne dass die geprüfte Sache kaputt
  war**. Der Kommentar an der Stelle beschrieb den Fehler bereits wörtlich und verhinderte ihn
  nicht: die Behebung lag eine Ebene zu flach. Verschlüsselt wird mit der `program_id`, nicht mit
  der Rolle — dieselbe Lösung wie bei den vier versteckten Politiken aus A-5.4.
* **Ein Wecker, der seinen Grund nicht NENNT, weckt fremde Entscheidungen mit auf.** `blocked` war
  ein Bit für vier Lagen (IPC, Budget, Pause, Park); jede Abspaltung reparierte die letzte
  Kollision und **stellte die nächste auf**. Seit der Grund-Menge (Z24) entfernt jeder Wecker
  genau seinen Grund, und eingereiht wird **nur bei leerer Menge** — ohne diesen zweiten Halbsatz
  ist die Menge bloss eine andere Schreibweise für dieselben Bits. Der Umbau deckte vier Stellen
  auf, die vom Durcheinander lebten: `RESUME` rief `unblock`, `thaw_thread` ebenso (obwohl
  `freeze` über `pause` einfriert), der IPC-**Fastpath** liess ein pausiertes Ziel unmittelbar
  laufen, und ein Cross-Core-Test weckte einen *geparkten* Thread mit dem Wecker für IPC.
  Merkmal der Klasse: Sie fallen nicht beim Gegenlesen auf, sondern in der Suite der Architektur,
  auf der der Zustand oft vorkommt.
* **Wer zwei Grössen zugleich ändert, darf nicht der bequemeren die Schuld geben.** Eine
  zusätzliche `println!`-Zeile kippte den Z4f-Checkpoint-Test — reproduzierbar, beim Zurücknehmen
  weg. Ich schrieb es der **Ausgabelänge** zu (also Wanduhrzeit, passend zu D13) und trug es so
  ins Register. Falsch: eine Zeile ändert **Ausgabelänge UND Binary**, und die Prüfung las
  `d[16] != 0`, also das **erste Byte des Kernel-Hashes** — die Zeile testete einen Hashwert.
  Falsch-Alarm bei 1 von 256 Bauten, blind bei **255 von 256** echten Überschreibungen. Die
  Zuordnung „reproduziert je Binary" hätte die Zeitthese sofort ausgeschlossen; ich hatte die
  passende Vorgeschichte und habe die unpassende Beobachtung nicht dagegen gehalten. **Zwei
  gleichzeitig veränderte Grössen sind zwei Hypothesen, nicht eine.**
* **Ein Prüfer, der ein einzelnes BYTE liest, prüft ein Byte — nicht die Eigenschaft.** Verglichen
  werden jetzt die 512 Byte selbst (SHA-256 vor und nach dem Boot): Erkennung **1 von 256** auf
  **256 von 256**. Und die Bilanzfolge gehört dazu: jedes bisherige PASS dieser Zeile war eine
  Aussage über ein Hash-Byte — ob der Kernel den Sektor je überschrieben hat, ist für die
  Vergangenheit **unbeantwortet**, nicht „nein".
* **WER MISST, SETZT DIE MARKE — der gemessene Pfad darf sie weder setzen noch löschen.** Das ist
  die gemeinsame Wurzel zweier Fehler desselben Tages: die vergiftete Mangel-Marke war **tot**,
  weil jeder `spawn_*`-Pfad sie in seiner ersten Anweisung löschte (zwischen Vergiften und erster
  Anforderung) — „der Pfad hat geschwiegen" war strukturell unerreichbar, während die Zeile es
  wörtlich versprach; und die `park`-Zeile las eine Grösse, die der gemessene Pfad selbst schrieb.
  Beide standen auf ALL PASS. **Die strukturelle Fassung ist ein Zähler, der nur wächst**
  (`MANGEL_GEN`): der Messende liest vorher und nachher, die Differenz ist die Aussage, und keine
  Refaktorierung des Pfades kann sie entwerten. Ein Rücksetzen im gemessenen Pfad ist immer ein
  Verdacht.
* **Eine Zahl, die ein Mensch parallel zur Wahrheit führt, ist dieselbe Klasse wie ein Prüfer, der
  seine Grösse nachrechnet — auch mit Ratsche.** `MELDESTELLEN` wurde von Hand gepflegt und von
  einem Wächter gehalten; die Ratsche hat einen Merge-Fehler gefangen (31 gegen 32, weil zwei
  Zweige unabhängig zählten) und war trotzdem **zwei Gedächtnisse für eine Tatsache**. Seit
  2026-08-11 leitet `kernel/build.rs` die Zahl aus **einem** Zähler ab; nach einem Merge kann sie
  nicht mehr falsch sein. Der Wächter wacht seither über die **Ableitung** — dass sie verdrahtet
  bleibt —, nicht über eine zweite Zahl. Fail-closed: lässt sich nicht zählen, bricht der Bau ab.
* **Cargo mischt `.cargo/config.toml` aus JEDEM Vorfahrenverzeichnis — und HÄNGT Arrays an.**
  Ein Arbeitsbaum unterhalb eines anderen Checkouts (`<repo>/.claude/worktrees/<id>`) erbt die
  Konfiguration ein zweites Mal; `-Tkernel/x86_64-link.ld` steht dann **zweimal** auf der
  Linkerzeile, `lld` wertet `SECTIONS` **zweimal** aus, und **alle** Linkersymbole tragen die
  Werte des zweiten Durchlaufs (`__text_start = 0x100000`). Der Bau läuft durch, das Abbild
  bootet nie. Das hat einen halben Tag und drei Behebungsversuche am **Linkerskript** gekostet —
  einem Symptom. Der Hauptbaum war nur zufällig immun (dort gibt es eine Konfiguration). Seit
  2026-08-10 steht das Skript in `kernel/build.rs` (`rustc-link-arg`, je Crate **genau einmal**,
  absoluter Pfad) und ein Wächter bricht bei einem zweiten `-T` ab. **Ein Entdoppler wäre die
  falsche Antwort gewesen:** er müsste raten, welche Wiederholung Absicht ist — das Wegwerfen
  eines legitim doppelten Flags ist eine stille Semantikänderung.
* **„`cargo build` läuft durch" ist kein Beleg, solange niemand die KONFIGURATION bindet.** Der
  Binärfingerabdruck schliesst „veralteter Build" für einen *Suitenlauf* aus; für die
  *Bauumgebung* stand dieselbe Tür offen. Jede Bauzeile, die als Beleg dient, druckt ihre
  effektiven Flags — und der Kernel trägt ihren Fingerabdruck (`CAPROCK_FLAGS_FP`) im Abbild.
  Eine Suite, die ihre Bauausgabe nach `/dev/null` schickt, ist dabei der eigentliche Riss.
* **Null ist ein Befund, kein Messwert.** Ein einseitiger Schwellenvergleich (`x < Schranke`) ist
  **grün, sobald die Messung ausfällt**. `NOSEL_TEXT` stand auf 0, weil der Bau kaputt war, und
  die F1-Zeile meldete `PASS` für „0 < 0x62000". Dieselbe Form wie ein nie gesetztes Bit, das als
  „kein Fehler" gelesen wird. Jede gemessene Grösse mit nur **einer** Schranke braucht eine
  Plausibilitätsuntergrenze — oder Null muss ausdrücklich als „nicht gemessen" ausscheiden.
* **Eine Seite, die NICHTS tut, kann eine Zusicherung unerfüllbar machen — wenn sie an einer
  Größenrelation liegt.** Die Wachseite (`USER_KSTACK_ALLOC = USER_KSTACK_SIZE + PAGE`, seit
  2026-08-10) hat auf aarch64 **jede gefärbte Kernel-Stack-Anforderung strukturell unmöglich**
  gemacht: 16 Farben, 4 Partitionen → ein Streifen ist **4** Farben breit, ein EL0-Kernel-Stack
  ist dort 16 KiB = **4** Seiten. Er passte *genau* — die Eigenschaft folgte aus
  `Stackseiten == Streifenbreite`, nicht aus der Struktur. Mit der Wache sind es **5**
  aufeinanderfolgende Seiten, also fünf **verschiedene** Farben; die passen in einen
  4-Farben-Streifen an keiner Adresse und bei keinem Füllstand. `alloc_colored` gab immer `None`,
  `spawn_isolated_colored` ebenso, der A1-Farbtest meldete lauter Nullen — **zehn Tage lang**
  (C9e). Die Pointe: auf aarch64 gibt es die Wache **gar nicht**
  (`hal::mmu::guard_unterstuetzt() == false`, `guard_unmap` ist ein Zähler). Eine Seite, die auf
  dieser Architektur nachweislich nichts bewirkt, hat den Farbtest der Architektur gekippt.
  Dieselbe Klasse wie „unten zuerst war ein Zufall der Größenrelation". Seither steht die Wache
  unter **keiner** Farbbedingung (`alloc_colored_vorspann_in`) — sie trägt keine Daten und belegt
  kein Cache-Set, die Zusicherung sagt über sie nichts aus.
* **Ein `None` aus einem Allokator heisst „kein Platz" — auch wenn es „geht prinzipiell nicht"
  heisst.** Genau daran hing die C9e-Suche: das Fehlerbild sah nach Speichermangel unter Last aus
  und war eine Unmöglichkeit, die bei leerem RAM genauso eintritt. Eine Anforderung, die
  **breiter als der Streifen** ist, ist kein Mangel; sie braucht einen eigenen Grund.
* **Ein Fehlschlag VOR dem `if let` färbt genau die Felder null, die dahinter entstehen — und
  das ist die Diagnose.** In der `color`-Zeile stand `uebergross_abgewiesen=1` inmitten lauter
  Nullen. Diese eine Eins hat die Frage entschieden: der Test war *sprechfähig* und lief bis zum
  Spawn; alles danach fand nicht statt. Wer eine Zeile aus lauter Nullen sieht, muss zuerst
  fragen, welche Felder **vor** und welche **nach** dem Abbruchpunkt gesetzt werden.
* **Dieselbe Klasse zweimal an einem Tag: arch-neutraler Kernelcode ruft eine HAL-Funktion, die
  es nur auf x86 gibt.** Am 2026-08-12 um 00:48 wurde `hal::mmu::guard_*` behoben und die
  Abnahme um `aarch64-bau` erweitert — „die Abnahme fängt die Klasse jetzt". Um 10:24 landete
  `irq_tiefe`/`irq_tiefe_melden` (C4, zweiter Summand der Stackrechnung) wieder **nur** in der
  x86-HAL, gerufen aus `kernel/src/kstackmark.rs` und `kernel/src/system.rs`: drei `E0425`, der
  aarch64-Bau kaputt. **Ein Gatter, das existiert und nicht gefahren wird, ist keins** — die
  Ratsche greift erst, wenn sie zwischen Commit und Merge liegt. Und die naheliegende Behebung
  (eine zweite Kopie in der aarch64-HAL) wäre die falsche gewesen: der Inhalt sind **zwei
  Atomics**, also arch-neutral. Er steht jetzt einmal in `caprock_hal::irqtiefe`, beide
  `exception`-Module reichen ihn weiter.
* **Ein Kriterium, das die geprüfte Sache nicht erreichen KANN, ist kein strenges Kriterium,
  sondern gar keins.** Die FP-Sonde musste „alle 64 Abgaben überstehen"; erreichbar waren 3, weil
  sie je Rundlauf-Runde eine Iteration vorankommt und eine Runde durch den **Tick** begrenzt ist,
  nicht durch das `YIELD`. Grün war unmöglich, also sagte rot nichts — die Zeile hatte keine
  Trennschärfe, und ihre Begründung im Bericht war zusätzlich seit A4 **überholt**. Gefragt ist
  die Größe, die sich wirklich ändert (Fortschritt, Korruptionsmeldung, **eigene**
  Verdrängungszahl), nicht die bequem formulierbare.
* **Eine Skalierungskurve ohne Lebendigkeitsbeweis zählt Leichen.** Die isolierte Reihe der
  Kapazitätskurve meldete 224/1504/3040 „isolierte Prozesse" — tatsächlich lag `kurven_arbeiter`
  in `.text`, jeder EL0-Thread faultete sofort an seiner eigenen Einsprungadresse (**228
  `el0-trap`-Zeilen bei 224 PDs**, am Ende **0 belegte VSpaces**). Gemessen war die
  Geschwindigkeit des Sterbens. Die Bedingung dahinter gilt weiter und ist allgemeiner als der
  Einsprungfehler: **jede gezählte Einheit muss eine Arbeit nachweisen, die nur ein LAUFENDER
  Träger leisten kann.** „Angelegt" ist keine solche Arbeit — `spawn` gibt `Some`, auch wenn der
  Thread nie eine Instruktion ausführt, und eine Kurve, die Rückgabewerte zählt, steigt dann
  genauso schön. Die berichtigte Reihe (220/1477) hängt an **belegten VSpaces** und **gehaltenen
  Seitentabellen-Rahmen**: Größen, die zurückgehen, sobald die Träger sterben. Dieselbe
  Unterscheidung wie `rx_used` gegen „Daten sind angekommen".
* **Eine Marke, die der geprüfte Pfad in seiner ersten Zeile löscht, ist keine Marke.**
  `MANGEL_VERGIFTET = 255` sollte „der Pfad hat geschwiegen" von „es lag an keiner Ressource"
  unterscheidbar machen — aber jeder `spawn_*`-Pfad ruft `mangel_zuruecksetzen()` als **erste**
  Anweisung, also zwischen dem Vergiften und der ersten Anforderung. Der Ausgang, den die
  Berichtszeile wörtlich versprach („steht er noch da, hat der Pfad GESCHWIEGEN"), war damit
  strukturell unerreichbar. Gemessen mit einer stumm gemachten Meldestelle: mit der Behebung
  meldet der Sweep `geschwiegen=5`, ohne sie `keiner=5` — und `keiner` heißt „lag an keiner
  Ressource", also genau das, wovon die Marke trennen sollte. **Wer eine Marke setzt, muss den
  Weg prüfen, den sie überleben soll** — sonst prüft der Prüfer eine Eigenschaft, die vor seiner
  ersten Messung schon weg ist. Dieselbe Form wie die leere Event-Queue ohne `CD.R`.
* **Ein Prädikat, das die geprüfte Größe nur STELLVERTRETEND liest, prüft den Stellvertreter.**
  Die Z4f-Nachprüfung „der abgewiesene Checkpoint blieb unverändert" las
  `d[0:8] == b'SL4KCKPT' and d[16] != 0x00` — und `d[16]` ist das **erste Byte des
  Kernel-Hashes** (`OFF_BODY = 16`), also genau das Byte, das der Negativfall zuvor mit `^= 0xFF`
  kippt. Damit hing das Urteil am SHA-256 des gerade gebauten Kernels: **Falsch-Alarm bei 1 von
  256 Bauten** (`hash[0] == 0xFF` → gekippt `0x00`, „überschrieben" ohne dass etwas geschrieben
  wurde) und **blinder Fleck bei 255 von 256** echten Überschreibungen. Das war D14: „eine
  `println!`-Zeile mehr kippt die Prüfung, nicht ihr Inhalt, ihre blosse Existenz" — eine
  Berichtszeile ändert **zwei** Größen, die Ausgabelänge und das Binary; nur die zweite ist
  deterministisch, und die Zeitthese las die falsche. Dieselbe Klasse wie `rx_used` gegen „Daten
  sind angekommen". Verglichen werden jetzt die **512 Byte selbst**, vor und nach dem Boot.
  Und: eine solche Zeile beschädigt die **Grün**-Bilanz — jedes ihrer PASS war eine Aussage über
  ein Hash-Byte, nicht über den Sektor.
* **Ein abgeleiteter Nenner neben handgeführten Summanden ist eine halbe Ableitung.**
  `system::MELDESTELLEN` kommt seit dem 2026-08-11 aus einem Zähler — die Aufteilung der
  C7-Abdeckung („11 provoziert + 9 Platz + 10 Ladepfad + 1") stand daneben als **Prosa**. Als die
  Guard-Page eine 32. Meldestelle mitbrachte, ging der Nenner mit und die Summanden nicht: 31
  gegen 32, im Bericht, unbemerkt. Prosa hat kein Gatter. Die Summanden stehen jetzt als
  Konstanten mit `const _: () = assert!(… == MELDESTELLEN)` — wer eine Meldestelle hinzufügt,
  bricht den Bau, bis er sie eingeordnet hat.
* **Der Guard eines `while let`-Scrutinees lebt ueber den RUMPF — und `SpinLock::lock` maskiert
  IRQs.** `while let Some(a) = SCHLANGE.lock().entnehmen() { laden(&a) }` sieht aus wie „entnehmen,
  freigeben, arbeiten" und ist „arbeiten unter der Sperre, mit gesperrten Interrupts". Im
  C8-Verifizierer haette das die gesamte Ed25519-/SHA-2-Pruefung mit maskierten IRQs gefahren —
  **also genau die Fassung „Praemption fuer die Dauer aus", die C8 ausdruecklich verworfen hat**,
  hereingeholt durch eine Temporaries-Lebensdauer statt durch eine Entscheidung. Gemessen mit einem
  `Drop`-Zeugen, nicht erschlossen: im `while let` faellt der Guard NACH dem Rumpf, in
  `let v = { l.lock().entnehmen() };` davor. Dieselbe Wurzel wie der `match lock() { .. }`-
  Selbst-Deadlock weiter oben, aber mit einem anderen Schaden: kein Haenger, sondern ein
  Latenzloch, das keine Pruefzeile ansieht. Abhilfe ist die **Funktionsgrenze**
  (`fn naechster() -> Option<Auftrag>`), nicht ein Kommentar.
* **Eine Statusmarke im obersten Bit kollidiert mit dem obersten ZAHLENFELD.** Das Ergebniswort der
  C8-Sonde packt Aussagen in die unteren und Zahlen in die oberen Bits; „gemessen" stand auf Bit 63,
  und dort liegt schon `verloren` (Bits 56..64). Beim Auspacken las sich die Marke als
  `verloren = 128`, das Urteil fiel durch, der Lauf ging in den Watchdog — **waehrend jedes einzelne
  Feld der Zeile darueber gruen war**. Das Bild ist von einem echten Befund nicht zu unterscheiden,
  und die Ursache ist eine Bitmaske. Wer Zahlen und Marken in ein Wort packt, muss die Belegung
  ausschreiben, nicht abzaehlen.
* **Wer einen tiefen Pfad VERSCHIEBT, verschiebt auch alles, was an „wo laufe ich" haengt.**
  `load_into_pd_mit` legt einen Thread ohne Manifest-Affinitaet auf `hal::cpu::core_id()`. Solange
  der Ladepfad im Aufrufer lief, war das der Kern des Aufrufers; mit dem Verifiziererthread waere es
  seiner geworden — jedes ohne Affinitaet geladene Programm haette den Kern gewechselt, als
  **Nebenwirkung einer Stack-Verschiebung**, ohne dass eine Zeile davon spricht. Der Auftrag traegt
  den Heimatkern deshalb mit. Dieselbe Klasse wie „ein Parameter, der zwei Bedeutungen traegt": eine
  Umgebungsgroesse wird zum Argument, sobald die Umgebung wechselt.
* **Ein neuer Abweispfad erbt die Aufraeumpflicht des alten NICHT.** Der Dispatch leitet fuer
  `SYS_LOAD` Endowment-Caps ab; auf dem angenommenen Weg raeumt `load_by_index` sie bei Misserfolg
  auf. Die zwei neuen Ausgaenge (`Ausgelastet`, `KeinVerifizierer`) kommen dort gar nicht mehr hin —
  ohne eigene Loeschung blieben sie als verwaiste CDT-Kinder liegen und blockierten sogar das
  `delete` des Eltern-Caps. Wer eine Funktion in „uebergeben" und „ausfuehren" zerlegt, muss jede
  Aufraeumzusage der alten Funktion einzeln neu unterbringen.
* **Ein Negativtest kann eine Eigenschaft absichern, die NIEMAND BENUTZT.** Die ganze
  Sidecar-Arithmetik von Z26/A3 (`slot_offset`, `slot_gueltig`, `slots_in`, `fenster_deckt`,
  `FRAME_MAX_BYTES`) hatte drei Tage lang im **ganzen Baum** keinen Aufrufer ausserhalb ihres
  eigenen Testmoduls — und zwei Mutationen in `tools/redirect-negativ.sh` (M5: Off-by-one an der
  Slot-Schranke, M6: Fenster nur am Slot-Anfang geprueft) belegten brav, dass die **Funktionen**
  richtig sind. Dass sie **gerufen** werden, belegte nichts. Das ist „eine gruene Zeile, die nichts
  gattert" eine Ebene tiefer, und es faellt schwerer auf: ein Negativtest sieht aus wie der
  strengste Pruefer im Haus. Dabei lag daneben eine echte Luecke: die Maskenbreite (64 Slots) und
  die Fensterlaenge in der Cap waren **zwei unabhaengige Zahlen** — eine 4-KiB-Cap haette 64 Slots
  vergeben und der Kernel bis 32 KiB geschrieben. `grep` nach den Aufrufern ist billiger als jede
  Mutation.
* **Lesen und Schreiben sind NICHT dieselbe Autoritaet — auch wenn dieselbe Region beides traegt.**
  Die Autoritaetstabelle von Z26/A3 fuehrte eine Zeile „Frame lesen **und** schreiben — ja, ganzer
  Frame". Der ganze Frame enthaelt aber `cs`/`ss` (x86) bzw. `spsr` (aarch64), also den **Ring**:
  ein Handler, der sie zurueckschreiben duerfte, befoerderte seinen Gast — die Rechteausweitung,
  gegen die die Weiche steht, nur von der anderen Seite. Gelesen wird deshalb der ganze Frame,
  zurueckgeschrieben nur die Allzweckregister. Wo eine Tabelle zwei Richtungen in einer Zeile
  fuehrt, steht dort vermutlich die schwaechere Haelfte einer staerkeren Vergabe.
* **Zwei Suiten, die denselben SCHLUESSEL verschieden aufsetzen** — dieselbe Rissform wie bei
  `iommu_platform=on`, nur teurer. `test-qemu-x86-load.sh` prueft seit dem 2026-08-01 die
  **Uebereinstimmung** von privatem und einkompiliertem oeffentlichem TrustedSAS-Schluessel
  (`tools/check_trusted_key.py`), mit einer ausfuehrlichen Begruendung im Skript;
  `test-qemu.sh` (aarch64) prueft bis 2026-08-13 nur die **Existenz** der privaten Datei. Ein
  `git checkout kernel/src/trusted_keys.rs` — also genau das Aufraeumen, das `AGENTS.md` am
  Sitzungsende verlangt — laesst die aarch64-Suite danach mit
  `root : FAILURES (Rejected(Unverified))` plus drei Folgezeilen scheitern. Das sieht wie ein
  Kernelbefund aus und ist ein Aufbauproblem. **Wer eine Pruefung in einer Suite verschaerft, muss
  die andere mitnehmen** — sonst faellt der Fall genau dort an, wo niemand ihn erwartet.
* **Ein Beweiswerkzeug sammelt in seiner Sitzungsablage AUF — eine Bilanz gehoert damit nicht
  automatisch zu den Quellen, die sie zu beschreiben behauptet.** `gnatprove` schreibt seine
  Ergebnisse je Objektverzeichnis fort. Beim Aufteilen von `spark/caprock.gpr` in zwei Projekte
  (Cap-Space und Scheduler) blieben die alten Ergebnisse stehen, und `gnatprove.out` meldete fuer
  S1 **232 Laufzeitpruefungen statt 99** — die Summe beider Module. Diesmal fiel es auf, weil die
  Ratsche riss (71 gegen 15 erwartete). **Die andere Richtung ist die gefaehrliche:** faellt eine
  Quelldatei weg, meldet dieselbe Mechanik still MEHR bewiesen, als gerechnet wurde, und die
  Ratsche schweigt. Seit 2026-08-13 leert `tools/spark-beweis.sh` das Objektverzeichnis je Lauf
  **und** prueft, dass die Bilanz genau EINE Einheit nennt, und zwar die erwartete. Dieselbe Form
  wie „zwei Suiten, die dasselbe Geraet verschieden aufsetzen": das Urteil hing an einem Zustand,
  der nicht im Diff steht.
* **Wer nach ERREICHBARKEIT priorisiert statt nach VORGESCHICHTE, hat das Werkzeug am Ende genau
  dort nicht, wo der letzte Fall lag.** Die zehn ungemessenen Mangel-Meldestellen des Ladepfads
  standen ein Jahr mit der Begründung „braucht ein Boot-Archiv, das die Hauptsuite bauartbedingt
  nicht hat" — während die Lade-Suite das Archiv hatte. Der Ladepfad ist derjenige, auf dem
  `NoResources` sechs Wochen lang stumm war und an dem `wasmhost` gestorben ist.

## Aufbau, grob

| Ort | Inhalt |
|---|---|
| `kernel/src/system.rs` | Kern der Faehigkeitsverwaltung, IOVA-Fenster, Teardown-Token, Audit |
| `kernel/src/addr.rs` | `Pa`, `Iova`, `DmaRegion` |
| `kernel/src/dmatests.rs` | architekturneutrale DMA-Tests, von beiden Hochlaufwegen gefahren |
| `kernel/src/arch/x86_64/bootinfo.rs` | `HandoverInfo` — eine Struktur, zwei Herkuenfte |
| `kernel/src/arch/x86_64/dmar_selftest.rs` | synthetisches DMAR fuer den Selbsttest |
| `crates/caprock-hal/` | `vtd`, `dmar`, `intc`, `timer`, `fault`, `iommu`-Fassade |
| `crates/caprock-cap/src/space.rs` | `Finalized`, CDT, `delete_leaf` |
| `crates/caprock-cap/src/checkpoint.rs` | Z4: die Verweigerungsregel (`classify`) **und** das Checkpoint-Format (`Image`, CRC-32). Abhaengigkeitsfrei, ohne `unsafe`, host-getestet — ein Checkpoint ist Eingabe, kein Zustand |
| `crates/caprock-virtio/` | virtio: `Transport` + `Queue`, darauf `rng`/`blk`/`net`, plus `probe_ecam`. **Ohne jede Abhaengigkeit** — wird von der Treiber-PD gelinkt (A-5.1) |
| `programs/hardware/virtio-blk/` | **der erste Treiber ausserhalb des Kerns** (A-5.1): loest sein Geraet selbst auf, bedient Anfragen ueber seinen Kanal, austauschbar im Betrieb; seit A-6 auch Blockdienst + GPT-Scan |
| `crates/caprock-part/` | GPT-Parser (A-6.2). Abhaengigkeitsfrei, `forbid(unsafe_code)`, host-getestet — fremde Plattenbytes gehoeren nicht in den Kern |
| `crates/caprock-fat/` | FAT16-Parser (A-6.3), ebenso |
| `programs/trusted/fs/` | **Dateisystem-PD** (A-6.3): faehrt kein Geraet, ruft den Blockdienst |
| `tools/mkgpt.py` | baut die GPT-Testabbilder, auch **kaputte** (`--break`) — beide Suiten benutzen dasselbe Werkzeug |
| `kernel/src/colors.rs` | Farbzuteilung, `run_color`, Prime+Probe (B-4.5) — arch-neutral |
| `crates/caprock-sched/src/cycles.rs` | Zyklenabrechnung (B-5.1) — **ohne jede Abhaengigkeit**, damit die Fallen mit Literalen statt mit einer Maschine ausloesbar sind |
| `tools/kernel-grenze.sh` | prueft, dass keine Treiber in die HAL wandern; mit Selbsttest |
| `tools/eingeschlossenheit.py` | **die Eintrittskarte fuer ein Handler-Modul (Z28)**: wer `[package.metadata.caprock] einschluss = "streng"` traegt, hat `forbid(unsafe_code)`, kein Bauskript und nur benannte Abhaengigkeiten. Zwei Ratschen als Mengen von Namen, 15 Sprechproben, **0 Kandidaten = Rueckgabecode 3** |
| `tools/spark-beweis.sh` | **das SPARK-Experiment (Zweig `spark-experiment`)**: GNATprove ueber Portierungen von `caprock-cap` (S1) und `caprock-sched` (S2). Zwei **getrennte** Ratschen, drei Gegenproben, uebersprungene Ruempfe als **Menge von Namen**. Bericht: `spark/README.md`, Befunde am Rust-Code: `todo.md` S2 |
| `tools/host-tests.sh` | die Host-Tests der reinen Crates an **einem** Ort (`caprock-cap` lief vorher nirgends) |
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
