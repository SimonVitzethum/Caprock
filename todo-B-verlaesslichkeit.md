# Strang B — Verlässlichkeit und Isolation

**Frage, die dieser Strang beantwortet:** Kann man dem Kernel glauben, was er zusichert — und
trägt die Isolation für fremde Tenants?

Gegenstück: [todo-A-ausfuehren.md](todo-A-ausfuehren.md). Gemeinsame Grundlage: [todo.md](todo.md)
(Abschnitt Z) und [docs/plan-betriebsbereit.md](docs/plan-betriebsbereit.md). Die
Koordinationsregeln stehen am Ende von Strang A und gelten für beide.

Dieser Strang ist **nicht** der kritische Pfad — das ist A. Er ist aber die Voraussetzung dafür,
dass die Ergebnisse von A überhaupt etwas bedeuten: B-1 zuerst, sonst misst niemand etwas
Verlässliches.

---

## B-1. Der Lauf muss wiederholbar sein (D0) — **zuerst, für beide Stränge**

- [x] **B-1.1 Ursache gefunden und behoben (2026-07-29): auf x86 war kein `SpinLock` IRQ-sicher.**
      `sel4lake-sync::irq_save_disable`/`irq_restore` waren **nur für aarch64** implementiert; der
      No-Op-Zweig darunter war für *Host*-Builds gedacht (`cargo test`), fing aber auch das
      x86_64-Kernel-Ziel. Damit lag genau der reentrante Ticket-Deadlock vor, vor dem der
      Kommentar am Modulanfang warnt: der Timer-Tick trifft einen Kontext, der ein Ticket hält,
      der Handler zieht ein neues, das alte wird nie bedient. Beobachtbar als sporadisches
      Stehenbleiben **mitten in einer `println!`-Ausgabe**, bei etwa einem Achtel der Läufe.
      Behoben über `cfg(all(target_arch = "x86_64", target_os = "none"))` — die Unterscheidung
      muss `target_os` sein, denn `cli` ist im Userspace privilegiert.
- [x] **B-1.2 Nachgewiesen (2026-07-29).** Vorher 7 von 8 bzw. 11 von 12 Läufen vollständig; nach
      dem Fix **16 von 16**. Gemessen gegen `SELFTEST COMPLETE`, `-cpu Skylake-Client`, 4 Kerne.
- [ ] **B-1.2b Weiter beobachten.** Wiederholungsläufe gegen `SELFTEST COMPLETE`, nicht
      gegen die gerade interessierende Zeile. **Merke:** die frühere Aussage „5 von 5 grün" war
      wertlos, weil sie nur auf die `color`-Zeile prüfte — ein Lauf, der danach hängenblieb,
      zählte als Erfolg.
- [ ] **B-1.3 Wiederholungslauf in die Suite.** Ein einzelner Durchlauf kann einen
      Nichtdeterminismus grundsätzlich nicht finden. `test-qemu-x86.sh` sollte den Bootlauf
      N-fach fahren können (Vorgabe klein, in CI groß) und die Quote melden.
- [ ] **B-1.4 Dieselbe Klasse Fehler suchen.** Ein `cfg`, das ein Bare-Metal-Ziel versehentlich in
      einen Host-Zweig fallen lässt, ist selten allein. Alle `cfg(not(target_arch = ...))` und
      `cfg(target_os = ...)` im Baum durchsehen.

## B-2. Der zweite Architekturzweig muss laufen

- [ ] **B-2.1 `test-qemu.sh` aus einem frischen Clone lauffähig machen.** Sie signiert
      TrustedSAS-Binaries mit `keys/trusted-test.ed25519`, und `/keys/` ist gitignored — aus einem
      Clone startet die Suite also nicht, und **jede aarch64-Zeile ist ungeprüft**. Zwei gangbare
      Wege: ein ausdrücklich als Test gekennzeichneter Schlüssel im Repo (eigener Key-Slot, im
      Kernel als `test` markiert), oder ein Skript, das beim ersten Lauf ein Paar erzeugt und
      `trusted_keys.rs` daraus generiert. Der Produktivschlüssel bleibt draußen.
