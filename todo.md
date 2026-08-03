# SEL4Lake — offene Punkte

Nur **noch nicht Erledigtes**. Was fertig ist, steht mitsamt Begründung in [done.md](done.md).
Reihenfolge innerhalb eines Abschnitts = Priorität. `[~]` = teilweise erledigt, Rest benannt.

---

## Z. Zielarchitektur (Stand 2026-07-29) — woran alles andere zu messen ist

> **Reihenfolge und Begründung:** [docs/plan-betriebsbereit.md](docs/plan-betriebsbereit.md).
> Hier steht *was* offen ist, dort *in welcher Reihenfolge* und *warum*.
>
> **Zur parallelen Bearbeitung geteilt in zwei unabhängige Stränge:**
> [todo-A-ausfuehren.md](todo-A-ausfuehren.md) — der Kernel führt fremden Code aus und tauscht ihn
> aus (Manifest, Root-Task, Caps, Hot-Reload, Treiber). **Kritischer Pfad.**
> [todo-B-verlaesslichkeit.md](todo-B-verlaesslichkeit.md) — die Zusicherungen tragen und sind
> messbar (Determinismus, ARM-Zweig, IOMMU, Färbung/SMT, Zeit/NUMA, Attestierung).
> Dateibesitz und die vier echten Kopplungen stehen am Ende von Strang A.

Der Mikrokern ist **das Basissystem eines Cloud-Servers**, nicht ein Hypervisor darunter und
keine VM darin. Es gibt keine Gastschicht: Isolation, Zeit- und Ressourcenverwaltung macht der
Kern selbst. Daraus folgen drei Anforderungen, die keine reine Fleißarbeit sind, sondern den
Entwurf mitbestimmen — und an denen die Abschnitte A–G im Folgenden hängen.

**Die Lastform (Simon, 2026-08-01): Vercel-artige Dienste.** Kurzlebige Funktionsaufrufe, hohe
Fluktuation, **viele Mandanten je Maschine**, fremder Code. Das ist keine Randnotiz, sondern legt
drei Größen fest, die sonst frei wählbar aussähen:

* **Spawn/Teardown ist der heiße Pfad**, nicht der Startpfad. Genau dort saßen die D0-Rennen.
* **Die Zahl gleichzeitig getrennter Mandanten ist heute 4.** `PARTITIONS = 4`, und `claim_stripe`
  scheitert ab dem fünften sauber (B-4.2) — richtig so, aber vier cache-getrennte Mandanten je
  Maschine sind für diese Lastform um Größenordnungen zu wenig. Und Erhöhen ist nicht gratis:
  `region_bytes()` ist `min(Farben, 64) / PARTITIONS` Seiten, bei `PARTITIONS = 64` also **eine
  Seite** je zusammenhängender Region. Diese Spannung ist der eigentliche Inhalt von B-4.1.
* **Die Maschine muss Blech sein.** Neu belegt am 2026-08-01 (B-4.5): läuft SEL4Lake selbst als
  Gast, schreibt der Wirt die Farbbits um, und die Cache-Trennung ist wirkungslos — gemessen, nicht
  vermutet. Wer „Isolation ohne VMs" verkauft und dafür gemietete VMs benutzt, verkauft nichts.

