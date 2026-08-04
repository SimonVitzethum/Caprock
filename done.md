# SEL4Lake — erledigte Punkte

Ausgelagert aus [todo.md](todo.md), damit dort nur steht, was noch zu tun ist.

Die Begründungen bleiben erhalten, und das ist der eigentliche Zweck dieser Datei: bei mehreren
Einträgen ist das Wertvolle nicht, **dass** etwas gelöst wurde, sondern **welche Annahme sich
dabei als falsch erwiesen hat**. Wer später dieselbe Abkürzung erwägt, findet hier, warum sie
schon einmal nicht getragen hat.

---

## D11 — der Ueberlauf einer Endpoint-Warteschlange ist benannt (2026-08-04)

**Der Fehler.** `TidQueue::enqueue` war ein `if self.count < QCAP { .. }` **ohne `else`**. Bei
`QUEUE_CAP = 32` landeten von 33 CALLs 32 in der Queue. Der 33.: `block_current` lief trotzdem,
der Sentinel `0xDEADBEEF` blieb unberuehrt im Frame (kein Ergebniscode), er stand in **keiner**
Struktur des Endpoints, 33 nachfolgende RECVs bedienten 32 — und `quiescence_of(..).is_quiescent()`
meldete ihn als **ruhig**, `audit` `(false,false)`, `purge_thread` `false`. Ein Faden haengt
dauerhaft, und JEDER Pruefer meldet Ordnung. Schlimmer als die leere Ereigniswarteschlange ohne
`CD.R`, weil die Ruhemeldung zusaetzlich einen Hot-Reload (A-4.2) freigegeben haette.

Dieselbe Zeile traf vier Wege: den 33. RECV ebenso; `bind_receiver` meldete `true`, obwohl
verworfen; `migrate_owner` meldete `true`, **loeschte die Antwortpflicht** und verlor den Aufrufer
— ein Client, der auf eine Antwort wartet, die niemand mehr schuldet.

**Eine fuenfte Fundstelle, die der Befund nicht nannte.** `Notification::wait` hat dieselbe Form
bei Kapazitaet 1: ein zweiter `WAIT` **ueberschrieb** `waiter`. Der Ueberschriebene blieb
blockiert und war danach fuer `purge_thread`/`audit`/`quiescence_of` unsichtbar. Dass die
Kapazitaet hier 1 statt 32 ist, aendert an der Struktur des Fehlers nichts.

**Die Behebung.** `enqueue -> bool`, `#[must_use]`, und **jede** Aufrufstelle wertet aus. Die
beiden, an denen `false` strukturell unmoeglich ist (`TidQueue::remove`, `rebind_server`), sagen
dort **warum** — nicht „das wird schon". `call`/`recv` weisen mit `ERR_EP_FULL` ab und blockieren
**nicht**; `bind_receiver` meldet Misserfolg; `migrate_owner` prueft `is_full()` **vor** dem
`take()` und ist im vollen Fall ein reines No-Op — fail-closed heisst hier: die Antwortpflicht
bleibt beim alten Besitzer, und der Aufrufer laeuft ueber den vorhandenen Weg (`owner_died` ->
`ERR_SERVER_GONE`) auf. Eine begonnene Transaktion, die ehrlich scheitert, statt einer, die
lautlos verschwindet.

**Warum ein DRITTER Code (`ERR_EP_FULL = 9`) und nicht `ERR_QUIESCING`.** Das war die Frage, die
der Befund offenliess. Die drei Lagen verlangen vom Client verschiedene Antworten: `ERR_BADCAP`
heisst „gibt es nicht" (nie wieder versuchen), `ERR_QUIESCING` „kommt gleich wieder" (die
Wartezeit ist durch den Austausch begrenzt und haengt nicht an fremdem Verhalten), `ERR_EP_FULL`
„gerade kein Platz" — eine **Lastaussage**: sie haengt an den anderen 32 Wartenden, kann sofort
wieder gelten, und wer stumpf wiederholt, verschaerft sie. Die beiden zusammenzuwerfen naehme dem
Client genau die Unterscheidung, die A-4.2 gerade eingefuehrt hat.

**Das Modell ist dem Code gefolgt, nicht umgekehrt.** `Verification/ipc/proofs/endpoint.rs`:
`dropped_*` -> `rejected_*` (ein Verlust ist unbeobachtbar, eine Abweisung ist eine Antwort),
neue `send_gate`/`recv_gate` mit Code 3, `core_eq` fuer „die Operation hat den Endpoint nicht
angefasst". **25 -> 30 Beweise, 0 errors.** Der Beweis, um den es geht, ist
`send_never_strands`/`recv_never_strands`: unter offenem Tor gibt es nur zwei Ausgaenge —
zugestellt/eingereiht oder abgewiesen-mit-Code. Einen dritten gibt es nicht mehr; genau der war
D11. Dazu `gate_three_reasons_distinct`, `bind_receiver_full_is_noop` und
`migrate_owner_full_keeps_token`.

Bemerkenswert am alten Stand: `send_drops_above_cap` **bewies den Verlust** — der Beweis war
richtig, der Code war falsch. Ein Beweis, der dem Code treu ist, kann das Falsche beweisen.

