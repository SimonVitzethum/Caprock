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
- [ ] **SMP**: INIT-SIPI-SIPI + Realmode-Trampolin (`power::cpu_on` meldet derzeit `NOT_SUPPORTED`)
- [ ] **Isolierte Adressräume**: PCID-Verwaltung + per-VSpace-Tabellenpool (`vspace_*` melden `false`)
      → ohne sie gibt es auf x86 keine Ring-3-PDs
- [ ] **IOMMU**: VT-d/AMD-Vi statt SMMUv3 (derzeit `NullIommuEnforcer`, ohne HW-Durchsetzung)
- [ ] **PCI-ECAM** über ACPI-MCFG (statt des `virt`-Board-Fensters)
- [ ] **Boot-Archiv** über Multiboot-Module (`SYS_LOAD` schlägt derzeit sauber fehl)
- [ ] **RAM-Plan** aus der Multiboot-Info statt fester 512 MiB (Trampolin reicht `EBX` nicht durch)
- [ ] Die ARM-seitigen Demo-/Testdienste (`threads/mod.rs`, 5000 Zeilen) sind stark ARM-gekoppelt
      (EL0-Isolation, MMIO/RTC, DMA) — auf x86 läuft derzeit ein kompakter eigener Bring-up-Test

---

## D. Verifikation

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