- [ ] **B-2.2 `hal::cache` auf aarch64 tatsächlich ausführen.** Die ARM-Fassung (CLIDR/CCSIDR
      inkl. FEAT_CCIDX) ist geschrieben, übersetzt und **nie gelaufen**. Genau die Fehlerform, die
      dieses Projekt schon dreimal bezahlt hat.
- [ ] **B-2.3 `README.md`.** Beschreibt einen aarch64-Kernel der Phase 7 — kein Wort vom
      x86-Port, von VT-d, von der Kern-Übergabe, vom Feature `selftest`. Bei einem
      Open-Source-Projekt die teuerste veraltete Datei überhaupt.
- [ ] **B-2.4 `docs/verification.md`** führt die Concurrency-Modellprüfung als offen, obwohl Loom
      Stufe 2 seit `c2116ac` existiert (todo D5, stale).

## B-3. Geräte-Zuteilung, die man einem Tenant geben darf (E 3b/4)

- [ ] **B-3.1 Queued Invalidation.** Vorbedingung, nicht Alternative: die Invalidierung des
      Interrupt-Entry-Cache existiert **nur** als QI-Deskriptor. Der Registerpfad ist ein
      Provisorium mit bekanntem Ablaufdatum — beim Umstieg wirklich umstellen, nicht beides halten.
- [ ] **B-3.2 Interrupt Remapping + Compatibility-Format-Interrupts abschalten.** Ohne IR kann ein
      durchgereichtes Gerät beliebige Interrupt-Nachrichten erzeugen; IR mit weiter erlaubtem CFI
      ist eine offene Tür an der Seite. **Muss vor dem ersten Tenant-Gerät stehen** (Strang A-5.3
      hängt daran).
- [ ] **B-3.3 Mehr-Einheiten-Aggregation.** `VtdCaps` ist noch eine Einheit; nötig ist das Minimum
      über alle DRHDs, die eine zuteilbare Gruppe scopen, und Fault-/Config-Zähler über alle
      Einheiten. Bis dahin ist das Oracle für alles blind, was nicht an Einheit 0 hängt.
- [ ] **B-3.4 x86-Fensterwahl:** `0xFEE0_0000–0xFEEF_FFFF` ist als IOVA unbenutzbar (VT-d
      behandelt DMA dorthin als Interrupt-Nachricht). Gehört als Bedingung an die Fensterwahl.

## B-4. Isolation, die für fremde Tenants trägt (A1-Rest, Z1, Z6)

- [ ] **B-4.1 Der gefärbte isolierte Pfad wird der Normalfall** (Z1). Heute ist `spawn_isolated`
      regulär und ungefärbt, `spawn_isolated_colored` die Ausnahme. Dazu gehört die Entscheidung
      über die Regionsgröße: Färbung verträgt sich nicht mit dem 2-MiB-Blockdeskriptor (512 Seiten
      überstreichen alle 256 gemessenen Farben) — also kleinere Regionen mit seitenweisem Mapping
      oder mehrere gefärbte Läufe je PD. **(Strang A ruft das auf: A-2.1.)**
- [ ] **B-4.2 Streifen-Freiliste.** `mask_for` vergibt heute rundläufig **ohne** Belegungsprüfung:
      mehr gleichzeitige PDs als Partitionen heißt stille Farbüberschneidung. Nötig ist ein
      sauberer Fehlschlag statt einer stillen Aufweichung. **(Berührt A-3.4: dynamische Tabellen
      heben die PD-Zahl.)**
- [ ] **B-4.3 SMT** (Z6). Cache-Coloring trennt den LLC und **prinzipiell nicht** L1/L2/TLB/
      Store-Buffer zwischen Geschwister-Hyperthreads. Entweder SMT aus, oder ein physischer Kern
      gehört zu jedem Zeitpunkt genau einem Tenant. Der zweite Weg braucht die CPU-Topologie
      (`CPUID.1F`/`0B`, MPIDR) im Scheduler — die liest heute niemand.
- [ ] **B-4.4 Die Zusicherung ehrlich aufschreiben.** `docs/invariants.md` muss sagen, dass A1 den
      **LLC** trennt und die kernlokalen Strukturen **nicht** — und dass TrustedSAS
      ausschließlich für eigenen Code ist, nie für Kundencode (gesetzte Regel, s. todo.md Z1).
