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

- [ ] **Z11a. Das Henne-Ei-Problem benennen und lösen.** Einen Plattentreiber kann man nicht von
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

- [ ] **Z4a. Anhalten mit definiertem Zustand.** Ein Thread muss an einer *benennbaren* Grenze
      stehenbleiben können, nicht irgendwo. Mitten in einem Syscall ist sein Zustand halb im
      Kernel; mitten in einer IPC-Transaktion hängt ein Partner. Nötig: ein Haltepunkt-Begriff
      (nur an Syscall-Grenzen, nie im Kernel), und ein `SYS_FREEZE`, das *wartet*, bis der Thread
      dort ist, statt ihn zu unterbrechen. Ohne das ist alles Weitere Zufall.
- [ ] **Z4b. Serialisierbarer Zustand.** Registersatz + FP/SIMD ist der leichte Teil (der
      Trap-Frame steht bereits). Der schwere Teil sind die **Caps**: eine Cap ist heute ein Index
      in eine globale Tabelle plus CDT-Kante. Über Maschinengrenzen bedeutet ein Index nichts.
      Nötig ist eine externe, maschinenunabhängige Darstellung (Objekttyp, Rechte, Badge,
      Herkunft) — und eine Regel, was mit Caps auf **maschinenlokale** Dinge passiert (MMIO,
      DMA-Regionen, Endpoints zu nicht mitwandernden PDs). Meine Erwartung: die müssen den
      Transfer **verweigern**, sonst wandert ein Thread mit Autorität, die auf der Zielmaschine
      etwas anderes bezeichnet. Das ist die gefährlichste Stelle des ganzen Vorhabens.
