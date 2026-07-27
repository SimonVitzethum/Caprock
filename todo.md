# SEL4Lake — offene Punkte

Stand: 2026-07-27 (nach ext-30). Reihenfolge innerhalb eines Abschnitts = Priorität.

---

## A. Offen aus dem Sicherheits-Review (ext-29)

### A1. Cache-/Timing-Seitenkanäle zwischen PDs
**Klasse:** Seitenkanal · **Aufwand:** Architekturänderung, kein Patch

Nicht adressiert. Zwei PDs teilen sich alle Cache-Ebenen; eine kann das Zugriffsmuster der anderen
über Laufzeitmessung beobachten. Gegenmaßnahme wäre **Cache-Coloring/Partitionierung**: der
physische Allokator müsste Frames nach Cache-Set-Farbe vergeben und das VSpace-Layout die Farben je
PD disjunkt halten. Betrifft `PhysAllocator` (Farb-bewusste Freilisten) + `vspace_map_*`. Bei
„hochsicher" als Projektziel die größte verbleibende Lücke.

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
**Nebenbedingung beim Vergrößern:** `ReplyFinal` hält ein `[(u32,u64); NOBJECTS]`-Array **auf dem
Kernelstack** (aktuell 2 KiB) — `NOBJECTS` hochzuziehen koppelt an die Kernel-Stackgröße. Vorher auf
eine wachstumsfähige Meldestruktur umstellen. Siehe auch [C3](#c3-cap--pd--ipc-tabellen-dynamisch).

### A4. Kein Syscall zum Löschen eigener Caps
**Klasse:** ABI-Lücke · **Aufwand:** klein-mittel

Die ABI kennt kein `CDELETE`/`CMOVE`/`CCOPY` — Cap-Slots einer PD räumt nur der Kernel (Teardown,
PDCTL). Folge: ein langlebiger Dienst, der dynamisch Caps empfängt (IPC-Grant), läuft gegen
`CAP_BUDGET_PER_PD` und kann sich **nicht selbst befreien**. Nötig: mindestens `SYS_CDELETE` (eigener
Slot, cap-gegatet auf den eigenen Cspace), sinnvollerweise auch ein wählbarer Empfangs-Slot für
Grants statt des festen `GRANT_RECV_SLOT`.

---

## B. Thread-Migration — **erledigt** (ext-30)

**Ziel:** Threads sind nicht mehr fest an den Kern gebunden, auf dem sie erzeugt wurden.
**Blocker war:** `ThreadId.slot` kodierte den Kern (`slot / PER_CORE`) — die Identität eines Threads
hing an seiner Kern-Affinität.

- [x] **B1** Kern-Zuordnung aus der ThreadId gelöst — globales **Thread-Directory**
      `gid -> (used, gen, core, local)`, lock-frei lesbar.
- [x] **B2** `migrate_to(tid, dst)` unter beiden Scheduler-Locks (aufsteigende Kern-Ordnung),
      **Push-Modell** (nur der abgebende Kern kann den Lazy-FP-Kontext sichern).
- [x] **B3** Kernel-Glue auf „lock-frei lesen → sperren → **erneut prüfen** → ggf. wiederholen"
      umgestellt (`system::with_owner`); `ThreadId::core()` existiert nicht mehr.
- [~] **B4** Policy vorhanden (`balance_once`, im Tick-Pfad hinter einem Intervall), aber per
      Default **AUS** (`system::set_balancing`). **Offen:** standardmäßig einschalten. Blocker sind
      nicht der Mechanismus, sondern die Demo-/Testthreads dieses Images, die feste Affinität
      voraussetzen (Cross-Core-IPC-Test, Budget-Donation, Platzierungs-Telemetrie) — die müssten
      erst affinitätsunabhängig formuliert werden.
- [x] **B5** Test `migrate`: Thread wechselt unter Last den Kern, läuft dort nachweislich weiter,
      dieselbe Tcb-Cap bezeichnet ihn weiterhin, cross-core-KILL, Scheduler-Audit 0.

**Ebenfalls offen:** Migration eines Threads mit aktiver **Budget-Donation** (die Donation-Links
sind lokale Slots — heute wird die Migration in dem Fall schlicht verweigert, bis der Server
geantwortet hat). Für eine kernübergreifende Donation müssten die Links globale `gid`s werden.

**Bewusst NICHT migriert:** der laufende Thread eines Kerns (nur bereite/blockierte) — sonst müsste
der Trap-Frame kernübergreifend übernommen werden.

---

## C. Flexible Kapazitäten — **weitgehend erledigt** (ext-30)

**Zielbild:** Dual-EPYC-Klasse — **256 Kerne**, **viele tausend** Prozesse, effizient.
Die Thread-/Scheduler-Seite ist umgestellt (Kapazität kommt beim Boot aus dem RAM, heiße Pfade
O(1); belegt durch den `scale`-Test mit 1024 gleichzeitigen Threads). Offen bleiben die Cap-/IPC-
Tabellen, die GIC-Skalierung und der x86-Port.

- [x] **C1** Neue Crate `sel4lake-slab` (`Slab`/`AtomicTable`/`FreeList`); Thread-Directory,
      per-Kern-TCB-Tabellen, FP-Kontexte, `VSPACE_OF`, Kernel-Stack-Zuordnung **und** die
      Sekundär-Stacks kommen beim Boot aus dem RAM (`system::configure`).
- [x] **C2** Kernzahl aus dem Device Tree (`Dtb::cpu_count`); `MAX_CORES = 256` dimensioniert nur
      noch Arrays von *Locks*/Atomics.
- [ ] **C3** <a id="c3-cap--pd--ipc-tabellen-dynamisch"></a>Cap-/PD-/Endpoint-/Notification-Tabellen
      dynamisch (hängt an [A3](#a3-globale-cap-tabelle-bleibt-geteilte-ressource-fester-größe):
      `ReplyFinal` zuerst vom Stack lösen).

### C4. Effizienz bei tausenden Threads (lineare Scans beseitigen)

Kapazität allein reicht nicht — mehrere Pfade sind **O(n)** in der Tabellengröße und werden bei
tausenden Threads zum Engpass:

- [x] `Scheduler::alloc_tcb` — **Freiliste** (O(1)).
- [x] Ready-Queues — **intrusive doppelt verkettete Listen** durch die TCBs: Einreihen/Ausklinken
      O(1) und **kein** Queue-Speicher mehr (vorher `[usize; PER_CORE]` je Priorität).
- [x] `system::least_loaded_core` — **lock-freier** Lastzähler je Kern.
- [x] MCS-Refill-Scan je Tick — entfällt vollständig, solange kein Budget erschöpft ist.
- [x] `thread_alive` / IPC-Liveness-Audit — lock-frei über das Directory statt Zielkern sperren.
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

### C6. x86-64-Port — **Stufe 4 erreicht** (ext-31, Branch `arch/x86_64`)

`sel4lake-hal` ist jetzt architekturselektiv (`src/aarch64/` + `src/x86_64/` hinter derselben API).
Der **Kernel-Kern läuft auf x86_64**: dieselben Selbsttests, derselbe präemptive Scheduler,
dasselbe cap-gesicherte IPC — ohne ein einziges `cfg(target_arch)` im Kern. Details:
`README-X86.md`.

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
- [ ] **PCID** als Optimierung: der Adressraumwechsel flusht derzeit den ganzen TLB (die ASID ist
      eine reine Software-Kennung). Mit `CR4.PCIDE` + getaggten Einträgen entfiele das
- [~] **IOMMU (VT-d)**: Bring-up mit **Default-Block** implementiert (`VtdEnforcer`): DMAR aus
      ACPI, Root-Tabelle mit lauter „not present"-Einträgen, `SRTP`+`TE`, Invalidierungs-Round-Trip.
      Die Hardware blockt damit **jede** nicht zugeteilte DMA — der sicherheitsrelevante Teil.
      **Offen:** die per-Gerät-Zuteilung (Kontext-Einträge + Second-Level-Tabellen je Domäne);
      `attach` meldet solange ehrlich `false` (sicher: geblockt statt ungeschützt). Danach erst
      sind die `dma`/`virtiorng`-Tests von ARM auf x86 übertragbar.
- [x] **PCI-ECAM + Enumeration**: Fenster aus der ACPI-**MCFG**, Geräte werden aufgezählt,
      virtio-rng gefunden, Bus-Master aktiviert. (BARs vergibt auf dem PC die Firmware — anders
      als auf `virt`, wo der Kernel das selbst tut.)
- [ ] **Boot-Archiv** über Multiboot-Module (`SYS_LOAD` schlägt derzeit sauber fehl)
- [x] **RAM-Plan** aus dem Multiboot-Speicherplan (BSP-Trampolin reicht `EBX` durch); CPU-Liste
      aus der ACPI-**MADT** statt fester Kernzahl
- [ ] Die ARM-seitigen Demo-/Testdienste (`threads/mod.rs`, 5000 Zeilen) sind stark ARM-gekoppelt
      (EL0-Isolation, MMIO/RTC, DMA) — auf x86 läuft derzeit ein kompakter eigener Bring-up-Test

---

## E. DMA-Härtung (aus dem Design-Review, ext-35)

Reihenfolge nach struktureller Wirkung, nicht nach Aufwand.

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
- [ ] **Descriptor-Typestate** (`Owned<Driver>`/`Owned<Device>`) treiberseitig. Ausdrücklich
      **Ergonomie, nicht TCB**: eine Compile-Zeit-Disziplin innerhalb der Treiber-PD trägt an der
      Vertrauensgrenze nichts — sie fängt Fehler des Treiberautors, nicht das Verhalten eines
      kompromittierten Treibers. Lohnt trotzdem, weil „Puffer steht armiert in der Queue, ist im
      sicheren Code aber wieder adressierbar" real und häufig ist.

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
- [ ] **D5** `docs/verification.md`: Tier-1-Roadmap führt „Concurrency-Modellprüfung der Locks" noch
      als offen, obwohl Loom Stufe 2 seit `c2116ac` existiert (stale).

---

## E. Bekannte Architekturgrenzen (dokumentiert, kein Bug)

- IPC überträgt **4 Registerwörter**; alles Größere über Shared Memory.
- TrustedSAS-PDs teilen einen Adressraum — gewollte Konsequenz der intralingualen Isolation
  (safe Rust kann keinen Zeiger auf fremden Speicher erzeugen); das Zertifikats-Gate erzwingt die
  Voraussetzung (`unsafe_status == ALL_PASS`).
- Tier 3 der Verifikation (funktionale Vollkorrektheit, Info-Flow, HW-Modell) ist Forschungsklasse.