- [ ] **B-4.5 Wirkung statt nur Zuteilung.** Der Test zeigt disjunkte Farbsätze, nicht messbar
      geringere Verdrängung. Ein Prime+Probe-Mikrobenchmark wäre der Beleg — unter TCG sinnlos
      (kein echter Cache), also erst auf Blech oder unter KVM.

## B-5. Zeit, Abrechnung, Speicherorte (Z2, Z5, Z8)

- [ ] **B-5.1 Verbrauch per Zyklenstempel** statt per Tick: `consumed_cycles` je TCB, gestempelt
      beim Ein- und Auswechseln, darauf eine Monitoring-Cap. **Ein schnellerer Tick wäre die
      falsche Antwort** — er erhöht Auflösung *und* Overhead; ein Zyklenstempel nur die Auflösung.
- [ ] **B-5.2 Tickless für Rechenkerne** (Z5): Timer nur armieren, wenn es etwas zu verdrängen
      gibt. Zusammen mit B-5.1 werden Abrechnung und Verdrängung entkoppelt — das ist der Punkt.
- [ ] **B-5.3 Kern-Isolierung als Politik:** keine IPIs, keine Balancierung, keine fremde
      Interrupt-Zustellung auf Rechenkernen.
- [ ] **B-5.4 NUMA** (Z8): Knoten aus ACPI SRAT/SLIT, Freilisten je Knoten, PD-Zuteilung
      knotenlokal. **Gemeinsam mit der Farbvergabe entscheiden**, nicht in zwei Schichten — sonst
      kämpfen beide Politiken um dieselbe Physadresse und wer zuerst zuteilt, gewinnt.
- [ ] **B-5.5 Zählgrenzen als Operationszahl** (todo D): Iterationen je Thread-Tod, CDT-Walk-Länge,
      `revoke`-Teilbaumgröße, Stackbytes je Thread — als **Anzahl**, damit maschinenunabhängig.

## B-6. Vertrauen in die Maschine (Z7, Z9)

- [ ] **B-6.1 Messbarer Boot + Attestierung.** Der Kunde will wissen, **worauf** er läuft — nicht
      beweisen, was er mitbringt (TrustedSAS ist nur für eigenen Code, s. Z1). Messkette bis in
      den Kernel, Signatur über die Messung. Vorbedingung für alles, was Tenant-Zustand über das
      Netz bewegt.
- [ ] **B-6.2 Fehlerdomäne festlegen und aufschreiben** (Z9). Ein Panic reißt heute den Knoten
      mit, eine VM tut das nicht. Die billige Variante ist eine Zeile Dokumentation („der Knoten
      ist die Fehlerdomäne"), die teure ist Eingrenzung auf die verursachende PD und
      Forschungsklasse. Die billige **jetzt** schlägt die teure irgendwann — aber getroffen und
      gesagt werden muss sie.

## B-7. Verifikation (D1–D5)

- [ ] **B-7.1** Kani läuft nur im CI-Gate; die ext-29-Änderung an `sel4lake-sync` ist dort nicht
      gegengeprüft — **und genau diese Crate hatte gerade den x86-IRQ-Fehler.**
- [ ] **B-7.2** Loom modelliert eine *Kopie* des Lock-Algorithmus; die IRQ-Maskierung ist dort
      prinzipiell nicht modellierbar. **Nach B-1.1 ist das keine Randnotiz mehr:** der Fehler lag
      exakt in dem Teil, den das Modell nicht abbildet. Ein Test, der die reale `cfg`-Auswahl
      prüft (baut das Kernel-Ziel wirklich den maskierenden Zweig?), wäre wirksamer als ein
      feineres Modell.
- [ ] **B-7.3** Verus: `delete_leaf` auf der vereinten Struktur, Kinderlisten-Erreichbarkeit,
      danach Scheduler/IPC.
- [ ] **B-7.4** Die ext-29-/ext-30-Invarianten haben Laufzeittests, aber keine Beweise. Für die
      Migration wäre die Sperrordnung „aufsteigende Kern-ID" ein lohnendes Loom-Modell.