**Belegt statt behauptet.** `tools/verus-modelltreue-ipc.sh` faehrt den echten Quelltext gegen das
uebersetzte Modell: **93 -> 99 Faelle**, **28 -> 35 Selbsttestfaelle**. Neu ist das **Hauptbuch
der Gestrandeten** (`Welt::gestrandete`): jeder Faden, der nach einer Operation blockiert ist und
in KEINER Struktur steht, wird vermerkt; nach einem Lauf ueber alle vier Ueberlaufwege muss die
Liste leer sein. Die Positivkontrolle dazu ist keine Zeile im Lauf, sondern **fuenf Mutationen**,
die D11 einzeln wiederherstellen (auch die Wurzel: „`enqueue` meldet beim Ueberlauf Erfolg") —
jede wird erkannt. Eine leere Liste ist nur dann eine Aussage, wenn sie sich fuellen kann.

Die `befund`-Mechanik des Waechters hat dabei genau das getan, wofuer sie gebaut wurde: als die
Behebung stand, schlug sie an („der Befund trifft nicht mehr zu") und verlangte, dass B2/B2b/B2c/B2d
aus ihrem Kopf heraus und hierher wandern.

**Im Kernel sichtbar:** Pruefzeile `epfull` (`system::run_epfull`, lokales Objekt wie `run_quiesce`
und `run_rebind`). Sie prueft den Weg, der ohne Scheduler auskommt — `bind_receiver` —, und die
Positivkontrolle steckt in der Anlage: die ersten 32 muessen gelingen **und auffindbar sein**,
sonst waere die Zeile von „bind_receiver geht nie" nicht zu unterscheiden. Gegenprobe gefahren:
die alte Fassung wieder eingesetzt -> `epfull : FAILURES` und `bringup : offen waren: epfull`.

Die drei blockierenden Wege (`call`/`recv`/`migrate_owner`) misst der Host-Waechter gegen
denselben Quelltext. Das steht so in der Pruefzeile, damit die Suite nicht mehr behauptet, als sie
prueft.

---

## VA == PA: die Annahme ist jetzt eine Liste, keine Gewohnheit (2026-08-04)

Nach dem Fenster-Umbau war die Frage nicht mehr „geht das?", sondern **„wo steckt dieselbe
Annahme noch?"**. Der Kernel bildet an manchen Stellen identisch ab -- die VA, die ein Subjekt
sieht, IST die PA. Jede solche Stelle traegt eine stillschweigende Annahme, und die Annahmen sehen
einander alle gleich. Zwei Fehler dieses Projekts hatten dieselbe Form, und beide waren
unsichtbar, **solange die zwei Zahlen zufaellig gleich waren**.

**Die Bestandsaufnahme.** Die Flaeche ist klein: neun Aufrufstellen in acht identisch abbildenden
HAL-Funktionen. Sie zerfallen in drei Klassen, und die Klasse entscheidet, was zu tun war.

**(1) Entfernt -- die Identitaet war eine Altlast.** `spawn_isolated_native` bildete Code- und
Stack-Frame identisch ab **und nahm die Physadresse des Code-Frames als Einsprungadresse**. Beide
gehen jetzt ins private Fenster (Plaetze `SLOT_CODE`/`SLOT_DATA`), der Entry ist eine VA. Damit
standen `vspace_map_region` und `vspace_map_code_region` ohne Aufrufer da -- **geloescht**, statt
als toter Pfad liegenzubleiben, den der naechste wieder benutzt.

**(2) Unmoeglich gemacht -- der Fehler, der schon zugeschlagen hatte.** `Scheduler::spawn_user`
nahm EINEN Wert fuer zwei Dinge: den EL0-Stackzeiger und die Reap-Region, die beim Thread-Tod an
den Allokator zurueckgeht. Beim Heben des GiB-0-Deckels wurde daraus ein `#PF
cr2=0x80_0000_0000` im **Kernel**. Die Funktion ist **geloescht**, nicht repariert: es gibt nur
noch `spawn_user_at`, das beide Werte verlangt. Der letzte Aufrufer -- ein SAS-Thread, bei dem die
Zahlen wirklich gleich sind, weil er sich die Identitaetskarte des Kernels teilt -- schreibt sie
jetzt zweimal hin. **Die Gleichheit ist dort ein Zufall der Umgebung und keine Eigenschaft des
Aufrufs**, und genau das soll der Quelltext sagen.

Reparieren haette hier nicht gereicht. Eine Funktion, die zwei Bedeutungen in einen Parameter
faltet, ist auch mit Warnschild noch die bequemere von zweien.

**(3) Benannt statt still -- die Identitaet IST die Zusicherung.** Neun Stellen bleiben:
`SYS_MAP`/`SYS_UNMAP` (der Aufrufer nennt eine Memory-Cap, also eine PA, und bekommt sie unter
derselben Zahl -- das ist die ABI, und sie zu aendern hiesse, ein Subjekt muesste eine VA nennen,
die es nicht kennt), die Geraetefenster (ein Treiber rechnet mit Adressen aus der
PCI-Enumeration, und die sind physisch) und zwei globale Kernel-Abbildungen ohne Subjekt.

**Der eigentliche Ertrag ist der Waechter.** `tools/identitaet.sh` haelt die Liste gegen den
Quelltext: jede identisch abbildende Stelle braucht einen Eintrag **mit Grund** -- und der Grund
beantwortet „warum gilt die Identitaet hier, und was waere die Folge, wenn sie faellt?". Kommt
eine Stelle dazu, schlaegt er an. Selbsttest in **beide** Richtungen: eine untergeschobene Stelle
wird erkannt, ohne sie schweigt er wieder.

Dabei prompt hereingefallen: der erste Anlauf suchte im Kopiebaum mit **absoluten** Pfaden, die
auf keinen Listeneintrag passten -- also meldete der Selbsttest seine eigene Mechanik als Befund.
Er hat damit funktioniert (er schlug an, wo nichts war), aber die Aussage war eine andere als
gemeint. Ein Waechter, dessen Schluessel nicht die des Registers sind, prueft eine zweite
Wirklichkeit -- dieselbe Falle wie `iova_window_clear_of_msi`, das die Fensterlage nachrechnete
statt sie zu lesen.

**Was der Waechter NICHT kann, und das steht in seinem Kopf:** er sieht Aufrufe, keine Absichten.
Ob eine erlaubte Stelle ihre Identitaet weiterhin zu Recht annimmt, prueft er nicht. Dafuer stehen
die Gruende in der Liste, und `isohigh` misst in beiden Suiten den Fall, der frueher strukturell
unmoeglich war.

---

## E-Rest 3d (Haelfte 2) — der GiB-0-Deckel fuer isolierte PDs ist weg (2026-08-04)

**Der Deckel war eine Zahl, nicht ein Gefuehl: 504.** GiB 0 abzueglich der ersten 16 MiB, je
2 MiB private Region — so viele isolierte PDs passten gleichzeitig, und das band frueher als
`MAX_VSPACES` (4096). Der Host-Test `gib0_deckel_ist_eine_zahl` rechnet es auf dem Speicherplan
von `-m 6G` nach und ist zugleich die Positivkontrolle des Umbaus.

**Die Ursache war die IDENTITAET, nicht „eine PD sieht es".** `vspace_map_block` leitet den
Tabellenindex aus der **Phys**adresse ab (VA == PA), und eine isolierte VSpace hat ihr eigenes
Seitenverzeichnis nur fuer GiB 0. Also musste die Region dorthin.

**Die Behebung: ein VA-Fenster ausserhalb der Identitaetskarte.** Es MUSS dort liegen, und das ist
der Punkt, an dem die naheliegende Loesung scheitert: der Kernel laeuft beim Syscall im Adressraum
*dieser* PD und greift dort ueber die Identitaetskarte auf beliebiges physisches RAM zu. Eine
User-VA irgendwo in GiB 0, die auf eine andere PA zeigt, **verdeckt** genau diese Sicht — der
Kernel laese an der Stelle den Speicher der PD statt den eigenen. Gewaehlt sind deshalb Bereiche,
die der Kernel nie identisch belegt: auf x86 `PML4[1]` (512 GiB; die Identitaetskarte benutzt
ausschliesslich `PML4[0]`), auf aarch64 `L1[9]` (die Eintraege 0..=8 sind belegt, 9..511 leer).
`vspace_map_user_window` legt die Tabellen an, `vspace_collect_user_window` gibt sie beim Abbau
zurueck.

**Belegt:** `isohigh : ALL PASS` bei 3G/4G/6G — die Regionen zweier isolierter PDs liegen bei
`0x1_02b0_0000` (4,04 GiB), und die **Farbtrennung haelt unveraendert**: die Farbbedingung ist
eine Aussage ueber die Physadresse und von der virtuellen Lage vollstaendig unberuehrt. Bei
512M/2560M meldet die Zeile `SKIP` — dort gibt es keinen Speicher oberhalb 4 GiB, die Frage ist
**nicht entscheidbar**, und das ist kein bestandener Test. Gegenprobe gefahren: die alte
Zuteilung wieder eingesetzt -> `isohigh : FAILURES` und die Suite rot.

**Zwei Annahmen der eigenen Notiz waren falsch — beide zugunsten der Sache.**

* „Der Preis ist der Verlust des 2-MiB-Block-Fastpaths." **Nein.** Der Fastpath hing nie an der
  Identitaet, sondern nur an der **Ausrichtung der VA**. Ein 2-MiB-ausgerichteter Block bleibt
  ein Blockdeskriptor; nur der Index kommt jetzt aus der VA statt aus der PA.
* „Gehoert mit B-4.1 zusammen entschieden." **Nein.** A1 ist gar nicht betroffen. Der Preis sind
  zwei bis drei 4-KiB-Rahmen je isolierter PD, und die duerfen selbst oben liegen.

**Der Fehler, der es teuer gemacht haette.** `Scheduler::spawn_user` nimmt EINEN Wert fuer zwei
Dinge: den EL0-Stackzeiger und die **Reap-Region**, die beim Thread-Tod an den Allokator
zurueckgeht. Solange VA == PA galt, war das dieselbe Zahl. Nach dem Umbau nicht mehr — gemessen
als `#PF cr2=0x0000008000000000` im **Kernel**: der Reap-Pfad gab eine virtuelle Adresse als
Physadresse frei. Behoben ueber das laengst vorhandene `spawn_user_at`, das der Ladepfad seit
A-2 benutzt (dort war VA != PA schon immer der Normalfall). **Ein Parameter, der zwei Bedeutungen
traegt, ist so lange harmlos, wie die beiden zufaellig gleich sind** — dieselbe Form wie das
`blocked`-Bit im Scheduler (D9) und wie „unten zuerst" als Zufall der Groessenrelation (E-Rest 3b).

**Gemessen:** RAM-Reihe 512M · 2560M · 3G · 4G · 6G (Hauptsuite) und 512M · 3G · 6G (Lade-Suite),
alle `== ALL PASS ==`; x86 `RUNS=8` und aarch64 `RUNS=4` mit identischer Signatur; Host-Tests,
Kerngrenze.

**Offen bleibt E-Rest 3e:** DMA-Regionen haengen weiterhin an GiB 0 — aus zwei Gruenden, die
auseinandergehoeren: sie werden identisch in die Treiber-PD abgebildet (dasselbe Fenster wuerde
es loesen), und sie sind **geraetesichtbar** — ob ein Geraet oberhalb 4 GiB adressiert, ist eine
Eigenschaft des Geraets, und die Angebotsliste fuehrt sie nicht.

---

## E-Rest 3d (Haelfte 1) — die Stellen sind aufgezaehlt, und der Speicher oben traegt (2026-08-04)

3b hatte den Zonenwunsch eingefuehrt, aber mit einer Vorsichtsmassnahme bezahlt: `mem_alloc` gab
„unten zuerst" vor, **weil** unbekannt war, wer alles darauf baut. Damit war der gesamte Speicher
oberhalb 4 GiB praktisch Reserve -- der Ausweichzaehler meldete ehrlich `0x`, also einen Pfad, der
nie lief.

**Die Aufzaehlung steht jetzt an EINER Stelle** (`enum Zone` in `kernel/src/system.rs`), nicht
verstreut in Kommentaren:

* `KernelOnly` (bevorzugt **oben**): Thread-/Cap-/IPC-Tabellen, Kernel-Thread-Stacks, die
  Segment- und Stack-Frames **geladener Programme**, alle L3-Seitentabellen, AP- und
  Sekundaerstacks. Gemessen bei 3G und 6G: **alle 28** dieser Allokationen liegen oberhalb
  4 GiB, Haupt- und Lade-Suite gruen.
* `PdMappable` (muss **tief**): alles, was identisch abgebildet wird (VA == PA). Das ist
  strukturell, nicht empirisch: `vspace_map_page_at` weist `va >= GIB1_END` ab, `pd_block_index`
  ebenso.
* harte Bedingung (`gib0_zone`, `None` statt einer unbrauchbaren Adresse): die private Region
  einer isolierten PD, `spawn_isolated_native` und `alloc_dma_region`. Der mittlere Fall kam
  hier dazu -- er stand noch auf „einmal fragen, danach pruefen, bei Verfehlung aufgeben".

**Die Gegenprobe ist der eigentliche Beleg.** Stellt man `system::alloc` auf `KernelOnly`, faellt
die **Lade-Suite** bei `-m 3G` aus (`drv`/`blkdev`/`dmaiso` -- die Treiber-PD wird nie bereit),
waehrend die **Hauptsuite gruen bleibt**. Eine Klassifikation, die nur gegen die Hauptsuite
geprueft worden waere, haette den Fehler durchgelassen. Das ist dieselbe Form wie D8/D9/D11: die
Suite loest den Fall nicht aus, den sie zu decken scheint.

**Zwei eigene Vermutungen widerlegt, beide gemessen statt geglaubt:**

* „Geladene Programmsegmente brauchen GiB 0." **Falsch.** `load_into_pd` bildet ueber
  `vspace_map_page_at` ab, und das nimmt VA und PA **getrennt** -- die Physadresse ist frei.
  Diese Vermutung hatte ich am selben Tag als Befund notiert; sie stand auf einer Messung, die
  durch einen anderen Fehler (den Selbst-Deadlock aus 3b) verfaelscht war. Eine Messung an einem
  kaputten Aufbau ist keine Messung.
* „Das Geraet erreicht nur GiB 0." **Falsch.** Mit der virtio-Region oberhalb 4 GiB liest
  `virtio-blk` den Sektor korrekt (`Geraet-DMA=1`, Magie stimmt). Die Region bleibt trotzdem
  konservativ tief -- aber aus einem anderen Grund: die 32-Bit-Faehigkeit ist eine Eigenschaft
  des Geraets, und die Angebotsliste enthaelt sie nicht (`Geraete-ohne-deklarierte-Adressbreite=1`).

**Der Zaehler ist zweiteilig und sagt jetzt etwas.** „PD-abbildbar musste nach oben ausweichen"
waere ein Befund (GiB 0 ist voll, und die Region liegt dort, wo eine PD sie nicht sieht); „reiner
Kernel-Speicher musste nach unten" ist harmlos. Gemessen: 512M und 2560M -> 0 / **28** (es gibt
oben nichts, der Pfad ist also GEFAHREN), 3G/4G/6G -> 0 / 0.

**Gemessen:** RAM-Reihe 512M · 2560M · 3G · 4G · 6G (Hauptsuite) und 512M (3x) · 3G · 6G
(Lade-Suite), alle `== ALL PASS ==`; x86 `RUNS=8` und aarch64 `RUNS=4` mit identischer Signatur;
sechs neue Host-Tests fuer das Zonen-**Intervall** (`alloc_in`), darunter eine Positivkontrolle,
die beim ersten Anlauf zu Recht durchfiel: sie stand auf dem 3G-Speicherplan, wo Best-Fit von
sich aus oben waehlt -- eine Positivkontrolle muss zu dem Aufbau passen, in dem sie steht.

Nebenbefund beim Bauen: die Suche kannte die Untergrenze, der **Zuschnitt** danach nicht (er
rechnete `start` aus `r.base` neu). Gefunden wurde in der Zone, herausgeschnitten darunter --
drei Host-Tests haben es sofort gezeigt.

**Offen bleibt** (s. `todo.md`): der Rest der Aufzaehlung (EL0-Kernel-Stacks, EL0-User-Stack,
IOMMU-Tabellen, Sentinel-Page -- alle konservativ, keine geprueft) und der 1-GiB-Deckel selbst,
der im VSpace-Layout liegt und nur mit einer nicht-identischen Abbildung faellt.

---

## E-Rest 3b — die Freiliste kennt den Zonenwunsch (2026-08-04)

**Der Befund war groesser als der Eintrag.** Notiert war: `alloc_dma_region` und isolierte
PD-Regionen muessen in GiB 0 liegen, fragen aber nach „irgendeiner" Region und geben bei
Verfehlung auf, ohne ein zweites Mal zu fragen. Richtig — aber die eigentliche Abhaengigkeit lag
eine Ebene tiefer: **„unten zuerst" war ueberhaupt ein Zufall der Groessenrelation.** Best-Fit
nimmt das kleinste passende Fragment; solange der Bereich oberhalb 4 GiB zufaellig groesser war
(`-m 4G`: 2048 gegen 2032 MiB; `-m 6G`: 4096 gegen 2032), landete alles Unbenannte unten. Bei
`-m 3G` (1024 gegen 2032) kehrt sich das um.

Gemessen, nachdem der alte Behelf entfernt war: nicht nur `dmawin`/`dmatok`/`iso` fielen aus,
sondern der ganze Ladepfad — `drv`, `blkdev`, `fs`, `part` reihenweise rot, die Treiber-PD kam gar
nicht hoch.

**Die Behebung, zweiteilig.** (1) `sel4lake_mem::alloc_below`/`alloc_colored_below` nehmen eine
Obergrenze; gesucht wird nur unter Fragmenten, die sie erfuellen koennen, und ein Fragment ueber
die Grenze hinweg wird an ihr **beschnitten** statt verworfen. Best-Fit vergleicht dabei den
**nutzbaren** Teil, nicht die Fragmentlaenge — sonst gewaenne ein riesiges Fragment, von dem nur
ein Zipfel unter der Grenze liegt, gegen ein kleines, das ganz hineinpasst. Farbe und Zone werden
in EINER Entscheidung getroffen (dasselbe Argument wie Z8 fuer NUMA: zwei nacheinander laufende
Politiken kaempfen gegeneinander). (2) „Unten zuerst" ist jetzt eine **ausgesprochene Politik** in
`mem_alloc`/`alloc_colored`, mit hohem Speicher als Ueberlauf. `claim_user_kstack` griff als
einzige Stelle am Wrapper vorbei und geht jetzt darueber — eine Vorgabe, an der eine einzige
Stelle vorbeigreift, ist keine Vorgabe.

Der Behelf im Speicherplan ist damit weg: hoher Speicher geht **vollstaendig** in die Freiliste
(bei `-m 3G` vorher 0 von 1024 MiB vergeben, jetzt 1024 von 1024).

**Zwei eigene Fehler unterwegs, beide gemessen und beide lehrreich.**

* `match MEM.lock() { .. None => MEM.lock() }` haelt den Guard bis zum Ende des `match`. Der
  Ausweichpfad war damit ein **Selbst-Deadlock** auf einem Spinlock — und zwar genau der Pfad,
  der selten laeuft. Die Lade-Suite blieb stehen, sobald der untere Bereich einmal nicht reichte.
  Ein Fehler im seltenen Zweig sieht aus wie ein Haenger und nicht wie ein Fehler.
* Der Zaehler fuer den Ausweich zaehlte zuerst **Versuche** statt **Wirkung** und meldete `1x` auf
  einer 512-MiB-Maschine, auf der es oberhalb 4 GiB gar keinen Speicher gibt. Gezaehlt hatte er
  eine absichtlich uebergrosse Anforderung aus dem Farbtest, die NIRGENDS passte. „Unten war kein
  Platz" und „es wurde oben genommen" sind zwei verschiedene Aussagen — dieselbe Verwechslung wie
  `rx_used` gegen „Daten sind angekommen".

**Gemessen.** RAM-Reihe 512M · 2560M · 3G · 4G · 6G auf der Hauptsuite, 512M · 3G · 6G auf der
Lade-Suite, alle `== ALL PASS ==`. Host-Tests mit **Positivkontrolle**:
`ohne_zone_waehlt_best_fit_den_oberen_bereich` belegt, dass Best-Fit ohne Zonenwunsch wirklich
oben landet — ohne diese Zeile sagte der Test darunter nichts, er koennte auch gruen sein, wenn
die Zone gar nichts bewirkt.

**Was NICHT behoben ist**, steht als E-Rest 3d in `todo.md`: welche Stellen GiB 0 wirklich
brauchen, ist nicht aufgezaehlt — „unten zuerst" ist eine Vorsichtsmassnahme, keine Zusicherung.
Und der 1-GiB-Deckel fuer Regionen mit PD-eigener Abbildung bleibt: `vspace_map_block` bildet
**identisch** ab (VA == PA), das ist eine Eigenschaft des VSpace-Layouts und mit Allokatorarbeit
nicht zu heben. Der Ausweichzaehler in der `mem`-Zeile meldet ehrlich `0x` — der Ueberlaufpfad ist
in QEMU **ungefahren**, seine Wirkung nur host-getestet.

---

## A1. Cache-Partitionierung zwischen PDs — **Stufe 1 erledigt** (x86)

Rest in [todo.md](todo.md#a1-cache--timing-seitenkanäle-zwischen-pds); hier steht, was trägt und
welche Annahmen dabei umgefallen sind.

**Gemessen, nicht angenommen.** `hal::cache` liest die LLC-Geometrie aus der Hardware — x86 über
`CPUID`-Blatt 4 (Unterblätter durchzählen, höchste Ebene mit Daten-/Unified-Cache), aarch64 über
`CLIDR_EL1`/`CCSIDR_EL1` inklusive der FEAT_CCIDX-Feldverschiebung. Farbe einer Seite =
`sets * line / PAGE`, immer auf die nächstkleinere Zweierpotenz abgerundet: nur echte Indexbits
sind Farbbits. Auf dem Testaufbau (QEMU q35, `-cpu Skylake-Client`): LLC L3 16 MiB, 16-fach,
64 B/Zeile, 16384 Sets → **256 Seitenfarben**.

**`1` ist kein Erfolgswert.** Meldet die Plattform keine Geometrie, gibt es eine Farbe — und
„die Farbsätze zweier PDs sind disjunkt" wäre dann strukturell wahr, ohne geprüft zu sein.
`colors::usable()` trennt das, der Test meldet **SKIP statt PASS**, und die Suite hat dafür einen
eigenen Zweig. Dieselbe Falle wie bei der SMMU-Event-Queue ohne `CD.R`, nur eine Ebene höher.
`qemu64` ist genau dieser Fall: das synthetische Modell meldet weder Blatt 4 noch x2APIC. Der
TCG-Rückfall der Suite steht deshalb jetzt auf `Skylake-Client` — die Lücken waren die des
**Modells**, nicht die des Kernels.

**Die Annahme, die umfiel: Färbung und der 2-MiB-Blockdeskriptor schließen einander aus.**
Aufeinanderfolgende Seiten tragen aufeinanderfolgende Farben, also überstreicht eine
zusammenhängende Region über `n` Seiten `n` aufeinanderfolgende Farben. Die 2-MiB-Region von
`spawn_isolated` sind 512 Seiten — bei 256 Farben also jede Farbe zweimal. Kein Zuteilungstrick
ändert das; es ist Arithmetik. Färbung gibt es deshalb nur mit einer Region, die **höchstens so
breit ist wie der Farbstreifen** (`MASK_BITS / PARTITIONS` Seiten, bei 4 Partitionen 64 KiB), und
mit seitenweisem Mapping statt eines Block-PTE. Der Preis steht im Code, nicht in einer Fußnote:
16 PTEs plus eine L3-Tabelle statt eines Deskriptors. `alloc_colored` weist eine zu große
Anforderung **ab**, statt still fremde Farben mitzunehmen.

**Auch die Kernel-Seite gehört dazu.** Kernel-Stack (16 KiB) und die obersten Seitentabellen einer
PD werden vom Kernel benutzt, aber *im Namen dieses Subjekts*; ihre Cache-Zeilen tragen dessen
Zugriffsmuster, und ein MMU-Walk hinterlässt dieselben Spuren. Beide kommen jetzt aus demselben
Streifen, und der Test weist das getrennt nach (`kernel_side_in_mask`) — sonst wäre es eine
Behauptung im Kommentar.

**Die zweite Annahme, die umfiel: `total_free()` ist als Leck-Orakel unbrauchbar, sobald andere
Kerne laufen.** Der erste Bilanz-Test verglich die globale Summe vor und nach dem Abbau und war
nichtdeterministisch — gleicher Kernel, mal grün, mal rot. Ursache: nebenher sammelt ein anderer
Kern den Stack des planmäßig gefaulteten `iso_probe`-Threads ein; fiel dieser Rückgang ins
Messfenster, sah eine bilanzneutrale Operation aus wie ein Gewinn. Ersetzt durch
`PhysAllocator::fully_free(base, len)`: die Frage lautet jetzt „ist **genau diese** Region wieder
frei", unabhängig von fremder Nebenläufigkeit. Ein Test, der aus fremdem Grund fehlschlägt, ist so
wenig wert wie einer, der nicht fehlschlagen kann.

**Wo die Arithmetik geprüft wird.** Farb- und Allokatorlogik liegen in `sel4lake-mem` und sind
reine Rechnung ohne Hardware — 13 Host-Unit-Tests (`rustc --test crates/sel4lake-mem/src/lib.rs`,
Laufzeit 0,00 s; der Umweg über `cargo` würde wegen `build-std` die halbe Standardbibliothek
übersetzen). Darunter ein **Sensitivitätstest**: ohne Maske muss die Eigenschaft umfallen. Ohne
ihn könnte der Disjunktheitstest grün sein, weil die Anordnung es zufällig hergibt, statt weil die
Maske wirkt. Der Kernel-Test rechnet die Farbe mit **derselben** `color_of` wie der Allokator —
eine zweite Fassung im Testmodul hätte am Ende nur die eigene Arithmetik bestätigt.

## B. Thread-Migration — **erledigt** (ext-30)

- [x] **B1** Kern-Zuordnung aus der ThreadId gelöst — globales **Thread-Directory**
      `gid -> (used, gen, core, local)`, lock-frei lesbar.

- [x] **B2** `migrate_to(tid, dst)` unter beiden Scheduler-Locks (aufsteigende Kern-Ordnung),
      **Push-Modell** (nur der abgebende Kern kann den Lazy-FP-Kontext sichern).

- [x] **B3** Kernel-Glue auf „lock-frei lesen → sperren → **erneut prüfen** → ggf. wiederholen"
      umgestellt (`system::with_owner`); `ThreadId::core()` existiert nicht mehr.

- [x] **B5** Test `migrate`: Thread wechselt unter Last den Kern, läuft dort nachweislich weiter,
      dieselbe Tcb-Cap bezeichnet ihn weiterhin, cross-core-KILL, Scheduler-Audit 0.

## C. Flexible Kapazitäten — **weitgehend erledigt** (ext-30)

- [x] **C1** Neue Crate `sel4lake-slab` (`Slab`/`AtomicTable`/`FreeList`); Thread-Directory,
      per-Kern-TCB-Tabellen, FP-Kontexte, `VSPACE_OF`, Kernel-Stack-Zuordnung **und** die
      Sekundär-Stacks kommen beim Boot aus dem RAM (`system::configure`).

- [x] **C2** Kernzahl aus dem Device Tree (`Dtb::cpu_count`); `MAX_CORES = 256` dimensioniert nur
      noch Arrays von *Locks*/Atomics.

### C4. Effizienz bei tausenden Threads (lineare Scans beseitigen)

- [x] `Scheduler::alloc_tcb` — **Freiliste** (O(1)).

- [x] Ready-Queues — **intrusive doppelt verkettete Listen** durch die TCBs: Einreihen/Ausklinken
      O(1) und **kein** Queue-Speicher mehr (vorher `[usize; PER_CORE]` je Priorität).

- [x] `system::least_loaded_core` — **lock-freier** Lastzähler je Kern.

- [x] MCS-Refill-Scan je Tick — entfällt vollständig, solange kein Budget erschöpft ist.

- [x] `thread_alive` / IPC-Liveness-Audit — lock-frei über das Directory statt Zielkern sperren.

### C6. x86-64-Port — **Stufe 4 erreicht** (ext-31, Branch `arch/x86_64`)

- [x] Boot (Multiboot→Long Mode), Serial, 4-Level-Paging + W^X + `CR0.WP`

- [x] IDT/Exceptions (256 Stubs, einheitliches Frame-Layout), LAPIC, Timer (PIT-kalibriert)

- [x] GDT/TSS, Ring 3, `syscall`/`sysret`, Context-Switch

- [x] HAL architekturselektiv; alle 13 Crates bauen für beide Architekturen

- [x] Kernel-Kern auf x86: Allokator, Capability-System, Scheduler (präemptiv), IPC, Audits

- [x] **SMP**: INIT-SIPI-SIPI + 16-bit-Trampolin (16→32→64 Bit), 4 Kerne in QEMU verifiziert —
      jeder Kern mit eigenem LAPIC-Timer und eigener Scheduler-Instanz

- [x] **Ring 3**: User-Threads laufen (Syscall per `int 0x80`, Fault auf Kernel-Speicher beendet
      den Thread) — im **SAS-Modell**, wie trusted PDs auf aarch64. Nötig dafür: `US` auf allen
      vier Paging-Ebenen, `TSS.RSP0` je Thread, eigene `.user_text`/`.user_data`-Sektionen

- [x] **Per-Prozess-Adressräume**: `vspace_*` vollständig implementiert (PML4→PDPT→PD, geteilte
      Kernel-PTs, supervisor-only Grundfläche + „hineingestanzte" User-Blöcke). Isolierte PDs
      laufen; Test `iso` zeigt: dieselbe Adresse ist für die SAS-PD lesbar, für die isolierte nicht

- [x] **PCI-ECAM + Enumeration**: Fenster aus der ACPI-**MCFG**, Geräte werden aufgezählt,
      virtio-rng gefunden, Bus-Master aktiviert. (BARs vergibt auf dem PC die Firmware — anders
      als auf `virt`, wo der Kernel das selbst tut.)

- [x] **RAM-Plan** aus dem Multiboot-Speicherplan (BSP-Trampolin reicht `EBX` durch); CPU-Liste
      aus der ACPI-**MADT** statt fester Kernzahl

## E. DMA-Härtung (aus dem Design-Review, ext-35)

- [x] **§2 sagt, was der Code leistet.** Die alte Formulierung („danach kann kein Gerät mehr in die
      Region DMAen") galt für künftige Übersetzungen und war für bereits übersetzte, in-flight
      Posted Writes falsch. Jetzt mit expliziter Arbeitsteilung Quiesce ↔ STE-Entfernung.

- [x] **BME-Clear + Flush-Read vor dem Unmap** (`pcie::quiesce_by_rid`). Schließt die Lücke, die
      die ehrliche Formulierung sichtbar macht, ohne gerätespezifisches Wissen (kein FLR).
      Nebeneffekt: die Reihenfolge stimmt jetzt (vorher lief das Gerät während des Unmaps weiter →
      Translation Faults statt Korruption, aber ein Fault-Sturm verdeckt echte Fehler).

- [x] **Granularitätsprüfung an der Cap-Prägung** (`CapError::Unaligned`, Test `dmaalign`).

- [x] **ATS-Entscheidung** mit den drei Bedingungen, unter denen sie revidiert werden dürfte.

- [x] **IOVA ≠ PA** — in zwei Schritten umgesetzt (ext-36). (a) Achsen als eigene Typen
      (`addr::Pa`/`addr::Iova`), `dma_prepare`/`dma_complete` auf die PA-Achse,
      `dma_addr_in_region` auf die IOVA-Achse — verhaltensneutral, weil die Werte noch gleich
      waren. (b) Fensterbasis oberhalb `RAM_TOP` weggedreht: Bump-Allokator je Kontext (1 GiB
      Fenster), 2-MiB-Schutzbänder (= größte Stage-1-Blockgranularität, ein 4-KiB-Band wäre von
      einem Block-Mapping überspannbar), IOVA 0 unabgebildet, keine IOVA-Wiederverwendung, keine
      arithmetische Beziehung zur PA, `DmaRegion::identity` **entfernt**, `DmaHandle.pa`
      kernelprivat. `stage1_map_region` indiziert mit der IOVA und trägt die PA ins Blatt.
      Der Negativtest in `virtiorng` hat die drei geforderten Teile (Queue vorher leeren,
      Eintrag prüfen statt zählen, Positivkontrolle im selben Lauf) und **kippt** in Schritt b.
      Nebenbefunde, die erst durch echte Übersetzung sichtbar wurden: der Treiber verlangt jetzt
      verbindlich `VIRTIO_F_ACCESS_PLATFORM` (sonst umgeht das emulierte Gerät die SMMU),
      `STE.S1STALLD` wird nur noch bei `IDR0.STALL_MODEL == 0b10` gesetzt (sonst `C_BAD_STE`),
      und im CD fehlten `A` (Terminate) und `R` (Fault aufzeichnen) — ohne `R` bliebe die
      Event-Queue per Konfiguration leer und der Negativtest prüfte nichts.
      Offen geblieben: nur der aarch64-Enforcer vergibt Fenster; `VtdEnforcer::attach` meldet
      weiterhin `None` (x86 hat keine per-Gerät-Zuteilung, s. C).

- [x] **Nacharbeit zu ext-36b — Belastbarkeit des Belegs.**
      * **Queue-Liveness**: „Event-Queue leer" zählt nur, wenn im selben Lauf ein echter
        `F_TRANSLATION` beobachtet wurde. Ein Selbsttest beim Hochlauf ist nicht konstruierbar
        (ein Übersetzungsfehler braucht eine echte Bus-Master-Anforderung; `ATOS` liefert ins
        `PAR`, nicht in die Event-Queue) — der Nachweis im selben Lauf ist die erreichbare Form.
      * **`dma_audit` Code 6**: `C_BAD_STE`/`C_BAD_CD`/… werden beim Leeren der Queue gezählt und
        überleben es. Ein solcher Eintrag heißt „der Stream übersetzt gar nicht" — genau der
        Zustand, in dem jedes Abwesenheits-Oracle bedeutungslos ist.
      * **Sensitivitätskontrolle**: Bypass-STE statt der eingeklappten „schreibt ODER faultet"-
        Fassung. Die Mutation „Fensterbasis 0" leistet es nicht — die Sentinel-Seite faultet auch
        bei IOVA = PA, sie unterscheidet die beiden Welten nicht.
      * **Fenstergrenzen** (`dmawin`): Fenster am Slot statt an der Erzeugung (die schärfere,
        nirgends notierte Grenze waren ~500 Kontext-*Erzeugungen*, nicht die ~256 Attaches je
        Kontext), Bump je Slot überlebt den Kontextabbau, drei getrennte Fehlerursachen
        (Fensterende / Eingangsbreite / **Geräte-Adressbreite**), alle geprüft.
      **Nicht-Wiederverwendung ist als dauerhafte Politik entschieden**, nicht als offener Punkt:
      eine IOVA, die nie zurückkommt, kann keine veraltete Übersetzung tragen. Die verbleibende
      Lebenszeit-Obergrenze je Slot ist ein geprüfter sauberer Fehlschlag, kein Betriebszustand.
      Falls sie je erreicht wird: Slot-Recycling beim Kontextabbau, nicht IOVA-Recycling im
      Kontext. Damit muss der Teardown-Token nur den **PA-Free** absichern — der Unmap bleibt
      zwingend (stehende Übersetzung auf freigegebene PA = der UAF), aber es gibt nichts
      zurückzugeben. Ein Zustand weniger.
      Ebenfalls erledigt: undeklarierte Geräte-Adressbreiten werden geführt und protokolliert
      statt stillschweigend als 64 Bit angenommen.

- [x] **Teardown-Token** (ext-37): `CapSpace::delete_leaf` gibt `ObjectKind::Dma` **nicht** mehr
      frei, sondern meldet die Region über `Finalized` (vormals `ReplyFinal`) zurück; der Kernel
      legt still, unmappt, synchronisiert und gibt erst gegen einen `DmaTeardownToken` frei. Der
      Token trägt eine `DmaRegion` (beide Achsen), nicht eine `PhysRegion`.
      Die vier Entscheidungen: **synchron** (ein Zwischenzustand, den es meistens nicht gibt, ist
      schwerer richtig zu halten als einer, den es nie gibt); **gebündelt** für `revoke`
      (`DmaEnforcer::finalize` nimmt den ganzen Stapel: alle entwaffnen, alle spülen, alle
      unmappen, ein `TLBI`+`SYNC`); **`KILL` darf Pending erzeugen** (`KillScope` in
      `destroy_pd`), überall sonst zählt es zusätzlich als Anomalie; **Audit-Code 7** prüft, dass
      eine Pending-Region weder in der Freiliste noch in einer Übersetzungstabelle steht.
      Die IOVA-Rückgabe entfällt, weil Nicht-Wiederverwendung Politik ist — ein Zustand weniger.
      Test `dmatok`: `cap_delete` ohne `dma_detach` baut ab und gibt frei (Basislinie exakt
      wiederhergestellt); eine StreamID ohne Gerät (Konfigurations-Read `0xFFFF`) landet
      deterministisch im Pending-Zustand, die Region fehlt genau um ihre Größe, der Unmap ist
      trotzdem erfolgt, Code 7 hält.

## D. Verifikation

- [x] **D5** Tier-1-Roadmap in `docs/verification.md` war stale — sie führte die
      Concurrency-Modellprüfung der Locks noch als offen, obwohl Loom Stufe 2 seit `c2116ac`
      steht. Abgehakt in `529bc35` (B-2.4), **mit der Grenze daneben**: Loom modelliert eine
      *Kopie* des Algorithmus, ein Fehler in der `cfg`-Auswahl bleibt für Loom wie für Kani
      unsichtbar. Genau dort lag B-1.1, der x86-IRQ-Deadlock.
      Der Punkt stand danach noch in `todo.md` — der Eintrag beschrieb also einen Zustand, den
      es seit `529bc35` nicht mehr gab. Eine Liste, die Erledigtes als offen führt, ist
      derselbe Fehler wie ein grüner Testlauf ohne Testergebnis: sie sagt etwas aus, wofür sie
      keinen Beleg hat.

---

## A-5.2. virtio auf x86 — Transport, Blockgerät, Netzkarte

**Erledigt 2026-08-01.** Der Rest von Strang A-5 (die Treiber-**PD**) steht als A-5.1 weiter offen;
was hier fertig wurde, ist die Treiberlogik samt Beleg, dass sie trägt.

### Die Annahme, die umfiel: „der RNG belegt den Transport"

Sie war zur Hälfte richtig. `virtio-rng` hat **eine** Deskriptorzelle mit Write-Flag, das Gerät
füllt sie — damit steht fest, dass Bus-Master-DMA **in** unseren Speicher ankommt. Was nicht
feststand: ob das Gerät unseren Speicher auch **liest**. Der RNG liest nie etwas von uns; die
Richtung kam in seinem Testfall strukturell nicht vor.

Das ist dieselbe Fehlerform wie die leere SMMU-Event-Queue ohne `CD.R` — eine Aussage, die wahr
aussieht, weil der Fall, der sie widerlegen könnte, gar nicht ausgeführt wird. Und sie trug bis in
den **Negativtest**: dass die VT-d-Root-Tabelle im Default-Block auch Lesezugriffe sperrt, war eine
Annahme über Hardwaresemantik. Genau solche Annahmen haben in diesem Projekt schon zweimal
danebengelegen (`STE.S1STALLD`, `GCMD` als Read-Modify-Write).

`blk` schließt beides in einer Transaktion:

| Glied der Kette | Richtung | Inhalt |
|---|---|---|
| 0 | Gerät **liest** | Anfragekopf: Typ, Sektornummer |
| 1 | Gerät schreibt | 512 Byte Sektordaten |
| 2 | Gerät schreibt | Statusbyte |

Kommt Glied 0 nicht an, weiß das Gerät nicht einmal, welchen Sektor es liefern soll.

### Gemessen (x86-Suite, 20 von 20 Läufen mit identischer Ergebnissignatur)

    virtio  : Transport (vor VT-d): Caps=1 Geraet-DMA=1 (64 Byte)
    vblk    : Lesen (vor VT-d): Kapazitaet=2048 Sektor(en) Status=0x00 geschrieben=513 Byte
              Magie=0x454b414c344c4553 (erwartet 0x454b414c344c4553)
    vnet    : MAC 52:54:00:12:34:56; Sendepuffer abgeholt=1 Rahmen empfangen=1 (76 Byte)
              ARP-Antwort von 10.0.2.2=1
    virtio  : Sperre (nach VT-d): Geraet-DMA=0 (0 Byte)  VT-d-Faults 0 -> 1
    vblk    : Sperre (nach VT-d): abgeschlossen=0 Status=0xff (vom Geraet unberuehrt)

Das Statusbyte wird **vor** der Anfrage auf `0xff` gesetzt, nicht auf 0. Auf 0 vorbelegt wäre die
Statusprüfung wertlos gewesen: `VIRTIO_BLK_S_OK` ist 0, der Test wäre also auch dann grün, wenn das
Gerät nie geantwortet hat. Nach dem VT-d-Aufbau steht dort weiterhin `0xff` — damit ist belegt, dass
das Gerät den Anfragekopf **nicht einmal gelesen** hat, und nicht bloß, dass keine Antwort kam.

### Warum der Inhalt geprüft wird und nicht die Länge

Ein Puffer voller Nullen ist von einem nie beschriebenen Puffer nicht zu unterscheiden — und ein
frisches Plattenabbild besteht genau daraus. Die Suite legt deshalb eine Magie („SEL4LAKE") in
Sektor 0. Die **Kapazität** ist die zweite, unabhängige Aussage: sie kommt aus dem
gerätespezifischen Konfigurationsraum (`VIRTIO_PCI_CAP_DEVICE_CFG`), den der RNG nicht hat und der
deshalb bis hierher nirgends aufgelöst wurde. Steht dort Müll, ist die Capability falsch
lokalisiert — ein Fehler, den die Magie allein nicht fände, weil der Datenpfad davon unberührt ist.
Die erwartete Zahl steht in der **Suite**, wo das Abbild entsteht, nicht im Kernel: dessen Aufgabe
ist, die Kapazität zu melden, nicht sie zu kennen.

Bei `net` dieselbe Frage, andere Antwort: geprüft wird nicht, ob der Empfangspuffer sich füllt,
sondern ob darin die **ARP-Antwort auf die eigene Anfrage** steht (Opcode 2, Absender-IP == die
angefragte). ARP ist dafür gewählt, weil es zustandslos ist, keinen Handshake und keine Zeitgeber
braucht — und weil die Antwort etwas trägt, das man nachrechnen kann.

**Beide Gegenproben laufen gelassen, nicht behauptet:** mit falscher Magie im Abbild meldet `vblk`
`FAILURES`, mit einer ARP-Zieladresse, für die niemand antwortet, meldet `vnet` `FAILURES` — und
beide Male läuft der Kernel in den Watchdog statt `SELFTEST COMPLETE` zu drucken. Sie stehen also
wirklich in `all_done()`.

### Was `net` zusätzlich prüft, das `blk` nicht kann

Zwei Queues mit **verschiedenem `queue_notify_off`**. Ein Treiber, der die Notify-Adresse der
ersten Queue für beide benutzt, weckt das Gerät auf der falschen Seite. Bei einem Einqueue-Gerät
kann dieser Fehler strukturell nicht auftreten — er wäre also bis zum ersten Mehrqueue-Gerät
unentdeckt geblieben.

Die Kopfgröße ist die zweite Falle: unter `VIRTIO_F_VERSION_1` sind es **immer 12 Byte**
(`virtio_net_hdr_mrg_rxbuf`), auch ohne ausgehandeltes `VIRTIO_NET_F_MRG_RXBUF`. Die 10-Byte-Fassung
gehört zum Legacy-Layout. Wer sie einsetzt, verschiebt jeden empfangenen Rahmen um zwei Byte und
findet den Ethertype an der falschen Stelle — ein Fehler, der wie „das Gegenüber antwortet nicht"
aussieht.

### Der Umbau, der dafür nötig war

`VirtioRng` war ein Monolith, in dem Handshake und Anfrage ineinander lagen. Bei einem Gerät fällt
das nicht auf; bei dreien wäre die bequeme Wahl gewesen, den Handshake zu **kopieren** — und damit
drei Fassungen derselben Zustandsmaschine zu haben, von denen zwei irgendwann still zurückbleiben.
Der virtio-Handshake ist genau die Sorte Ablauf, bei der eine vergessene Zeile (`FEATURES_OK` nicht
zurückgelesen) nicht auffällt, bis ein Gerät die Features ablehnt.

Herausgelöst sind deshalb `Transport` (Status, Features, Queues, Notify, Konfigurationsraum) und
`Queue` (Split-Ring). Alle drei Geräte sitzen darauf; die Crate hat weiterhin **keine einzige
Abhängigkeit**, `tools/kernel-grenze.sh` führt weiterhin **keine** Ausnahme.

`Queue` trägt bewusst **nur die CPU-Achse**. Die Gerätesicht kennt allein `Transport::queue_setup`,
wo sie in die Adressregister geht. Beide Adressen in einer Struktur zu halten, aus der man sich je
nach Zweck die passende greift, ist genau die Bequemlichkeit, die aus zwei Achsen wieder eine macht.

### Nebenbefund: die Lade-Suite war seit heute früh rot, und zwar aus diesem Grund

`test-qemu-x86-load.sh` startete `-device virtio-rng-pci` **ohne** `disable-legacy=on,iommu_platform=on`.
Das Gerät ist dann transitional und bietet `VIRTIO_F_ACCESS_PLATFORM` nicht an; der Treiber bricht
ab — richtig so. Nur steht `virtio` seit heute in `all_done()`, also wurde der Lauf nie fertig und
lief in den Watchdog. Das sah wie ein Hänger aus und war eine Gerätekonfiguration: die
`virtio`-Prüfung kam am selben Tag dazu, `test-qemu-x86.sh` bekam die Schalter, diese Suite nicht.
**Zwei Suiten, die dasselbe Gerät verschieden aufsetzen, sind ein Riss, durch den genau so etwas
fällt.**

---

## A-5.1. Der erste Treiber außerhalb des Kerns

**Halb erledigt 2026-08-01.** Die Treiber-PD läuft und bedient ihr Gerät; die **Richtungsumkehr**
(Treiber als Dienst, hot-reloadbar) steht weiter in [todo-A-ausfuehren.md](todo-A-ausfuehren.md).

### Gemessen

    devassign: zuteilbar: RID 0x0020, Konfigurationsseite 0xb0020000, BAR 0xfe004000+0x4000
    drv     : Teilergebnis der PD: Features-ok=1 used-fortgeschritten=1 Status=0x00
              geschrieben=513 Kapazitaet=2048
    drv     : Treiber-PD meldete=1 (laeuft=1 Fenster-gemappt=1 Geraet-aufgeloest=1);
              DMA-Region 0x3b51000, Geraetesicht 0x20e00000;
              erste acht Byte des Sektors 0x454b414c344c4553 (erwartet 0x454b414c344c4553)

`programs/hardware/virtio-blk` ist ein geladenes HardwareLand-Programm. Es mappt drei Fenster, löst
sein Gerät **selbst** auf, fährt den virtio-Handshake und liest Sektor 0 per Bus-Master-DMA. Der
Kern führt dabei **keinen** virtio-Schritt aus.

### Der Einwand, der wegfiel: „der Konfigurationsraum ist geräteweit"

Bis hierher galt: ein Treiber darf den PCI-Konfigurationsraum nicht sehen, denn wer ihn liest,
sieht jedes Gerät der Maschine. Deshalb sollte der Kern die virtio-Strukturen auflösen und dem
Treiber das **Ergebnis** reichen. Der Preis dieser Lösung: der Kern müsste virtio kennen.

Der Einwand stimmt für ein globales Adressregister (`0xCF8`/`0xCFC`) und für das ECAM-Fenster als
Ganzes. Er stimmt **nicht für eine Funktion**: ECAM bildet `(bus, dev, func)` auf je 4 KiB ab, also
auf genau eine Seite. Eine Funktion ist mappbar, ohne die Nachbarn mitzugeben.

Damit fällt die Aufteilung anders und besser aus: der Kern behält die **Enumeration** (welches
Gerät gibt es, wer bekommt es), der Treiber macht seinen **Capability-Lauf** auf seiner eigenen
Seite. `sel4lake_virtio::probe_ecam` ist dafür da — und die HAL ruft **dieselbe** Routine auf, statt
eine zweite Fassung davon zu halten.

### Zwei Aussagen aus zwei Quellen

Der Treiber meldet, dass seine **Transaktion** durchlief (Badge seiner endowten Notification). Der
Kernel prüft die **Bytes** in der Region, die er ausgegeben hat. Keine der beiden genügt allein:
meldet nur der Treiber, glaubt man dem Code, der es behauptet; prüft man nur die Bytes, könnte sie
auch jemand anders geschrieben haben. Die Gegenprobe mit falscher Magie im Abbild zeigt genau das —
`meldete=1`, und trotzdem `FAILURES`.

### Was dafür fehlte (und beim Bauen sichtbar wurde)

**1. `vspace_map_device` war auf x86 ein Stumpf, der `false` lieferte.** Der ganze
HardwareLand-Gerätepfad existierte dort nicht — nicht als Skip, sondern als Abwesenheit. Auf ARM
trägt ihn der RTC-Test seit ext-22; auf x86 hätte niemand gemerkt, dass er fehlt, bevor jemand ihn
brauchte. Genau die Fehlerform, wegen der die DMA-Tests nach `kernel/src/dmatests.rs` gewandert sind.

Die Implementierung hat eine Falle, die zählt: `vspace_create_base` hängt GiB 1..3 **jeder**
isolierten PD an dieselben statischen Tabellen (`ISO_PD_HIGH`) — drei Frames gespart, und richtig,
solange dort nichts PD-Spezifisches steht. Ein Gerätefenster ist genau das. Wer es dort hineinschriebe,
gäbe es **jeder** isolierten PD, und zwar lautlos: die Cap-Prüfung liefe korrekt durch, die Isolation
wäre trotzdem weg. Beim ersten Gerätefenster in einem GiB entsteht deshalb eine **private Kopie** der
Tabelle. `vspace_collect_device_tables` gibt sie beim Abbau zurück und lässt die geteilten in Ruhe —
die freizugeben wäre kein Leck, sondern das Gegenteil: der Allokator bekäme statischen Kernel-Speicher
als freies RAM.

**2. `SYS_MAP` konnte MMIO-/DMA-Caps nicht.** Es nahm nur `Memory`-Caps. Ein Treiber-PD hält aber
genau die anderen beiden. Dazu kam die Frage, **wo** seine Fenster liegen: `map` bildet identity ab,
und die physische Lage steht nirgends im Programm. Der bequeme Weg wäre ein Boot-Info-Block gewesen —
und der wäre „eine Autoritätsquelle neben dem Manifest" (so steht es seit A-2.1 in `loader::boot_arg`,
und es stimmt). `SYS_MAP` **beschreibt** jetzt stattdessen die Cap, die der Aufrufer ohnehin hält:
Basis, Länge und — bei DMA — die **Gerätesicht**. Neue Autorität entsteht dabei keine.

**3. Das Manifest galt nur für den Root-Task.** `SYS_LOAD` endowte ausschließlich, was der Lader
delegierte; die `initial_caps` aller anderen Einträge waren ein Wunschzettel. Jetzt gilt: die
**Loader-Cap** sagt, WER laden darf, das **Manifest** sagt, WAS das Geladene bekommt. Erst dadurch
kann ein Treiber Geräte-Autorität haben, ohne dass der Root-Task sie je besessen hätte — er könnte
sie sonst gar nicht weiterreichen.

**4. HardwareLand war über `SYS_LOAD` nicht ladbar.** `load_image` wies die Domäne ab: ein Backend ist
per Entwurf an einen Partner gebunden (ext-22), eine „bare" HardwareLand-PD bricht `domain_audit`.
Wer lädt, **ist** dieser Partner — eine andere Antwort gibt es nicht. Der Dispatch reicht deshalb die
Aufrufer-PD durch, und der Kanal entsteht **vor** den Anfangs-Caps: seine Notification *ist* die Cap,
die das Manifest mit `ntfn` meint. Eine frisch erzeugte wäre eine fremde gewesen, und die
HardwareLand-Cap-Policy hätte sie zu Recht abgewiesen — der Treiber hätte Geräte-Autorität gehabt und
keinen Weg, etwas zu sagen.

### Der Fehler, der eine Runde kostete

MMIO-Fenster wurden pauschal **schreibgeschützt** gemappt: im Dispatch stand `ro_kind = true` für
`ObjectKind::Mmio`. Der Treiber faultete beim allerersten Registerschreibzugriff
(`FAR=0xfe004014` = `common_cfg + device_status`). Ein Registerfenster, das man nicht beschreiben
kann, steuert nichts — ob ein Fenster schreibbar ist, sagt die **Cap**, nicht seine Art. `ro` bleibt
der DMA-Richtung `DeviceRead` vorbehalten: dort liest das Gerät, und der Puffer ist gegen es
geschützt.

Gefunden wurde er nur, weil die PD **Stufenbadges** meldet (läuft / Fenster gemappt / Gerät
aufgelöst) und ihr Teilergebnis in die eigene DMA-Region schreibt. Ohne das wäre jeder Fehlschlag
dieselbe Zeile gewesen — „meldete=0" —, egal ob das Programm nie startete, sein Fenster nicht mappen
konnte oder das Gerät nicht fand.

### Gegenproben (gefahren, nicht behauptet)

* Falsche Magie im Abbild → `drv : FAILURES`, obwohl der Treiber `meldete=1`.
* Kein Blockgerät → `drv : SKIP`, und der Treiber startet **gar nicht** (`NoDevice`, fail-closed):
  ein Treiber ohne Gerät meldet später einen Fehler, den niemand mit der Zuteilung in Verbindung
  bringt.

### Regression

x86 `RUNS=10` → 10 von 10 identische Signatur, `== ALL PASS ==`; Lade-Suite `== ALL PASS ==`;
aarch64 `== ALL PASS ==` (inkl. `rtc` — der ARM-Gerätepfad — und `virtiorng`, das jetzt durch
dieselbe `probe_ecam`-Routine läuft); `tools/kernel-grenze.sh` ohne Ausnahme.

---

## A-5.1. Der Treiber als Dienst — und sein Austausch

**Erledigt 2026-08-02.** Vorstufe (die PD läuft und liest einmalig einen Sektor) steht weiter oben;
hier steht der Teil, der A-5.1 abschließt: die **Richtungsumkehr** und der **Austausch**.

### Gemessen (Lade-Suite)

    drv : Zuteilung: DMA-Region 0x3b51000, Geraetesicht 0x20e00000
    drv : Anfrage 1 an v1: Status=0 Bytes=0x454b414c344c4553 Kapazitaet=2048 bedient=1
    drv : Austausch: Ergebnis=0 (umgebunden OHNE Empfaengerluecke); v1 bereit=1 v2 bereit=1
    drv : Anfrage 2 an v2: Status=0 Bytes=0x454b414c344c4553 Kapazitaet=2048 bedient=2

### Die Richtungsumkehr

Bis hierher rief der Kernel den Treiber: er selbst fuhr den virtio-Handshake. Jetzt **wartet** der
Treiber (`recv`), ein Client **fragt** (`call`), der Treiber **antwortet** (`reply`). Der Kernel ist
Client, nicht Treiber.

Das ist keine Umbenennung. Es ist die Bedingung dafür, dass „austauschbar" eine Eigenschaft des
**Mechanismus** wird statt dieses einen Treibers: in `loader::reload_driver` kommt das Wort
„virtio" nicht vor. Der Kernel ersetzt einen Empfänger an einem Endpoint; was dahinter steckt, weiß
er nicht. Genau das ist die Abnahmebedingung.

### Der Austausch, und in welcher Reihenfolge

1. Die neue Fassung wird geladen — in eine **zweite** Backend-PD am **selben Kanal**. Der Endpoint
   ist das, was den Austausch überlebt; bekäme v2 einen eigenen, müsste jeder Client umgehängt
   werden. Genau die gerissene IPC-Beziehung, gegen die A-4.1 gebaut ist.
2. Sie bekommt **dieselbe** Gerätezuteilung — neue Caps, dieselben Fenster, dieselbe DMA-Region,
   dieselbe IOVA. Das Gerät wird nicht losgelassen und nicht neu angehängt: die Übersetzung im
   IOMMU-Kontext bleibt stehen. Das ist der Unterschied zwischen einem Austausch und einem Neustart.
3. Erst wenn v2 wirklich am Endpoint steht, wird **stillgelegt** (A-4.2) und **umgebunden** (A-4.1).
   Weil v2 vorher schon Empfänger war, ist der Ausgang `overlapped`: der Endpoint hatte zu **keinem**
   Zeitpunkt null Empfänger.

Dass A-4.1 damit zum ersten Mal an einem **echten** Dienst abgenommen ist, ist ein Nebenertrag: der
`rebind`-Selbsttest lief bis dahin am lokalen `Endpoint`-Objekt, weil der überlappende Erfolgsfall
sonst nicht herstellbar war.

**Zur Überlappung:** zwischen Schritt 2 und dem Abbau von v1 halten kurzzeitig **beide** Fassungen
Caps auf dasselbe Gerät. Das ist nicht dasselbe wie „zwei Treiber": die Stilllegung sorgt dafür,
dass in diesem Fenster keiner von beiden eine Anfrage bekommt. Autorität zu **halten** und sie zu
**benutzen** sind verschiedene Dinge, und nur das zweite wäre hier ein Fehler.

### Woran man sieht, dass es ein Austausch war und kein Neustart

Der **Bedienungszähler** liegt in der DMA-Region, nicht in einer Programmvariablen. Eine Variable
stirbt mit der Fassung, die Region nicht. `bedient=1` → `bedient=2` belegt deshalb, dass v2 dieselbe
Region geerbt hat. Bekäme sie eine frische, stünde dort wieder 1 — und der Test wäre rot.

Dazu kommen zwei **verschieden gebadgte** Bereit-Meldungen (v1 und v2 bekommen beim Endowment
unterschiedliche Badges). Ohne sie wäre „eine Fassung hat sich zweimal gemeldet" von „zwei
Fassungen haben sich gemeldet" nicht zu unterscheiden.

### Der Fehler, den erst die Wiederverwendung sichtbar gemacht hat

`queue_setup` nullte nur den **Treiberteil** der Ringe (`avail`) — `used` gehört schließlich dem
Gerät. Das ist genau falsch, und es fällt nur auf, wenn eine Region wiederverwendet wird:

* das Gerät setzt seinen used-Index beim Reset auf 0 zurück;
* im Speicher steht aber noch der Endstand der vorigen Fassung, hier 1;
* die neue Fassung merkt sich diesen Stand als Ausgangswert und wartet auf eine Änderung — das
  Gerät schreibt nach der ersten Anfrage wieder genau 1.

Der Treiber wartet also auf einen Fortschritt, der bereits eingetreten ist, und läuft in seine
Poll-Schranke. Im Log sah das aus wie ein stummes Gerät (`Status=1`), und die Bytes im Puffer waren
trotzdem richtig — weil sie noch von v1 stammten. Genau die Sorte Befund, die ohne Inhaltsprüfung
als Erfolg durchgegangen wäre.

Die Spec ist eindeutig: **der Treiber** initialisiert den Speicher der Queue, bevor er sie
freigibt, und vor `QUEUE_ENABLE` gehört er ihm. Gegenprobe gefahren: mit wieder entfernter
`used`-Nullung meldet Anfrage 2 `Status=1`, und der Lauf ist rot.

### Was A-5.1 ausdrücklich NICHT einschließt

**`CAP_IRQ`.** Der Treiber pollt. Ein Geräte-Interrupt käme auf x86 per MSI-X, und seit B-3.2 steht
die Interrupt-Remapping-Tabelle auf lauter „not present": ein Gerät ohne IRTE kann keinen Interrupt
auslösen — mit Absicht. Eine IRTE-**Vergabe** gibt es nicht (nachgesehen, nicht vermutet). Der Weg
dorthin ist B-3-Arbeit. `endow_from_manifest` weist `CAP_IRQ` deshalb **ab**, statt eine Autorität
zu erteilen, die niemand einlöst — dieselbe Regel wie bei `CAP_PD_CONTROL` in A-2.1.

### Regression

Lade-Suite `== ALL PASS ==`; x86 `RUNS=8` → 8 von 8 identische Signatur, `== ALL PASS ==`;
aarch64 `== ALL PASS ==`; `tools/kernel-grenze.sh` ohne Ausnahme.

---

## A-6.1. Das Dienstprotokoll über dem Treiber

**Erledigt 2026-08-02.** Der erste Schritt von A-6 („über dem Sektor"), und alles davon läuft
**außerhalb des Kerns**.

### Gemessen (Lade-Suite)

    blkdev : INFO Status=0 Kapazitaet=2048 Sektorgroesse=512;
             READ(0)=0 WRITE(100)=0 FLUSH=0;
             Rueckgelesen=0x454b414c344c4553 (erwartet 0x454b414c344c4553);
             READ(jenseits der Platte)=3 (erwartet 3 = Bereich)

### Der Puffer des Treibers ist die Ablage

`READ` füllt ihn, `WRITE` schreibt ihn zurück — der Client nennt Sektoren, keine Adressen. Das ist
kein Notbehelf, sondern das übliche Staging-Modell eines Blockgeräts: der Puffer muss DMA-fähig und
gerätesichtbar sein, und beides kann ein beliebiger Client-Puffer nicht zusagen.

Der Nebeneffekt ist ein **besserer Test**: `READ(0)` holt die Magie von der Platte, `WRITE(100)`
schreibt **genau diese Bytes** woandershin. Kein erfundenes Muster — echte Daten, die von der Platte
kamen. Und der Rückleseschritt ist die eigentliche Aussage: eine quittierte Schreibanfrage ist eine
Quittung, keine Daten.

Eine geteilte Übertragungsfläche kommt erst mit A-6.2, wo es einen Abnehmer dafür gibt. Eine
Schnittstelle vor ihrem ersten Benutzer belegt nur eine Vermutung.

### `ST_RANGE` ist ein eigener Status

„Ich habe nicht gefragt" ist für den Client eine andere Lage als „das Gerät hat nein gesagt". Die
erste ist sein Fehler, die zweite nicht. Die Bereichsprüfung liegt deshalb **vor** dem Gerät: ein
Gerät, das über sein Ende hinaus gefragt wird, darf antworten, wie es will — der Dienst hat vorher
nein zu sagen. Geprüft wird beides: der Sektorbereich gegen die Kapazität **und** die Anzahl gegen
die Puffergröße (sonst schriebe das Gerät hinter das Ende der Region).

### Der Fehler, der eine Runde kostete: die Erwartung hing an der falschen Größe

`completed()` verlangte `Sektoren * 512 + 1` geschriebene Bytes. Beim **Lesen** stimmt das (Daten +
Statusbyte); beim **Schreiben** und beim **Flush** schreibt das Gerät nur das Statusbyte, also genau
eins. Ergebnis: jede Schreibanfrage fiel durch — und zwar mit `Status=1`, was nach einem Gerätefehler
aussieht, **während die Daten längst auf der Platte standen** (der Rückleseschritt fand sie). Ohne
den Rückleseschritt hätte der Befund in die falsche Richtung gezeigt.

Die Erwartung steht jetzt als `expected_written` **im Ergebnis**, gebildet dort, wo die Richtung
bekannt ist. Eine Größe, die von etwas abhängt, das der Aufrufer nicht sieht, gehört nicht in seine
Rechnung.

### Zwei eigene Fehler mit derselben Form: ein Urteil, das seinen eigenen Bericht auslösen soll

Zweimal in Folge stand ein Prüfergebnis in `all_done()` **und** wurde erst im Bericht gebildet. Dann
kann es den Bericht nicht auslösen: der Lauf läuft in den Watchdog und druckt das Ergebnis trotzdem.
Im Log sieht das aus wie „grün, aber gehangen".

Beim zweiten Mal kam eine Variante dazu: die Zustandsmaschine wartete auf einen Treiber-Dienst, den
es ohne Boot-Archiv **nie** geben kann — also blieb das Urteil ungesetzt und die Hauptsuite hing.
`driver_service().is_none()` taugt dort nicht als Kriterium: unmittelbar nach dem Start ist es auch
dann `None`, wenn gleich einer kommt. „Es gibt kein Archiv" ist die Aussage, die von Anfang an
feststeht.

**Regel daraus:** ein Wert, der in `all_done()` steht, muss **außerhalb** des Berichts entstehen.

### Regression

Lade-Suite `== ALL PASS ==`; x86 `RUNS=8` → 8 von 8 identische Signatur, `== ALL PASS ==`;
aarch64 `== ALL PASS ==`; `tools/kernel-grenze.sh` ohne Ausnahme.

**Ein Nebenbefund, der nicht von hier stammt:** die aarch64-Suite zeigte in einem von vier Läufen
ein sporadisches `xfer : FAILURES` (Capability-Transfer in IPC), die drei anderen waren grün. Sie
hat keine Wiederholungsmessung wie die x86-Suite (`RUNS`), also fällt so etwas nur zufällig auf.
Gehört zu D0/B-1.3, nicht zu A-6.

---

## A-6.2. Die Partitionstabelle — im Dienst gelesen, nicht im Kern

**Erledigt 2026-08-02.**

### Gemessen

    part : GPT-Scan Status=0 belegte Eintraege=2 (erwartet 2);
           erste Partition LBA 34 ueber 967 Sektoren (erwartet 34 / 967)

Gegenprobe mit drei kaputten Tabellen — **alle drei abgewiesen, mit unterscheidbarem Grund**:

| kaputt gemacht | gemeldet |
|---|---|
| Signatur | `ABGEWIESEN, Grund=2` (Signatur) |
| Kopf-CRC | `ABGEWIESEN, Grund=5` (Kopf-CRC) |
| Eintrags-CRC | `ABGEWIESEN, Grund=8` (Eintrags-CRC) |

Dazu **14 von 14** Host-Tests des Parsers (Sekunden, über `rustc --test`, nicht über den
`build-std`-Pfad).

### Warum der Parser nicht in den Kern gehört

Eine Partitionstabelle sind **fremde Bytes auf einer Platte**, die ein beliebiger Mandant
geschrieben haben kann. Sie mit Kernprivileg zu interpretieren ist genau die Klasse Fehler, die man
dort nicht haben will. `sel4lake-part` hängt deshalb an nichts, ist `forbid(unsafe_code)`, und wird
vom **Blockdienst** gelinkt — der außerhalb des Kerns läuft (A-5.1).

Dass ein Blockdienst Partitionen meldet, ist dabei nichts Ungewöhnliches: das ist die Aufgabe einer
Blockschicht.

### Zwei Stellen, an denen ein GPT-Parser gern schlampt

**Der Kopf-CRC läuft über den Kopf, in dem das CRC-Feld selbst steht** — und dieses Feld muss dabei
als **Null** gelten. Wer es mitrechnet, bekommt nie eine Übereinstimmung; wer es *überspringt*
statt es zu nullen, verschiebt alle folgenden Bytes. Beides sieht wie „Tabelle kaputt" aus.

Der eigene Test ist beim ersten Anlauf genau da hineingetappt: er baute eine Mutation und rechnete
die Prüfsumme neu, während im CRC-Feld noch die alte stand. Ergebnis war `HeaderCrc` statt des
Fehlers, den der Test auslösen wollte — er hätte also etwas anderes belegt, als er behauptet. Jetzt
gibt es dafür `reseal()`, und die Falle steht in der Modul-Doku.

**Die Eintragsliste kommt in Stücken.** Sie ist 16 KiB groß, eine Anfrage liefert 4 KiB. Die
Prüfsumme muss trotzdem über das **Ganze** gehen — wer je Stück prüft, prüft ein Stück und glaubt an
die Tabelle. `Crc32` ist deshalb fortschreibbar. Und der letzte Happen darf nicht auf die
Sektorgrenze aufgerundet eingerechnet werden: die Prüfsumme gilt für `entries_bytes`, nicht für die
gelesenen Sektoren.

### Jede Bedingung einzeln, mit eigenem Fehler

Ein Parser für fremde Daten ist Angriffsfläche. `PartError` unterscheidet acht Fälle, statt in ein
`Option` zusammenzufallen: der Unterschied zwischen „hier ist gar keine GPT" (Signatur) und „hier
ist eine, die nicht mehr stimmt" (CRC) ist der Unterschied zwischen einer unformatierten Platte und
einem Datenverlust. `num_entries * entry_size` wird **geprüft** multipliziert — eine Längenprüfung
hinter einem übergelaufenen Produkt ist eine Attrappe.

### Die Testabbilder baut ein eigenes Werkzeug

`tools/mkgpt.py`, nicht `sgdisk`/`parted`. Zwei Gründe:

* eine Suite, die an einem Fremdwerkzeug hängt, fällt auf einem Rechner ohne dieses Werkzeug als
  „Test rot" aus statt als „Aufbau unvollständig" — eine Verwechslung, die dieses Projekt schon
  mehrfach bezahlt hat;
* nur ein eigenes Werkzeug kann den **Negativfall** herstellen (`--break signature|header-crc|
  entries-crc`), gegen den der Parser abgenommen wird.

**Beide Suiten benutzen dasselbe Werkzeug.** Zwei Suiten, die dieselbe Platte verschieden aufsetzen,
wären derselbe Riss wie zwei, die dasselbe Gerät verschieden aufsetzen — das stand am 2026-08-01
schon einmal in dieser Datei. Die Magie liegt seither auf LBA 34 (erster Sektor der ersten
Partition); auf LBA 0 steht der schützende MBR.

### Der Watchdog kann jetzt sagen, worauf er gewartet hat

Er druckte „nicht alle Aussagen belegt" und sonst nichts. Damit ist ein Hänger von einem nicht
erfüllten Kriterium nicht zu unterscheiden — man sieht, DASS es nicht fertig wurde, und muss raten.
Beim Umstellen auf GPT hat das eine Runde gekostet: alle Einzelzeilen standen auf `ALL PASS`, der
Lauf lief trotzdem in die Notbremse. Jetzt nennt sie die offenen Punkte beim Namen
(`bringup : offen waren: root vblk blkdev part`), und der Befund war in einer Zeile sichtbar: der
Kernel-Test aus A-5.2 las noch LBA 0, wo jetzt der MBR steht.

### Nebenbefund: die aarch64-Suite ist nicht deterministisch

Über gut zehn Läufe an diesem Tag: **drei** sporadische Ausfälle an **drei verschiedenen**
Prüfungen (`xfer`, `dtb`, einer weiteren), dazwischen 4 von 4 grün am Stück. Die ARM-Suite hat
keine Wiederholungsmessung wie `RUNS` auf x86 — so etwas fällt dort nur zufällig auf, und ein
einzelner roter Lauf ist von einem echten Befund nicht zu unterscheiden. Gehört zu D0/B-1.3.

### Regression

Lade-Suite `== ALL PASS ==`; x86 `RUNS=6` → 6 von 6 identische Signatur, `== ALL PASS ==`;
aarch64 4 von 4 `== ALL PASS ==`; Parser-Host-Tests 14/14; `tools/kernel-grenze.sh` ohne Ausnahme.

---

## A-6.3. Ein lesendes Dateisystem als eigene PD

**Erledigt 2026-08-02.** Damit ist A-6 vollständig: Blockdienst, Partitionstabelle, Dateisystem —
und **kein Schritt davon liegt im Kern**.

### Gemessen

    fs : Status=0 (0=Datei gelesen, 2=nicht gefunden, 3=Kette defekt, 4=Kette zu kurz)
         Groesse=20 erste acht Byte=0x454b414c344c4553 Cluster=1

Der Weg dahin: `INFO` → `SCAN` (GPT, beide Prüfsummen) → Bootsektor der Partition → `parse_boot` →
Wurzelverzeichnis durchsuchen → Clusterkette folgen. Jeder Sektor kommt über den Blockdienst per
IPC; die PD selbst fasst kein Register an.

Dazu **16 von 16** Host-Tests des FAT-Parsers (Sekunden, über `rustc --test`).

### Warum das eine eigene PD ist und der GPT-Scan nicht

Den GPT-Scan macht der Blockdienst selbst (A-6.2) — das ist bei einer Blockschicht üblich und war
die kleinere Änderung. Ein **Dateisystem** dort unterzubringen wäre etwas anderes: es interpretiert
beliebig viel fremde Struktur, und je mehr davon in der PD liegt, die das Gerät steuert, desto
weniger bedeutet „Fehlereindämmung". Ein Treiber hat mit Verzeichniseinträgen nichts zu tun.

### Der Fund, der den Entwurf verbessert hat: das Zertifikats-Gate hat Nein gesagt

Die FS-PD ist TrustedSAS und braucht deshalb ein Zertifikat (ADR 0014). Das Gate hat sie
**abgewiesen**:

    fs: 2 unsafe [PROGRAMM] forbid_unsafe_code=NEIN
    UNSAFE-AUDIT FEHLGESCHLAGEN fuer fs -> KEIN Zertifikat.

Die zwei `unsafe` waren die rohen Zugriffe auf das gemappte Fenster. Der bequeme Ausweg wäre
gewesen, die PD nach UserLand zu verschieben (kein Zertifikat nötig) — und damit eine Regel zu
umgehen, statt ihr zu folgen.

Der richtige Ausweg war, das `unsafe` **dorthin zu legen, wo es hingehört**: ins auditierte SDK,
das ohnehin auf der Allowlist steht. `libsel4lake::Window` ist die Kapsel — er entsteht **nur** aus
`map_window` (also aus Basis und Länge, die der Kernel gerade selbst gemappt hat), seine Felder sind
privat, und **jeder** Zugriff wird gegen die Länge geprüft, mit geprüfter Addition. Dieselbe Bauart
wie `Verified` im Manifest-Parser: die Bedingung trägt der Typ, nicht die Disziplin des Aufrufers.

Danach ist die PD `forbid(unsafe_code)` und das Gate lässt sie durch. **Ein Prüfer, der Nein sagt,
hat den Entwurf verbessert** — das ist der Zweck.

### Warum eine eigene Übertragungsfläche und nicht die DMA-Region

Die DMA-Region des Treibers ist **non-coherent** gemappt (Normal-NC), damit das Gerät
hineinschreiben kann, ohne dass jemand Cache-Wartung fährt. Eine zweite, **gecachte** Abbildung
derselben Seiten in der Client-PD wäre auf x86 ein Attribut-Alias — laut SDM undefiniert, in der
Praxis „geht meistens". Also eine eigene Region aus normalem RAM, in beiden PDs mit denselben
Attributen, und der Treiber **kopiert**. Genau das tut ein echter Treiber ohnehin, sobald der
Puffer des Clients nicht DMA-fähig ist.

### Drei Rollen, drei Badges — und was passiert, wenn man das übersieht

Root-Task, Treiber und Client melden sich alle über eine endowte Notification. Beim ersten Anlauf
teilten sich Treiber und Client eine Ablage: die zuletzt geladene PD überschrieb sie, und der
Kernel wartete auf ein Signal am **falschen Objekt**. Der Austausch meldete daraufhin `NotReady` —
was nach einem Zeitproblem aussieht und eine vertauschte Ablage war.

Zweiter Fund derselben Sorte: beim Hot-Reload fehlte der neuen Fassung **Slot 6** (die
Übertragungsfläche). Sie brach korrekt ab — der Treiber verlangt sie —, und der Austausch meldete
wieder `NotReady`. **Wer eine Fassung ersetzt, muss ihr alles geben, was die alte hatte**; eine
Endowment-Liste, die beim Ersetzen von der beim Erstladen abweicht, ist ein Riss.

Und: zwei Clients desselben Dienstes brauchen eine **Reihenfolge**. Liefen sie gleichzeitig,
mischten sich ihre Anfragen, und der Austausch fiele mitten in ein fremdes Gespräch. Der
Bedienungszähler wird deshalb **relativ** geprüft (`r2 == r1 + 1`) statt absolut — ein fester
Startwert wäre eine Aussage über die Reihenfolge aller Clients statt über den Austausch.

### Das Testabbild

`tools/mkgpt.py` baut jetzt auch ein **lesbares FAT16** (`--fat16`, `--file`) — selbst gebaut, aus
demselben Grund wie die GPT: `mkfs.vfat` ist ein Fremdwerkzeug, und eine Suite, die daran hängt,
fällt ohne es als „Test rot" aus statt als „Aufbau unvollständig".

Die Platte hat **zwei** Partitionen: die erste trägt das FAT16, die zweite ist roh und trägt die
Magie der älteren Tests. Getrennt, weil die Magie sonst auf dem FAT-Bootsektor läge — zwei Tests,
die sich dieselbe Fläche teilen, sind ein Riss, durch den beide fallen können.

**Die Größe der Partition ist eine Bedingung, kein Detail:** unter 4085 Clustern ist es FAT12, nicht
FAT16, und als FAT16 gelesen liefert eine FAT12-Tabelle lauter falsche Ketten, ohne dass irgendetwas
kaputt aussieht. `mkgpt.py` prüft das und bricht ab, statt ein Abbild zu schreiben, das der Parser
zu Recht ablehnt.

### Regression

Lade-Suite `== ALL PASS ==`; x86 `RUNS=6` → 6 von 6 identische Signatur, `== ALL PASS ==`;
aarch64 `== ALL PASS ==`; Host-Tests 14/14 (GPT) und 16/16 (FAT); `tools/kernel-grenze.sh` ohne
Ausnahme.

---

## A-6.4. Schreiben — und der zweite Melder

**Erledigt 2026-08-02.**

### Gemessen

    fs   : Schreiben Status=0; Rueckgelesen Status=0 Groesse=700 (erwartet 700) geprueft=700 Byte
    PASS : A-6.4: OK: HELLO.TXT, 700 Byte, 2 Cluster, 2 FAT-Kopien gleich
           -- unabhaengig vom Kernel am Abbild nachgelesen

Die Datei wächst von 20 auf 700 Byte, also **über einen zweiten Cluster hinaus**. Eine
Schreibprobe, die in den vorhandenen Cluster passt, prüft die Kettenverlängerung nicht — und die
ist der Teil, an dem ein Schreiber schiefgeht.

### Der Fund, der den Aufwand rechtfertigt

`tools/checkfat.py` liest dasselbe Abbild mit einer **zweiten, unabhängigen** Implementierung
(Python, andere Sprache, andere Rechnung) — auch das Muster ist dort **noch einmal** hingeschrieben
und nicht importiert: zwei Implementierungen derselben Regel stimmen nur überein, wenn die Regel
eingehalten wurde; eine geteilte Funktion stimmte auch dann, wenn beide falsch sind.

Die Gegenprobe hat den Wert sofort gezeigt. Schreibt die PD absichtlich nur **eine** FAT-Kopie:

    fs   : ALL PASS      <- der Kernel meldet Erfolg
    FAIL : A-6.4: die 2 FAT-Kopien sind NICHT gleich

Der Kernel-seitige Befund bleibt grün, **weil die PD über Kopie 0 zurückliest** — sie bestätigt
ihr eigenes Ergebnis mit derselben Sicht, mit der sie es geschrieben hat. Nur der unabhängige Leser
sieht, dass ein anderer Leser (der Kopie 1 benutzt) etwas anderes sähe. Genau dafür ist die zweite
Quelle da.

### Der häufigste Schreibfehler überhaupt

Ein FAT-Dateisystem hat üblicherweise **zwei** Kopien der Tabelle. Wer nur die erste fortschreibt,
hinterlässt ein Dateisystem, das jedes Prüfwerkzeug als beschädigt meldet — und das ein Leser der
zweiten Kopie anders sieht als der Schreiber. Die Zahl der Kopien steht im Bootsektor; sie zu
ignorieren ist der Fehler, den `Fat16::fat_copy_lba` unmöglich machen soll, und der Host-Test
`schreiben_setzt_beide_fat_kopien` fängt ihn.

### Und der Flush

Ohne ihn ist „geschrieben" eine Aussage über einen Puffer, nicht über die Platte. Das ist der
Unterschied zwischen einem Dateisystem, das einen Stromausfall überlebt, und einem, das es meistens
tut. Der Statuscode des Flush wird **weitergereicht**, nicht verworfen — ohne ausgehandeltes
`VIRTIO_BLK_F_FLUSH` darf das Gerät ihn ablehnen, und dann gilt die Dauerhaftigkeitszusage nicht.

### Was nicht dabei ist

Anlegen und Löschen von Dateien, Unterverzeichnisse, lange Namen. Die Suche nach einem freien
Cluster geht nur über den **ersten** FAT-Sektor — für die Probe reicht das, für einen
Produktivtreiber nicht, und das steht im Code an der Stelle statt in einer Fußnote.

### Regression

Lade-Suite `== ALL PASS ==`; x86 `RUNS=6` → 6 von 6 identische Signatur, `== ALL PASS ==`;
aarch64 `== ALL PASS ==`; Host-Tests 20/20 (FAT) und 14/14 (GPT); `tools/kernel-grenze.sh` ohne
Ausnahme.

---

## B-3.4. Der Interrupt-Nachrichtenbereich als IOVA

**Erledigt 2026-08-02.**

Auf x86 behandelt VT-d eine DMA-Schreibung nach `0xFEE0_0000..0xFEF0_0000` als
**Interrupt-Nachricht** und befragt die Übersetzung gar nicht. Eine IOVA dort ist damit unbenutzbar
— und zwar auf die unangenehmste Art, die es gibt: die Seitentabellen sähen völlig richtig aus, das
Gerät erzeugte trotzdem Interrupts statt Speicherzugriffen. **Kein Fault, kein Eintrag in der
Fehlerwarteschlange, nur Daten, die nirgends ankommen.**

### Strukturell statt geprüft

Die Fensterbasis liegt jetzt oberhalb des Bereichs. Die Alternative wäre gewesen, bei jeder Vergabe
zu prüfen — und eine Bedingung, die an jeder Vergabestelle richtig geprüft werden muss, vergisst die
nächste Vergabestelle. Der Preis sind ein paar GiB ungenutzter IOVA-Raum von 39 Bit. Das ist kein
Preis.

Der Bereich kommt aus der HAL (`iommu::interrupt_message_window`) statt aus einer `cfg`-Verzweigung
beim Aufrufer. Auf aarch64 liefert sie `None` — und das ist eine **Zusage** („jede IOVA wird
übersetzt"), keine Unkenntnis. Sollte je eine Plattform dazukommen, deren MSI-Doorbell außerhalb der
Übersetzung liegt, gehört sie dorthin und nicht in eine Sonderbehandlung.

### Es war ein echtes Loch

Die Gegenprobe ohne das Überspringen: `dmawin : ... kein Kontext-Fenster im
Interrupt-Nachrichtenbereich=0`, `dmawin : FAILURES`. Das Fenster lag vorher **wirklich** darin —
mit 512 MiB RAM beginnt es bei ~`0x2000_0000` und spannt über 39 Bit, `0xFEE0_0000` liegt mitten
drin.

Der Nachweis steht als eigenes Feld (`msi_clear`) und eigene Zeile, nicht als stille Konjunktion:
die anderen Fenstergrenzen führen zu einem sauberen Fehlschlag, dieser Bereich zu **gar keinem**.
Genau deshalb muss die Aussage einzeln dastehen.

---

## A1-Rest. `sel4lake_mem::stripe` rechnete über 64 Bit statt über die Farben

**Erledigt 2026-08-02** (parallel bearbeitet, Ergebnis hier integriert).

CLAUDE.md führte das seit dem 2026-08-01 als offen: „`sel4lake_mem::stripe` hat denselben Fehler".
Es stimmte — und der Fehler war schlimmer als die Notiz vermuten ließ.

### Gemessen, nicht argumentiert

`stripe(i, n)` rechnete `per = MASK_BITS / n`, also Blöcke über alle 64 Bit. `ColorMask::contains(c)`
fragt aber Bit `c % 64`; bei 16 Farben werden die Bits 16..63 **nie** befragt. Gegen den
unveränderten Code, `n = 4`:

    colors=16   Streifen 0: 16 Farben, erwartet 4    <- der ganze Farbraum
    colors=16   Streifen 1..3: je 0 Farben           <- keine einzige
    colors=256  Streifen 0..3: je 64 Farben          <- zufällig richtig

**Die schlimmere Hälfte:** `is_empty()` meldete für diese Sätze `false`, und `intersects` meldete
„disjunkt" — leere Mengen schneiden sich nicht. Der Selbsttest `run_stripe_alloc` wäre auf aarch64
**grün** gewesen, ohne dass irgendetwas getrennt war. Das ist exakt die Fehlerform, gegen die das
Entwurfsprinzip dieses Projekts geschrieben ist: ein Prüfer, der über Abwesenheit entscheidet, ohne
sprechfähig zu sein.

### Die Signatur musste sich ändern

`stripe(i, n)` → `stripe(i, n, colors)`. Ein festes 64-Bit-Muster kann nicht gleichzeitig bei 16
und bei 256 Farben richtig **und** zusammenhängend sein: bei 16 verlangt Korrektheit Streifen `i` =
Farben `4i..4i+3`, bei 256 einen 16-Bit-Block. Das legt unvereinbare Muster fest. Die einzige
parameterfreie Alternative wäre Verschränkung (Lauflänge 1) — die das Modul ausdrücklich verwirft,
weil `region_bytes()` auf dem Zusammenhang steht.

Bei 256 Farben sind die Masken **bit-identisch** zu vorher; x86 verhält sich unverändert, und die
Suite bestätigt das (`color : ALL PASS`, `stripe : ALL PASS`, Werte unverändert).

### Sensitivitätsmessung statt Behauptung

Alte Rechnung hinter der neuen Signatur wiederhergestellt: **17 passed, 5 failed**. Danach:
**22 passed, 0 failed** (vorher 18). Die vier neuen Tests laufen ausdrücklich über beide
Farbanzahlen — ein Test nur bei 256 hätte den Fehler nie gesehen.

### Zwei Nicht-Änderungen, bewusst

* **`pick_free` hat den Fehler nicht.** Seine Schranke `n > 32` ist die Breite des `taken: u32`,
  eine echte Eigenschaft des Typs, keine Farbanzahl.
* **`alloc.rs` `npages > MASK_BITS` bleibt.** Sieht nach derselben Familie aus, ist keine: eine
  Verschärfung auf `colors.min(MASK_BITS)` wäre ein Falsch-Negativ.

---

## B-3.3. Mehr-Einheiten-Aggregation — die Hälfte, die schlimmer war

**Erledigt 2026-08-02** (parallel bearbeitet, Ergebnis integriert; nur `x86_64/vtd.rs` und
`x86_64/dmar.rs` übernommen).

Die Todo-Notiz sagte: „`VtdCaps` ist noch eine Einheit". Das stimmte **nicht mehr** — `caps_common()`
aggregierte längst, ebenso die Fault-Zähler und `discover`. Was die Notiz **nicht** sagte, war der
schlimmere Teil:

| Stelle | was wirklich passierte |
|---|---|
| `init()` | stellte **nur Einheit 0** scharf. Einheiten 1..n blieben mit `TE = 0` — und das heißt nicht „blockiert", sondern **keine Übersetzung**. Das ist kein blindes Oracle, das ist kein Schutz. |
| `invalidate_context_cache()` | lief nur auf Einheit 0. Nach `context_clear` blieb das Gerät an den übrigen Einheiten aus dem Cache übersetzt. |
| `flush_entry` / `slpt_map` | lasen die Fähigkeiten von **Einheit 0** und trafen damit **Politik**: `ECAP.C` entscheidet über `clflush`, `ECAP.SC` über `SNP`. Fehlt einer anderen Einheit `SC`, ist `SNP` dort ein **reserviertes** Bit — wörtlich der `STE.S1STALLD`-Mechanismus, den dieses Projekt schon einmal bezahlt hat. |
| Sprechfähigkeit | fehlte ganz. `ru32` liefert bei fehlender Basis `0`; eine deklarierte, stumme Einheit trug damit genauso zu `faults_empty() == true` bei wie eine fehlerfreie. |

### Was daraus folgt

Bei Fähigkeiten ist das **Minimum** die einzige sichere Richtung — wer das Maximum nähme, verspräche
etwas, das eine der Einheiten nicht kann. Bei Fehlern umgekehrt die **Summe**: ein Fault an Einheit 1,
den niemand zählt, ist ein Isolationsbruch, den das Oracle nicht sieht.

Stumme und abgeschnittene Einheiten zählen jetzt in `CFG_ERRORS` und werden über den bestehenden
Audit-Code 6 sichtbar — je einmal gelatcht, weil beides Zustände sind und keine Ereignisse; sonst
wäre der Zähler eine Funktion der Aufrufhäufigkeit.

### Ein Befund, der nur gemeldet werden konnte — und jetzt behoben ist

`VtdEnforcer::detach` hatte `let Some(caps) = caps_common() else { return }`. Solange `caps_common()`
praktisch immer `Some` war, fiel das nicht auf. Seit eine stumme Einheit erkannt wird, kann es `None`
werden — und dann bliebe eine **Übersetzung stehen, während ihre Region freigegeben wird**. Genau die
Lage, gegen die das Teardown-Token existiert, nur ohne jede Meldung.

Behoben: der Ausfall wird gezählt und über einen neuen `dma_audit`-**Code 8** sichtbar. Ein stilles
`return` wäre die schlimmere Variante desselben Fehlers gewesen — kein Schutz **und** kein Hinweis
darauf.

### Grenze der Aussage

Die Mehr-Einheiten-Pfade sind auf QEMU q35 nicht erreichbar (genau **eine** DRHD). Sie sind also
durch keinen Lauf gedeckt, sondern nur durch Codeprüfung und dadurch, dass der Ein-Einheit-Fall
bitgleich bleibt. Das steht hier, damit niemand die grüne Suite für einen Beleg der Aggregation hält.

---

## D6. Die aarch64-Suite maß gar nicht — und das sah aus wie ein Kernelfehler

**Teilweise erledigt 2026-08-02.**

Über rund 13 Läufe fielen **vier verschiedene** Prüfungen aus (`xfer`, `dtb`, `prio`, `loadstop`),
scheinbar zufällig, rund 30 %. Das sah nach Nichtdeterminismus im Kernel aus. Es war zur Hauptsache
die **Testmechanik**, und zwar in zwei Schichten:

1. **Die Ausgabe hing an einer Pipe**, beendet wurde mit `--signal=KILL`. Ein per SIGKILL
   erschlagenes QEMU flusht seinen stdout-Puffer nicht. Dieselbe Falle hatte die x86-Suite am
   2026-08-01 schon einmal bezahlt — sie schreibt seither in eine Datei.
2. Nach dem Umbau auf eine Datei teilten sich **alle Läufe eine** Datei. Gelegentlich fehlten dann
   **frühe** Bootzeilen in der ausgewerteten Ausgabe, während alle Ergebniszeilen da waren. Die
   Signatur blieb deshalb identisch, und trotzdem fiel je nach Lauf eine **andere** Prüfung durch —
   genau die, deren Zeile nicht Teil der Signatur ist.

Jeder Lauf bekommt jetzt seine eigene Datei. Ergebnis: `RUNS=16` → **16 von 16** mit identischer
Signatur, `ALL PASS`.

### Was das über Messen sagt

Die ARM-Suite lief bis heute **genau einmal**. Ein roter Lauf war dort von einem echten Befund nicht
zu unterscheiden, ein grüner sagte nichts über den nächsten. Die x86-Suite vergleicht seit B-1.3 die
Ergebnissignatur über `RUNS` Läufe — dieselbe Mechanik ist jetzt auch hier, und zwar **dieselbe** und
keine zweite Fassung davon.

### Was NICHT erledigt ist

In einer Messung dazwischen (gemeinsame Datei) standen 14 von 16, und die beiden Abweichungen waren
**echte** Watchdog-Läufe — diese Zeile kommt vom Kernel, nicht von der Mechanik. 16 saubere Läufe
schließen eine Rate von 12,5 % nicht aus (P ≈ 12 %). Der Hänger bleibt offen; er ist jetzt nur
erstmals messbar, und bei Abweichung liegt das volle Log in `build/diag/`.

Im ARM-Hänger sind **alle Ergebniszeilen grün** und der Bericht kommt trotzdem aus der Notbremse —
dieselbe Form wie D0 auf x86. Der nächste Schritt ist deshalb derselbe wie dort: die Notbremse muss
sagen, **worauf** sie gewartet hat.

---

## B-7.2. Loom prüfte eine Kopie — und die Kopie war nicht das Problem

**Erledigt 2026-08-02** (parallel bearbeitet, Ergebnis integriert und selbst nachgemessen).

Die Notiz sagte: „Loom modelliert eine *Kopie* des Lock-Algorithmus." Das stimmte —
`Verification/concurrency/loom/src/lib.rs` trug in Zeile 2 wörtlich „**GETREUE Kopie** der
Lock-Logik". Zeile für Zeile geprüft waren die Ordnungen **heute** sogar identisch. Das ist aber
kein Trost, sondern der Punkt: **nichts hielt sie zusammen**. Eine geänderte Ordnung im echten Lock
hätte den Beweis nicht gestreift.

Jetzt übernimmt `tools/loom-verify.sh` `crates/sel4lake-sync/src/lib.rs` **unverändert**; die
Beweise stehen in derselben Datei (`#[cfg(all(loom, test))] mod loom_proofs`), wie die
Kani-Beweise auch. Vier cfg-Schalter genügen: `no_std` nur ohne Loom, Atomics und `UnsafeCell` aus
`loom`, und ein `spin_hint()`, das unter Loom `yield_now()` ist (Looms Threads laufen kooperativ —
eine Schleife ohne Abgabepunkt liefe endlos, statt zu explorieren).

### Der eigentliche Fund war ein anderer

Die Kopie war nicht der Grund, warum der Beweis wenig bewies. Der Grund war `core::cell::UnsafeCell`:

| Mutation im **echten** Lock | mit `core`-Zelle | mit `loom`-Zelle |
|---|---|---|
| `fetch_and(!RW_WRITER)` → `store(0)` | 3 von 6 fallen | 3 von 6 fallen |
| Ticket-Release `Release` → `Relaxed` | **0 — unbemerkt** | **2 fallen** |

Mit `core::cell` prüfte Loom nur das **Atomic-Protokoll**. Eine abgeschwächte Ordnung, die die
Nutzlast nicht mehr veröffentlicht, lief durch alle Beweise. Erst weil `data` unter Loom aus
`loom::cell` kommt, liegt der Zugriff im verfolgten Fenster und wird gegen die Atomics geordnet.

**Selbst nachgemessen**, nicht übernommen: `Ordering::Release` → `Relaxed` in Zeile 317 des echten
Locks → `test result: FAILED. 8 passed; 2 failed`. Danach zurückgesetzt und wieder `10 passed`.

### Was der Beweis weiterhin NICHT abdeckt

* **Die IRQ-Maskierung.** Loom modelliert Threads, keine Unterbrechungen; ein Interrupt mitten im
  kritischen Abschnitt **desselben** Kerns ist kein Thread-Interleaving. Unter Loom läuft das
  Host-Ziel, dort ist `IRQ_MASKING_IMPLEMENTED == false`. Der reentrante Ticket-Deadlock (der
  x86-Fehler vom 2026-07-29) wird allein von der Übersetzungszeit-Zusicherung `target_os = "none"`
  gehalten.
* **`Deref`/`DerefMut` sind zweigeteilt** — der Kernel dereferenziert einen `*mut T`, Loom einen
  verfolgten Griff. Das *Protokoll* ist geprüft, die zwei Zeilen Zeigerarithmetik nicht. Kleiner
  Rest derselben Fehlerform, aber benannt.
* **Looms Speichermodell ist C11**, nicht aarch64/x86.

### Die tote Kopie ist gelöscht

`Verification/concurrency/loom/src/{lib,ticket}.rs` sind weg. Bleiben sie liegen, steht genau die
Kopie weiter im Baum, die dieser Punkt beseitigt hat — und sieht dabei autoritativ aus. In
`docs/adr/0023` steht jetzt ein Vorspann, der den überholten Zustand als überholt markiert, statt
ihn stillschweigend falsch werden zu lassen. `hierarchy.rs`/`crosscore.rs` bleiben: die modellieren
**andere** Gegenstände (globale Sperrordnung, Cross-Core-IPC), keine Nachbildungen.

---

## D6, Teil 2. Die ARM-Notbremse zeigte den Stand von vor 35 Sekunden

**Erledigt 2026-08-02.**

Die `DBG pending`-Zeile lief genau **einmal**, nach ~25 s. Im beobachteten Hänger ist das zu früh:
dort sind am Ende **alle** Ergebniszeilen grün, der Bericht kommt trotzdem aus der Notbremse, und
der 25-s-Schnappschuss zeigt lauter Tests, die danach noch fertig wurden. Damit war nicht zu sagen,
welche Bedingung bei Ablauf offen war.

Jetzt läuft sie ein zweites Mal, kurz **vor** der Deadline. Das ist das ARM-Gegenstück zu dem, was
die x86-Notbremse seit heute tut (`bringup : offen waren: …`) — und dort war der Befund damit in
einer Zeile sichtbar.

---

## B-7.1. Die Notiz war überholt — die Lücke lag woanders

**Erledigt 2026-08-02** (parallel bearbeitet, integriert und selbst nachgemessen).

Die Notiz sagte: „Kani läuft nur im CI-Gate; die ext-29-Änderung an `sel4lake-sync` ist dort nicht
abgedeckt." Gemessen stimmte das **nicht**: `tools/kani-verify.sh` führt `sync` seit Längerem als
Ziel, und die CI ruft das Skript **ohne Argumente**, also mit allen vier Zielen.

**Woher die Notiz kam, ist trotzdem sichtbar** — und das ist der lehrreiche Teil: der CI-Job hieß
*„Kani — Tier-1-Beweise (Loader-Parser)"*, sein Kopfkommentar nannte nur `cert/archive/elf`. Wer die
CI liest statt das Skript, muss schließen, `sync` sei ungegatet. Dieselbe Fehlerform wie B-7.2, eine
Ebene höher: **eine Beschreibung, die neben der Sache herläuft** — und die dann als „Befund" in eine
Aufgabenliste wandert und dort Arbeit erzeugt, die es nicht braucht. Die Beschriftung ist korrigiert.

### Die echte Lücke: konkrete Werte statt eines Zustandsraums

Die drei vorhandenen Beweise liefen mit **konkreten** Werten — ein Leser, zwei Leser, ein Schreiber.
Das ist dieselbe Größenordnung, die Loom seit B-7.2 über Interleavings abdeckt. Über den
Zustandsraum, in dem die Zusicherungen des Zustandsworts tatsächlich leben, sagte **keines** der
beiden Werkzeuge etwas:

| neuer Beweis | Eigenschaft, die nur Kani sieht |
|---|---|
| Release bewahrt beliebige Leserzahl | genau die Zusicherung, für die dort `fetch_and(!RW_WRITER)` steht und nicht `store(0)` |
| Leser setzt nie das Writer-Bit | für **jede** Leserzahl unter der Grenze (2^31 Zustände) |
| Leserzahl-Grenze ist scharf | die Grenze ist **erreichbar** — damit die Annahme des vorigen nicht leer ist |
| Ticketüberlauf ist unschädlich | von **beliebigem** Ticketstand, auch am `u32`-Wrap |
| `writers_waiting` bleibt ausgeglichen | eine Drift um eins wäre eine dauerhafte Lesersperre, sichtbar erst weit weg von der Ursache |

`sync` steht damit bei **8 statt 3** Beweisen; alle vier Ziele zusammen: 7 + 6 + 8 + 7, 0 failures.

### Zwei Werkzeuge, dieselbe Zeile, verschiedene Richtungen

`store(0)` statt `fetch_and(!RW_WRITER)` kostet unter **Loom** 3 von 6 Beweisen, unter **Kani** 1 von
8. Das ist kein Widerspruch, sondern der Ertrag: Loom zeigt, dass *ein* Interleaving die Zahl 2
verfehlt; Kani zeigt, dass die Leserzahl für **jeden** der 2^31 möglichen Werte verlorengeht.
Selbst nachgemessen: **7 verified, 1 failure**, danach wieder 8/0.

### Der Inert-Check deckte nur eine Crate ab

Er soll belegen, dass `cfg(kani)` den Normalbau nicht bricht — und kopierte dafür ausschließlich
`sel4lake-loader`. `sel4lake-sync` trägt seit B-7.1/B-7.2 **zwei** externe cfgs (`kani` *und*
`loom`). Der Kernel-Build fängt das mit ab, aber nicht als benannte Zusicherung, und ein Schutz, den
niemand ausspricht, fällt beim nächsten Umbau unbemerkt weg. Jetzt ist er dabei; lokal
nachgestellt: `Finished`.

### Was weiterhin NICHT abgedeckt ist

* **Die IRQ-Maskierung — auch von Kani nicht.** Kani läuft auf dem Host-Target, dort ist
  `IRQ_MASKING_IMPLEMENTED == false` und die Maskierung ein No-Op. Der x86-Fehler vom 2026-07-29 war
  ein **`cfg`-Auswahlfehler**; weder Kani noch Loom kann ihn sehen, weil beide ein Ziel bauen, auf
  dem der No-Op-Zweig richtig ist. Gehalten wird das allein von der Übersetzungszeit-Zusicherung
  `#[cfg(target_os = "none")] const _: () = assert!(IRQ_MASKING_IMPLEMENTED, …)` — ein
  *Compile*-Gate, kein Beweis-Gate. Genau so steht es jetzt auch im Code.
* **Die Leserzahl-Grenze ist eine Grenze, keine Unmöglichkeit:** bei `RW_WRITER - 1` gleichzeitigen
  Lesern kippt die Eigenschaft (bewiesen). Praktisch unerreichbar, aber jetzt benannt statt
  stillschweigend vorausgesetzt.
* Kani bleibt **single-threaded**; echte Gleichzeitigkeit deckt Loom ab.

---

## B-5.5. Der Prüfer war begrenzt, `revoke` nicht

**Erledigt 2026-08-02** (parallel bearbeitet, integriert; Kernel-Seite hier ergänzt).

### Der Befund stand genau verkehrt herum

| Stelle | begrenzt? |
|---|---|
| `audit_cdt`, Geschwister- und Elternkette | **ja** (`steps > nslots`) |
| `revoke`, Abstieg zum Blatt | **nein** |
| `revoke`, äußere Löschschleife | **nein** |
| `move_cap`, Kinder umhängen | **nein** |
| `child_count` | **nein** |

Ausgerechnet der **Prüfer** war gegen einen zyklischen CDT geschützt. Er läuft auf Anforderung.
`revoke` läuft auf **Mandantenwunsch** — und unter der CAPS-Sperre. Aus einer Datenstrukturanomalie
wäre damit kein Latenzproblem geworden, sondern ein stehender Knoten.

Dazu: ein Kindindex außerhalb der Tabelle war ein Kernel-Panic aus Mandantendaten (`slots[i]` statt
`slots.get(i)`).

### Die Schranke ist hergeleitet, nicht gegriffen

Ein azyklischer Lauf besucht keinen Slot zweimal — also `slots.len()`. Das ist dieselbe Zahl, die
`audit_cdt` seit jeher nimmt; sie steht jetzt als **eine** benannte Quelle da statt zweimal
eingestreut.

Ein Überlauf **bricht ab und wird gezählt**, statt stillschweigend abzuschneiden: ein unvollständiges
`revoke` lässt Abkömmlinge am Leben, die weg sein sollten. Das ist kein Latenzbefund, sondern ein
Autoritätsbefund — deshalb prüft der Kernel ihn jetzt als `cdt_audit`-**Code 9**. Ohne diese Prüfung
wäre die Zählung vorhanden und die Prüfung nicht; genau der Zustand, den dieselbe Datei bei A-3.3
schon einmal hatte.

### Als Operationszahl, nicht als Zeit

    cdtlen : Abstieg 1/80256 Schritte, Revoke 4/80256 Loeschungen

Erst der **Abstand zur Schranke** macht „begrenzte kritische Sektion" zu einer Aussage. Zeit wäre
das falsche Maß: sie hängt an Taktrate und Emulation und sagt auf Blech, unter KVM und unter TCG
jeweils etwas anderes.

### Sensitivität — und warum ein Test „terminiert" heißt

| Mutation an der echten Schranke | Ergebnis |
|---|---|
| `steps > limit` → `steps > limit * 4` | 5 passed, 1 failed |
| Prüfung **entfernt** (Zustand vor B-5.5) | der Test „terminiert auf einem Zyklus" **hängt**, nach 60 s abgebrochen |
| unverändert | 6 passed, 0 failed |

Der zweite Fall ist der eigentliche Beleg. **Dass der Test überhaupt zurückkehrt, ist das Ergebnis** —
ohne Schranke läuft dieselbe Schleife endlos, im Kernel unter der CAPS-Sperre.

### Nebenertrag: sechs Tests, die nirgends liefen

`sel4lake-cap` hatte **keinen** Host-Test-Pfad. `cargo test -p sel4lake-cap` scheitert am
erzwungenen Custom-Target (`build-std`), und kein Skript baute die Crate außerhalb des Workspace.
Die sechs neuen Tests wären also entstanden und nie gelaufen.

`tools/host-tests.sh` sammelt jetzt alle reinen Crates an einem Ort — abhängigkeitsfreie über
`rustc --test` (Sekunden), solche mit Abhängigkeiten über ein Wegwerf-Projekt im TMPDIR:

    mem 22 · part 14 · fat 20 · cap 6   →  == HOST-TESTS: ALL PASS ==

Ein Test, der nirgends läuft, ist kein Test, sondern eine Absichtserklärung.

### Offen gelassen, benannt

`revoke` gibt bei Überlauf weiterhin `Ok(())`. Ein eigener `CapError` wäre die stärkere Antwort und
wäre machbar (der Kernel matcht nirgends erschöpfend auf `CapError`) — aber das ändert eine
öffentliche Aufzählung und ist eine Entscheidung für den Kernel, nicht für eine Crate-Änderung.
Bis dahin trägt Code 9 die Aussage.

---

## B-5.1. Die Abrechnung hing am Tick

**Der Zustand vorher war schlimmer als die Notiz vermutete.** `grep cycles` in
`crates/sel4lake-sched/`: kein Treffer. Belastet wurde ausschliesslich in `on_tick`, und auch dort
nur bei `tick == true`; `block_current`, `switch_to` und der YIELD-Pfad rechneten **gar nichts** ab.
Die Verzerrung war damit nicht „bis zu 10 ms je Umplanung", sondern **vollstaendig**: ein Thread,
der 9,9 ms rechnet und dann blockiert, zahlte **null**. Wer das systematisch tut, rechnet dauerhaft
umsonst — und der Nachbar, der zufaellig beim Tick lief, zahlte dessen Anteil mit. Fuer eine Cloud,
die CPU-Zeit verkauft, ist das kein Rundungsfehler, sondern ein Abrechnungsfehler mit Methode.

**Ein schnellerer Tick waere die falsche Antwort** — er erhoeht Aufloesung *und* Overhead. Ein
Stempel beim Ein- und Auslasten erhoeht nur die Aufloesung.

### Der Schnitt: die Uhr gehoert dem Kernel, die Rechnung nicht

Die Arithmetik liegt **abhaengigkeitsfrei** in `crates/sel4lake-sched/src/cycles.rs` und wird als
Datei auf dem Host geprueft (`tools/host-tests.sh cycles`, 9 Tests). `sel4lake-sched` als Ganzes
haengt an `sel4lake-hal` (arch-Asm) und wird auf dem Host nie bauen; laese das Modul die Uhr selbst,
waere jede Falle nur auf einer bestimmten Maschine ausloesbar statt mit einem Literal.

### Die drei Fallen

* **Rueckwaertssprung ist ein Fehler, kein Wrap.** `wrapping_sub` waere die naheliegende Zeile und
  die teuerste: ein Zaehler, der um 100 Zyklen zurueckspringt, ergaebe eine Differenz von rund
  `2^64` — das Konto ist sofort und dauerhaft erschoepft, aus 20 ns Messfehler wird ein Thread, der
  nie wieder laeuft. Aus **einem** Stempelpaar laesst sich Wrap nicht von Ruecksprung trennen; man
  muss sich entscheiden, und die Entscheidung ist eindeutig: 64 Bit bei 5 GHz wrappen nach ~117
  Jahren, ein nicht-invarianter Zaehler springt beim ersten Frequenzwechsel.
* **Plausibilitaetsgrenze** `2^40` (~3 min bei 5 GHz), bewusst grosszuegig: sie soll kaputte Proben
  fangen, nicht lange. Zu eng gezogen verfaelschte sie die Abrechnung nach unten — derselbe Fehler
  wie die Ticks, nur andersherum.
* **Migration.** Der Kern wird **mitgestempelt** und **vor** der Zahl geprueft. Ein Zyklenzaehler ist
  nur innerhalb eines Kerns eine Zeitachse; ueber einen Kernwechsel ist die Differenz keine Dauer,
  sondern eine Zufallszahl.

`Source::Untrusted` ist die Vorgabe: ohne ausdrueckliche Invarianz-Zusage wird **nichts**
abgerechnet, und `CycleStats::measurable()` trennt „hat nicht gerechnet" von „konnte nicht messen".
`consumed == 0` ohne gueltige Probe ist eine Leerstelle, keine Null — dieselbe Form wie die
Sprechprobe der IOMMU-Einheiten.

### Im Kernel: ein Zaehlerstand, nicht zwei

`system::charged()` klammert jede Umplanung (`on_tick` beide Pfade, `block_current`, `switch_to`,
`exit_current`). Es liest die Uhr **einmal**. Zwei Lesungen waeren die naheliegende Fassung und
hinterliessen ein Loch: die Zyklen der Umplanung selbst laegen zwischen den Stempeln und gehoerten
niemandem, die Summe aller Konten waere systematisch kleiner als die verstrichene Zeit — also genau
der Fehler, gegen den B-5.1 antritt, nur kleiner. Mit einem Stempel ist die Achse **lueckenlos**
aufgeteilt, und das ist nachpruefbar. Der Preis ist benannt: die Kosten der Umplanung traegt der
ankommende Thread. Das ist eine Zuordnungsentscheidung, keine Messungenauigkeit.

### Die Pruefung — und ein Loch im ersten Entwurf

```
cycacct : ALL PASS -- B-5.1: 63 Zyklenproben gegen 39 Ticks (1127396473 Zyklen verbucht;
          verworfen: rueckwaerts=0 unplausibel=0 Kernwechsel=0 Quelle-nicht-zugesichert=0;
          Maschine invariant=true)
```

`Proben > Ticks` **ist** die Aussage: die Tick-Rechnung kann hoechstens einmal je Tick belasten,
jede weitere Probe ist Rechenzeit, die vorher niemand zahlte.

Der erste Entwurf waehlte den Zweig danach, ob `rejected_source == 0` war. Damit haette ein
**vergessenes `set_cycle_source`** den Test bestanden: alles landete im Ablehnungszaehler, der
Untrusted-Zweig war erfuellt, und der Bericht meldete gruen fuer einen Kernel, der gar nicht
abrechnet. Die Sollgroesse muss von aussen kommen — jetzt entscheidet `hal::timer::invariant_tsc()`,
was die Maschine kann, und der Kernel muss sich daran messen lassen.

### Sensitivitaet (echte Mutationen, x86-Suite)

| Mutation | Ergebnis |
|---|---|
| M1: `set_cycle_source` entfernt | **FAILURES** — 0 Proben, `Quelle-nicht-zugesichert=86`, Maschine invariant=true |
| M2: Klammerung nur noch am Tick (`block_current`/`switch_to`/YIELD roh) | **FAILURES** — **39 Proben gegen 47 Ticks** |
| M3: `stamp_current` entfernt | **FAILURES** — 0 Proben, 0 Zyklen |
| unveraendert | ALL PASS |

M2 ist der aussagekraeftigste: weniger Abrechnungsereignisse als Ticks — genau der Zustand vorher.

### Offen

`consumed_cycles(tid)` liefert die Zahl, aber es gibt noch **keine Monitoring-Cap**, ueber die ein
Mandant sie abfragen kann; das gehoert zu B-6.1. Und `next_refill` bleibt tickbasiert: stellt B-5.2
die Verdraengung auf Zyklen um, muss es mitwandern, sonst gibt es zwei Zeitachsen fuer dasselbe
Budget.

**Nebenbefund am Werkzeug:** `tools/host-tests.sh` meldete ein **fehlendes** Ziel im selben Wortlaut
wie ein **kaputtes** („liess sich nicht uebersetzen"). In einem unvollstaendigen Baum las sich das
wie ein kaputtes Projekt, und umgekehrt haette sich ein wirklich kaputtes Ziel als „ist halt nicht
da" abtun lassen. Ein fehlendes Ziel ist jetzt ein eigener Ausgang — es faellt durch, sagt aber,
warum.

---

## D5. Eine rote Zeile, die niemand ansah

Der aarch64-Kernel meldete `root : FAILURES (NoManifest)`, und `test-qemu.sh` prueft diese Zeile
**gar nicht**. Ein Urteil, das niemand ansieht, unterscheidet nicht mehr zwischen „wie immer" und
„gerade gebrochen" — derselbe Fehler wie `-no-shutdown` (rc war immer 124), nur mit umgekehrtem
Vorzeichen.

**Behoben:** `test-qemu.sh` erzeugt den Manifest-Schluessel **vor** dem Build (die oeffentliche
Haelfte wird in `kernel/src/manifest_keys.rs` einkompiliert), signiert `init` fuer aarch64
(`certs/init-arm.cert` — das x86-Zertifikat traegt hier nicht, anderer Hash), baut ein Manifest mit
genau einem `root`-Eintrag und legt `init` als **erstes** Archivmodul ab. `main.rs` ruft
`manifest_report()` wie die x86-Seite. Geprueft werden jetzt `archive : 11 Modul`,
`manifest : ALL PASS` und `root : ALL PASS`.

### Fund 1: die Startmenge stand im falschen Dokument

`boot_arg` gab dem Root-Task `archive.count()`. Das Archiv ist ein **Behaelter**, das Manifest ist
das **Autoritaetsdokument** — und auf x86 fiel der Unterschied nie auf, weil dasselbe Skript beide
erzeugte. Zwei Zahlen, die uebereinstimmen, weil sie aus derselben Hand kommen.

Auf aarch64 liegen zehn Testdienste im Archiv, die nicht zur Startmenge gehoeren. `init` haette
`count = 11` bekommen und sie als Startmenge geladen: die adversarialen Dienste ein zweites Mal,
und `probe`, das gar kein ELF ist.

Die Zahl kommt jetzt aus dem Manifest. Die dadurch noetige Zusage — der Root-Task spricht die
uebrige Startmenge ueber einen **Archivindex** an (`SYS_LOAD`), bekommt ihre **Groesse** aber aus
dem Manifest, also muessen die Manifest-Eintraege `0..n` auf den Archivpositionen `0..n` liegen —
ist eine **gepruefte Regel** und keine Gewohnheit: `RootTaskError::StartSetNotPrefix`, fail-closed.

Negativkontrolle: `init` an Archivposition 1 statt 0 →
`root : FAILURES (StartSetNotPrefix)`.

### Fund 2: `loadstop` mass eine globale Baseline mit einer lokalen Sperre

Der L4-Test vergleicht globale Zaehler (freier Speicher, freie VSpaces, Kernel-Stacks) vor und nach
laden+abbauen — abgesichert mit `local_irq_disable()`, das aber nur **einen** Kern stillstellt.
Sieben andere und der Einsammler laufen weiter.

Solange dort sonst nichts passierte, ging das gut. Mit dem Root-Task passiert etwas: `init` beendet
sich, und seine Freigabe faellt gelegentlich mitten in die Messung. Gemessen:

```
DBG loadstop: loaded=true free 4204597248->4204613632 vsp 4085->4085 kstack 9989->9989
```

**16 KiB mehr** hinterher — kein Leck, ein fremder `free`. Die Zeile meldete trotzdem FAILURES, und
weil `all_done()` auf dem Bestehen dieses Tests besteht, sah es aus wie ein Haenger.

Der Punkt ist allgemeiner als der Root-Task: eine Gleichheitsaussage ueber eine geteilte Groesse
braucht ein Fenster, in dem niemand sonst daran schreibt. Die lokale IRQ-Sperre war nie dieses
Fenster — sie sah nur so aus. Jetzt wird erst **Ruhe festgestellt** (zwei gleiche Proben in Folge)
und dann gemessen; ein Lauf, der nie ruhig wird, faellt **durch** und sagt das auch so („nicht
messbar" ist kein bestandener Test). Ein echtes Leck faellt weiterhin durch — es steht in jeder
Wiederholung in den Zahlen.

### Nachgemessen, nicht angenommen

`todo.md` hatte ausdruecklich gewarnt: ein laufender Root-Task ist ein zusaetzlicher Thread.
`RUNS=6 ./test-qemu.sh` → **6 von 6** mit identischer Ergebnissignatur, `== ALL PASS ==`.
x86-Suite und x86-Lade-Suite ebenfalls `== ALL PASS ==` (beide fahren denselben geaenderten
`loader.rs`-Pfad).

---

## A-5.3. Die Zuteilung stand im Enumerator, nicht im Manifest

Bis A-5.2 sagte das Manifest ueber Geraete-Autoritaet nur `mmio,dma` -- „diese Komponente darf ein
Registerfenster und eine DMA-Region halten". **Welches** Geraet das ist, entschied der Kernel, und
zwar nach Fundreihenfolge. Der Kommentar an `DRIVER_DEVICE` sagte das offen und zog die richtige
Konsequenz: es wurde **genau eines** angeboten, weil jede Auswahl unter mehreren eine im Kernel
versteckte Politik gewesen waere.

A-5.3 nimmt dieser Begruendung die Grundlage, statt sie zu umgehen.

### Der Selektor

Der Manifest-Eintrag traegt jetzt `vendor`/`device`/`class` (je einzeln „beliebig") in **8 der 12
reservierten Bytes**. `ENTRY_LEN` bleibt 96 -- das Format ist eingefroren, und jedes bestehende
signierte Manifest bleibt gueltig.

**Die Rueckwaertsfalle wurde ausdruecklich gestellt und entschaerft.** Ein vor A-5.3 erzeugtes
Manifest hat dort Nullen. Waeren die als `vendor=0 device=0 class=0` gelesen worden, passte der
Selektor auf **kein** Geraet -- die Formaterweiterung haette jedem alten Dokument still die
Geraete-Zuteilung weggenommen, und zwar mit derselben Meldung wie ein echter Fehlgriff. Null heisst
deshalb „nicht gesetzt" und wird auf `ANY` abgebildet; ein Host-Test haelt das fest.

**Keine Instanz.** Kein Bus, kein Geraet, keine Funktion. Ein Manifest, das `00:04.0` nennt, ist an
die Topologie EINER Maschine gebunden und auf der naechsten stillschweigend falsch -- es zeigte
dann auf ein anderes Geraet, nicht auf keines. Der Selektor benennt eine **Art**; welche Instanz
das ist, sieht nur der Kernel, weil nur er enumeriert.

**Fail-closed.** Passt kein Geraet, gibt es keines. Der Rueckfall „nichts passt → nimm irgendeins"
waere die bequeme Zeile und genau die versteckte Politik, gegen die der Selektor antritt, nur eine
Ebene tiefer.

### Die Pruefung -- und warum die naheliegende nichts taugt

„Der Treiber hat ein Geraet bekommen" ist wertlos: die Aussage ist auch dann wahr, wenn er das
falsche bekam. Geprueft wird deshalb dreierlei, und der erste Punkt ist der wichtigste:

1. **Es gab ueberhaupt etwas zu entscheiden.** Bei nur einem angebotenen Geraet trifft jeder
   Selektor dieselbe Wahl -- weniger als zwei Angebote sind ein **SKIP mit Begruendung**, kein PASS.
   Die Lade-Suite startet dafuer jetzt zusaetzlich eine `virtio-net-pci`.
2. Die Zuteilung passt auf den Selektor **des Manifest-Eintrags** (gelesen aus dem Dokument, nicht
   aus dem Kernel-Zustand).
3. **Das nicht gewaehlte Geraet ist noch frei.** Erst das macht aus „es passte" ein „es wurde
   ausgewaehlt".

```
devsel  : ALL PASS (A-5.3: das Manifest verlangt 1af4:1042, zugeteilt wurde RID 0x0018 1af4:1042;
          2 angeboten, 1 vergeben, 1 liegen geblieben)
```

### Zwei Negativfaelle in der Suite, zwei Mutationen im Code

| Fall | Ergebnis |
|---|---|
| Selektor `vendor=dead,device=beef` (unerfuellbar) | **keine Zuteilung**, `0 vergeben` -- kein Ersatzgeraet |
| Selektor `vendor=1af4,device=1041` (die **Netzkarte**) | `zugeteilt wurde RID 0x0020 1af4:1041` -- obwohl das Blockgeraet in der Angebotsliste DAVOR steht |
| Mutation: „nichts passt → nimm irgendeins" | Negativfall 4 **faellt** |
| Mutation: Selektor ignorieren (`ANY` statt `e.device`) | Negativfall 4 **und** 5 fallen |

Negativfall 5 ist der aussagekraeftigste: er trennt „es passte" von „es war ohnehin das erste".

### Ein Fehler unterwegs, der sich lohnte

Der erste Entwurf berichtete direkt nach `start_root_task_reported()` -- und sah `0 Zuteilungen`.
Der Grund: die Funktion **laedt** den Root-Task, sie fuehrt ihn nicht aus. Die Treiber-PD entsteht
erst, wenn `init` selbst laeuft und `SYS_LOAD` ruft. Ein Bericht an dieser Stelle haette einen
funktionierenden Kernel fuer kaputt erklaert.

### Nebenertrag am Werkzeug

`sel4lake-loader` hat **49** `#[test]`s -- und lief in keinem Skript. Es gibt kein
`.github/workflows/` in diesem Baum, und Kani prueft Beweise, keine `#[test]`s; das ist nicht
dasselbe. Jetzt in `tools/host-tests.sh` (Gesamtstand: mem 22 · part 14 · fat 20 · cycles 9 ·
loader 49 · cap 6).

### Was NICHT belegt ist

Der Kernel findet die *Kandidaten* weiterhin ueber `hal::pcie::find(VIRTIO_VENDOR, …)`, weil die
BAR-Bestimmung durch den virtio-Faehigkeitslauf geht. Die **Auswahl** steht sauber im Manifest, das
**Angebot** noch nicht.

Und: es laeuft immer nur **eine** Treiber-PD. Dass zwei zugeteilte Geraete **voneinander** isoliert
sind, ist damit nicht belegt -- der Fall, der es widerlegen koennte, kommt gar nicht vor. Dieselbe
Form wie `virtio-rng` vor A-5.2. Steht als **A-5.4** in `todo-A-ausfuehren.md`.

---

## B-4.5 (Teil 2). Der Prime+Probe war nicht reproduzierbar — und die Ursache war NICHT der Allokator

`todo-B-verlaesslichkeit.md` notierte als Nebenbedingung: der Aufbau brauche „~50 MiB in 800
gefärbten Einzelregionen" und trage damit „an die Fragmentgrenze des Allokators"; dort sei er
nicht reproduzierbar (54–179 Zyklen bei identischem Aufbau, `balanciert` kippte). **Nachgemessen
am 2026-08-02 stimmt der Befund, die Ursache aber nicht.**

### Was der Aufbau tatsächlich tat (8 Aufbauten in einem Boot, x86 unter KVM)

Der Aufbau war **byte-identisch reproduzierbar**: in allen Läufen dieselben Physadressen
(`vbase=0x3b40000 dbase=0x3b50000 gbase=0x4540000`), dieselbe Fragmentzahl davor (4) und danach
(4), dieselbe freie Summe. Der Fragment-Höchststand lag bei **419 von `MAX_FRAGMENTS = 1024`**,
`balanciert` war in **8 von 8** wahr, und jede der 800 Regionen war nach dem Abbau nachweislich
frei (`region_fully_free`, 800 von 800 geprüft).

Die Zahlen streuten trotzdem: `disjunkt` = 147, 141, 141, **45**, 134, 135, 189, 140. Ein
identischer Aufbau kann keine Streuung erzeugen — **die Ursache lag also in der Messung.**
Gefunden wurden drei, alle drei derselben Sorte wie die zwei bereits im Code dokumentierten
Fallen: eine Messung, die nicht messen *kann*, was sie zu messen behauptet.

### Ursache 1: Der Angreifer lief linear — und ein linearer Strom verdrängt nichts

Falle 1 („ein linearer Durchlauf misst den Vorauslader") war nur für das **Opfer** behoben. Für
den Angreifer stand im Code ausdrücklich: „hier genügt ein linearer Lauf: er soll den Cache
*füllen*, nicht ihn messen." Das ist auf jedem Cache mit verdrängungsresistenter Einlagerung
falsch — ein rein sequenzieller Strom ist genau das Muster, das als „kein Wiedergebrauch"
erkannt und an der LRU-Position eingelagert wird. Gemessen, 10 Aufbauten mit identischer
Zuteilung, Angreifer 24 MiB gegen 16 MiB LLC:

| Angreifer | Positivkontrolle trug in |
|---|---|
| linear (bis 2026-08-02) | **3 von 10** |
| bit-umgekehrt | **10 von 10** |

Im nicht tragenden Fall lag `gleichfarbig` bei 43 gegen `ungestoert` 38 Zyklen: der Angreifer war
schlicht wirkungslos. Der Angreifer läuft jetzt in derselben bit-umgekehrten Reihenfolge wie die
Zeigerkette des Opfers — gleich viele Speicherzugriffe, andere Reihenfolge.

### Ursache 2: Das Minimum ist für zwei von drei Messpunkten der falsche Schätzer

Im Code stand: „gewertet wird das Minimum. Störungen können eine Messung nur verlängern, nie
verkürzen." Für `base` stimmt das (reine Latenz). Für `shared` und `disjoint` ist die gesuchte
Größe die **Verdrängung**, und jede Störung, die den Angreifer bremst oder den Arbeitssatz
teilweise stehen lässt, macht die Messung *kürzer*. Das Minimum greift also systematisch in genau
die Richtung, die die Positivkontrolle zerstört. Gemessen (dieselben 10 Aufbauten, 16 Proben):

| Schätzer | Positivkontrolle trug in |
|---|---|
| Minimum | **4 von 10** |
| Median | **10 von 10** |

Gewertet wird jetzt der Median; gemeldet werden **Minimum/Median/Maximum** für alle drei
Messpunkte. Eine Messung, die ihre eigene Streuung nicht kennt, taugt für eine Blech-Aussage
nicht. Dazu kommt ein **Auflösungs-Gate**: überlappen die Verteilungen von `ungestoert` und
`gleichfarbig` (`q3 >= q1`), ist der Medianvergleich eine Zahl ohne Aussage → SKIP mit den
Quartilen, kein Urteil.

**Ehrlich dazu:** die Mutation „wieder Minimum statt Median" fiel nach den beiden anderen
Korrekturen in 6 von 6 Läufen **nicht** mehr durch — mit richtig dimensioniertem Opfer und
gestreutem Angreifer ist auch das Minimum weit über der Schwelle. Der Median bleibt, weil seine
Verzerrung nachgewiesen und einseitig ist, nicht weil er hier noch etwas rettet.

### Ursache 3: „Das Opfer überschreitet den L2" war eine feste Zahl, kein abgeleiteter Wert

Falle 2 war 2026-08-01 mit `VICTIM_BYTES = 2 MiB` und der Begründung „die Messmaschine hat
~1,5 MiB L2 je Kern" behoben — also mit einer Konstanten, die auf genau einer Maschine gilt. Auf
der heutigen Messmaschine meldet die Ebene unter dem LLC **4 MiB**; das 2-MiB-Opfer lag damit
wieder vollständig privat (`ungestoert` = 38–40 Zyklen, L2-Latenz), und der *farblich disjunkte*
Angreifer räumte es von dort heraus — was er darf, denn Färbung partitioniert nur den LLC.
`effect` wäre selbst bei perfekt wirkender Färbung durchgefallen.

Die Opfergröße wird jetzt aus der Geometrie abgeleitet, und zwar gegen **beide** Schranken:

```text
    Größe der nicht partitionierten Ebene   <   Opfer   ≤   LLC / PARTITIONS
```

Dafür ist `hal::cache::below_llc()` neu (x86 aus `CPUID.4`, aarch64 aus `CLIDR_EL1`/`CCSIDR_EL1`,
beide über dieselbe Zerlegung wie `llc()` — eine zweite Fassung wäre die Doppelung, an der die
Farbarithmetik hier schon einmal auseinandergelaufen ist).

**Das Fenster kann leer sein, und auf dieser Maschine ist es das:** 4 MiB private Ebene gegen
16 MiB / 4 Partitionen = 4 MiB Farbanteil. Untergrenze = Obergrenze, kein gültiges Opfer. Das ist
keine Schwäche des Tests, sondern eine **Aussage über die Maschine**, und sie gehört neben A1:
wo eine nicht partitionierte Cache-Ebene so groß ist wie ein ganzer Farbanteil des LLC, kann
Färbung mit dieser Streifenzahl nichts schützen, was nicht ohnehin privat liegt. Der Test meldet
das mit beiden Zahlen, statt ein Urteil zu erfinden.

**Für den noch offenen Blech-Lauf ist das die konkrete Anforderung an die Maschine:**
`L2 < LLC / PARTITIONS`. Auf einem Server-Xeon (1–2 MiB L2, 32+ MiB LLC) ist sie erfüllt; auf
einem hybriden Notebook-Kern mit 4 MiB E-Core-L2 nicht.

### Der Aufbau kommt nicht mehr aus 800 `alloc_colored`-Aufrufen

Die Fragmentierung war **nicht** die Ursache der Streuung — aber die Bauart trug drei stille
Risiken, und sie sind mit demselben Schnitt weg:

* Der Fragment-Höchststand (419) skaliert mit `1/region_bytes`: auf einer Maschine mit 16 statt
  256 Farben wären es viermal so viele, und `MAX_FRAGMENTS` ist 1024.
* `MAX_ATTACKER = 384` war auf der Messmaschine **exakt erreicht**. Jede Maschine mit größerem
  LLC hätte einen stillschweigend zu kleinen Angreifer bekommen — also eine stillschweigend
  geschwächte Positivkontrolle. Der Bericht nennt die Angreifergröße jetzt in Prozent des LLC.
* Die drei Regionslisten lagen als lokale Arrays auf dem Kernel-Stack: **12,8 KiB Rahmen bei
  16 KiB Stack**. Dieselbe Falle, die `ReplyFinal` schon einmal gestellt hat.

Jetzt kommt der Speicher aus wenigen großen, **ungefärbten** Blöcken (8 MiB, auf der Messmaschine
14 Stück), und die farbreinen Läufe werden darin **gesucht** — mit `color_of` und derselben Maske,
die auch der Allokator befragt. Das ist die stärkere Aussage: die Farbe jeder benutzten Seite wird
an der Verwendungsstelle nachgerechnet, statt einem `alloc_colored` geglaubt zu werden. Neu ist
auch eine ausdrückliche Prüfung `farbtreu`: Opfer und gleichfarbiger Angreifer müssen Farben
**teilen**, der disjunkte Angreifer mit keinem von beiden eine — sonst messen die drei Messpunkte
nicht, was ihre Namen sagen.

### Zwei Fehler, die erst die Sensitivitätsprüfung gefunden hat

**Der Bilanzprüfer prüfte nur, was er selbst getan hatte.** Erster Entwurf: `region_fully_free`
innerhalb der Freigabeschleife. Gegenprobe (Schleife um einen Block verkürzt): `bilanz=1`, Lauf
grün, 8 MiB weg. Ein Prüfer, der nur das prüft, was er selbst angefasst hat, kann ein **Vergessen**
nicht sehen. Die geholten Spannen werden jetzt getrennt von den Caps geführt und in einer eigenen
Schleife geprüft; dieselbe Mutation meldet danach
`pprobe : FAILURES (Speicher NICHT vollstaendig zurueck ...)`.

**Ein halbe Sekunde IRQ-Sperre kippt andere Tests.** `SpinLock::lock()` maskiert Interrupts für
die gesamte Lebensdauer des Guards; der Arena-Guard lebt über Aufbau, Messung und Abbau. Mit einem
zwischenzeitlich 8 MiB großen Opfer waren das rund 1,5 s ohne Timer-Tick auf dem Bootkern — der
Lauf endete im `bringup : WATCHDOG` mit `ipc : FAILURES`. Der Prime+Probe hatte einen Test
gekippt, der mit ihm nichts zu tun hat; genau deshalb ist der Farbtest auf aarch64 aus
`spawn_demo` ausgehängt. Zwischen zwei Proben werden die Interrupts jetzt kurz aufgemacht — hier
zulässig, weil `PP_ARENA` von genau einer Stelle genommen wird und kein Interrupt-Pfad sie zieht.

### Und: die Zeile wurde von keiner Suite gelesen

`pprobe` stand seit 2026-08-01 in jedem x86-Log und kam in **keinem** Testskript vor. Seit
2026-08-02 prüft `test-qemu-x86.sh` alle drei Ausgänge — `FAILURES` ist ein FAIL, das **Fehlen**
der Zeile ebenfalls (dann ist der Hochlauf vorher stehengeblieben), und im SKIP-Fall wird
zusätzlich verlangt, dass `farbtreu=1 bilanz=1` gilt: ein SKIP, der über den Aufbau nichts sagt,
wäre eine Aussage über gar nichts.

### Sensitivität (Ausgaben im Wortlaut in der Sitzungsnotiz)

Fünf Mutationen am eigenen Code: vergessene Freigabe → `FAILURES (Speicher NICHT vollstaendig
zurueck)`; „disjunkter" Angreifer aus dem Opferstreifen → `FAILURES (Farbwahl gebrochen)`;
Urteilspfad erzwungen → `FAILURES` (unter KVM schützt die Farbe nicht — richtig so); Urteilspfad
erzwungen **und** disjunkter Angreifer stillgelegt → `ALL PASS` (der Zweig kann beide Ausgänge);
linearer Angreifer → `SKIP -- die Positivkontrolle traegt nicht` in 2 von 6 Läufen, mit wechselndem
Grund von Lauf zu Lauf. Die fünfte (Minimum statt Median) fiel **nicht** durch, s. o.

### Zahlen nachher

10 Boots, x86 unter KVM, identischer Aufbau:
`ungestoert` Median 79–88, `disjunkt` Median 174–271, `gleichfarbig` Median 183–247;
Positivkontrolle in 10 von 10, `farbtreu`/`bilanz` in 10 von 10, **10 von 10 mit identischer
Ergebnissignatur**. Acht Aufbauten in einem Boot: `disjunkt` Median 189–215 (±7 %) gegen vorher
45–189 (Faktor 4,2). `RUNS=5 ./test-qemu-x86.sh` → `== ALL PASS ==`, 5 von 5 identische Signatur.
Bootzeit 0,9 s → 1,3–1,6 s (KVM) bzw. 2,8 s (TCG): der Prime+Probe läuft jetzt bei **jedem** Start
statt nur am Blech-Tag.

**Nachtrag beim Zusammenfuehren (2026-08-02):** die `pprobe`-Zeile lief bis dahin gegen eine
**erfundene** Cache-Geometrie. QEMU meldet unter `-cpu host` ohne `host-cache-info=on` seine
Legacy-Deskriptoren (L3 16 MiB/16-fach, L2 4 MiB) statt der der Maschine. Der SKIP-Grund war
deshalb ein Aufbau-Artefakt ("das Opfer ueberschreitet die private Ebene nicht" -- 4096 gegen
4096 KiB), nicht die Sache. Mit dem Schalter in **beiden** x86-Suiten:

```
cache   : LLC L3 24576 KiB, 12-fach, 64 B/Zeile, 32768 Sets -> 512 Seitenfarbe(n)
color   : ALL PASS (zwei isolierte PDs teilen sich keine Cache-Farbe)
pprobe  : Opfer 4096 KiB (privat 2048, ueber-privat=ja), Angreifer 32768 KiB (133% des LLC)
          ungestoert=79/86/89 disjunkt=242/275/…
pprobe  : SKIP -- unter einem Hypervisor NICHT ENTSCHEIDBAR (CPUID.1:ECX[31]). Die
          Positivkontrolle traegt (gleichfarbig=281 vs ungestoert=86), der disjunkte Farbsatz
          schuetzt NICHT (disjunkt=275).
```

Der Zugewinn ist nicht bloss Genauigkeit: **512 ist keine 256.** Genau daran hing der
A1-Rest-Fehler -- `stripe` rechnete mit `MASK_BITS` statt mit der Farbanzahl und war bei 256
zufaellig richtig. Eine Suite, die nie etwas anderes als 256 Farben sieht, kann diese Klasse von
Fehlern grundsaetzlich nicht finden. `RUNS=3` → 3 von 3 identische Signatur, Lade-Suite ebenfalls
`== ALL PASS ==`.

---

## A1 / B-4.2 / B-4.5 auf aarch64. Drei Tests, die nicht liefen — und die man nicht vermissen konnte

Bis 2026-08-02 setzte `threads::spawn_demo` auf aarch64 `COLOR_OK`, `STRIPE_ALLOC_OK` und
`PPROBE_OK` **hart auf `true`**. Der Grund im Kommentar war echt: an jener Stelle, ganz am Anfang
von `spawn_demo`, belegen und geben die Tests Speicher frei, *bevor* die baseline-empfindlichen
Prüfungen ihre Ausgangswerte nehmen; danach fiel mal `captest`, mal `sched` durch. Der Ausweg war
falsch.

**Warum falsch: es gab keine Zeile.** Kein `color`, kein `stripe`, kein `pprobe` im ARM-Protokoll,
kein Check in `test-qemu.sh` — und drei dauerhaft wahre Konjunkte in `all_done()`. Ein Ausfall der
Färbung auf ARM hätte `== ALL PASS ==` gemeldet. Das ist nicht „ein Test fehlt", das ist „ein Test
besteht immer", und die zweite Form ist die gefährlichere: sie sieht aus wie Erfolg. Ein
weggelassener Test hinterlässt eine Lücke, die jemand bemerken kann; ein hart gesetztes `true`
hinterlässt eine Zusicherung, an die man sich gewöhnt.

### Die Lösung ist der Platz, nicht das Weglassen

Die drei Tests hängen jetzt als **letztes Glied der Testkette** in `demo_report_then_idle`
(`run_color_suite`), gegatet auf `cross` (letzter adversarialer Dienst), `strand` (letzte
Scheduler-Messung) und `loadstop` (Ressourcen-Bilanz). Danach nimmt keine Prüfung mehr eine
Baseline; was der Farbtest an der Freiliste anrichtet, kann niemanden mehr kippen. Der
Gate-Ausdruck steht als Zusicherung im Code: wer eine baseline-empfindliche Prüfung dahinter
einhängt, muss ihn ergänzen.

**Die Ruhephase aus D5 (`loadstop_quiet`) wird gemessen, aber NICHT als Tor benutzt** — und das
ist der interessantere Teil. `run_loadstop` braucht sie, weil es eine Gleichheitsaussage über
**globale** Zähler trifft; dort entscheidet ein fremder `free` im Messfenster über PASS oder FAIL.
Die Farbtests treffen diese Aussage nicht: sie prüfen ihre Bilanz **regionsgenau**
(`region_fully_free` je geholter Region), seit A1 diese Falle einmal bezahlt hat. Damit ist die
Ruhephase hier eine Diagnose: findet sich nach der gesamten Testkette *kein* ruhiges Fenster,
werkelt dort noch etwas, obwohl alles fertig gemeldet hat — das gehört ins Protokoll. Ein Tor
daraus zu machen wäre verkehrt: der Test schwiege dann ausgerechnet dort, wo etwas nicht stimmt.

### Was dabei herauskam — A1 gilt auf aarch64

    cache   : LLC L2 1024 KiB, 16-fach, 64 B/Zeile, 1024 Sets -> 16 Seitenfarbe(n)
    color   : 16 Farben, 4 Partitionen, Region 16 KiB · in_mask=1 kernelseite=1 … disjunkt=1
              uebergross_abgewiesen=1 bilanz=1
    color   : ALL PASS
    stripe  : ALL PASS

Das ist **die 16-Farben-Aufteilung** — genau der Fall, den die `MASK_BITS`-Verwechslung falsch
machte (Streifen 0 bekam alle 16 Farben, die Streifen 1–3 keine, und weil leere Mengen sich nicht
schneiden, meldete der Selbsttest „disjunkt"). Der Test, der den Fehler gefunden hat, belegt jetzt
seine Behebung auf derselben Maschine.

### Zwei Messfehler, die erst der ARM-Lauf sichtbar gemacht hat

**1. Die Uhr war zu grob, und der Test hat es nicht gemerkt.** `chase` meldete Zyklen **je
Kettenglied** (`dt / n`). Auf aarch64 ist `cycles()` `CNTPCT_EL0` — ein architektonischer Zähler
mit grober Granularität. Bei 4096 Gliedern ergab die Ganzzahldivision **0**:

    pprobe  : … ungestoert=0/0/0 disjunkt=0/0/1 gleichfarbig=0/0/1
    pprobe  : SKIP -- nicht aufloesbar: die Verteilungen … ueberlappen (q3=0 >= q1=0)

Das Auflösungs-Gate hat den Fall gefangen — mit der falschen Begründung. Gemessen wurde die
Granularität des Zählers, nicht der Cache. `chase` meldet jetzt die **Gesamtdauer**; alle
Vergleiche des Tests sind Verhältnisse und gelten dafür unverändert, nur mit `n`-mal mehr
Auflösung. Die vertraute Zahl „Zyklen je Glied" steht weiter im Bericht, mit einer Nachkommastelle
von Hand (`je_glied`) — `0` wäre dort keine Messgröße, sondern eine verlorene Aussage. Dazu ein
eigener Ausgang `clock_ok`: tickt der Zähler während eines ungestörten Durchlaufs überhaupt nicht,
sagt der Bericht das als **Befund über die Uhr**, statt es unter „nicht auflösbar" zu verbuchen.

**2. `run_stripe_alloc` wäre auf einer Maschine mit einer Farbe durchgefallen.** Seit `stripe` die
Farbanzahl kennt, liefert es unter zwei Farben korrekt `None` — der Test las das als „nur 0 von 4
Streifen vergebbar" und meldete FAILURES. Das wäre ein Fehlschlag über die *Maschine* gewesen, nicht
über die Belegungsführung, die dort geprüft wird. Jetzt SKIP mit Grund, dieselbe Trennung wie in
`run_color`: nicht durchführbar ist nicht durchgefallen.

### `pprobe` auf aarch64: wohlgestellt, und trotzdem SKIP

    pprobe  : Opfer 256 KiB (privat 32, ueber-privat=ja), Angreifer 1536 KiB je Farbsatz (150% des
              LLC), 1 Bloecke, Kette 4096 Glieder, 9 Proben · Zyklen je Durchlauf (min/median/max):
              ungestoert=2562/2611/5140 disjunkt=3447/3602/4845 gleichfarbig=2903/3032/5413
              · je Glied (Median): 0.6/0.8/0.7 · farbtreu=1 bilanz=1
    pprobe  : SKIP -- die Positivkontrolle traegt nicht (Median gleichfarbig=3032 vs
              ungestoert=2611)

Das Opfergrößen-Fenster ist auf ARM **nicht leer** (32 KiB private Ebene gegen 256 KiB Farbanteil
des 1-MiB-LLC) — anders als unter QEMU/x86, wo QEMUs synthetische Cache-Deskriptoren es zuklappen.
Der Test ist hier also wohlgestellt und scheitert an der einzigen Stelle, an der er scheitern
darf: TCG hat keinen echten Cache, die Positivkontrolle kann nicht tragen, und über den disjunkten
Fall ist damit nichts auszusagen. Aufbau, Farbwahl (`farbtreu=1`) und Bilanz (`bilanz=1`) sind
trotzdem geprüft — und `test-qemu.sh` verlangt genau das auch im SKIP-Fall: ein SKIP, der über den
Aufbau nichts sagt, wäre eine Aussage über gar nichts.

### Gemessen, nicht angenommen

8 Läufe `./test-qemu.sh` (der committete Stand kennt kein `RUNS=`, deshalb einzeln gefahren und
die Signaturen verglichen): **8 von 8 `== ALL PASS ==` mit identischer Ergebnissignatur**,
`color : ALL PASS`, `stripe : ALL PASS`, `pprobe : SKIP` in jedem Lauf — und **`captest` und
`sched` in allen acht Läufen unverändert grün**. Das war die eigentliche Bedingung: ein Test, der
andere Tests kippt, macht das gesamte Ergebnis unbrauchbar, und das wiegt schwerer als drei
zusätzliche grüne Zeilen.

---

---

## B-6.2: die Fehlerdomaene — und ein Panic, der nicht den Knoten reisst, sondern verschluckt wird

**Erledigt 2026-08-02.** Ergebnis: [docs/fehlerdomaene.md](docs/fehlerdomaene.md) (betreiberseitig),
Invariante §14 in [docs/invariants.md](docs/invariants.md).

Die **Festlegung** ist die billige Variante aus Z9, bewusst: **der Knoten ist die Fehlerdomäne,
Redundanz wird über Knoten gebaut.** Dazu die zweite Hälfte, die im Eintrag nicht stand: **alle PDs
im globalen SAS-Adressraum (`VSPACE_OF == 0`) bilden untereinander EINE Domäne.** Ihre Trennung
ruht auf intralingualer Sicherheit (Zertifikats-Gate ADR 0014) und nicht auf der Hardware —
gemessen ist das der Kontrast, den `iso`/`vspace` ohnehin schon prüften: der SAS-Faden **liest** die
fremde Probe-Adresse erfolgreich, die isolierte PD faultet an derselben. Damit ist „kein Kundencode
in TrustedSAS" keine Vorsichtsmaßnahme mehr, sondern die Konsequenz einer benannten Fehlerdomäne.

**Beim Aufschreiben fiel eine Ungenauigkeit auf, die man leicht übernimmt:** „TrustedSAS teilt
einen Adressraum" stimmt so nicht. `Domain::TrustedSas` ist eine Vertrauens*stufe* und darf global
**oder** isoliert laufen (`domain_audit` verlangt Isolation nur für `HardwareLand`/`UserLand`,
`crates/sel4lake-microkit/src/lib.rs:246–250`); **extern geladene** TrustedSAS-PDs bekommen heute
sogar immer eine eigene VSpace (`kernel/src/loader.rs:719–722`). Die Fehlerdomäne hängt also am
**Adressraum**, nicht am Etikett — und eine Zusicherung, die das Etikett nennt, wäre in beide
Richtungen falsch: zu streng für geladene Trusted-Dienste, zu lasch, falls je etwas anderes global
laufen dürfte.

### Die Prämisse des Todo-Eintrags war falsch — und die Wahrheit ist unangenehmer

Der Eintrag sagte: *„Ein Panic reißt heute den Knoten mit."* Gemessen (QEMU, x86_64 `-smp 4` unter
KVM, aarch64 `-smp 8`, je ein absichtlich ausgelöster Panic an definierter Stelle) stimmt das für
die **häufigsten** Fälle nicht:

* **Panic in einem Kernelfaden (EL1/Ring 0), Bootkern.** Der Lauf läuft **61 s weiter**, alle vier
  Kerne ticken (6102/6088/6084/6080), die gesunden Worker erreichen ~4900 Runden, der gepanickte
  steht bei 2 — `sched : Worker-Runden [4908, 4910, 2]`. `ipc`, `ring3`, `iommu`, `dmatok`,
  `quiesce`, `rebind`, `state`, `capsz` im selben Lauf: **ALL PASS**. **Auf aarch64 dasselbe**
  (8 Kerne, 6004–7006 Ticks, `worker 0 count=106773`, `worker 1 count=107023`, `worker 2 count=2`).
* **Panic auf einem Sekundärkern.** Prüfsignatur **Zeile für Zeile identisch** zum sauberen Lauf
  (31 Ergebniszeilen, `diff` leer), `== SELFTEST COMPLETE ==`, `rc=0`. Der gepanickte Kern tickt
  danach weiter (`sched : core 1 ticks=11`). **Ohne die Konsolenzeile wäre der Panic durch nichts
  nachweisbar.**

Die Ursache steht in zwei Zeilen: `panic.rs` ruft ein `halt()`, das die Interrupts **nicht
maskiert** — x86 `kernel/src/arch/x86_64/mod.rs:294` ist `loop { hlt }` **ohne `cli`** (die
HAL-Fassung `crates/sel4lake-hal/src/x86_64/cpu.rs:257` würde maskieren, der Panic-Pfad benutzt sie
nicht), aarch64 `loop { wfe }` mit unverändertem DAIF. Der nächste Timer-Tick holt den Kern in den
Scheduler zurück.

**Und für die anderen beiden Fälle stimmt der Satz — dort aber still:**

* **Panic unter gehaltener `MEM`-Sperre**: das Log endet mitten im Hochlauf, kein `smp : ... online`,
  **keine Watchdog-Zeile**, `rc=124`. Der Ticket-Lock (`crates/sel4lake-sync/src/lib.rs:170–178`)
  hat keine Schranke; `now_serving` steht für immer, und weil es ein *Ticket*-Lock ist, blockiert
  nicht nur der nächste Zieher, sondern jeder.
* **Panic im Steuerfaden des Bootkerns**: der Knoten läuft weiter, meldet aber nie wieder etwas —
  Notbremse und Abschlussbericht hängen an genau diesem Faden (`bringup.rs:1065–1067` sagt das
  selbst). **Von außen nicht von einem Deadlock zu unterscheiden.** Wer einen D0-Hänger untersucht,
  muss zuerst ausschließen, dass er einen Panic vor sich hat.

### Die eigentliche Lücke

Nicht „ein Panic reißt den Knoten", sondern: **derselbe Fehler hat vier verschiedene Ausgänge, und
welcher eintritt, hängt davon ab, WO er auftrat — nicht, wie schlimm er war.** Für ein
Sicherheitsprodukt ist der verschluckte Panic der schlechtere: ein Panic heißt, eine Invariante ist
nachweislich verletzt, und danach vergibt derselbe Kernel weiter Capabilities, teilt Speicher zu
und schaltet Adressräume um. Ein Absturz wäre ein *definierter* Zustand; „läuft weiter mit
verletzter Invariante" ist keiner.

Dieselbe Form wie die leere Event-Queue ohne `CD.R` und wie `virtio-rng`, das nur schreibt: eine
Aussage sah wahr aus, weil der Fall, der sie widerlegt, nie gelaufen war. Hier war der Fall trivial
herstellbar — ein `panic!()` und ein Bootvorgang.

### Doppelfehler: es gibt keinen Wächter

Ein Panic im Panic-Handler rekursiert unbegrenzt. Gemessen **362 Ebenen** auf einem 64-KiB-AP-Stack,
**ohne** `#DF` (Vektor 8 ist in `crates/sel4lake-hal/src/x86_64/exception.rs:494` nur *benannt*, es
gibt keinen IST-Stack dafür), **ohne** Schutzseite am Kernel-Stack. Der Lauf endete erst, als der
Bootkern die Maschine abschaltete — was jenseits des Stackendes passiert wäre, ist damit **nicht**
gemessen und steht in der Nicht-gemessen-Liste. Sichtbarer Nebeneffekt: die Ausgabe des sterbenden
Kerns verschränkt sich zeichenweise mit der der gesunden, weil `emit_raw` im Panic-Pfad absichtlich
sperrfrei ist.

### Was nicht gebaut wurde, und warum

Billig wären: IRQs im Panic-Pfad maskieren (Minuten), Rekursionswächter im Panic-Handler (Minuten),
`panic` → `system_off` (~1 h; beide Abschaltwege existieren und werden heute **nur** aus
Testabschlusspfaden gerufen, nie aus `panic.rs`). Die drei zusammen machen die Festlegung wahr,
statt sie zu behaupten. Sie sind aber eine **Entscheidung**, keine Reparatur: sie tauschen
Verfügbarkeit gegen Ehrlichkeit, und heute ist der Knoten in den Fällen C/D nachweislich
*verfügbarer* als die Zusicherung verlangt. Diese Entscheidung gehört nicht in einen Nebensatz.
Aufwände und die weiteren Punkte (Schranke im Ticket-Lock — zieht Loom/Kani nach sich; `#DF` mit
IST; Schutzseiten; Panic-IPI, der ohne NMI genau den Fall verfehlt, den er treffen soll) in
`docs/fehlerdomaene.md` §6.

### Der ehrliche VM-Vergleich steht jetzt da, wo ein Betreiber ihn liest

Ein Wirt mit 100 VMs verliert bei einem **Gastkern**-Panic einen Gast; ein SEL4Lake-Knoten hat gar
keine Gastkernschicht — was ein Gastkern täte, tut der geteilte Kern. Der Handel in einem Satz: eine
VM-Plattform hat *viele große* Fehlerdomänen (Größenordnung 10⁷ LOC je Mandant), SEL4Lake hat *eine
kleine* (~18 kLOC `kernel/src`, ~36 kLOC mit `crates/`). Weniger Code kann ausfallen — aber wenn er
ausfällt, fällt alles aus. Daraus zwei betriebliche Sätze, die vorher nirgends standen:
**ein Knoten ist keine Redundanzeinheit** (zwei Repliken auf einem Knoten sind eine), und
**Wartung ist knotengranular**, solange es keine Live-Migration einzelner PDs gibt (Z3/Z4).

### Nicht geprüft (steht auch im Dokument)

Blech (alles lief unter QEMU); der Stacküberlauf jenseits Ebene 362; Panic unter `CAPS` (gemessen
wurde `MEM`); Panic in einem Interrupt-Handler; die aarch64-Fassung der Fälle E/F/G (nur C wurde
dort gegengeprüft); ob ein Panic die IOMMU-Tabellen in einem Zwischenzustand hinterlässt. Eine
Fehlerdomäne auf ungeprüften Annahmen ist schlimmer als keine — deshalb ist die Liste Teil der
Zusicherung, nicht ihr Anhang.

---

## A-5.4 (Teil 1). Zwei Treiber-PDs — und vier versteckte Politiken, die dabei herausfielen

**Was fehlte.** A-5.3 belegt, dass *eine* Treiber-PD das *benannte* Geraet bekommt. Nicht belegt
war, dass zwei zugeteilte Geraete **voneinander** getrennt sind — denn es lief immer nur eine.
Dieselbe Form wie `virtio-rng` vor A-5.2: eine Aussage sieht wahr aus, weil der Gegenbeweis nie
laeuft.

**Der zweite Treiber** (`programs/hardware/virtio-net`) ist bewusst klein. Er faehrt kein Netzwerk;
er holt sein Geraet, faehrt EINE echte ARP-Transaktion und meldet, was er dabei gesehen hat. Die
Transaktion ist die **Positivkontrolle** fuer den noch offenen Teil 2 — ohne sie hiesse "der
Fremdzugriff kam nicht an" nur, dass ueberhaupt nichts lief.

### Vier Stellen, die "der eine Treiber" meinten

Der Umbau war kein Aufwand nebenbei, sondern der eigentliche Ertrag. Bei EINEM Treiber war jede
dieser Stellen richtig; beim zweiten wurde jede zu einer **stillen Fehlwahl** — mit gueltigen Caps
und ohne eine einzige Fehlermeldung:

| Stelle | war | ist |
|---|---|---|
| `DriverAssign`-Suche | „die erste benutzte" (4×) | Schluessel `program_id` aus dem Manifest |
| `DRIVER_SERVICE` | ein Platz, „der zuletzt geladene" | Liste, `driver_service_of(id)` |
| `DRIVER_NTFN` | ein Register | die Notification **dieses** Dienstes |
| geteilte Uebertragungsflaeche | „die eine" | die des **benannten** Dienstes |

Der Reihe nach aufgetreten, und jede war im Log als etwas anderes sichtbar: der Blockdienst
antwortete nicht (`Status=18446744073709551615`), der Austausch meldete `kein Dienst`, `v2 meldete
bereit=0` bei einem Austausch, der sauber gelaufen war, und die Dateisystem-PD verschwand
ersatzlos aus dem Bericht.

**Die Regel, die daraus wurde:** wo eine Wahl mehrdeutig waere, wird **abgewiesen statt geraten**.
`driver_service()` liefert bei zwei Diensten `None` — genau daran ist der Kernel-Testclient beim
ersten Lauf aufgelaufen, was richtig war.

### Der Client benennt seinen Dienst

Die letzten 4 reservierten Bytes des Manifest-Eintrags tragen jetzt `service_id` (A-5.4). Damit ist
die in A-5.3 offen benannte Luecke zu: „gib mir einen Endpoint" hiess bei zwei Diensten „gib mir
irgendeinen", und welchen, entschied die Ladereihenfolge. `0` bleibt zulaessig und heisst „es gibt
ohnehin nur einen"; gibt es mehrere, wird abgewiesen.

### Gemessen

```
devsel  : Eintrag 3 verlangt 1af4:1042, bekam RID 0x0018 1af4:1042
devsel  : Eintrag 5 verlangt 1af4:1041, bekam RID 0x0020 1af4:1041
devsel  : ALL PASS (2 angeboten, 2 vergeben, 0 frei. JEDE Zuteilung wurde gegen den Selektor
          IHRES Manifest-Eintrags geprueft)
```

Beide Negativfaelle sind dadurch **schaerfer** geworden: ein unerfuellbarer Selektor trifft genau
den Eintrag, der ihn traegt, und der zweite Treiber bleibt unbeeintraechtigt; und zeigt der
Selektor der BLOCK-PD auf die Netzkarte, bekommt sie die Netzkarte — obwohl das Blockgeraet in der
Angebotsliste davor steht.

### Offen (Teil 2)

Der eigentliche Nachweis: Geraet A darf nicht in die DMA-Region von PD B schreiben. Der Knopf dafuer
steht (`VirtioNet::arp_probe_rx_at` verschiebt **genau eine** Adresse, damit das Geraet weiter
laufen kann), das Programm kennt die Anfragen `OP_SELF`/`OP_FOREIGN` — der Kernel-Testclient, der
beides ausloest und gegen die VT-d-Faults haelt, fehlt noch.

---

## A-5.4 (Teil 2). Das Geraet des einen Treibers erreicht die Region des anderen nicht

Die Mandantenaussage, auf die A-5 hinauslaeuft — und bis hierher war sie **unbelegt**, weil immer
nur eine Treiber-PD lief.

### Der Aufbau, und warum genau EINE Adresse wandert

`VirtioNet::arp_probe_rx_at` verschiebt allein die Adresse des **Empfangspuffers**. Virtqueues und
Sendepuffer bleiben in der eigenen Region, denn das Geraet muss **laufen** koennen: wuerden die
Ringe mitwandern, faende es nicht einmal die Deskriptoren, der Versuch scheiterte an der falschen
Stelle, und „nichts kam an" bewiese nur, dass nichts lief.

Dass die Netz-PD die fremde IOVA genannt bekommt, ist kein Loch, sondern der Kern der Aussage:
eine Adresse zu **kennen** hilft nicht, wenn der Uebersetzungskontext sie nicht aufloest. Ein
Angreifer, der raten muesste, bewiese nur, dass Raten schwer ist.

### Vier Zahlen, und keine reicht allein

```
dmaiso  : Positivkontrolle features=1 tx=1 rx=1 arp=1 · Fremdversuch features=1 tx=1 rx=1 arp=0
          · Opfer 0x00000000ffe00800 -> 0x00000000ffe00800 · VT-d-Faults=1
dmaiso  : ALL PASS
```

1. **Positivkontrolle** — derselbe Treiber, dasselbe Geraet, dieselbe Kette.
2. **Keine Daten** beim Fremdversuch.
3. **Das Opfer unberuehrt**, vom KERNEL nachgeprueft. Dass der Angreifer „nichts bekommen" meldet,
   ist eine Aussage ueber ihn.
4. **Mindestens ein VT-d-Fault** — der *aktive* Beleg, dass geblockt wurde. Ohne ihn waere „keine
   Antwort" auch mit einem stummen Gegenueber vereinbar.

### Zwei Fehler im eigenen Entwurf, beide gemessen

**Der Fremdversuch meldete zuerst `arp_reply = true`** — obwohl VT-d nachweislich blockiert hatte
(ein Fault gezaehlt). Grund: `arp_probe` nullte vom Empfangspuffer nur **acht Byte**; `ethertype`
steht bei +12, `oper` bei +20, die Absender-IP bei +28. Die zweite Probe las die Antwort der
**ersten**. Solange nur eine Probe je Lauf lief, konnte das nicht auffallen. Genau die Fehlerform,
die dieses Projekt schon zweimal bezahlt hat: ein Puffer mit den richtigen Bytes darin ist von
einem beschriebenen Puffer nicht zu unterscheiden, solange niemand vorher aufraeumt.

**`rx_used` gehoerte nicht ins Kriterium.** Das Bit kommt aus dem used-Ring und sagt, dass das
Geraet den Deskriptor *abgearbeitet* hat — es ist auch dann 1, wenn die Schreibung an der IOMMU
scheiterte (QEMU legt den Puffer trotzdem zurueck). „Das Geraet hat gehandelt" und „die Daten sind
angekommen" sind zwei Aussagen, und nur die zweite gehoert hierher.

Und ein dritter, der bei der Ablage passierte: `DMAISO_OK` entstand zuerst **im Bericht** und steht
in `all_done()` — was der Bericht setzt, kann den Bericht nicht ausloesen. Das Urteil faellt jetzt
in `drv_service_step`.

### Sensitivitaet

| Mutation | Ergebnis |
|---|---|
| M1: dem Angreifer die **eigene** Region nennen | `Fremdversuch arp=1`, `VT-d-Faults=0` → **FAILURES** |
| M2: Gegenueber antwortet nicht (`net=192.0.2.0/24`) | `Positivkontrolle rx=0 arp=0` → **SKIP mit Begruendung**, kein Urteil |
| unveraendert | ALL PASS |

M1 belegt, dass der Test einen **gelungenen** Fremdzugriff erkennt — ohne ihn hiesse „kein Zugriff"
nur, dass der Pfad ueberhaupt nichts tut. M2 belegt den dritten Ausgang: nicht messbar ist weder
bestanden noch durchgefallen.

**Gemessen:** x86 `ALL PASS`, Lade-Suite `ALL PASS`, aarch64 3/3 identische Signatur, Host-Tests
121, Kerngrenze sauber.

---

## Z4a + Z4b. Der Haltepunkt, und was auf keinen Fall mitwandert

Z4 ist ausdruecklich mehrstufig, und die Reihenfolge ist bindend. Z4e haengt an einem Netzstack
(Z10, gibt es nicht) und an Attestierung (Z7); gebaut sind deshalb die beiden Stufen, die **heute**
pruefbar sind — und Z4b ist die, die die Notiz selbst „die gefaehrlichste Stelle des ganzen
Vorhabens" nennt.

### Z4a: „benennbar" heisst zwei pruefbare Dinge

`system::freeze_thread` haelt einen Thread genau dann an, wenn beides gilt:

1. **Nicht im Kernel.** Ein Thread auf einem Kern kann mitten in einem Syscall stehen; einer auf
   **keinem** Kern ist per Konstruktion an einer Trap-Grenze stehengeblieben. Das wird ueber
   `current_id` je Kern **gefragt**, nicht geglaubt.
2. **Keine offene IPC-Beziehung** (`thread_quiescence`) — sonst bliebe ein Partner zurueck. Das ist
   Z4d Stufe 1: Migration nur ohne offene Transaktionen.

Die drei Ausgaenge sind unterscheidbar, und der Unterschied traegt: `StillRunning` ist
**voruebergehend** (der Reschedule-IPI ist unterwegs, wiederholen), `Busy` ist eine **Absage** (wer
darauf wartet, wartet ewig).

**Gewartet wird nicht hier.** Z4a verlangt ein `SYS_FREEZE`, das wartet — aber diese Funktion nimmt
die Scheduler-Sperre jedes Kerns, und in einer Schleife darauf zu warten, dass ein anderer Kern
voranschreitet, waehrend man seine Sperre haelt, ist die Bauanleitung fuer einen Deadlock. Der
Aufruf ist idempotent; das Warten gehoert dem Aufrufer.

### Die Pruefung misst die WIRKUNG, nicht den Rueckgabewert

```
freeze  : laeuft-vorher=true (35->38) eingefroren=true steht=true (38->38)
          aufgetaut=true laeuft-wieder=true (38->41) IPC-Rolle-abgewiesen=true
freeze  : ALL PASS
```

Der Rundenzaehler eines Workers muss sich **vorher** bewegen (sonst belegt „er steht" nichts,
sondern beschreibt einen Thread, der nie lief), waehrend des Einfrierens **stehen**, und nach dem
Auftauen **wieder laufen** — ohne den dritten Schritt waere ein `freeze`, das den Thread
kaputtmacht, von einem korrekten nicht zu unterscheiden. Dazu die vierte Aussage: der IPC-Server in
`RECV` wird **abgewiesen**. „Blockiert" und „ruhend" sehen von aussen gleich aus und sind es nicht.

| Mutation | Ergebnis |
|---|---|
| M1: `freeze` deplant nicht | `steht=false (37->44)` → **FAILURES**, und die Suite wird rot |
| M2: IPC-Rolle wird nicht geprueft | `IPC-Rolle-abgewiesen=false` → **FAILURES** |

### Drei Fehler im eigenen Entwurf, alle gemessen

1. **`MAX_CORES` statt `num_cores()`.** Die Schleife lief bis 8, konfiguriert waren 4 —
   `current_id` auf einer Scheduler-Instanz ohne Tabellen ist ein Programmfehler, und sie sagt das
   auch so (`kein laufender Thread (gefragter Kern 4, Instanz-Kern 0, TCB-Kapazitaet 0)`).
   **Nebenbefund:** der Panic hat den Knoten **nicht** gestoppt — der Lauf lief bis in die
   Pruefungen weiter. Genau das, was B-6.2 am selben Tag gemessen hat, hier zufaellig noch einmal.
2. **Das Beobachtungsfenster war kuerzer als ein Tick.** 400 000 Leerdurchlaeufe sind unter KVM
   knapp eine Millisekunde; die Verdraengung kommt nach 10 ms. Die Positivkontrolle meldete
   `laeuft-vorher=false` bei einem voellig gesunden Worker — gemessen wurde die Fensterlaenge.
   Gezaehlt wird jetzt in **Ticks**.
3. **`FREEZE_OK` stand in `all_done()`**, wird aber im Bericht gesetzt — was der Bericht setzt, kann
   den Bericht nicht ausloesen. Jetzt kein Konjunkt mehr; gelesen wird die Zeile von der **Suite**,
   und dass sie gelesen wird, ist die Lehre aus D5. Die Mutation belegt es: M1 macht die Suite rot.

### Z4b: was auf keinen Fall mitwandert

`crates/sel4lake-cap/src/checkpoint.rs`, abhaengigkeitsfrei und host-getestet (7 Tests). Die Regel
in einem Satz: **was auf der Zielmaschine nicht dasselbe bezeichnen kann, wandert nicht mit — es
wird verweigert, nicht ersetzt.** Der bequeme Weg waere, eine MMIO-Cap „auf das entsprechende
Geraet drueben" abzubilden; es gibt kein entsprechendes Geraet, es gibt ein anderes.

**Die Entscheidung braucht einen Umfang.** Bei `Mmio`/`Irq`/`Dma` ist sie fuer sich klar; bei einem
**Endpoint** nicht: wandert der Partner mit, ist die Cap sinnvoll uebertragbar, bleibt er zurueck,
zeigt sie ins Leere. Dieselbe Cap ist mal uebertragbar und mal nicht, und was zutrifft, haengt am
`Scope`, nicht an der Cap. Eine Klassifikation ohne ihn waere kuerzer und in der Haelfte der Faelle
falsch.

**Fail-closed:** was nicht ausdruecklich als uebertragbar gefuehrt ist, wird verweigert — ein neuer
Objekttyp, den niemand betrachtet hat, wandert damit nicht mit, statt durchzurutschen, weil er in
keiner Ablehnungsliste stand.

Drei Entwurfsentscheidungen, die die Tests festhalten:

* **Speicher wandert als Inhalt, nicht als Adresse.** Zwei Regionen gleicher Laenge an
  verschiedenen Physadressen sind extern **gleich** — das ist der Beleg. Die Zielmaschine faerbt
  neu; Farbe ist maschinenlokal (ein Argument dafuer, sie nie in die ABI zu heben).
* **Beziehungen werden ueber den Platz im Checkpoint bezeichnet**, nicht ueber eine ID — sonst
  waere die maschinenlokale Zahl doch mitgewandert, nur getarnt.
* **Ein Budget traegt eine Vorbedingung** (`SameTickSemantics`). Die Zahlen sind Ticks, und ein Tick
  bedeutet auf einer Maschine ohne invarianten Zaehler etwas anderes (Z4f, B-5.1). Sie zu
  verschweigen hiesse, die Abrechnung still falsch werden zu lassen.

Und die Ablehnungsgruende sind **einzeln** und nicht ein Sammel-`false`: „das Geraet gibt es dort
nicht" und „der Partner bleibt zurueck" sind verschiedene Befunde, und nur der zweite laesst sich
durch einen groesseren Umfang beheben.

### Was NICHT gebaut ist

Z4c (Dirty-Tracking), Z4d ueber Stufe 1 hinaus (Endpoint-Proxys), Z4e (Transport — braucht Netz und
Attestierung), Z4f (Maschinenvergleich). Und es gibt **keinen** vollstaendigen Checkpoint: die
externe Darstellung existiert als Typ und als Regel, ein Serialisierer und ein Wiederhersteller
nicht. Was hier steht, ist die Grundlage, auf der beides gebaut werden kann — und die Absage, ohne
die beides gefaehrlich waere.

> **Nachtrag 2026-08-03:** Serialisierer und Wiederhersteller gibt es jetzt, s. den naechsten
> Abschnitt. Der Satz „ein Checkpoint existiert nicht" gilt damit nicht mehr.

## Z4 Stufe 2. Ein Thread ueber die Bootgrenze

Z4a haelt einen Thread an, Z4b sagt, was mitwandern darf. Was fehlte, war die **Grenze**. Hier ist
sie die Maschinengrenze in der Zeit: derselbe Rechner, drei Laeufe, dazwischen je ein Neustart.
Alles, was der Kernel im RAM haelt, ist weg; was bleibt, ist ein Sektor.

**Die Aussage in einem Satz:** der Fortschrittszaehler eines Threads startet im naechsten Lauf bei
dem Wert aus dem vorigen, nicht bei null — und der Checkpoint, aus dem er kommt, wird abgewiesen,
wenn er zu einem anderen Kernel-Image gehoert.

### Der Entwurf: derselbe Kernel, kein Flag

Der Kernel liest `CKPT_SECTOR` und entscheidet daraufhin, was dieser Lauf ist. Vier Ausgaenge:

| Befund | Lauf |
|---|---|
| kein Checkpoint (leerer Sektor) | Kaltstart → speichern, Epoche 1 |
| passt alles | wiederherstellen → weiterarbeiten → wieder speichern, Epoche + 1 |
| Struktur kaputt (Version/Laenge/Pruefsumme) | abweisen, mit Code |
| Magie ja, **Kernel-Hash nein** | **abweisen** — Z4f in klein |

Ein Schalter waere bequemer gewesen und waere zugleich der Fehler: ein Kernel, dem man sagen muss,
ob er wiederherstellen soll, kann nicht merken, dass der Checkpoint gar nicht zu ihm gehoert.

**Der Transport ist der, den es schon gibt.** `programs/hardware/virtio-blk` nimmt `OP_WRITE` und
kopiert die geteilte Uebertragungsflaeche auf die Platte; der Kernel legt seine 512 Byte hinein und
ruft wie jeder andere Client (`ckpt_client` in `bringup.rs`). Ein eigener Speicherpfad im Kern waere
ein zweiter Treiber gewesen — genau das, was A-5.1 abgeschafft hat. `programs/hardware/virtio-blk`
ist **unveraendert**.

### Das Format

`crates/sel4lake-cap/src/checkpoint.rs`, neben der Regel, die entscheidet, was hineindarf.
Abhaengigkeitsfrei, ohne `unsafe`, host-getestet — fremde Bytes werden nirgends mit Kernprivileg
interpretiert, dieselbe Linie wie `sel4lake-part`/`sel4lake-fat`.

| Offset | Breite | Feld |
|---|---|---|
| 0 | 8 | Magie `SL4KCKPT` |
| 8 | 4 | Formatversion (1) |
| 12 | 4 | Rumpflaenge |
| 16 | 32 | **`kernel_code_hash`** |
| 48 | 8 | Fortschritt (`WORKER_ROUNDS[0]`) |
| 56 | 8 | Nonce (Zyklenzaehler beim Einfrieren) |
| 64 | 8 | Epoche |
| 72 | 4 | Zahl der Caps |
| 76 | 4 | Vorbedingungen (Bit 0 = `SameTickSemantics`) |
| 80 | 16·n | je Cap: Typ-Tag + Wert (`ExternKind`) |
| … | 4 | CRC-32 |

Feste Breiten, Little-Endian, wie das Manifest. `decode` prueft in dieser Reihenfolge: Magie →
Version → **beide** Laengenfelder gegeneinander und gegen die Bytefolge → Pruefsumme → Kernel-Hash.
Die Reihenfolge ist die Aussage: erst wenn 1–4 stehen, ist ein Fehlschlag bei 5 wirklich „ein
Checkpoint eines ANDEREN Kernels" und nicht bloss „kaputte Bytes".

`Image::build` ist die Stelle, an der Z4b wirkt: es ruft `classify_all`, und **eine einzige nicht
uebertragbare Cap verhindert den Checkpoint** — mit Slot und Grund, vor dem Schreiben.

Die Pruefsumme ist bewusst das uebliche CRC-32 (`zlib.crc32`): nur so kann ein *unabhaengiger*
Leser in einer anderen Sprache einen Checkpoint herstellen oder nachpruefen. Genau das braucht der
Negativfall unten — dieselbe Form wie `tools/checkfat.py`.

### Der Fehler im eigenen Entwurf, der die ganze Stufe umgebaut hat

Der erste Aufbau war: Lauf 1 speichert den Zaehler, Lauf 2 liest ihn und setzt ihn. Die Suite
verglich beide Zahlen, alles gruen. **Die Mutation hat den Aufbau widerlegt**: ein Kernel, der den
gefundenen Wert LIEST und MELDET, ihn aber nicht in den Thread schreibt, kam durch —

```
bootckpt: wiederhergestellt ... Fortschritt=151 ... Zaehler-danach=151     (M1, nichts gesetzt)
```

Der Grund: unter KVM ist der Hochlauf reproduzierbar. Gemessen an derselben Stelle des Hochlaufs:
**133, 142, 146, 148, 150, 151, 155 Runden** — Streuung rund 20. Der gespeicherte Wert und der
selbst gezaehlte fallen zusammen. Die Vorgabe „der Zaehler steht bei jedem Lauf woanders" ist auf
dieser Maschine **falsch**, und ein reproduzierbarer Wert kann nicht belegen, dass er geerbt wurde.

Die Nonce fing das nicht auf: sie belegt die **Herkunft der Bytes**, nicht die **Wirkung** des
Wiederherstellens. Zwei verschiedene Fragen — dieselbe Unterscheidung wie zwischen „das Geraet hat
gehandelt" (`rx_used`) und „die Daten sind angekommen" (A-5.4).

**Die Loesung ist eine wachsende Kette.** Jeder Lauf, der durchkommt, laesst den wiederhergestellten
Thread noch **mindestens 100 Runden** arbeiten (`CKPT_MIN_DELTA`, rund das Fuenffache der gemessenen
Streuung) und speichert erst dann, mit Epoche + 1. Ab dem zweiten Glied liegt der gespeicherte Wert
strukturell ausserhalb dessen, was ein Lauf ohne Wiederherstellung erreicht. Gemessen:

```
Lauf 1 (Kaltstart):     gespeichert Fortschritt=244  Epoche=1
Lauf 2:  wiederhergestellt Fortschritt=244 Zaehler-vorher=147 Zaehler-danach=244  Epoche=1
         gespeichert        Fortschritt=345                                        Epoche=2
Lauf 3:  wiederhergestellt Fortschritt=345 Zaehler-vorher=151 Zaehler-danach=345  Epoche=2
         gespeichert        Fortschritt=446                                        Epoche=3
```

`Zaehler-vorher` ist der Stand, den dieser Lauf **ohne jede** Wiederherstellung erreicht haette.
151 gegen 345 — die beiden Zahlen sind trennbar, und erst dadurch ist „geerbt oder selbst gezaehlt?"
ueberhaupt eine Messung. Die Suite prueft das ausdruecklich als **Positivkontrolle**: sind sie
gleich, ist der Lauf *nicht messbar* und faellt durch, statt gruen zu melden.

Nebenbefund derselben Runde: der Kaltstart fror den Thread ein und wartete dann auf einen Zuwachs,
den ein eingefrorener Thread nicht erreichen kann (`Zuwachs-erreicht=0`, 20 s Notschranke).
Einfrieren gehoert zum **Anfassen** des Zustands, nicht zum Warten darauf.

### Ein zweiter Fehler, den der Umbau freigelegt hat — und der aelter ist als Z4

`drv : ALL PASS` las den Datenpuffer der Treiber-PD **im Bericht** und verglich ihn mit der Magie
des Abbilds. Solange nur `drv_client` diesen Puffer benutzte, ging das gut: sein letzter Schritt war
ausdruecklich so gelegt, dass am Ende die Magie dort steht. Mit einem **zweiten** Client stand dort
dessen Sektor, und die Zeile meldete `FAILURES` fuer einen Treiber, der alles richtig gemacht hatte:

```
drv : Zuteilung: ... Sektor 0x54504b434b344c53 (erwartet 0x454b414c344c4553)
                          ^-- "SL4KCKPT", little-endian
```

Der Puffer eines Treibers gehoert dem **letzten Client**, nicht der Aussage. Der Wert wird jetzt
**erfasst, wenn die Aussage gilt** (Ende der Dienstfolge) und im Bericht nur noch gelesen; der
Momentanwert steht daneben als Diagnose. Das ist kein Umgehen des Befunds, sondern seine Behebung:
die Zeile misst seither, was sie behauptet.

### Was geprueft wird — und was ohne Negativfall nichts belegen wuerde

| Pruefung | Aussage |
|---|---|
| Kette 1 → 2 → 3, Epochen 1 → 2 → 3 | der Zustand **waechst** ueber Bootgrenzen, statt in jedem Lauf neu zu entstehen |
| `Zaehler-danach == Fortschritt` | der geerbte Wert steht **im Thread**, nicht bloss im Bericht |
| `Zaehler-vorher != Fortschritt` | **Positivkontrolle**: es war ueberhaupt etwas zu messen |
| Nonce des Erzeugers | die Bytes stammen aus dem vorigen Lauf, sind kein Rest im Puffer |
| `eingefroren=1` | angefasst wurde ein **stehender** Thread (Z4a) |
| Verweigerung, Grund 1/2/3 | Z4b am echten Gegenstand, s. u. |
| fremder Kernel-Hash → Lesecode 7 | Z4f in klein, s. u. |

**Negativfall 1 — eine nicht uebertragbare Cap.** In **jedem** Lauf wird zusaetzlich die
**Treiber-PD** klassifiziert; sie haelt echte Geraete-Autoritaet. Ihr Kanal (Endpoint +
Notification) liegt dabei ausdruecklich **im Umfang** — damit als Grund nur uebrigbleibt, was durch
keinen groesseren Umfang behebbar ist. Gemessen: `Slot 3, Grund 1` (Geraetefenster). Grund 5
(„Partner nicht im Umfang") waere behebbar gewesen und haette die Regel nicht belegt; die Suite
laesst nur 1/2/3 gelten.

**Negativfall 2 — ein Checkpoint eines fremden Kernel-Images.** Hergestellt wird er so, wie ein
echter aussaehe: der Sektor bleibt **strukturell heil**, nur das Hashfeld traegt einen anderen Wert,
und die Pruefsumme wird mit `zlib.crc32` **neu gerechnet**. Ein blosses Bytekippen waere der
schwaechere Fall — der faellt schon an der Pruefsumme durch, und der Test haette „kaputte Bytes"
gemessen statt „falsches Image". Ergebnis:

```
bootckpt: ABGEWIESEN Sektor=32710 Lesecode=7 (... 7=FREMDES KERNEL-IMAGE)
          -- der Sektor bleibt unveraendert, es wurde weder geladen noch ueberschrieben
```

Zwei Nachpruefungen dazu, beide noetig: es wurde **nicht** wiederhergestellt, und es wurde
**nicht** gespeichert. Ein Kernel, der nach der Abweisung seinen eigenen Checkpoint schriebe,
machte aus einem fremden Zustand lautlos einen eigenen.

### Sensitivitaet — zwei Mutationen, im Wortlaut

| Mutation | Wirkung |
|---|---|
| **M1** `WORKER_ROUNDS[0].store(…)` beim Wiederherstellen entfernt (Wert wird gelesen und gemeldet, nicht gesetzt) | `FAIL: … Lauf 2 fand den Zustand aus Lauf 1 nicht (erwartet … Zaehler-danach=241; bekam 241 / … / 156)`, dazu drei weitere FAIL-Zeilen; Kette bricht (241 → 256 statt 241 → 341); `== FAILURES ==` |
| **M2** Kernel-Hash-Vergleich in `Image::decode` entfernt | Host-Test `fremdes_kernel_image_wird_abgewiesen … FAILED` (21/22); Suite: der manipulierte Checkpoint wird **geladen** (`wiederhergestellt … Epoche=3`) statt abgewiesen → zwei FAIL-Zeilen, `== FAILURES ==` |

M1 ist die wichtigere: sie ist genau die Mutation, die den **ersten** Aufbau nicht rot machte.

### Was serialisiert wird — und was ausdruecklich nicht

**Drin:** Magie, Formatversion, `kernel_code_hash`, Fortschritt, Nonce, Epoche, das Ergebnis der
Cap-Klassifikation (je Cap ein `ExternKind`) und die Vorbedingung `SameTickSemantics`. 116 Byte bei
zwei Caps.

**Nicht drin — und der wichtigste Punkt dieses Abschnitts:**

* **Kein Trap-Frame, keine Register.** Das waere die naechste Stufe gewesen, und sie ist ohne Z4c
  **wertlos oder schlimmer**: ein Trap-Frame traegt `RIP` und `RSP`, und `RSP` zeigt in einen Stack,
  dessen Inhalt nach dem Neustart frischer Speicher ist. Wiederhergestellt ergaebe das einen Thread,
  der an der richtigen Adresse mit einem falschen Stack weiterlaeuft — ein Fehler, der spaeter und
  woanders auftritt. Registersatz ohne Speicher ist kein Checkpoint, sondern ein Zeiger ins Leere.
  Der Registersatz ist ohnehin der leichte Teil; der schwere ist Z4c.
* **Kein Speicher** (Z4c). Die Cap wandert als `Region { len }` — als **Laenge**, nicht als Inhalt.
  Der Inhalt ist auf der Zielseite null.
* **Keine offenen IPC-Beziehungen** (Z4d Stufe 1). `Scope::EMPTY`, und `freeze_thread` weist einen
  Thread mit offener Transaktion ohnehin ab.
* **Keine Authentifizierung** (Z4e). Die Pruefsumme sagt „strukturell heil", nicht „von wem". Ein
  Checkpoint ist derzeit nur so vertrauenswuerdig wie das Medium, auf dem er liegt. Ueber ein Netz
  waere das offen; ueber einen lokalen Sektor ist es die vorhandene Vertrauensgrenze.
* **Kein Maschinenvergleich** (Z4f). Geprueft wird das **Kernel-Image**, nicht die Maschine. Zwei
  Rechner mit demselben Image und verschiedener Cache-/NUMA-Klasse oder ohne `invtsc` waeren nicht
  unterscheidbar — die Vorbedingung `SameTickSemantics` steht im Checkpoint, aber **niemand prueft
  sie**. Das ist die naechste offene Stelle.
* **Nicht gemessen:** ob die Folge auch dann traegt, wenn der Blockdienst mitten im Schreiben
  ausgetauscht wird. Der Austausch (A-5.1) laeuft im selben Lauf, aber **vor** dem Checkpoint.

### Ort im Ablauf, und warum

Die Checkpoint-Folge laeuft **nach** der Dienst-/Blockdienst-Folge und **vor** dem Kreuz-DMA-Nachweis
(A-5.4). Nicht danach: A-5.4 liest das erste Wort der fremden DMA-Region vor und nach dem
Fremdversuch, und eine Platten-E/A dazwischen ginge durch genau diese Region — der „unabhaengige
Zeuge" saehe eine Aenderung, die der Angreifer nicht gemacht hat.

Der Sektor liegt auf **32710**, ausserhalb beider Partitionen (34..20000, 20001..32700) und
ausserhalb der GPT-Sicherungskopie (Eintraege ab 32735, Kopf auf 32767). Frei bleibt 32701..32734.
Die A-6-Pruefungen (GPT, FAT16, `tools/checkfat.py`) sind unberuehrt.

`bootckpt` steht als **Fertig-Merker** in `all_done()`, damit der Bericht nicht vor der Platten-E/A
kommt — das **Urteil** steht dort ausdruecklich nicht, es entsteht im Bericht und wird von der Suite
gelesen. Dieselbe Aufteilung wie bei `freeze` (Z4a).

**Gemessen (2026-08-03):** Lade-Suite `== ALL PASS ==` (39 Pruefungen, davon 12 neu), Host-Tests
`mem 22 · part 14 · fat 20 · cycles 9 · loader 50 · cap 22` → `ALL PASS`, Kerngrenze sauber.

## B-7.3. Der `delete`-Beweis war 37 Tage rot — und das Gate darüber grün gemeldet

**Ausgangslage, gemessen statt erinnert.** `tools/verus-verify.sh` fuhr 16 Beweisdateien; eine
davon, `Verification/capability-system/proofs/cap_space.rs`, meldete `9 verified, 1 ERRORS`.
Zwei Fehler in `proof fn delete`: ein `assert forall … by {}` mit **leerem Rumpf** für die
Eltern-Klausel („assertion failed"), und `function body check: Resource limit (rlimit) exceeded`
für die Funktion selbst.

**Der Befund, der schwerer wiegt als der Beweis.** Der Commit, der das eingebracht hat, heißt
`3384abb` „Phase 1 (Capability-System) Schritt C2a: delete (Leaf) gegen die VOLLE cap_inv
**bewiesen**" (2026-06-27). Die README der Komponente führte seither `10 verified ✅`. Das
CI-Gate `.gitea/workflows/verus.yml` stammt vom **selben Tag** und läuft `on: [push, pull_request]`;
beide Commits sind auf `origin/arch/x86_64` **und** `origin/master`.

Der naheliegende Ausweg wäre gewesen, das auf die Verus-Version zu schieben: die CI pinnte
`0.2026.06.20.911e4e7`, lokal lief `0.2026.07.27.31579f0`. **Nachgemessen — der Ausweg trägt
nicht.** Die alte Datei fällt unter der gepinnten Fassung mit **denselben zwei Fehlern**. Der
Beweis war nie grün. Damit bleiben genau zwei Möglichkeiten, und beide sind ein eigener Befund:
das Gate lief nie (kein Runner), oder es lief rot und niemand hat das Ergebnis gelesen. Ein Gate,
dessen Ausgang niemand liest, ist kein Gate — es ist eine Beschriftung. Das ist dieselbe Form wie
„ein Test, der nirgends läuft, ist kein Test", nur eine Ebene höher.

*(Die Version bleibt trotzdem eine Falle: zwei verschiedene Beweiser für dieselbe Aussage, ohne
eine Stelle, an der das auffiele. Der Pin steht jetzt auf der Fassung, gegen die entwickelt wird;
beide sind mit dem neuen Stand nachgemessen, je `18 verified, 0 errors`.)*

### Was der leere `by {}` wirklich brauchte

Die Klausel **gilt** — sie war kein Beweisproblem, sondern eine fehlende Kette aus fünf Schritten,
die Z3 nicht selbst findet:

1. der gelöschte Slot ist tot ⟹ jeder in `cs2` lebende Slot `s` ist ein **anderer**;
2. also lebte `s` schon in `cs`;
3. sein `parent` ist unverändert (Rahmen-Lemma);
4. das Ziel `p` ist **nicht** der gelöschte Slot — der **einzige** Punkt, an dem `no_children`
   gebraucht wird;
5. `object`/`rank` von `p` stehen still.

Ohne (4) ist die Klausel schlicht falsch. Der leere Rumpf war also nicht „zu wenig Rechenzeit",
sondern eine Behauptung ohne Argument.

### Die rlimit-Wand stand nicht vor einer schweren Klausel, sondern vor ihrer Summe

Die frühere Fassung hatte die richtige Idee (per-Klausel-Asserts), aber sie standen **alle im
selben Funktionsrumpf** — also in **einer** SMT-Query, zusammen mit der Refcount-Rechnung. Das ist
Gliederung, nicht Dekomposition. Die vier Struktur-Klauseln liegen jetzt in eigenen `proof fn`
(`lemma_del_parent`/`_next`/`_prev`/`_first_child`), jede mit eigener Query und eigenem Budget.

Dazu wurde der Eingriff selbst aus dem Rumpf herausgezogen und als **Spezifikation** hingeschrieben
(`unlink1`/`unlink2`/`unlink_slots` als explizite `Seq::update`-Folge), statt als `let`-Kette im
Beweis zu leben. Ergebnis: **von `rlimit exceeded` (24,3 s bis zum Abbruch) auf 3,0 s**, ganz ohne
`#[verifier::rlimit]` — das alte `#[verifier::rlimit(50)]` ist ersatzlos weg. Datei jetzt
`18 verified, 0 errors`; die ganze Suite `98 verified` über 16 Dateien in 14,0 s.

### Kinderlisten-Erreichbarkeit — mit der Hälfte, ohne die die andere wertlos ist

`unreachable_after_delete` sagt **beides**:

1. **kein lebender Slot zeigt noch auf den gelöschten** — über **keine** der vier Kanten
   (`parent`, `next`, `prev`, `first_child`), und der Slot selbst ist tot;
2. **der Elternknoten jedes anderen Slots ist unverändert.**

Ohne (2) bestünde ein Eingriff, der sauber aushängt und nebenbei ein fremdes Kind umhängt, die
Prüfung (1) mühelos. Gemessen: genau diese Mutation (M8) lässt den Beweis fallen.

Nebenbefund aus dem Beweis: **`cap_inv` schließt Selbst-Geschwister nicht aus.** `next[i]==Some(i)`
zusammen mit `prev[i]==Some(i)` erfüllt alle sieben Klauseln. Das ist bekannt (README §14 führt es
als Reachability-Ausbaustufe), aber es ist an mehreren Stellen des Löschbeweises der Fall, den man
einzeln ausschließen muss — und es ist der Grund, warum die Reihenfolge der Schreibzugriffe
überhaupt eine Rolle spielt.

### Empfindlichkeit: 8 von 9 Mutationen fallen — und die neunte ist eine bewiesene Redundanz

| # | Mutation | Ausgang |
|---|---|---|
| M1 | `next[pv]` nicht fortgeschrieben | **gefallen** |
| M2 | `first_child[par]` nicht nachgezogen | **gefallen** |
| M3 | `prev[nx]` nicht fortgeschrieben | **gefallen** |
| M4 | Blatt-Vorbedingung `first_child is None` entfernt | *durchgegangen* |
| M5 | `no_children` entfernt (Blatt-Vorbedingung bleibt) | **gefallen** |
| M6 | `cap_inv` (6) ohne `prev[first_child] is None` | **gefallen** |
| M7 | `cap_inv` (4-sib) ohne geteilten Elternknoten | **gefallen** |
| M8 | `unlink` hängt den Nachfolger um (`parent := None`) | **gefallen** |
| M9 | Slot zuerst geleert statt zuletzt | **gefallen** |

**M4 ist kein Loch.** `first_child is None` **folgt** aus `no_children` + Klausel 6: hätte das
Blatt ein `first_child`, so hätte dieses Kind `parent == Some(i)` — was `no_children` verbietet.
Statt das nur zu behaupten, steht es als `lemma_leaf_from_no_children` in derselben Datei und wird
mitverifiziert. Die Vorbedingung bleibt trotzdem stehen, weil sie das ist, was der reale Code an
dieser Stelle prüft. Das **Paar** M4/M5 ist die eigentliche Aussage: die beiden Vorbedingungen sind
nicht unabhängig, sondern geordnet — `no_children` trägt allein, die Blatt-Eigenschaft nicht.

### Die Grenze zum echten Quelltext: `tools/verus-modelltreue.sh`

**Ein grüner Beweis kann per Konstruktion nicht bemerken, dass sich der Code unter ihm bewegt
hat** — das Modell ändert sich ja nicht mit. Bis hierher war die Modell-Treue eine *dokumentierte
Annahme* (README §11/§12). Das ist die Form, die B-7.2 teuer bezahlt hat.

Der Wächter reduziert beide Seiten auf **dieselbe normalisierte Ereignisfolge** aus Verzweigung und
Feldzuweisung und verlangt Gleichheit — heute 12 Ereignisse, deckungsgleich:

```
BRANCH prev · ARM some prev · WRITE prev.next := next
             · ARM none prev · BRANCH parent · ARM some parent · WRITE parent.first_child := next
BRANCH next · ARM some next · WRITE next.prev := prev
WRITE self.* := EMPTY · DELETE_LEAF unlink release_slot refcount--
```

`match Option { Some/None }` und `if … is Some { } else { }` fallen dabei auf dieselbe Form; der
Dialektunterschied verschwindet, die Struktur bleibt stehen. **Selbsttest, 8 Fälle:** vier
Mutationen am echten Code (Feldwert vertauscht, `first_child`-Zweig gelöscht, Leerung vorgezogen,
`release_slot` vor `unlink`), drei am Modell (Feldwert vertauscht, Zweig gelöscht, `unlink1`
umbenannt → *der Wächter liest ins Leere*) müssen ihn auslösen — **und eine kosmetische Änderung
auf beiden Seiten** (Binder umbenannt, Kommentare, Leerzeilen) darf ihn **nicht** auslösen. Beide
Hälften sind nötig: ein Wächter, der immer schreit, wird abgeschaltet; einer, der nie schreit, ist
eine Kopie mit Zertifikat.

**Der erste Lauf fand sofort zwei Abweichungen** — beide im Modell, beide behoben, indem das Modell
dem Code angeglichen wurde (nicht umgekehrt):

* **Die `first_child`-Fortschreibung stand unter der falschen Bedingung.** Das Modell zog nach,
  wenn `first_child[par] == Some(i)` galt; der Code schreibt, sobald `prev is None && parent is
  Some` — **ohne** zu prüfen, ob `i` überhaupt der Kopf war. Unter `cap_inv` **allein** ist das
  nicht dasselbe: Klausel 6 sagt nur die Gegenrichtung („der Kopf hat `prev == None`"), nicht
  „jedes Kind mit `prev == None` ist der Kopf". Erst die Reachability-Klausel 4r macht beides
  gleich. Der Code ist damit nicht falsch (`cap_inv` bleibt in beiden Fassungen erhalten), aber er
  **verlässt sich auf 4r** — und 4r ist genau die Klausel, die es noch nicht gibt. Der Beweis führt
  den Fall jetzt mit; die Abhängigkeit steht im Quelltext des Beweises.
* **Die Reihenfolge war umgedreht.** Das Modell leerte den Slot **zuerst**, der Code **zuletzt**.
  Bei einem Selbst-Geschwister sind das verschiedene Endzustände (s. o.).

Beides hätte kein Beweis der Welt gefunden: der alte Beweis wäre mit dem alten Modell grün gewesen.

### Und der Einsammler selbst war die dritte Lücke

`tools/verus-verify.sh` sammelte `verus/*.rs` + `Verification/*/proofs/*.rs`. Eine Beweisdatei
irgendwo sonst — ein Unterverzeichnis unter `proofs/`, eine neue Ablage, eine Datei eine Ebene
höher — lief damit **nirgends**, und zwar lautlos: das Skript meldete weiter `0 errors`, weil es
sie gar nicht kannte. Jetzt entscheidet der **Inhalt** (jede `.rs` mit einem `verus!`-Block, die
Wegwerf-Worktrees unter `.claude/` ausgenommen — dort liegen **alte Kopien derselben Dateien**),
ein leerer Fund ist ein Fehler statt eines Erfolgs, und die verwendete Verus-Version steht in der
Ausgabe. `--selftest` schiebt eine nachweislich falsche Beweisdatei unter, **an einer Stelle, die
die alten zwei Globs nicht getroffen hätten**, und verlangt, dass der Lauf daran scheitert; das
Gate fährt diesen Selbsttest jetzt vor den Beweisen.

*(Ein Fehler im eigenen Entwurf, unterwegs gefunden: der Selbsttest prüfte die Fundliste mit
`beweisdateien | grep -q`. `grep -q` schließt die Pipe früh, der Erzeuger stirbt an SIGPIPE, und
`set -o pipefail` macht daraus einen Fehlschlag der ganzen Pipeline — der Selbsttest meldete „wird
nicht eingesammelt", obwohl sie eingesammelt wurde. Ein Prüfer, der an seiner eigenen Mechanik
scheitert und das als Befund ausgibt, ist derselbe Fehler wie die Pipe in der ARM-Suite, nur in
klein.)*
