# SEL4Lake — erledigte Punkte

Ausgelagert aus [todo.md](todo.md), damit dort nur steht, was noch zu tun ist.

Die Begründungen bleiben erhalten, und das ist der eigentliche Zweck dieser Datei: bei mehreren
Einträgen ist das Wertvolle nicht, **dass** etwas gelöst wurde, sondern **welche Annahme sich
dabei als falsch erwiesen hat**. Wer später dieselbe Abkürzung erwägt, findet hier, warum sie
schon einmal nicht getragen hat.

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