- [ ] **Z4c. Speicher mitnehmen.** Die private Region der PD ist der Zustand. Bei 64 KiB
      ([A1](#a1-cache--timing-seitenkanäle-zwischen-pds)) ist das billig, bei 2 MiB weniger.
      Offen: Dirty-Tracking, damit nicht alles kopiert werden muss, und die Frage, ob die
      **Farbe** auf der Zielmaschine erhalten bleiben muss (nein — Farbe ist maschinenlokal; die
      Zielmaschine färbt neu; das ist ein Argument dafür, Farbe nie in die ABI zu heben).
- [ ] **Z4d. IPC-Beziehungen.** Ein wandernder Thread mit offenem `CALL` hat einen wartenden
      Server zurückgelassen. Entweder Migration nur ohne offene Transaktionen (einfach, ehrlich,
      wahrscheinlich richtig für Stufe 1), oder Endpoint-Proxys über das Netz (ein eigenes
      Projekt).
- [ ] **Z4e. Transport + Vertrauen.** Ein Checkpoint ist der vollständige Zustand eines Tenants.
      Er geht verschlüsselt und authentifiziert über das Netz, oder gar nicht. Hängt an
      [Z7](#z7-attestierung-und-messbarer-boot): die Zielmaschine muss nachweisen können, dass sie
      dieselbe Kernelversion mit denselben Zusicherungen fährt — sonst wandert der Zustand in eine
      schwächere Umgebung, und der Tenant merkt es nicht.
- [ ] **Z4f. Identische Server als Vorbedingung benennen.** „gleicher Server" muss geprüft
      werden, nicht angenommen: gleiche Architektur, gleiche Kernelversion, gleiche
      Cache-/NUMA-Klasse. Ein Vergleich, der nur die Architektur prüft, lässt einen Thread von
      einer Maschine mit `invtsc` auf eine ohne wandern — und die Abrechnung aus [Z2](#z-zielarchitektur-stand-2026-07-29--woran-alles-andere-zu-messen-ist)
      wird still falsch.

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

- [ ] Ein Kernel-Panic reißt heute den ganzen Knoten mit; eine VM tut das nicht. Für „VMs
      ersetzen" braucht es eine ausdrückliche Antwort — entweder „der Knoten ist die
      Fehlerdomäne, plant entsprechend" (legitim, muss aber dokumentiert und den Tenants gesagt
      sein), oder Kernel-Fehler werden auf die verursachende PD eingegrenzt. Das Zweite ist
      Forschungsklasse; das Erste ist eine Zeile in der Dokumentation, die heute fehlt.

### Z10. I/O überhaupt
**Klasse:** Grundlage · **Aufwand:** groß

- [ ] Kein Netzstack, kein Blockgerät, und `virtio` ist bis heute **aarch64-only**. Eine Cloud
      ohne Netz und Speicher ist keine. Hängt an [F2](#f-debug-testcode-aus-dem-release-build-nehmen)
      (Root-Task) und an [C6](#c6-x86-64-port--rest) (Boot-Archiv): ohne einen Weg, Userland zu
      starten, gibt es auch keinen Treiber, der laufen könnte.
- [ ] Für durchgereichte Geräte ist **Interrupt Remapping** ([E](#e-dma-härtung--rest), Schritt 4)
      keine Kür: ohne IR kann ein Gerät beliebige Interrupt-Nachrichten erzeugen. Das ist der
      Standardausbruch aus einer Geräte-Zuteilung und muss vor dem ersten Tenant-Gerät stehen.

---

## A. Offen aus dem Sicherheits-Review (ext-29)

### A1. Cache-/Timing-Seitenkanäle zwischen PDs
**Klasse:** Seitenkanal · **Aufwand:** Architekturänderung, kein Patch

`[~]` **Stufe 1 steht und ist auf x86 gemessen** (s. [done.md](done.md)): Cache-Geometrie wird aus
der HW gelesen, der Allokator vergibt farbrein, und zwei so erzeugte PDs teilen sich nachweislich
keine Cache-Farbe — Region, Kernel-Stack und Seitentabellen. Offen bleibt das Folgende.

- [ ] **Der reguläre Weg ist weiterhin ungefärbt.** `spawn_isolated` (2-MiB-Region) kann es
      strukturell nicht sein: 2 MiB sind 512 Seiten, also 512 aufeinanderfolgende Farben — bei den
      gemessenen 256 Farben überstreicht ein einziger Blockdeskriptor jede Farbe zweimal. Färbung
      gibt es nur über `spawn_isolated_colored`, und die kostet die kleinere Region
      (`colors::region_bytes()`, bei 4 Partitionen 64 KiB) plus seitenweises Mapping statt eines
      Block-PTE. **Die Entscheidung, welcher Weg der reguläre sein soll, steht aus** — solange
      `spawn_isolated` der Normalfall ist, ist A1 im Normalbetrieb *nicht* wirksam.

- [ ] **Die Farbanzahl begrenzt die Anzahl gleichzeitig getrennter PDs.** `ColorMask` ist 64 Bit,
      `PARTITIONS` teilt das in disjunkte Streifen. Mehr gleichzeitige PDs als Partitionen heißt:
      zwei teilen sich einen Streifen. Heute vergibt `mask_for` rundläufig, **ohne** zu prüfen, ob
      der Streifen schon belegt ist — der Aufrufer bekommt also stillschweigend eine Farbüberschneidung.
      Nötig: eine Streifen-Freiliste, und ein sauberer Fehlschlag statt einer stillen Aufweichung,
      wenn keine disjunkte Partition mehr frei ist.

- [ ] **Nur auf x86 gemessen.** `hal::cache` hat eine aarch64-Fassung (CLIDR/CCSIDR inkl. FEAT_CCIDX),
      die **nie gelaufen ist** — die ARM-Suite braucht `keys/trusted-test.ed25519`, und das ist
      gitignored. Ungeprüfter Code auf dem zweiten Zweig ist genau die Fehlerform, die dieses Projekt
      schon dreimal getroffen hat (leere Event-Queue, nie ausgeführter x86-Testpfad,
      DMAR-Ausschlusspfad). Bis dahin gilt A1 als **x86-only**.

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

- [ ] **Kein Nachweis der *Wirkung*, nur der Zuteilung.** Der Test zeigt, dass die Farbsätze
      disjunkt sind. Er zeigt **nicht**, dass daraus eine messbar geringere gegenseitige Verdrängung
      folgt. Ein Prime+Probe-Mikrobenchmark (eine PD misst ihre eigene Zugriffszeit, während die
      andere den Cache durchläuft, gefärbt vs. ungefärbt) wäre der eigentliche Beleg. Unter TCG ist
      er sinnlos — der Emulator hat keinen echten Cache; also erst auf Blech oder unter KVM.

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

## D0. Instabilität des x86-Laufs (2026-07-29, offen)
**Klasse:** Fehler · **Aufwand:** unbekannt, zuerst einzugrenzen

- [ ] **Der x86-Lauf ist nicht mehr deterministisch.** Gemessen: von 8 Läufen desselben Images
      erreichten nur 4–6 `SELFTEST COMPLETE`; die übrigen bleiben stehen, und zwar an
      **verschiedenen** Stellen (einmal nach `color : ALL PASS` beim `bringup :`-Schritt, einmal
      bei `dmatok : FAILURES`). Verschiedene Abbruchstellen sprechen gegen einen einzelnen
      kaputten Testpfad und für etwas Gemeinsames: Zeitverhalten oder Speicherlage.

      **Beide naheliegenden Verdächtigen sind gemessen und ausgeschieden** (2026-07-29, je 8 Läufe
      desselben Images, `SELFTEST COMPLETE` als Kriterium):

      | Aufbau | vollständig |
      |---|---|
      | `HEAD` (498e149, **ohne** A1), `-cpu Skylake-Client` | 7 von 8 |
      | mit A1, `-cpu Skylake-Client` | 7 von 8 |
      | mit A1, `-cpu qemu64` | 5 von 8 |

      Also: **die Instabilität ist älter als die A1-Arbeit** und wurde nicht von ihr eingeschleppt.
      Der CPU-Modellwechsel ist ebenfalls nicht die Ursache — `Skylake-Client` ist sogar stabiler
      als `qemu64`. Damit bleibt ein **vorbestehender Fehler im x86-Lauf**, der bisher nur deshalb
      nicht auffiel, weil die Suite üblicherweise einmal statt achtmal läuft.

      Nächste Eingrenzung: mehrere hängende Läufe mit Vollprotokoll vergleichen. Bisher beobachtet
      wurden **verschiedene** Abbruchstellen (nach `color : ALL PASS`, bei `dmatok`), was gegen
      einen einzelnen kaputten Pfad und für ein Rennen spricht — Kandidaten: der SMP-Hochlauf
      (`cpu_on`/`ap_entry`), die Konsolensperre, oder die Idle-Schleife mit `all_done()`.

      Merke für die Diagnose: die Aussage „5 von 5 grün" aus der ersten Runde war **wertlos**,
      weil sie nur auf die `color`-Zeile grepte und nicht auf `SELFTEST COMPLETE`. Ein Lauf, der
      nach der geprüften Zeile hängenbleibt, zählte dort als Erfolg. Wer hier weitermisst: immer
      gegen das **Ende** des Laufs prüfen, nicht gegen die interessierende Zeile.

- [ ] **Sobald die Ursache feststeht:** der Lauf muss wieder wiederholbar sein, bevor A1 als
      abgenommen gilt. Ein Testaufbau, der in einem Drittel der Fälle stehenbleibt, kann keine
      Aussage über irgendeine Eigenschaft tragen — auch nicht über die, die er gerade grün meldet.

---

## D. Verifikation

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

- [ ] **D1** Kani lokal nicht ausführbar (nur CI-Gate) — die ext-29-Änderung an `sel4lake-sync` ist
      dort **nicht** gegengeprüft worden.

- [ ] **D2** Loom modelliert eine **Kopie** des Lock-Algorithmus; die IRQ-Maskierung ist prinzipiell
      nicht modellierbar (kein DAIF in Loom). Deadlockfreiheit gegenüber Preemption bleibt Argument,
      nicht Beweis.

- [ ] **D3** Die ext-29-Invarianten (Grant-Nicht-Leck, Zeroing, Cap-Budget) und die
      ext-30-Invarianten (Directory-Kohärenz, Migrations-Sperrordnung) haben Laufzeittests, aber
      keine Verus-/Kani-Beweise. Für die Migration wäre die Sperrordnung „aufsteigende Kern-ID"
      ein lohnendes Loom-Modell (zwei Kerne migrieren gegeneinander).

- [ ] **D4** Verus laut `docs/verification.md` offen: `delete_leaf` auf der vereinten Struktur,
      Kinderlisten-Erreichbarkeit, danach Scheduler/IPC.

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

- [ ] **F1** `feature = "selftest"` (zunächst **in** `default`): `mod threads`/`selftest`/
      `dmatests`/`dmar_selftest` und `testsupport` dahinter. Aufwand ~½ Tag, davon der größere
      Teil **nicht** das Gating selbst, sondern beide Konfigurationen in die Testskripte zu
      nehmen — ein `--no-default-features`-Build, den niemand baut, verrottet still, und genau
      diese Fehlerform hat in diesem Projekt schon mehrfach zugeschlagen (leere Event-Queue,
      nie ausgeführter x86-Testpfad, DMAR-Ausschlusspfad).
- [ ] **F2** Erst wenn ein **Root-Task** existiert: `default = []` umstellen.
      **Warum nicht sofort:** schaltet man heute alles ab, bleibt `configure() → idle`. Der
      Kernel hat derzeit keinen Nicht-Test-Zweck — auf x86 gibt es kein Boot-Archiv, und auf
      aarch64 ruft den Loader niemand außer `threads/mod.rs`. Das Gating wäre also kein
      schlankerer Kernel, sondern ein leerer. Der ehrliche Ersatz ist der seL4-Weg: ein
      Startprogramm aus dem Archiv laden und ihm die Wurzel-Caps übergeben.
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