**Z1. Kein VM-Overhead, Isolation vollständig durch den Kern.** Der isolierte Pfad (eigene VSpace
je PD, gefärbt) muss der **Normalfall** werden, nicht die Ausnahme. Heute ist `spawn_isolated` der
reguläre Weg und ungefärbt; `spawn_isolated_colored` ist der Sonderweg (s. [A1](#a1-cache--timing-seitenkanäle-zwischen-pds)).

**Gesetzte Regel (Simon, 2026-07-29): TrustedSAS ist ausschließlich für eigenen Code. Kein
Kundencode läuft jemals in einer TrustedSAS-PD.** Das ist keine Vorsichtsmaßnahme, sondern folgt
aus dem Modell: TrustedSAS-PDs teilen sich einen Adressraum, und ihre Isolation ruht auf
*intralingualer* Sicherheit — safe Rust kann keinen Zeiger auf fremden Speicher erzeugen. Diese
Voraussetzung lässt sich nur an eigenem Quellcode prüfen (das Zertifikats-Gate ADR 0014 tut genau
das: `unsafe_status == ALL_PASS`), niemals an einem fremden Binary.

Drei Folgerungen, die den Entwurf binden:

* **Der isolierte Pfad ist der Produktpfad.** Seine Leistung und seine Skalierbarkeit *sind* das
  Produkt — nicht die des SAS-Pfads. Optimierungsarbeit gehört dorthin.
* **Das Zertifikats-Gate ist ein Betreiberwerkzeug, keine Kundenschnittstelle.** Kunden brauchen
  kein Zertifikat und bekommen keins; ein Weg, auf dem ein Kunde ein TrustedSAS-Zertifikat
  erlangen könnte, wäre ein Entwurfsfehler. Umgekehrt heißt das: [Z7](#z7-attestierung-und-messbarer-boot)
  betrifft die **Maschine**, nicht das Kundenbinary — der Kunde will wissen, *worauf* er läuft,
  nicht beweisen, *was* er mitbringt.
* **Zwei Klassen, zwei Zusicherungen.** Eigene Dienste (Treiber, Netzstack, Kontrollebene) dürfen
  TrustedSAS sein und zahlen dafür keinen Adressraumwechsel. Alles Fremde ist isoliert, gefärbt
  und wird so abgerechnet. Das gehört in `docs/invariants.md`, damit die Grenze nicht später aus
  Bequemlichkeit verwischt.

**Z2. Abrechnung feiner als der Zeittakt — „nur zahlen, was man braucht".** 100 Hz heißt 10 ms
Granularität; das ist zugleich die Untergrenze der Abrechnung *und* eine Jitterquelle, die HPC
gerade abschaltet. Ziel ist eine Abrechnung, die **nicht** an der Tick-Rate hängt: Verbrauch wird
per TSC/CNTVCT beim Ein- und Auswechseln gestempelt (Zyklen, nicht Ticks), und der Tick selbst
wird für Rechenkerne abgeschaltet statt beschleunigt. Ein schnellerer Tick wäre die falsche
Antwort — er erhöht die Auflösung *und* den Overhead; ein Zyklenstempel erhöht nur die Auflösung.
Siehe [Z5](#z5-tickless-für-rechenkerne) und [D](#d-verifikation) (per-TCB-`consumed_cycles`).

**Z3. Threads einfrieren, versetzen, weiterlaufen lassen (Server-übergreifend).** Das ist die
Eigenschaft, die VMs heute liefern (Live-Migration) und die ein VM-freier Entwurf ersetzen muss.
Sie ist die **anspruchsvollste** Anforderung im ganzen Dokument, weil sie quer zu fast allem liegt:
ein Thread ist nicht nur sein Registersatz, sondern sein Speicher, seine Caps, seine
IPC-Beziehungen und seine Position in den Kerneltabellen. Zerlegung in [Z4](#z4-checkpointrestore-eines-threads).

### Z11. Boot-Image = Kernel + **eine** Manifestdatei; alles andere außerhalb und austauschbar
**Klasse:** Aufbau/Betrieb · **Aufwand:** groß, betrifft Loader, Cap-System und IPC

**Gesetzte Regel (Simon, 2026-07-29):** Im Boot-Image liegen genau zwei Dinge — der Kernel und
eine Datei, die festlegt, *was* geladen wird. Alles Übrige (Treiber, Netzstack, Dienste,
Kontrollebene) liegt außerhalb des Images und ist **zur Laufzeit austauschbar, ohne dass IPC-
Beziehungen verlorengehen**.

Das ist die schärfere Fassung der bestehenden Projektgrenze („nur Mikrokern, Caps, Scheduler, IPC
und Microkit-Runtime im Image") und zieht Konsequenzen nach sich, die über sie hinausgehen.

- [ ] **Z11a. Das Henne-Ei-Problem benennen und lösen.** *(Bootloader-Weg belegt 2026-08-01:
      `tools/mkgrubiso.sh` baut ein GRUB-ISO aus Kernel + Boot-Archiv; ein echter Bootloader
      liefert nachweislich dasselbe wie QEMUs `-kernel`/`-initrd` — Speicherplan, ein Modul,
      `mbmod : ALL PASS`, `archive : 2 Modul(e) -> ALL PASS`, Root-Task läuft,
      `SELFTEST COMPLETE`. Einziger Unterschied: die Ladeadresse des Moduls, wie erwartet.
      Die Sorge wegen `Flags = 0` im Multiboot-Header war unbegründet — GRUB liefert
      Speicherplan und Module auch ungefragt. **Offen bleibt die zweite Hälfte:** das
      Nachladen zur Laufzeit. Der Plattentreiber dafür ist da und läuft seit A-5.1 (2026-08-01)
      als **geladenes Userland-Programm** — es bedient Leseanfragen über seinen Kanal, inhaltlich
      geprüft. Was noch fehlt, ist alles darüber: eine Partitionstabelle und ein Dateisystem.
      Ein Sektor ist noch kein Archiv.)*
      Einen Plattentreiber kann man nicht von
      der Platte laden. Der einzige ehrliche Ausweg: **der Bootloader liefert die Startmodule**
      (Multiboot-Module bei GRUB) — die liegen damit außerhalb des Kernel-Images, kommen aber
      trotzdem zum Boot an. Das Manifest nennt sie; der Kernel prüft, dass genau das ankam, was
      dort steht. Nachladen über Netz/Platte gibt es erst, wenn die zugehörigen Treiber laufen.
      Ohne diese Trennung („Startmenge vom Bootloader" vs. „Nachladen zur Laufzeit") wird das
      Manifest ein Wunschzettel, den niemand einlösen kann.
- [ ] **Z11b. Das Manifest ist ein Autoritätsdokument, kein Konfigurationsfile.** Es legt fest,
      wer geladen wird *und welche Caps er bekommt* — also die gesamte Anfangsverteilung von
      Autorität. Damit **muss** es signiert und gegen das Kernel-Image gebunden sein. Wer die
      Datei tauschen kann, besitzt die Maschine; ein Manifest ohne Signatur wäre eine
      Hintertür mit Dateiendung. Verwandt mit ADR 0014, aber nicht dasselbe: dort werden
      *Binaries* zertifiziert, hier die *Zuteilung*.
- [ ] **Z11c. Das Manifest ist der natürliche Ort für die Politik.** Farbstreifen ([A1](#a1-cache--timing-seitenkanäle-zwischen-pds)),
      NUMA-Knoten ([Z8](#z8-numa)), Kern-Affinität, Budget, Priorität — alles Dinge, die heute im
      Code stehen oder gar nicht existieren. Wenn sie ins Manifest wandern, sind sie
      *überprüfbar* und *änderbar ohne Neubau*. Wichtig dabei: die Farbe selbst gehört **nicht**
      hinein (sie ist maschinenlokal, s. [Z4c](#z4-checkpointrestore-eines-threads)) — wohl aber
      „diese Komponente bekommt einen exklusiven Streifen".
- [ ] **Z11d. Hot-Reload ohne IPC-Verlust — was dafür wirklich nötig ist.** Auf ARM existiert der
      Fall bereits (Phase 7: eine Server-PD wird über *dieselbe* Endpoint-Cap ersetzt). Für einen
      Betriebsanspruch reicht das aber nicht; drei Dinge fehlen:
      1. **Endpoint-Identität unabhängig vom Thread.** Clients halten Caps auf den *Endpoint*,
         nicht auf den Server-Thread — das trägt heute schon. Der Austausch ist dann ein
         Umbinden, wer auf dem Endpoint empfängt. Das muss **atomar** sein: zwischen „alter
         Server weg" und „neuer Server empfangsbereit" darf kein Zustand liegen, in dem ein
         `CALL` mit `NoEndpoint` scheitert.
      2. **Schwebende Transaktionen.** Ein Client, der in `CALL` blockiert, hält eine Reply-Cap
         auf den alten Server. Entweder der neue Server erbt die offenen Replys, oder der
         Austausch findet nur an einem *ruhenden* Punkt statt (keine offene Transaktion). Die
         zweite Variante ist ehrlich und für Stufe 1 wahrscheinlich richtig — sie braucht aber
         einen Begriff von „ruhend", den es heute nicht gibt. Verwandt mit [Z4a](#z4-checkpointrestore-eines-threads):
         derselbe Haltepunkt-Begriff trägt beides.
      3. **Zustandsübergabe.** Die neue Fassung braucht den Zustand der alten. Entweder eine
         Speicherregion, die den Austausch überlebt (dann ist ihr Format eine ABI und muss
         versioniert werden), oder ein ausdrückliches Übergabeprotokoll. Ohne Festlegung wird
         Hot-Reload ein Neustart mit Datenverlust und heißt nur anders.
- [ ] **Z11e. Protokollversionen prüfen, nicht hoffen.** Das Manifest nennt die Schnittstellen-
      version je Komponente; ein Austausch, der die Version ändert, wird **abgewiesen** statt
      durchgelassen. Sonst redet ein neuer Server mit alten Clients in einer Sprache, die beide
      für dieselbe halten.
- [ ] **Z11f. Was NICHT hot-reloadbar sein kann, muss benannt sein.** Der Kernel selbst, und
      alles, was eine Cap auf maschinenlokale Hardware hält, während sie in Benutzung ist
      (IOMMU-Kontexte, aktive DMA-Regionen). Eine Liste dessen, was der Austausch *nicht* umfasst,
      gehört in `docs/invariants.md` — sonst wird aus „alles ist austauschbar" im Betrieb eine
      Überraschung.

### Z4. Checkpoint/Restore eines Threads
**Klasse:** neues Subsystem · **Aufwand:** groß, mehrstufig

Reihenfolge ist hier keine Geschmacksfrage — jede Stufe ist Vorbedingung der nächsten.

- [~] **Z4a: der Haltepunkt steht** (2026-08-02, `system::freeze_thread`, Prüfzeile `freeze`).
      Details in [done.md](done.md#z4a--z4b-der-haltepunkt-und-was-auf-keinen-fall-mitwandert).
      „Benennbar" heisst zwei prüfbare Dinge: **nicht auf einem Kern** (über `current_id` je Kern
      gefragt, nicht geglaubt) und **keine offene IPC-Beziehung**. Die drei Ausgänge sind
      unterscheidbar, und der Unterschied trägt: `StillRunning` ist vorübergehend, `Busy` ist eine
      **Absage** — wer darauf wartet, wartet ewig. Geprüft wird die **Wirkung**: der Rundenzähler
      muss sich vorher bewegen, dann stehen, dann wieder laufen; zwei Mutationen machen die Suite
      rot.

      **Offen bleibt der Syscall.** `freeze_thread` ist eine Kernel-Funktion, kein `SYS_FREEZE`,
      und sie **wartet nicht** — sie nimmt die Scheduler-Sperre jedes Kerns, und darin auf einen
      anderen Kern zu warten ist die Bauanleitung für einen Deadlock. Der Aufruf ist idempotent;
      das Warten gehört dem Aufrufer. Ein blockierender Syscall darüber ist eine ABI-Frage und
      eine eigene Stufe.
- [~] **Z4b: die Verweigerungsregel steht, der Serialisierer nicht** (2026-08-02,
      `crates/sel4lake-cap/src/checkpoint.rs`, 7 Host-Tests). Der Registersatz war ohnehin der
      leichte Teil; gebaut ist der schwere: **was auf der Zielmaschine nicht dasselbe bezeichnen
      kann, wandert nicht mit — es wird verweigert, nicht ersetzt.** Verweigert werden `Mmio`,
      `Irq`, `Dma`, `Reply`, `Loader` und jede Beziehungs-Cap, deren Partner nicht im Umfang ist;
      die Gründe sind **einzeln** und kein Sammel-`false`, weil nur manche durch einen grösseren
      Umfang behebbar sind. Fail-closed: ein neuer Objekttyp wandert nicht mit, statt
      durchzurutschen.

      Drei Entscheidungen stecken in den Tests: Speicher wandert als **Inhalt** (zwei Regionen
      gleicher Länge an verschiedenen Physadressen sind extern gleich), Beziehungen über den
      **Platz im Checkpoint** statt über eine ID, und ein Budget trägt eine **Vorbedingung**
      (`SameTickSemantics`, Z4f) statt sie zu verschweigen.

      ~~**Offen:** ein Serialisierer und ein Wiederhersteller.~~ Beide stehen seit 2026-08-03,
      s. Stufe 2 direkt darunter.
- [~] **Z4 Stufe 2: ein Thread über die BOOTGRENZE** (2026-08-03, Prüfzeile `bootckpt`).
      Details in [done.md](done.md#z4-stufe-2-ein-thread-ueber-die-bootgrenze). Derselbe Kernel
      speichert und stellt wieder her und **entscheidet selbst welches** — er liest den Sektor
      (`CKPT_SECTOR = 32710`, ausserhalb beider Partitionen). Transport ist der vorhandene
      Blockdienst; der Kern ist Client, `programs/hardware/virtio-blk` blieb unverändert. Das
      Format liegt in `crates/sel4lake-cap/src/checkpoint.rs` (feste Breiten, LE, CRC-32,
      host-getestet), und `Image::build` ruft `classify_all`: **eine nicht übertragbare Cap
      verhindert den Checkpoint vor dem Schreiben.**

      **Der Befund, der den Entwurf umgebaut hat, gehört in die Merkliste:** der Fortschrittszähler
      ist unter KVM **reproduzierbar** (gemessen 133…155 an derselben Stelle des Hochlaufs). Eine
      Mutation, die den gefundenen Wert liest und meldet, ohne ihn zu setzen, traf den gespeicherten
      Wert exakt — und die Suite blieb grün. Erst eine **wachsende Kette** (jeder Lauf arbeitet
      +100 Runden weiter und speichert mit Epoche + 1) trennt „geerbt" von „selbst gezählt"; die
      Positivkontrolle dazu (`Zaehler-vorher != Fortschritt`) steht in der Suite.

      **Offen bleibt hier:** kein Trap-Frame und keine Register — das wäre ohne
      [Z4c](#z4-checkpointrestore-eines-threads) wertlos oder schlimmer, weil `RSP` in einen Stack
      zeigt, dessen Inhalt nach dem Neustart frischer Speicher ist. Und der Checkpoint ist
      **nicht authentifiziert** (Z4e): die Prüfsumme sagt „strukturell heil", nicht „von wem".
- [ ] **Z4c. Speicher mitnehmen.** Die private Region der PD ist der Zustand. Bei 64 KiB
      ([A1](#a1-cache--timing-seitenkanäle-zwischen-pds)) ist das billig, bei 2 MiB weniger.
      Offen: Dirty-Tracking, damit nicht alles kopiert werden muss, und die Frage, ob die
      **Farbe** auf der Zielmaschine erhalten bleiben muss (nein — Farbe ist maschinenlokal; die
      Zielmaschine färbt neu; das ist ein Argument dafür, Farbe nie in die ABI zu heben).

      **Seit Stufe 2 ist das die BLOCKIERENDE Stufe, nicht eine nebenläufige.** Der Checkpoint
      trägt eine Speicher-Cap als `Region { len }` — als **Länge**, nicht als Inhalt; drüben ist
      sie null. Und ein Trap-Frame lässt sich erst dann sinnvoll mitnehmen, wenn der Stack
      mitkommt: `RSP` ohne Stackinhalt ist ein Zeiger ins Leere, und der Fehler tritt später und
      woanders auf.
- [ ] **Z4d. IPC-Beziehungen.** Ein wandernder Thread mit offenem `CALL` hat einen wartenden
      Server zurückgelassen. Entweder Migration nur ohne offene Transaktionen (einfach, ehrlich,
      wahrscheinlich richtig für Stufe 1), oder Endpoint-Proxys über das Netz (ein eigenes
      Projekt).
- [ ] **Z4e. Transport + Vertrauen.** Ein Checkpoint ist der vollständige Zustand eines Tenants.
      Er geht verschlüsselt und authentifiziert über das Netz, oder gar nicht. Hängt an
      [Z7](#z7-attestierung-und-messbarer-boot): die Zielmaschine muss nachweisen können, dass sie
      dieselbe Kernelversion mit denselben Zusicherungen fährt — sonst wandert der Zustand in eine
      schwächere Umgebung, und der Tenant merkt es nicht.
- [~] **Z4f. Identische Server als Vorbedingung benennen.** „gleicher Server" muss geprüft
      werden, nicht angenommen: gleiche Architektur, gleiche Kernelversion, gleiche
      Cache-/NUMA-Klasse. Ein Vergleich, der nur die Architektur prüft, lässt einen Thread von
      einer Maschine mit `invtsc` auf eine ohne wandern — und die Abrechnung aus [Z2](#z-zielarchitektur-stand-2026-07-29--woran-alles-andere-zu-messen-ist)
      wird still falsch.

      **Ein Stück davon steht (2026-08-03):** der Checkpoint trägt `kernel_code_hash` und wird
      abgewiesen, wenn er zu einem anderen Kernel-Image gehört (Lesecode 7, Negativfall in
      `test-qemu-x86-load.sh`). Das ist die **Kernelversion**, und nur sie.

      **Nicht geprüft — und das ist genau die Lücke, die dieser Punkt meint:** die MASCHINE. Zwei
      Rechner mit demselben Image, aber verschiedener Cache-/NUMA-Klasse oder einer ohne `invtsc`,
      sind heute nicht unterscheidbar. Die Vorbedingung `SameTickSemantics` steht **im** Checkpoint
      (`precondition_bits`), aber niemand wertet sie aus — sie zu tragen und nicht zu prüfen ist
      der halbe Weg, und der halbe Weg sieht von aussen aus wie der ganze.

### Z5. Tickless für Rechenkerne
**Klasse:** Scheduler/Timer · **Aufwand:** mittel

- [ ] Der 100-Hz-Tick läuft auf **jedem** Kern. Für einen Kern, auf dem genau ein rechenbereiter
      Thread liegt, ist jeder Tick reiner Verlust — und in HPC die klassische „OS-Noise"-Quelle,
      die Kollektivoperationen über tausende Ränge hinweg verschleppt. Nötig: Timer nur armieren,
      wenn es etwas zu verdrängen gibt (>1 lauffähiger Thread oder ein ablaufendes Budget), sonst
      abschalten. Zusammen mit [Z2](#z-zielarchitektur-stand-2026-07-29--woran-alles-andere-zu-messen-ist)
      ergibt das: Abrechnung per Zyklenstempel, Verdrängung per Bedarf — die beiden werden
      entkoppelt, und genau das ist der Punkt.
- [ ] Kern-Isolierung als Politik: Rechenkerne bekommen keine IPIs, keine Balancierung
      (`balance_once` ist per Default aus — hier ist das zufällig schon richtig), keine
      Interrupt-Zustellung außer ihren eigenen.

### Z6. SMT — die Lücke, die Cache-Coloring NICHT schließt
**Klasse:** Seitenkanal · **Aufwand:** Scheduler-Politik, klein bis mittel

- [ ] [A1](#a1-cache--timing-seitenkanäle-zwischen-pds) partitioniert den **LLC**. Zwei
      Hyperthreads auf demselben physischen Kern teilen sich aber L1, L2, TLB, Store-Buffer und
      Branch-Predictor — dagegen hilft keine Farbe, und zwar prinzipiell nicht. Solange SMT an ist
      und der Scheduler Geschwister-Threads beliebig belegt, ist die A1-Zusicherung auf einer
      SMT-Maschine deutlich schwächer, als sie klingt. Zwei gangbare Wege: SMT abschalten (kostet
      Durchsatz, ist ehrlich) oder **Gang-Scheduling der Geschwister** — ein physischer Kern
      gehört zu jedem Zeitpunkt genau einem Tenant. Der zweite Weg braucht die
      Topologie (`CPUID.1F`/`0B` bzw. MPIDR) im Scheduler; die liest heute niemand.
- [ ] Solange das offen ist, gehört es in die Zusicherung: `docs/invariants.md` muss sagen, dass
      A1 den LLC trennt **und die kernlokalen Strukturen nicht**.

### Z7. Attestierung und messbarer Boot
**Klasse:** Vertrauen · **Aufwand:** mittel-groß

- [ ] Das Zertifikats-Gate (ADR 0014) sichert, welche **Binaries** starten dürfen. Ein Tenant
      will aber aus der Ferne nachweisen können, **was auf der Maschine läuft**, bevor er seinen
      Zustand dorthin gibt — Measured Boot, Messkette bis in den Kernel, Signatur über die
      Messung. Vorbedingung von [Z4e](#z4-checkpointrestore-eines-threads); ohne das ist
      Thread-Migration ein Vertrauensvorschuss an eine unbekannte Maschine.

### Z8. NUMA
**Klasse:** Speicher/Skalierung · **Aufwand:** mittel, greift in den Allokator

- [ ] Der `PhysAllocator` hat **eine flache Freiliste ohne Knotenbegriff**. Beim Zielbild
      Dual-EPYC ist das kein Detail: Speicher am falschen Knoten kostet grob Faktor zwei an
      Latenz, und bei CXL wird es schlimmer. Nötig: Knoten aus ACPI SRAT/SLIT lesen, Freilisten je
      Knoten, PD-Zuteilung knotenlokal, und der Scheduler darf einen Thread nicht auf einen Kern
      legen, dessen Knoten seinen Speicher nicht hält.
- [ ] **Verzahnt mit A1**: Farbe *und* Knoten müssen **gemeinsam** vergeben werden. Zwei
      unabhängig entwickelte Politiken kämpfen sonst gegeneinander — der Farbstreifen erzwingt
      eine Physadresse, der Knoten eine andere, und wer zuerst zuteilt, gewinnt. Das gehört in
      **eine** Entscheidung, nicht in zwei Schichten.

### Z9. Fehlerdomäne
**Klasse:** Betrieb · **Aufwand:** Entwurfsentscheidung

- [~] **Die Festlegung steht (B-6.2, 2026-08-02) — und die Prämisse dieses Eintrags war falsch.**
      „Ein Kernel-Panic reißt den Knoten mit" stimmt **nicht**, und das ist die schlechtere
      Nachricht: der Panic-Pfad hält den Kern **ohne IRQ-Maskierung**, der nächste Timer-Tick
      holt ihn zurück in den Scheduler. Gemessen lief der Knoten **61 s weiter** und meldete
      `ipc`, `ring3`, `iommu`, `dmatok` als ALL PASS — mit einer nachweislich verletzten
      Invariante. Vier verschiedene Ausgänge je nachdem **wo** der Panic auftritt, darunter ein
      stiller Totalausfall (unter der MEM-Sperre) und ein Zustand, der von außen nicht von einem
      Deadlock zu unterscheiden ist.

      Festlegung in `docs/fehlerdomaene.md` (betreiberseitig, mit Messtabelle und VM-Vergleich),
      normativ als `docs/invariants.md` §14.

      **Offen ist die Entscheidung, nicht die Beschreibung:** die Festlegung ist heute
      *behauptet*, nicht *hergestellt*. Drei Maßnahmen von zusammen etwa einem halben Tag machen
      sie wahr — IRQs im Panic-Pfad maskieren, ein Rekursionswächter im Panic-Handler,
      `panic → system_off` (beide Abschaltwege existieren und werden aus `panic.rs` **nie**
      gerufen). Das ist ein Abwägen zwischen Verfügbarkeit und Ehrlichkeit und gehört Simon.

### Z10. I/O überhaupt
**Klasse:** Grundlage · **Aufwand:** groß

- [ ] Kein Netzstack, kein Dateisystem. Eine Cloud ohne Netz und Speicher ist keine.
      *(Zwei Schritte erledigt, 2026-08-01: **A-5.2** brachte Treiberlogik für Blockgerät und
      Netzkarte, kernfrei in `crates/sel4lake-virtio`; **A-5.1** brachte die erste Treiber-PD —
      `programs/hardware/virtio-blk` liest einen Sektor als geladenes Userland-Programm, mit
      eigener Gerätezuteilung aus dem Manifest.)*
      Seit 2026-08-02 ist der Treiber ein **Dienst**: er bedient Anfragen über seinen Kanal und
      ist im Betrieb austauschbar (A-5.1 abgeschlossen).
      Seit 2026-08-02 steht darüber ein **Speicherstapel, vollständig außerhalb des Kerns**:
      Blockdienst (A-6.1), GPT (A-6.2) und ein lesendes **FAT16-Dateisystem als eigene PD**
      (A-6.3). Eine Datei wird über GPT → FAT16 → Blockdienst → Treiber gelesen.
      Seit A-6.4 **schreibt** sie auch (zweiter Cluster, beide FAT-Kopien, Flush, jedes Byte
      zurückgelesen; unabhängig am Abbild nachgeprüft).
      **Was weiterhin fehlt:** Dateien anlegen/löschen, Unterverzeichnisse, lange Namen — und ein
      **Netzstack**. Jeder Treiber **pollt**: eine IRQ-Cap ist nicht erteilbar, solange es keine
      IRTE-Vergabe gibt (B-3).
- [ ] Für durchgereichte Geräte ist **Interrupt Remapping** ([E](#e-dma-härtung--rest), Schritt 4)
      keine Kür: ohne IR kann ein Gerät beliebige Interrupt-Nachrichten erzeugen. Das ist der
      Standardausbruch aus einer Geräte-Zuteilung und muss vor dem ersten Tenant-Gerät stehen.

---

## A. Offen aus dem Sicherheits-Review (ext-29)

### A1. Cache-/Timing-Seitenkanäle zwischen PDs
**Klasse:** Seitenkanal · **Aufwand:** Architekturänderung, kein Patch

`[~]` **Stufe 1 steht und ist auf BEIDEN Architekturen gemessen** (s. [done.md](done.md)):
Cache-Geometrie wird aus der HW gelesen, der Allokator vergibt farbrein, und zwei so erzeugte PDs
teilen sich nachweislich keine Cache-Farbe — Region, Kernel-Stack und Seitentabellen. x86 seit
2026-08-01 (seit 2026-08-02 mit der ECHTEN Geometrie der Maschine, 512 Farben — vorher meldete
QEMU 256 erfundene), aarch64 seit 2026-08-02 (16 Farben — die Aufteilung, an der die
`MASK_BITS`-Verwechslung hing). Offen bleibt das Folgende.

- [ ] **Der reguläre Weg ist weiterhin ungefärbt.** `spawn_isolated` (2-MiB-Region) kann es
      strukturell nicht sein: 2 MiB sind 512 Seiten, also 512 aufeinanderfolgende Farben — bei den
      gemessenen 256 Farben überstreicht ein einziger Blockdeskriptor jede Farbe zweimal. Färbung
      gibt es nur über `spawn_isolated_colored`, und die kostet die kleinere Region
      (`colors::region_bytes()`, bei 4 Partitionen 64 KiB) plus seitenweises Mapping statt eines
      Block-PTE. **Die Entscheidung, welcher Weg der reguläre sein soll, steht aus** — solange
      `spawn_isolated` der Normalfall ist, ist A1 im Normalbetrieb *nicht* wirksam.

- [x] **Die Farbanzahl begrenzt die Anzahl gleichzeitig getrennter PDs** — erledigt mit B-4.2,
      hier bis 2026-08-01 nur nicht nachgetragen. `claim_stripe`/`release_stripe`
      (`kernel/src/colors.rs`) führen Belegung über `STRIPES_TAKEN`; ist kein Streifen frei, gibt es
      `None`, und die PD entsteht **gar nicht erst** — kein Ersatzsatz, keine „alle Farben"-Rückfallebene.
      `mask_for` (rundläufig, ungeprüft) steht nur noch im Test selbst; der Spawn-Pfad
      (`system.rs:2069`) nimmt `claim_stripe`. Der Test `stripe` deckt beide Ausgänge ab.

- [x] **Nur auf x86 gemessen** — behoben am 2026-08-01. Der fehlende Schlüssel war nur die
      *äußere* Hürde; B-2.1 erzeugt ihn inzwischen selbst. Die *innere* saß tiefer: `run_color` und
      `run_stripe_alloc` hatten ihren **einzigen Aufrufer in `arch/x86_64/bringup.rs`**. Die ARM-Suite
      hätte also auch mit Schlüssel nichts gemessen — sie rief nur `colors::report()` (Geometrie),
      nie den Test. Genau die Fehlerform, die dieses Projekt dreimal bezahlt hat, ein viertes Mal.

      Behandlung wie bei `dmatests.rs`: der Test liegt arch-neutral in `kernel/src/colors.rs` und
      wird von **beiden** Hochlaufwegen gefahren (aarch64 aus `threads::spawn_demo`, ganz am Anfang —
      `run_stripe_alloc` braucht den Ruhezustand). Die Druckstelle liegt **einmal** in
      `colors::report_color`; die vorherige Verdopplung war die Ursache des `FAIL`-statt-`FAILURES`-
      Fehlers. `color`/`stripe` stehen jetzt in der ARM-Abschlussbedingung und in `test-qemu.sh`.

      **Nicht** neu ist die Geometriemessung — die steht seit B-2.2 (2026-07-29) und wurde dort
      gegen drei CPU-Modelle geprüft, die *verschiedene* Werte liefern (`cortex-a72`/`a53` → 16,
      `max` → 32). Genau das belegt, dass `CCSIDR_EL1` gelesen und nicht eine Konstante
      zurückgegeben wird. Neu ist, dass die **Zuteilung** dort geprüft wird: bis heute lief auf
      aarch64 die Meldung, nicht der Test.


      **Der Umzug hat sofort zwei echte Fehler gefunden — beide nur auf aarch64 sichtbar, beide
      inzwischen BEHOBEN:**

      1. `region_bytes()` rechnete mit `sel4lake_mem::MASK_BITS` (64) statt mit der tatsächlichen
         Farbanzahl. Auf x86 (256 Farben) zufällig richtig; auf aarch64 (16 Farben) umfasst ein
         Streifen nur 4 Farben, eine 64-KiB-Region aber 16 aufeinanderfolgende Seiten — also jede
         Farbe, mehrfach. **Behoben** (Laufzeitrechnung `min(count(), MASK_BITS) / PARTITIONS`).
      2. `sel4lake_mem::stripe` teilte ebenfalls `MASK_BITS` auf statt `count()`. Bei 16 Farben
         umfasste Streifen 0 damit *alle* Farben und die Streifen 1–3 keine — und weil leere Mengen
         sich nicht schneiden, meldete der Selbsttest trotzdem „disjunkt". **Behoben am
         2026-08-02**: `stripe(i, n, colors)` nimmt die Farbanzahl als Parameter, mit Host-Tests
         gegen 16 **und** 256 Farben (ein Test nur gegen 256 wäre grün gewesen und hätte nichts
         belegt).

      **Drittens — der Test war auf aarch64 AUSGEHÄNGT, und das ist seit 2026-08-02 behoben.**
      An seinem alten Platz ganz am Anfang von `threads::spawn_demo` belegte und gab er Speicher
      frei, *bevor* die baseline-empfindlichen Tests ihre Ausgangswerte nehmen; danach fiel mal
      `captest`, mal `sched` durch. Der Ausweg war damals, ihn auszuhängen — und dabei
      `COLOR_OK`/`STRIPE_ALLOC_OK`/`PPROBE_OK` **hart auf `true`** zu setzen. Das war die
      schlechtere Hälfte: drei dauerhaft wahre Konjunkte in `all_done()`, keine Berichtszeile,
      kein Check in `test-qemu.sh` — die Abwesenheit war damit nicht bloß unbelegt, sie war
      **unsichtbar**.

      Die Lösung ist der **Platz**, nicht das Weglassen: die drei Tests hängen jetzt als letztes
      Glied der Testkette in `demo_report_then_idle` (`run_color_suite`), hinter `cross`, `strand`
      und `loadstop`. Danach nimmt keine Prüfung mehr eine Baseline. Gemessen (8 Läufe,
      `test-qemu.sh`): `color : ALL PASS`, `stripe : ALL PASS`, `pprobe : SKIP` (unter TCG kann die
      Positivkontrolle nicht tragen — kein echter Cache), `captest` und `sched` in allen acht
      Läufen unverändert grün, identische Ergebnissignatur. Damit ist A1 **zum ersten Mal auf
      aarch64 belegt** — und zwar auf der 16-Farben-Aufteilung, also genau dem Fall, den (2) falsch
      machte. Details in [done.md](done.md).
- [ ] **Way-Partitionierung (Intel CAT / ARM MPAM) nicht betrachtet.** Sie träfe dieselbe
      Eigenschaft über die Hardware statt über den Allokator, käme ohne kleinere Regionen und ohne
      seitenweises Mapping aus — und ließe sich mit der Färbung kombinieren. QEMU emuliert CAT
      nicht, also wäre sie hier **nicht prüfbar**; das ist ein Grund, sie zurückzustellen, kein
      Grund, sie zu vergessen.
- [ ] **A1 trennt den LLC und sonst nichts.** Kernlokale Strukturen (L1, L2, TLB, Store-Buffer,
      Branch-Predictor) bleiben geteilt, sobald zwei Tenants auf Geschwister-Hyperthreads liegen —
      s. [Z6](#z6-smt--die-lücke-die-cache-coloring-nicht-schliesst). Und die Farbvergabe muss mit
      der NUMA-Zuteilung **gemeinsam** entschieden werden, nicht nacheinander, s.
      [Z8](#z8-numa).

- [~] **Der Wirkungsnachweis ist gebaut, aber nicht erbracht** (B-4.5, s.
      [todo-B-verlaesslichkeit.md](todo-B-verlaesslichkeit.md)). Der Prime+Probe läuft
      arch-neutral bei **jedem** Start und wird von beiden Suiten gelesen; Aufbau,
      Positivkontrolle, Farbwahl und Bilanz sind auch dort geprüft, wo kein Urteil möglich ist —
      ein Prüfpfad, der zum ersten Mal am Zieltag läuft, ist am Zieltag kaputt.

      **Die alte Annahme „erst auf Blech ODER unter KVM" war falsch:** unter einem Hypervisor ist
      die Frage nicht schwer zu messen, sondern **prinzipiell nicht entscheidbar** — ein Gast färbt
      gastphysische Adressen, und die zweite Übersetzungsstufe schreibt die Farbbits um. Gemessen
      (Gast): `ungestoert=41 disjunkt=234 gleichfarbig=210` — die Positivkontrolle trägt, der
      disjunkte Farbsatz schützt **nicht**.

      **Offen:** ein Lauf auf echter Hardware — und die Maschine muss eine Bedingung erfüllen:
      `Größe der nicht partitionierten Cache-Ebene < LLC / PARTITIONS`. Sonst gibt es **keine
      gültige Opfergröße**, und das ist eine Eigenschaft der Maschine, nicht des Tests.

### A2. Spectre-v2 auf HW ohne FEAT_CSV2
**Klasse:** Seitenkanal · **Aufwand:** klein, aber HW-abhängig

`array_index_nospec` (v1) und die Barriere beim VSpace-Wechsel sind da. Für v2 auf HW, die **nicht**
`CSV2 >= 1` meldet, wäre Branch-Predictor-Invalidierung beim Adressraumwechsel nötig —
`SMCCC_ARCH_WORKAROUND_1` per SMC. QEMU `virt` bietet den Firmware-Call nicht an → erst auf echter
HW (STM32MP257F-DK) umsetzbar/testbar. Boot-Report `spec :` zeigt bereits, was die HW garantiert.

### A3. Globale Cap-Tabelle bleibt geteilte Ressource fester Größe
**Klasse:** Fairness/Skalierung · **Aufwand:** mittel-groß

`CAP_BUDGET_PER_PD` begrenzt jetzt den **Schaden**, ersetzt aber nicht das seL4-Modell (jede PD
bekommt ihren CNode aus dem **eigenen** Untyped-Budget). Solange die Tabelle geteilt ist, bleibt
Kapazität eine globale Größe.
**Nebenbedingung beim Vergrößern: erledigt** (A-3.3). `ReplyFinal`/`Finalized` hielt sein
`[(u32,u64); NOBJECTS]`-Array **auf dem Kernelstack**, und nicht als einziges: `dma_finalize` hielt
eine Kopie derselben Regionen, jede `finalize`-Implementierung der Enforcer nochmals ein
`[usize; NOBJECTS]`. Gemessen am x86-Release war der Rahmen von `cap_delete`/`cap_revoke` deshalb
**6168 Byte** — gross genug, dass der Compiler eine Stack-Probe einbaute. Jetzt leiht die Struktur
ihren Speicher vom Aufrufer (ein statischer Puffer im Kernel), Rahmen 72 bzw. 88 Byte. `NOBJECTS`
hochzuziehen koppelt damit nicht mehr an die Stackgrösse. Siehe auch
[C3](#c3-cap--pd--ipc-tabellen-dynamisch).

*(Die frühere Angabe „aktuell 2 KiB" war veraltet: Kernel-Threads haben 64 KiB, EL0-Threads einen
16-KiB-EL1-Stack. Der Punkt stand trotzdem — 6,2 KiB waren 38 % des kleineren der beiden.)*

### A4. Kein Syscall zum Löschen eigener Caps
**Klasse:** ABI-Lücke · **Aufwand:** klein-mittel

Die ABI kennt kein `CDELETE`/`CMOVE`/`CCOPY` — Cap-Slots einer PD räumt nur der Kernel (Teardown,
PDCTL). Folge: ein langlebiger Dienst, der dynamisch Caps empfängt (IPC-Grant), läuft gegen
`CAP_BUDGET_PER_PD` und kann sich **nicht selbst befreien**. Nötig: mindestens `SYS_CDELETE` (eigener
Slot, cap-gegatet auf den eigenen Cspace), sinnvollerweise auch ein wählbarer Empfangs-Slot für
Grants statt des festen `GRANT_RECV_SLOT`.

---

## B. Thread-Migration — Rest

**Ziel:** Threads sind nicht mehr fest an den Kern gebunden, auf dem sie erzeugt wurden.
**Blocker war:** `ThreadId.slot` kodierte den Kern (`slot / PER_CORE`) — die Identität eines Threads
hing an seiner Kern-Affinität.

- [~] **B4** Policy vorhanden (`balance_once`, im Tick-Pfad hinter einem Intervall), aber per
      Default **AUS** (`system::set_balancing`). **Offen:** standardmäßig einschalten. Blocker sind
      nicht der Mechanismus, sondern die Demo-/Testthreads dieses Images, die feste Affinität
      voraussetzen (Cross-Core-IPC-Test, Budget-Donation, Platzierungs-Telemetrie) — die müssten
      erst affinitätsunabhängig formuliert werden.

**Ebenfalls offen:** Migration eines Threads mit aktiver **Budget-Donation** (die Donation-Links
sind lokale Slots — heute wird die Migration in dem Fall schlicht verweigert, bis der Server
geantwortet hat). Für eine kernübergreifende Donation müssten die Links globale `gid`s werden.

**Bewusst NICHT migriert:** der laufende Thread eines Kerns (nur bereite/blockierte) — sonst müsste
der Trap-Frame kernübergreifend übernommen werden.

---

## C. Flexible Kapazitäten (Zielbild: 256 Kerne, viele tausend Prozesse)

**Zielbild:** Dual-EPYC-Klasse — **256 Kerne**, **viele tausend** Prozesse, effizient.
Die Thread-/Scheduler-Seite ist umgestellt (Kapazität kommt beim Boot aus dem RAM, heiße Pfade
O(1); belegt durch den `scale`-Test mit 1024 gleichzeitigen Threads). Offen bleiben die Cap-/IPC-
Tabellen, die GIC-Skalierung und der x86-Port.

- [ ] **C3** <a id="c3-cap--pd--ipc-tabellen-dynamisch"></a>Cap-/PD-/Endpoint-/Notification-Tabellen
      dynamisch (hing an [A3](#a3-globale-cap-tabelle-bleibt-geteilte-ressource-fester-größe):
      `ReplyFinal` zuerst vom Stack lösen — **das ist seit A-3.3 erledigt**, der Weg ist frei).

### C4. Effizienz bei tausenden Threads (lineare Scans beseitigen)

Kapazität allein reicht nicht — mehrere Pfade sind **O(n)** in der Tabellengröße und werden bei
tausenden Threads zum Engpass:

- [ ] `CapSpace::{free_slot_index, alloc_object}` — lineare Scans → Freilisten.

- [ ] `PdTable::create` — linearer Scan → Freiliste.

- [ ] `purge_ipc_queues` — iteriert **alle** Endpoints + Notifications je Thread-Tod.

- [ ] **Kernel-Stacks: 64 KiB je Thread.** Bei zehntausenden Threads ist das die bestimmende
      Speichergröße (nicht die Tabellen) — 10 000 Threads = 640 MiB nur Stacks.

### C5. GIC-Skalierung (ARM-Blocker für > 8 Kerne) — **weiterhin offen**

`sel4lake-hal::gic` ist **GICv2** (8-CPU-Grenze, `GICD_SGIR`-Zielmaske ist 8 Bit). Mehr als 8 Kerne
brauchen auf ARM **GICv3/GICv4** (Redistributoren je Kern, `ICC_SGI1R_EL1`, ITS für MSI). QEMU:
`-machine virt,gic-version=3` (bis 512 vCPUs). **Solange das fehlt, sind >8 Kerne auf ARM nicht
testbar**, unabhängig von den Kapazitäten. Die Kapazitätsseite ist seit ext-30 vorbereitet
(`MAX_CORES = 256`, Tabellen zur Boot-Zeit dimensioniert) — es fehlt die Interrupt-Hardware.

### C6. x86-64-Port — Rest

`sel4lake-hal` ist jetzt architekturselektiv (`src/aarch64/` + `src/x86_64/` hinter derselben API).
Der **Kernel-Kern läuft auf x86_64**: dieselben Selbsttests, derselbe präemptive Scheduler,
dasselbe cap-gesicherte IPC — ohne ein einziges `cfg(target_arch)` im Kern. Details:
`README-X86.md`.

- [ ] **PCID** als Optimierung: der Adressraumwechsel flusht derzeit den ganzen TLB (die ASID ist
      eine reine Software-Kennung). Mit `CR4.PCIDE` + getaggten Einträgen entfiele das

- [~] **IOMMU (VT-d)**: Schritte 1–3 erledigt (s. [done.md](done.md)) — `attach` liefert `Some`,
      `dmawin`/`dmatok` laufen auf x86 mit **derselben** Funktion wie auf ARM.
      **Offen:** Schritt 4 (Interrupt Remapping inkl. CFI-Abschaltung, setzt Queued Invalidation
      voraus) und die Mehr-Einheiten-Aggregation (AGAW-Minimum, Zähler über alle DRHDs).

- [ ] **Boot-Archiv** über Multiboot-Module (`SYS_LOAD` schlägt derzeit sauber fehl)

- [~] Die ARM-seitigen Demo-/Testdienste (`threads/mod.rs`, 5402 Zeilen) sind stark ARM-gekoppelt
      (EL0-Isolation, MMIO/RTC, DMA). `dmawin`/`dmatok` sind nach `dmatests.rs` herausgezogen und
      laufen auf beiden Architekturen; **offen** sind `dmagen` (braucht ein arch-neutrales
      Leaf-Rücklesen; die Kohärenz-Teilprüfung existiert auf x86 gar nicht — legitimer SKIP) und
      der virtio-Negativtest (hängt am ARM-virtio-Treiber). Hängt mit [F1](#f-debug-testcode-aus-dem-release-build-nehmen)
      zusammen: dasselbe Modul, zwei Gründe es aufzuteilen.

---

## E. DMA-Härtung — Rest

Reihenfolge nach struktureller Wirkung, nicht nach Aufwand.

- [ ] **Kern-Uebergabe an SEL4Lake (Variante B) — Stufe 2.** Stufe 0/1a/1b sind belegt:
      `tools/handover/` nimmt fuenf E-Cores offline, schickt ihnen INIT-SIPI-SIPI in ein
      Trampolin und bringt sie in den Long Mode mit eigener GDT, eigenen Seitentabellen und
      eigenem CR3 — wiederholbar, ohne Reboot, `rmmod` gibt alles zurueck.
      Erledigt auf der Kernel-Seite:
      * **x2APIC** (MSR-Pfad). Entscheidung fuer maximale Leistung *und* weil xAPIC nur 255
        Kerne adressieren kann. Zweistufiger Uebergang aus -> xAPIC -> x2APIC.
      * **Speicher als Bereichsliste** (`init_mem_regions`), im QEMU-Lauf immer zerstueckelt
        gefahren.
      * **`HandoverInfo`** als gemeinsame Struktur beider Startwege, vom Multiboot-Pfad
        mitbenutzt und mitgeprueft.
      Offen, in dieser Reihenfolge:
      1. **Bild-Platzierung ohne Relokation.** Der Kernel ist auf 1 MiB gelinkt, und dort liegt
         Linux. Statt ihn positionsunabhaengig zu machen: das Modul legt das Bild in einen
         4-MiB-Block (`alloc_pages(order=10)` — genau die Buddy-Obergrenze, das Bild ist 3,1 MB)
         und bildet in **seinen** Seitentabellen VA 1 MiB auf diese PA ab. `mmu::init_primary`
         muss die Abbildung dann uebernehmen statt reiner Identitaet — ein Versatz aus der
         `HandoverInfo`, keine Relokation.
      2. **Konsole als Ring** statt UART: die serielle Schnittstelle gehoert dem Wirt, zwei
         Schreiber ergeben verschachtelte Zeilen. Ring in der `HandoverInfo`, vom Modul ueber
         debugfs lesbar.
      3. **Was auf uebergebenen Kernen ausbleiben muss:** VT-d (Linux besitzt die Einheiten fuer
         Interrupt-Remapping — `GCMD.TE` toetet jeden Linux-DMA) und `intc::init_dist()` (legt
         den 8259 still, der ist global).
      4. **ELF-Laden im Modul** und der Sprung ins Bild.
      Wenn ein uebernommener Kern echten Kernel-Code faehrt, ist `release=1` beim Entladen nicht
      mehr selbstverstaendlich — dann `release=0` und Reboot.

- [ ] **VT-d-Zuteilung (Punkt 6)** — Abnahmekriterium **vorab**: nicht „neue x86-Tests grün",
      sondern **`dmaalign`/`dmawin`/`dmagen`/`dmatok`/Audit 4–7/Negativtest hören auf zu
      skippen** — ohne x86-Sonderpfade in den Tests. Ein separater `vtdtest` wäre das
      Warnsignal: er hieße, dass die Eigenschaften auf x86 anders formuliert sind, und dann
      existiert doch ein zweiter Entwurf. Was danach noch skippt, ist die ehrliche Liste dessen,
      was x86 nicht hat.
      Zerlegung:
      1. [x] **`VtdCaps`** einmal beim Hochlauf lesen, protokollieren, jede Bedingung daraus
         ableiten (nicht an der Verwendungsstelle entscheiden). Dazu die Reihenfolge-Falle
         behoben: `GCMD` ist kein RMW-Register, und `TE = 0` heißt **freier DMA**, nicht
         Blockade — jedes Kommando geht jetzt über `gcmd_issue`, das die Zustandsbits aus `GSTS`
         übernimmt. Ein `SRTP`-Schreibzugriff „nur mit dem Kommandobit" hätte die Übersetzung
         für die Dauer des Wechsels abgeschaltet, und der Test hätte es als Erfolg gelesen.
         **Messung auf der Zielplattform (QEMU q35 + intel-iommu):** SAGAW `0x6` (39 **und** 48
         Bit) → 39 Bit / 3 Level gewählt, damit die Fensterarithmetik nicht zweimal existiert;
         MGAW 48 → Eingangsgrenze `0x80_0000_0000`, identisch zur ARM-Seite; ND → 65536 Domains;
         QI und IR vorhanden; Scalable Mode aus; **ein** Fault-Recording-Register.
         Drei Werte, die Arbeit nach sich ziehen:
         * **`CM = false`** — anders als erwartet. QEMUs `intel-iommu` hat `caching-mode` per
           Default **aus**, das heißt: der Aufbau verzeiht hier ein fehlendes „nach dem Anlegen
           invalidieren", und auf `CM = 1`-Hardware bräche es. Die Richtung der Divergenz ist
           also umgekehrt zur Erwartung, die Konsequenz dieselbe: unbedingt invalidieren, `CM`
           nur protokollieren. Für Schritt 3 zusätzlich `caching-mode=on` in den Testaufbau, um
           genau das zu erzwingen.
         * **`ECAP.C = false`** — die Einheit ist **nicht** page-walk-kohärent. Schreibvorgänge
           auf Root-/Context-/Second-Level-Einträge brauchen einen Cache-Clean. Betrifft die
           Tabellen, nicht die Puffer — ein Pfad, den `dma_granule()` gar nicht abdeckt, und den
           QEMU nicht bestraft.
         * **`SC = false`** — keine Snoop Control, No-Snoop ist nicht überstimmbar. „x86 ist
           kohärent" gilt damit nur, solange kein Gerät No-Snoop benutzt. Gehört als Bedingung
           an `dma_granule() == 1`, nicht als Konstante.
      2. [x] DMAR/DRHD inkl. Device-Scope, **Gruppenbildung aus ACS**, RMRR-Ausschlüsse.
         Umgesetzt als **reine Funktion über eingespeiste Daten** (`hal::dmar`, `forbid(unsafe)`):
         `parse` nimmt einen Byte-Slice, `build_groups` eine Topologiebeschreibung. Der reale Pfad
         füllt beides aus ACPI/PCI-Enumeration, der Selbsttest aus Literalen — auf dem
         Standardaufbau (flach, keine RMRR) liefen Ausschlusspfad und Gruppenfälle sonst **nie**.
         Sensitivität belegt: „Catch-all in derselben Schleife" macht `catch_all_last` **und**
         `bridge_scope_subtree` rot, „Typ 2 wie Typ 1" nur letzteres — beide Male genau die
         Zusicherung, die die Mutation verletzt.
         Reale Messung (QEMU q35): 7 Geräte, 1 Einheit, **5 Gruppen**, 0 Ausschlüsse, keine ATSR,
         Segment durchweg 0, Oracle 0.
         Offen aus diesem Schritt: mehrere DRHD-Einheiten sind **geparst**, aber `VtdCaps` ist
         noch eine Einheit — das Minimum über alle Einheiten, die eine zuteilbare Gruppe scopen,
         und die Aggregation von Fault-/Config-Zählern über alle Einheiten stehen aus (bis dahin
         wäre das Oracle für alles blind, was nicht an Einheit 0 hängt). Eine Gruppe, die über
         Einheiten streut, wird bereits **vollständig** ausgeschlossen (`GroupSpansUnits`).
         Alt: Ausgabe:
         Liste zuteilbarer **Gruppen** (nicht Geräte — ohne ACS auf allen Upstream-Bridges ist
         die Isolationsgranularität die Gruppe: Peer-to-Peer hinter einem Switch umgeht die
         IOMMU, Multifunktionsgeräte ohne ACS teilen die RID-Sicht). Die Benennung `dma_group`
         von Anfang an, nicht nachträglich — sonst hängen Tests an der zu starken Aussage.
         RMRR-behaftete Geräte werden **abgewiesen und protokolliert**, nicht mit einer Lücke
         zugeteilt: ein Teil ihres Zugriffs liegt per Konstruktion außerhalb der Kontrolle.
      3. [x] Root-/Context-Tabellen + SLPT, `attach` liefert `Some`. **Meilenstein erreicht.**
         Befund vorweg, der das Kriterium betraf: die DMA-Tests haben auf x86 nicht *geskippt* —
         `threads/mod.rs` ist `#[cfg(target_arch = "aarch64")]`, es gab sie dort **gar nicht**.
         Ein Test, den es auf einer Architektur nicht gibt, kann dort auch nicht grün werden.
         `dmawin`/`dmatok` liegen deshalb jetzt arch-neutral in `kernel/src/dmatests.rs`, und
         beide Bring-up-Pfade rufen **dieselbe Funktion** — nicht eine nachgebaute. Beide auf
         x86 grün, aarch64 unverändert.
         Umgesetzt: SLPT (3 Level, 39 Bit), Kontext-Einträge für **alle** RIDs der Gruppe (sie
         landen in `DmaCtx::sids` und laufen damit durch dieselbe `ctx_quiesce`-Schleife wie auf
         ARM — die Falle aus `13810e9` ist strukturell vermieden), DID ab 1 (bei `CM=1` ist 0
         reserviert), `FPD` bleibt aus, Kontext-Cache **vor** IOTLB, unbedingt auch nach dem
         Anlegen. Richtung fällt heraus statt hinzuzukommen: Präsenz *ist* `R|W`, `ToDevice`
         wird zu „nur R". `SNP` wird ohne `ECAP.SC` nicht gesetzt (reserviert). Jeder
         Tabellenschreibzugriff geht durch `write_entry`, das ohne `ECAP.C` `clflush`+`sfence`
         ausführt und nie ohne Flush zurückkehrt — auch auf den Zwischenebenen.
         Kontext-Tabellen entstehen **einmal beim Bring-up** (256 x 4 KiB), nicht beim ersten
         `attach`: sie sind eine Bus-Struktur, dürfen beim Abbau eines Kontexts nicht freigegeben
         werden, und eine lazy angelegte, nie freigegebene Tabelle sähe in jeder
         Ressourcenbilanz wie ein Leck aus. Symmetrisch zur linearen Stream-Tabelle auf ARM, und
         der DMA-Pfad kommt damit ohne Allokation aus.
         Nebenbefund im Test selbst: die 32-Bit-Annahme in `dmawin` war **maschinen**-, nicht
         architekturabhängig — auf dem x86-Aufbau mit 512 MiB RAM liegt das Fenster unter 4 GiB,
         und der Test wäre grün gewesen, ohne die Eigenschaft zu prüfen. Jetzt 20 Bit.
         Offen aus diesem Schritt: `dmagen` und der virtio-Negativtest hängen noch am
         ARM-Modul (Leaf-Rücklesen bzw. virtio-Treiber); QI bleibt bewusst ungenutzt, der
         Kernel fährt ausschließlich den Registerpfad.
         Alt:
         Dabei: **`FPD`** (Fault Processing Disable) im Kontext-Eintrag ist wörtlich `CD.R` noch
         einmal — gesetzt, würde der Negativtest wieder an einer strukturell leeren Beobachtung
         bestehen. Und `FSTS.PFO` (Overflow der Fault-Recording-Register) muss in denselben
         Zähler wie `config_errors()`, sonst bedeutet „keine weiteren Faults" nach einem Sturm
         nichts. (`fault_overflow()` steht bereits.)
      3b.[ ] **Queued Invalidation ist Vorbedingung von 6.4, nicht Alternative.** Die
         Invalidierung des Interrupt-Entry-Cache existiert nur als QI-Deskriptor, nicht als
         Registeroperation — IR lässt sich ohne QI nicht korrekt betreiben. Der Registerpfad ist
         damit ein **Provisorium mit bekanntem Ablaufdatum**: nichts Weiteres darauf aufbauen,
         und beim Umstieg wirklich umstellen statt beides zu halten.
      4. [ ] Interrupt Remapping (`GCMD.IRE`) inkl. **Abschalten des Compatibility-Format-
         Interrupts** — IR aktiv bei weiter erlaubtem CFI ist eine offene Tür an der Seite.
         Eigene Zeile in `docs/invariants.md` §2, weil es dieselbe Struktur hat wie die
         BME/STE-Arbeitsteilung: zwei Mechanismen, von denen keiner den anderen ersetzt.

- [ ] **x86-Fensterwahl** (vor der VT-d-Zuteilung, s. C): `0xFEE0_0000–0xFEEF_FFFF` ist als
      IOVA **unbenutzbar**. VT-d behandelt DMA-Requests dorthin als Interrupt-Nachrichten und
      schickt sie durch das Interrupt-Remapping statt durch die Second-Level-Tabellen — eine IOVA
      in diesem Fenster wird also *nicht übersetzt*, egal was in der Tabelle steht. Auf einer
      Maschine mit RAM oberhalb 4 GiB liegt die aus `RAM_TOP` abgeleitete Basis ohnehin darüber,
      auf einer kleineren nicht. Gehört als Bedingung an die Fensterwahl, zusammen mit IR, ACS
      und RMRR.

- [ ] **Descriptor-Typestate** (`Owned<Driver>`/`Owned<Device>`) treiberseitig. Ausdrücklich
      **Ergonomie, nicht TCB**: eine Compile-Zeit-Disziplin innerhalb der Treiber-PD trägt an der
      Vertrauensgrenze nichts — sie fängt Fehler des Treiberautors, nicht das Verhalten eines
      kompromittierten Treibers. Lohnt trotzdem, weil „Puffer steht armiert in der Queue, ist im
      sicheren Code aber wieder adressierbar" real und häufig ist.

---

## D0. Instabilität des x86-Laufs (2026-07-29, **neu gemessen 2026-08-01 und 2026-08-03**)
**Klasse:** Fehler · **Aufwand:** eingegrenzt, Vollprotokoll eines Hängers steht aus

- [ ] **2300 Läufe am 2026-08-03, keine einzige Abweichung. Die alte Quote ist damit
      ausgeschlossen — die URSACHE ist es nicht.** Gemessen nach dem Tagesstand (Z4 Stufe 2,
      A-5.4, B-7.3):

      | Reihe | Läufe | Bedingung | Ergebnis |
      |---|---|---|---|
      | vormittags | 200 | Leerlauf, KVM | 200 von 200, identische Signatur |
      | mittags | 600 | 5 parallele Ströme à 120, 20 vCPU auf 20 Kernen | 600 von 600 |
      | nachmittags | 1500 | 5 parallele Ströme à 300, 8 min Wandzeit | 1500 von 1500 |

      Alle fünf Ströme beider Lastreihen tragen dieselbe Signatur `e419003d625f` — auch über
      die beiden getrennten Aufrufe hinweg.

      **Die Grundlinie, gegen die zu rechnen ist.** Nicht die alte Gesamtquote von 1,5 %
      (6/400): davon waren **4 das Farbrennen**, und das ist seit dem 2026-08-01 behoben
      (500/500). Für das hier noch offene Bild, den **Hänger ab `sched`**, lautet sie 2/400
      unter Last und 1/200 im Leerlauf, also rund **0,5 %**.

      | Frage | Antwort |
      |---|---|
      | Ist eine Rate von 0,5 % noch haltbar? | **Nein.** `0,995²³⁰⁰ ≈ 1·10⁻⁵` |
      | Ist eine Rate von 0,1 % ausgeschlossen? | **Nein.** `0,999²³⁰⁰ ≈ 10 %` |
      | Obere 95-%-Schranke (Dreierregel) | `3/2300 ≈ 0,13 %` |

      **Was hier NICHT behauptet wird, und das ist der eigentliche Punkt.** Niemand hat diesen
      Hänger behoben. Die letzte D0-Arbeit hat das *Farbrennen* beseitigt, nicht ihn. Er ist
      also nicht *repariert*, sondern **unter die Messschwelle gefallen** — und dafür gibt es
      drei Erklärungen, die diese Messung nicht auseinanderhält: (a) eine der vielen Änderungen
      seither (A-5.x, B-5.1, Root-Task auf x86, Z4) hat ihn nebenbei mitgenommen, (b) die
      Lastform ist eine andere — die alte Serie lief parallel zur **Lade-Suite**, die neue gegen
      fünf Kopien ihrer selbst, (c) die alte Quote 2/400 war eine Schwankung und die wahre Rate
      lag immer bei ~0,1 %, wo sie auch jetzt noch liegen dürfte.

      Ein Fehler, der ohne bekannte Ursache verschwindet, ist nicht zu. Er ist nur nicht mehr
      **greifbar** — und damit auch nicht mehr debuggbar, was die Lage schlechter macht, nicht
      besser. Der Eintrag bleibt offen und wandert nicht nach `done.md`.

      **Was ihn wirklich schließen würde:** ein Lauf gegen die *ursprüngliche* Lastform (parallel
      zur Lade-Suite, nicht gegen Kopien der eigenen Suite) in derselben Größenordnung. Fällt er
      auch dort sauber aus, ist (b) erledigt und nur noch (a)/(c) offen.

      **Zwei Vorbehalte, die zur Zahl gehören.** (a) Die alte Serie lief parallel zur
      **Lade-Suite**, die neue gegen fünf Kopien ihrer selbst. Beides ist Last, aber ein
      Fehlerbild, das am Zusammenspiel mit dem Blockgerät hängt, träfe die neue Anordnung
      schwächer. (b) `pprobe` meldet unter KVM grundsätzlich `SKIP` (`CPUID.1:ECX[31]`) und
      urteilt in dieser Reihe nicht mit — der Eintrag steht in der Signatur, fällt also auf,
      ist aber kein bestandener Test.

      **Nebenertrag: die Signaturprüfung hatte ein Loch.** `test-qemu-x86.sh` vergleicht jeden
      Lauf gegen den **ersten Lauf desselben Aufrufs**. Fünf parallele Ströme mit je einer in
      sich stimmigen, untereinander aber verschiedenen Signatur hätten damit fünfmal grün
      gemeldet. Der Quervergleich über die Ströme ist deshalb Teil der Messung (alle fünf
      `e419003d625f`).

      Dabei fast eine falsche Aussage produziert: ein erster Quervergleich über die *rohen*
      Zusammenfassungszeilen ergab fünf verschiedene Hashes — das war die **Kalibrierung**
      (LAPIC 999937800 gegen 1000032500 Hz, TSC 2804 gegen 2803 MHz), nicht der Kernel. Der
      Vergleich muss durch dieselbe `run_signature`-Extraktion laufen, die auch der Test
      benutzt; alles andere misst Rauschen.

- [ ] **Der x86-Lauf ist noch nicht deterministisch — aber zwei Größenordnungen seltener als
      gedacht.** Neu gemessen am 2026-08-01 (200 Läufe, KVM, `-cpu host,+invtsc`, lastfreier
      Rechner mit 20 Kernen):

      | Messung | Quote |
      |---|---|
      | 2026-07-29, 8 Läufe, TCG | 4–6 von 8 vollständig |
      | 2026-08-01, 200 Läufe, KVM | **199 von 200 mit identischer Ergebnissignatur** |

      Der eine abweichende Lauf (Nr. 174) **riss das Zeitlimit** — er wurde von außen erkannt
      (`rc=124`), nicht aus dem Logtext. Ihm fehlen ausschließlich die Zeilen ab dem
      Scheduler-Test (`sched`, `ipc`, `ring3`, `capsz`, `capsum`, `iso`, `root`, `cdelete`,
      `audit`, `SELFTEST COMPLETE`); alles davor ist vollständig. Er blieb also nach
      `bringup : 3 Worker + 2 PDs eingeplant` stehen. Das trifft genau die drei Kandidaten,
      die hier schon standen: **SMP-Hochlauf** (`cpu_on`/`ap_entry`), **Konsolensperre**,
      **Idle-Schleife mit `all_done()`**.

      **Warum die alte Zahl nicht belastbar war.** Drei Schichten verdeckten einander, jede
      musste einzeln weg, bevor eine Messung überhaupt etwas aussagen konnte:

      1. `report_and_off()` druckte `SELFTEST COMPLETE` **bedingungslos**, auch nach dem
         Watchdog (behoben in B-1.8) — ein abgebrochener Lauf zählte als vollständig.
      2. `color` druckte als einzige Stelle im Kernel `FAIL` statt `FAILURES` (behoben
         2026-08-01) — ein durchgefallener Test fiel damit aus der Ergebnissignatur **heraus**
         statt als Abweichung aufzufallen.
      3. `-no-shutdown` ließ **jeden** Lauf ins Zeitlimit laufen (behoben 2026-08-01) —
         `rc=124` war immer wahr und trug keine Information. Ein Hänger musste deshalb aus
         einer Zeile erschlossen werden, die der Kernel selbst drucken muss.

      Seit (3) ist der Rückgabewert ein **zweiter, unabhängiger Melder**: er liegt außerhalb
      des Kernels und lässt sich von keinem Kernelfehler stillstellen. Genau er hat Lauf 174
      gefangen.

      Nebenbefund von (3): ein Lauf dauert jetzt 0,86 s statt 130 s (Faktor 152, zeichengleiche
      Ausgabe). Erst dadurch sind 200 Läufe bezahlbar — vorher hätte dieselbe Aussage sieben
      Stunden gekostet, und deshalb gab es sie nicht.

- [ ] **Zwei getrennte Fehlerbilder** (400 Läufe am 2026-08-01, davon 6 Abweichungen; die
      Serie lief parallel zur Load-Suite, die Quote gilt also *unter Last* — im Leerlauf waren
      es 1 von 200):

      | Bild | Läufe | Rate | vor dem 2026-08-01 sichtbar? |
      |---|---|---|---|
      | `color : FAILURES`, `rueckgelesen=0` | 138, 141, 228, 377 | 4/400 | **nein** — `FAIL` fiel aus der Signatur |
      | Hänger ab `sched` | 115, 235 | 2/400 | **nein** — `rc` war immer 124 |

      Beide waren strukturell unsichtbar. Vollprotokolle liegen unter
      `build/diag/abweichung-lauf-N.log`.

- [ ] **`color`-Fehlschlag: die Buchführung wird geschrieben, NACHDEM der Thread lauffähig ist**
      (Ursache am 2026-08-01 im Kernel lokalisiert, noch nicht behoben — ein erster Versuch im
      Test hat nicht gewirkt, weil das Fenster nicht dort liegt). Alle vier Fehlschläge sind zeichengleich:

          gut:    rueckgelesen=1 (kstack=1 l1=1 l2=1)
          kaputt: rueckgelesen=0 (kstack=0 l1=0 l2=0)

      Alles andere stimmt in allen Läufen — `in_mask`, `kernelseite`, `disjunkt`,
      `uebergross_abgewiesen`, `bilanz`. Es ist **kein Farbfehler**, die Zuteilung ist richtig.

      Die drei Felder kommen aus `kstack_of(t.slot())` und `vspace_tables_of(asid_of(...))`;
      beide liefern `0`, sobald Slot bzw. ASID nicht mehr belegt sind. Dass sie **immer
      gemeinsam** kippen und nie einzeln, spricht für eine Ursache statt drei.

      `run_color` in `kernel/src/colors.rs` beschreibt dasselbe Rennen bereits — für
      `balanced`: „nebenher sammelt ein anderer Kern den Stack des gerade gefaulteten
      `iso_probe`-Threads ein, und je nachdem, ob dieser Rückgang ins Messfenster fällt, wurde
      derselbe Kernel mal grün und mal rot gemeldet". Der `iso_probe`-Thread faultet
      **absichtlich** (Isolationstest). Wird er eingesammelt, bevor die Kernelseite gelesen
      wird, sind Stack und ASID weg.

      **Die Stelle** (`spawn_isolated_colored_inner` in `kernel/src/system.rs`):

          let tid = { let mut sched = SCHEDS[core].lock();
                      sched.spawn_user(core, entry, arg, kbase, …) };   // lauffähig ab hier
          match tid { Some(t) => { record_user_kstack(t.slot(), kbase); // Buchführung DANACH
                                   set_vspace_of(t.slot(), packed);

      Der Thread läuft, sobald `SCHEDS[core]` frei ist; Stack- und VSpace-Buchführung folgen
      erst danach. Auf vier Kernen kann er dazwischen anlaufen, faulten und eingesammelt
      werden.

      **Folge jenseits des Tests:** Trifft der Einsammler das Fenster, sieht er
      `base_of[slot] == 0` und gibt den Kernel-Stack **nie frei** — ein Leck, das kein Test
      heute sucht. `set_vspace_of` schreibt danach in einen Slot, der bereits neu vergeben sein
      kann.

      Zu tun: die Buchführung **innerhalb** von `SCHEDS[core].lock()` erledigen, bevor der
      Thread lauffähig wird — nicht danach. Der Test bekäme die Werte dann verlässlich; das
      Leck verschwände als Nebenwirkung. Ein Versuch, allein im Test früher zu lesen, wurde
      gemessen und half nicht (3 von 500 statt 4 von 400 — Rauschen).

- [ ] **Hänger ab `sched` — NICHT behoben, und seit dem 2026-08-01 abends deutlich häufiger.**
      Die Notbremse ist repariert und greift nachweislich; der Hänger selbst ist offen.

      **Gemessene Rate, gleicher Tag, gleiche Maschine:**

          Stand 15bc289 (vor virtio/pprobe):   1 von 500   (0,2 %)
          Stand 9503212 (danach):              4 von 100   (4 %)

      Kein neuer Fehler: 3 der 4 tragen die `WATCHDOG`-Zeile, alle brechen bei ~144 statt 190
      Ausgabezeilen ab, die abweichende Signaturzeile ist immer `ipc : FAILURES`. Es ist derselbe
      Hänger. Aber der virtio-Test macht zwei vollständige Geräte-Handshakes mit langen
      Poll-Schleifen, und das hat das Timing so verschoben, dass ein latenter Fehler um den Faktor
      20 sichtbarer wurde.

      **Das ist ein Geschenk, kein Rückschritt:** ein Fehler mit 0,2 % ist praktisch nicht
      debuggierbar, einer mit 4 % schon. Wer ihn sucht, sollte den aktuellen Stand nehmen, nicht
      den ruhigeren von vorher.
      Läufe 115 und 235 blieben **nach** `smp : 4 von 4 Kern(en) online` stehen, ohne
      `WATCHDOG`-Zeile. Der SMP-Hochlauf war also erfolgreich. Die Notbremse stand hinter
      `system::reap()`, und das nimmt `SCHEDS[core].lock()` und `MEM.lock()` — blockiert der
      Einsammler, dreht sich die Schleife nie weiter: **die Notbremse wurde von dem
      ausgehungert, was sie überwachen soll**. Sie steht jetzt davor und zählt Ticks statt
      Umdrehungen (50 Mio Umdrehungen sind unter KVM Millisekunden und unter TCG Minuten —
      dieselbe Zahl meinte je nach Aufbau etwas anderes).
      Gegenprobe, zwei Serien zu je 500 Läufen: **kein einziger riss das Zeitlimit** (vorher 2 von
      400). Das heißt aber **nicht „keine Hänger mehr"** — in der zweiten Serie steht in Lauf 328

          bringup : WATCHDOG — nicht alle Aussagen belegt (nach 61s, 14898083 Umdrehungen)

      und derselbe Lauf zeigt `ipc : FAILURES`. Der Hänger trat also weiterhin auf (~1 von 500); die
      Notbremse hat ihn in eine saubere, sichtbare Abweichung verwandelt statt in ein Zeitlimit.
      Das ist der gewünschte Ausgang — und zugleich der erste Beleg, dass dieser Wächter überhaupt
      auslösen kann. Vorher war „0 Zeitlimits" zweideutig: er konnte funktionieren oder stumm sein.
      **Offen bleibt:** blockiert `reap()` selbst, hilft auch das nicht — dafür bräuchte es
      eine Notbremse im Timer-Interrupt, außerhalb dieses Fadens. Steht so im Code.

- [ ] **Sobald die Ursache feststeht:** der Lauf muss wieder wiederholbar sein, bevor A1 als
      abgenommen gilt. Ein Testaufbau, der stehenbleibt, kann keine Aussage über irgendeine
      Eigenschaft tragen — auch nicht über die, die er gerade grün meldet. Bei 0,5 % ist die
      Frage allerdings eine andere als bei 25 %: es geht nicht mehr um Brauchbarkeit der Suite,
      sondern um einen echten, seltenen Fehler im Kernel.

---
## D. Verifikation

- [ ] **KEIN CI-RUNNER — alle Gates warten (2026-08-03). Das ist der Rest eines Befunds, dessen
      erste zwei Schichten heute zu sind.** Der Reihe nach, weil jede Schicht die nächste verdeckt
      hat:

      | # | Befund | Stand |
      |---|---|---|
      | 1 | Gates lagen in `.gitea/workflows/`, der Server ist **GitLab** — er liest das nicht | **behoben**: `.gitlab-ci.yml` |
      | 2 | `.gitea/workflows/kani.yml` war seit Anlage **ungültiges YAML** (`: ` im Jobnamen, Z. 63 Sp. 49) | **behoben** + `tools/ci-yaml.sh` mit Selbsttest |
      | 3 | **kein Runner registriert** | **offen — hier** |

      Gemessen, nicht vermutet: Pipeline 5 stand **284 s** in der Warteschlange, Pipeline 6
      **939 s (16 min)** — beide mit allen Jobs `pending`, `runner=None`, `tag_list` leer. Ein
      registrierter Runner greift binnen Sekunden zu; 16 Minuten sind keine Langsamkeit mehr. Die Runner-API (`/projects/2/runners`) braucht ein Token, das hier nicht vorliegt —
      **welcher** Runner fehlt (kein Runner / falsche Tags / `paused`), ist damit noch nicht
      auseinandergehalten.

      Vor dem Push existierten **zwei** Pipelines in der ganzen Projektgeschichte, beide vom
      2026-05-23, beide auf `main`, beide mit GitLabs Auto-DevOps-Vorlage
      (`semgrep-sast`, `container_scanning`, `code_quality`, `build`, `test`) — nie mit unseren
      Gates.

      **Warum das mehr ist als eine Konfigurationsaufgabe.** Der Verus-Beweis in `cap_space.rs`
      war 37 Tage rot, während der zugehörige Commit „delete (Leaf) gegen die VOLLE cap_inv
      bewiesen" hieß und der README „10 verified" behauptete. Die naheliegende Erklärung war
      „gelaufen, aber niemand hat hingesehen". Sie ist falsch: **es gab nichts zu sehen.**
      Ein Gate, das auf keinem Server steht, ist keine schwächere Zusicherung, sondern keine —
      und es ist von einem grünen Gate äußerlich nicht zu unterscheiden. Das ist dieselbe Form
      wie die leere Ereigniswarteschlange ohne `CD.R`.

      **Abnahmebedingung — und sie ist absichtlich nicht „die CI ist grün":** eine Pipeline-ID
      vorzeigen können, deren `verus`-Job **gelaufen** ist. Dazu drei Fälle, die
      auseinandergehalten gehören, weil sie von außen gleich aussehen:
      (a) keine Pipeline → Konfiguration nicht gefunden;
      (b) Pipeline mit `pending`-Jobs → kein Runner (der Fall heute);
      (c) Jobs mit Ergebnis → erst hier sagt grün etwas.

      Nebenpunkt, sobald ein Runner läuft: der `verus`-Job fährt `--selftest` **vor** dem
      eigentlichen Lauf. Wenn der Selbsttest grün ist, aber der Hauptlauf auch — dann erst ist
      belegt, dass das Gate fehlschlagen *kann* und trotzdem nicht fehlschlägt.

      Eigene Falle dabei, für den nächsten, der misst: **jeder Push bricht die vorige Pipeline
      desselben Zweigs ab.** Pipeline 4 und 5 gingen so auf `canceled` — das sah nach „Runner
      hat abgelehnt" aus und war mein eigener nächster Commit.

- [ ] **Zyklenzähler weiterführen** (Stufe 1 teilweise erledigt): `hal::timer::cycles()` /
      `cycles_per_sec()` / `invariant_tsc()` stehen auf beiden Architekturen, serialisierend und
      gegen den PIT kalibriert. Offen: die per-TCB-Abrechnung (`consumed_cycles`, gestempelt beim
      Ein-/Auswechseln) und darauf die Monitoring-Cap.
      **Invarianz ist Telemetrie, keine Bestehensbedingung** — aber die Einschränkung hing am
      Emulator, nicht am Entwurf: unter KVM mit `-cpu host,+invtsc` meldet dieselbe Prüfung
      `true` (`+invtsc` muss **explizit** angefordert werden, QEMU lässt es auch bei `-cpu host`
      weg, weil es die Live-Migration blockiert). Zen/EPYC hat Invariant TSC durchgehend. Der
      TCG-Fallback bleibt und sagt im Log, dass die Werte dort indikativ sind — und dass die
      Zusage anderswo vorhanden ist.

- [ ] **RAM-Größe als Testparameter** (teilweise erledigt): `test-qemu-x86.sh` nimmt `-m` als
      zweiten Parameter (512M/8G geprüft). Für aarch64 steht dasselbe noch aus.

- [ ] **Zählgrenzen + Lock-Sektion als Operationszahl** (nächster Schritt, s. Diskussion):
      Iterationen je Thread-Tod in `purge_ipc_queues`, CDT-Walk-Länge, `revoke`-Teilbaumgröße,
      Tabellenbelegung, Stackbytes je Thread — als **Anzahl**, damit maschinenunabhängig. Die
      Amdahl-Rechnung zerfällt dann in „Operationen unter Lock je Thread-Lebenszyklus" (jetzt
      verfügbar) mal „Kosten je Operation" (auf Blech kalibriert). Mit KVM ist die direkte
      Sektionsmessung zusätzlich möglich (Auflösung 51 Zyklen), also beides.

- [ ] **D1** *(Kernfrage beantwortet, Rest ist eine Entwurfsentscheidung)* Kani deckt die
      ext-29-Eigenschaft von `sel4lake-sync` **strukturell nicht** ab — belegt 2026-08-01: Kani
      baut für das Host-Ziel, dort greift der dritte `cfg`-Zweig mit
      `IRQ_MASKING_IMPLEMENTED = false` und `irq_save_disable()` als No-Op; der Lauf sagt es
      selbst (`warning: constant IRQ_MASKING_IMPLEMENTED is never used`). Ein grünes `sync`
      beweist Speichersicherheit und Arithmetik, nicht IRQ-Sicherheit.
      **Kompensiert:** die Eigenschaft trägt der Wächter zur Übersetzungszeit, und dass der
      auslösen *kann*, ist jetzt geprüft statt behauptet — `tools/guard-verify.sh` baut
      unverändert (muss übersetzen) und mit zerstörten `cfg`-Zweigen (muss am Wächter
      abbrechen), für beide Bare-Metal-Ziele; im Kani-Gate verdrahtet. Details:
      `docs/kani-lauf.md`.
      **Bleibt offen:** ob zusätzlich ein Kani-Harness gebaut wird, der die Maskierung als
      *Modell* mitführt (statt sie wegzu-`cfg`-en) und Reentranz aus dem IRQ-Pfad als
      Eigenschaft formuliert. Das wäre ein echter Beweis statt eines Wächters — aber er fände
      einen `cfg`-Auswahlfehler weiterhin nicht, denn er liefe auf demselben Host-Ziel.
- [~] **D2 halb erledigt (2026-08-02, B-7.2).** Die **Kopie** ist weg: Loom prüft den echten
      `sel4lake-sync`-Quelltext (das Skript kopiert ihn unverändert, die Beweise stehen in
      derselben Datei), die alten Nachbauten sind gelöscht. Der Fund dabei: mit
      `core::cell::UnsafeCell` prüft Loom nur das **Atomic-Protokoll** — eine abgeschwächte
      Ordnung im Ticket-Release lief durch ALLE Beweise; erst mit `loom::cell::UnsafeCell` fallen
      2 von 10.

      **Offen bleibt die IRQ-Maskierung**, und sie ist prinzipiell nicht modellierbar (kein DAIF
      in Loom). Deadlockfreiheit gegenüber Preemption bleibt damit Argument, nicht Beweis.

- [ ] **D3** Die ext-29-Invarianten (Grant-Nicht-Leck, Zeroing, Cap-Budget) und die
      ext-30-Invarianten (Directory-Kohärenz, Migrations-Sperrordnung) haben Laufzeittests, aber
      keine Verus-/Kani-Beweise. Für die Migration wäre die Sperrordnung „aufsteigende Kern-ID"
      ein lohnendes Loom-Modell (zwei Kerne migrieren gegeneinander).

- [x] **D5 erledigt (2026-08-02): der aarch64-Kernel hat einen Root-Task — und der Weg dorthin fand
      zwei Fehler, die nichts mit dem Manifest zu tun hatten.**
      Details in [done.md](done.md#d5-eine-rote-zeile-die-niemand-ansah).

      Der Anlass war die Zeile `root : FAILURES (NoManifest)`, die die Suite **gar nicht prüfte**.
      Behoben: `test-qemu.sh` baut ein signiertes Manifest mit genau einem `root`-Eintrag, `init`
      bekommt ein aarch64-Zertifikat (`certs/init-arm.cert`), `manifest_report()` wird auf ARM
      gerufen wie auf x86, und die Suite prüft `root`, `manifest` und die Modulzahl.

      **Was dabei herauskam, war wertvoller als der Anlass:**

      1. **Die Startmenge stand im falschen Dokument.** `boot_arg` gab dem Root-Task
         `archive.count()` — die Größe des *Behälters*, nicht der *Startmenge*. Auf x86 stimmte
         beides überein, weil dasselbe Skript Archiv und Manifest erzeugte. Auf aarch64 liegen zehn
         Testdienste im Archiv, die nicht zur Startmenge gehören; `init` hätte sie als Startmenge
         geladen — darunter `probe`, das gar kein ELF ist. Jetzt kommt die Zahl aus dem Manifest,
         und die dadurch nötige Zusage (Manifest-Einträge `0..n` liegen auf Archivpositionen
         `0..n`) ist eine **geprüfte Regel**: `RootTaskError::StartSetNotPrefix`, fail-closed.
         Negativkontrolle gefahren.
      2. **`loadstop` maß eine globale Baseline mit einer lokalen Sperre.** `local_irq_disable()`
         stellt einen Kern still; sieben andere und der Einsammler laufen weiter. Gemessen wurde
         `free 4204597248 -> 4204613632` — **16 KiB mehr** hinterher, ein fremder `free`, kein Leck.
         Die Prüfung stellt jetzt erst Ruhe fest und misst dann; ein Lauf, der nie ruhig wird,
         fällt **durch** („nicht messbar" ist kein bestandener Test).

      Nachgemessen wie gefordert: `RUNS=6` → 6/6 mit identischer Signatur, `== ALL PASS ==`.

- [~] **D6 (2026-08-02): die aarch64-Suite hatte keine Wiederholungsmessung — jetzt hat sie eine.**

      **Was der Befund war:** über rund 13 Läufe fielen **vier verschiedene** Prüfungen aus
      (`xfer`, `dtb`, `prio`, `loadstop`), scheinbar zufällig. Das sah nach Kernel-Nichtdeterminismus
      aus und war zur Hauptsache **die Testmechanik**:

      1. Die Ausgabe hing an einer **Pipe**, beendet wurde mit `--signal=KILL`. Ein per SIGKILL
         erschlagenes QEMU flusht seinen stdout-Puffer nicht.
      2. Nach dem Umbau auf eine Datei teilten sich alle Läufe **eine** Datei. Gelegentlich fehlten
         dann **frühe** Bootzeilen in der ausgewerteten Ausgabe, während alle Ergebniszeilen da
         waren — die Signatur blieb identisch, und trotzdem fiel je nach Lauf eine andere Prüfung
         durch. Jeder Lauf bekommt jetzt seine eigene Datei.

      **Gemessen nach dem Umbau:** `RUNS=16` → 16 von 16 mit identischer Signatur, `ALL PASS`.

      **Was NICHT erledigt ist:** in einer Messung davor (gemeinsame Datei) standen 14 von 16, und
      die beiden Abweichungen waren echte Watchdog-Läufe — die Zeile kommt vom Kernel, nicht von
      der Mechanik. 16 saubere Läufe schließen eine Rate von 12,5 % nicht aus (P ≈ 12 %). Der
      Hänger ist damit **offen**, aber erstmals messbar: die Suite hält jetzt bei Abweichung das
      volle Log (`build/diag/arm-abweichung-lauf-N.log`), und `RUNS` ist der Weg, die Rate zu
      bestimmen.

      **Nächster Schritt:** die aarch64-Notbremse soll sagen, **worauf** sie gewartet hat — auf
      x86 tut sie das seit heute (`bringup : offen waren: …`), und dort war der Befund damit in
      einer Zeile sichtbar. Im ARM-Hänger sind alle Ergebniszeilen grün und der Bericht kommt
      trotzdem aus der Notbremse; ohne diese Zeile ist nicht zu sagen, welche Bedingung fehlte.

- [ ] **D4** Verus laut `docs/verification.md` offen: `delete_leaf` auf der vereinten Struktur,
      Kinderlisten-Erreichbarkeit, danach Scheduler/IPC.

- [ ] **D8 FEHLER, GEMESSEN: ein erschöpfter Thread kommt über `unblock` zurück in die
      Ready-Liste und läuft auf leerem Konto — ohne jede Cap.** (2026-08-03)
      **Klasse:** Fehler · **Aufwand:** Behebung liegt vor und ist gemessen, aber sie hat drei
      Teile und eine falsche Fassung ist schlimmer als der Fehler.

      Werkzeug: `tools/sched-erschoepfung-messen.sh` (~3 s, 96 Messwerte, `--nur-echt` für den
      Einzellauf). Der **echte** `crates/sel4lake-sched/src/lib.rs` wird gelinkt — genau eine
      Zeile unterscheidet den Harness vom Original (`#![no_std]`), Stellvertreter ist nur
      `init_thread_frame`. Keine Zweitfassung des Schedulers.

      **Die Kette, und sie braucht kein Privileg.** Der PDCTL-Weg (PAUSE → RESUME) verlangt eine
      `PdControl`-Cap. Der zweite nicht:
      `switch_to` setzt beim IPC-CALL `blocked = true` am **Aufrufer** und spendet dem Server
      dessen Konto (`sc_donor`) → `on_tick` belastet über `acct = sc_donor.unwrap_or(cur)` und
      setzt `depleted = true` **am blockierten Aufrufer** → `reply` ruft `ops.unblock(caller)`
      (`sel4lake-ipc:653`) → `unblock` (`sched:658`) prüft `depleted` nicht →
      `enqueue_ready` (`sched:1099`) auch nicht.

      | Gemessen (M1 / M2) | Wert |
      |---|---|
      | in der Ready-Liste, **Liste gelaufen** statt `queued` gelesen | 1 |
      | dabei `depleted` / `remaining` | 1 / 0 |
      | `audit()` | **0** |
      | Alternative im Augenblick der Wahl bereit | 1 |
      | wird `current`, verbraucht eine volle Zeitscheibe | 1 |

      **Warum die Aussage das Paar braucht.** „Läuft auf leerem Budget" allein kann denselben
      Wert aus einem anderen Grund annehmen: ist **kein anderer Thread lauffähig** (M6), lässt
      `dequeue_highest` `current` stehen, und `depleted_count` wächst auch ohne jedes `unblock`.
      Deshalb trägt erst **„wird `current`" ∧ „eine Alternative stand im selben Augenblick
      bereit"**. Dieselbe Unterscheidung wie bei Z4 („der Wert stimmt" gegen „der Wert wurde
      geerbt").

      **Korrektur an der ersten Herleitung.** „`depleted_count` wächst bei jedem Tick" stimmt
      **nicht**: nach dem erneuten Erschöpfen setzt `on_tick` `requeue = false`, der Thread ist
      wieder off-queue und läuft nicht von selbst weiter. Der Zuwachs ist **+1 je
      PAUSE/RESUME-Paar** (M3a: 6 Runden → `depleted_count` 7 bei **einem** echt erschöpften
      Konto), `next_refill` verschiebt sich um genau die eine dabei verbrauchte Tick.
      Kontrolle M3b (dieselben Ticks ohne PAUSE/RESUME): Zähler bleibt 1, keine Drift.

      **Die Folge, die bleibt (M5).** Nach vollständigem Refill steht `depleted_count = 3` bei
      **0** wirklich erschöpften Konten. Der Zähler kehrt nie auf 0 zurück ⇒
      `if self.depleted_count > 0 { self.refill_depleted(); }` ist **dauerhaft wahr**, der
      O(n)-Scan läuft ab da in jedem Tick. Die Zusicherung im Kommentar darüber — „der Tick
      kostet nichts, unabhängig von der Tabellengröße" — fällt damit.

      **Gegenprobe.** Mit dem Wächter verschwindet **jede** M1/M2/M3a/M5-Wirkung; die
      M4/M4b/M6-Werte bleiben. Der Test misst also genau diese Ursache und trennt sie sauber von
      der anderen.

      **Zweiter, unabhängiger Fehler (M4): PAUSE hält nicht.** `refill_depleted` reiht **ohne
      `!blocked`-Prüfung** wieder ein. Gemessen: erschöpfen → PAUSE → 100 Ticks → der
      **pausierte** Thread (`blocked = 1`) wird `current` und verbraucht eine Zeitscheibe,
      `audit()` = 0. Dafür braucht es kein `unblock`. Steht ein höher priorisierter Läufer
      daneben (M4b), bleibt er mit `blocked = 1` in der Ready-Liste und `audit()` = **2** — das
      belegt zugleich, dass `audit()` sprechfähig ist und in M1 **geschwiegen** hat.

      **Behebung — drei Teile, und die naheliegende Fassung ist die falsche.**
      1. `if blocked && !depleted { … }` ist **schädlich** (gemessen als G-a): der Rumpf wird
         übersprungen, `blocked` bleibt `true`, das RESUME wird verschluckt. Zusammen mit der
         Behebung von M4 (G-d) misst man `ticks_mit_budget = 0`, `ende.blocked = 1` —
         **vollständiges Verhungern.** Die halbe Behebung, die schlimmer ist als der Befund.
      2. Richtig und modellgleich (G-b):
         ```rust
         if self.tcbs[s].blocked {
             self.tcbs[s].blocked = false;
             if !self.tcbs[s].depleted { self.enqueue_ready(s); }
         }
         ```
         Wer ihn danach einreiht, ist gemessen: `refill_depleted` (M7: 2 Refills, 6 Ticks mit
         echtem Budget, am Ende nicht blockiert). Kein Verhungern.
      3. Dazu G-c in `refill_depleted`, sonst bleibt M4 stehen:
         `_ => { if !self.tcbs[slot].blocked && self.current != Some(slot) { self.enqueue_ready(slot); } }`
      4. Und **B2 schließen**: `audit()` braucht im Queue-Lauf ein `if t.depleted { return <neuer
         Code> }`. Ohne das bleibt genau der Zustand unbeobachtbar, der die Aussage widerlegt —
         dieselbe Form wie die leere Event-Queue ohne `CD.R`.

      **Offen und ungeprüft:** der Donee-Zweig in `refill_depleted`
      (`self.tcbs[d].blocked = false; enqueue_ready(d)`) setzt `blocked` bedingungslos zurück —
      dieselbe Frage, nicht gemessen.

      **Auch das noch nicht gemessen:** dass die Kette in einem *laufenden* Kernel eintritt.
      Gemessen ist die Zustandsmaschine am echten `Scheduler`; die Erreichbarkeit aus dem
      Syscall ist aus dem Quelltext argumentiert (`system.rs:6588`, `system.rs:2743`,
      `sel4lake-ipc:653`), nicht end-to-end ausgelöst.

- [ ] **D7 Das IPC-Modell trägt für den echten Endpoint nur einen Ausschnitt — gemessen, nicht
      geschätzt** (2026-08-03, `tools/verus-modelltreue-ipc.sh`, 32 Fälle · 12 Selbsttestfälle).

      Der Wächter fährt den **echten** `sel4lake-ipc`-Quelltext gegen ein aus der Beweisdatei
      **übersetztes** Modell (nicht abgeschrieben) und misst die Entsprechung unter einer
      hingeschriebenen Abbildung. Ergebnis: `call`/`recv` entsprechen `send`/`recv` in beiden
      Zweigen, über beide Kern-Pfade, FIFO-treu, über Ketten hinweg. Und drei Löcher, jedes
      einzeln nachgemessen:

      **(a) `ep_inv` gilt am echten Endpoint NICHT.** Über die *öffentliche* Schnittstelle sind
      Zustände mit wartenden Sendern UND geparkten Empfängern erreichbar: `bind_receiver` sieht
      die Sender-Queue nicht an, und `migrate_owner` reiht den Aufrufer wieder als Sender ein,
      während ein Empfänger geparkt sein darf. Beides ist gewollt (A-4.1/A-4.3) — aber es heißt,
      die Rendezvous-Invariante hält der **Typ** nicht, sondern die Aufrufdisziplin des Kernels,
      und über die sagt der Beweis nichts. Dass `threads::mod` heute erst migriert und dann v2
      erzeugt, ist eine Reihenfolge, keine Zusicherung.

      **(b) `send_no_loss` gilt am echten Endpoint NICHT.** `Seq::push` ist unbeschränkt,
      `TidQueue::enqueue` verwirft ab `QUEUE_CAP` **still**. Gemessen am 33. Sender an *einem*
      Endpoint: `msgs_total` bleibt auf 32. Im Quelltext benannt, nirgends gemessen — bis jetzt.
      (Das ist zugleich die alte Baustelle „der 33. Sender wird still verworfen".)

      **(c)** `!used` (`ERR_BADCAP`), `quiescing` (`ERR_QUIESCING`) und der Leichen-Zweig
      (`frame_of == None`) haben im Modell **kein** Gegenstück.

      Offen ist nicht der Wächter, sondern das Modell. Damit es weiter trägt, bräuchte es
      mindestens: `used`/`quiescing` als Bits mit Abweisung; eine **Schranke** auf beiden
      Warteschlangen (dann wäre `send_no_loss` nur unter `len < QUEUE_CAP` beweisbar — was der
      Wahrheit entspricht); `caller`/`reply_owner` samt `reply` als dritter Operation; und
      `bind_receiver`/`migrate_owner` als Operationen, unter denen `ep_inv` dann nachweislich
      *nicht* erhalten bleibt — die Invariante müsste zu „kein Rendezvous ist fällig, außer
      während eines laufenden Austauschs" abgeschwächt werden. Kernel-Quelltext ist deshalb
      **nicht** geändert worden; die Befunde stehen im Kopf von
      `Verification/ipc/proofs/endpoint.rs`.

---

## F. Debug-/Testcode aus dem Release-Build nehmen

**Befund (gemessen):** rund **8 000 der 28 800 Zeilen** — gut ein Viertel — sind Prüfinfrastruktur,
und sie wird bei jedem Start mitkompiliert und ausgeführt:

| | Zeilen |
|---|---|
| `threads/mod.rs` (Demo-Threads + Testdienste) | 5 402 |
| `threads/fuzz.rs` | 1 299 |
| `selftest.rs` | 363 |
| `arch/x86_64/dmar_selftest.rs` | 249 |
| `dmatests.rs` | 186 |
| `testsupport` in `system.rs` | 558 |

Dazu **364 Telemetrie-Statics** (`static X_OK: AtomicBool`), davon 339 in den Testmodulen selbst,
und 234 `println!`-Aufrufe über 731 Zeilen. Der Fuzzer hängt bereits an `kernel-fuzz`; der Rest
nicht.

**Die Abhängigkeit ist einseitig** — geprüft: weder `system.rs` noch `loader.rs` noch irgendeine
Crate ruft in die Testmodule. Es gibt genau **acht** Aufrufstellen (`main.rs`, `bringup.rs`), und
`testsupport` wird außerhalb der Testmodule nur an **einer** Stelle benutzt (`peek_dma_words` in
der virtio-Demo).

- [x] **F1/F2 erledigt (2026-07-30, A-2.2).** `feature = "selftest"` steht, `default = []` ist
      gedreht, und **beide** Konfigurationen werden gebaut: `test-qemu-x86.sh` baut den
      `--no-default-features`-Bau mit und vergleicht die `.text`-Größe — ein Gating, das nichts
      schrumpfen lässt, ist wirkungslos geworden, und der Build allein zeigte das nicht.
      Vorbedingung war der Root-Task (A-2.1): vorher wäre das Gating kein schlankerer Kernel
      gewesen, sondern ein leerer.
- [ ] **F3** Boot-Meldungen (`mmu:`, `apic:`, `mem:`) **nicht** mitgaten — sie sind kein
      Debug-Code, sondern das einzige Lebenszeichen eines Kernels ohne Konsole. Dass die HAL mit
      8 401 Zeilen nur **sieben** `println!` enthält, zeigt, dass die Trennung dort schon
      eingehalten wird; die Grenze verläuft zwischen Bring-up-Meldung und Testbericht.

---

## G. Bekannte Architekturgrenzen (dokumentiert, kein Bug)

- IPC überträgt **4 Registerwörter**; alles Größere über Shared Memory.
- TrustedSAS-PDs teilen einen Adressraum — gewollte Konsequenz der intralingualen Isolation
  (safe Rust kann keinen Zeiger auf fremden Speicher erzeugen); das Zertifikats-Gate erzwingt die
  Voraussetzung (`unsafe_status == ALL_PASS`).
- Tier 3 der Verifikation (funktionale Vollkorrektheit, Info-Flow, HW-Modell) ist Forschungsklasse.
