# SEL4Lake — Systeminvarianten (Konsolidierung, ext-22…ext-28)

Dieses Dokument macht die bis ext-25 **impliziten** Invarianten explizit: die Sperrordnung, die
DMA-Revoke-Reihenfolge, die Region-Balance, den `RegionView`/`Pod`-Sicherheitsvertrag und das
SMMU-unter-QEMU-Verhalten. Es ist die Referenz für die **formale Verifikation** (welche Eigenschaft
trägt welche Isolations-/Sicherheitsaussage) und für jede künftige Erweiterung (welche Ordnung darf
nicht gebrochen werden). Wo eine Invariante maschinell geprüft wird, ist das Audit/der Test genannt.

## 1. Sperrordnung (Lock-Hierarchie)

Alle globalen Locks haben einen **Rang**. Ein Pfad darf Locks nur mit **streng steigendem** Rang
schachteln; `MEM` ist **innerster** Lock (hält nie einen weiteren). Verklemmungsfreiheit folgt aus
der Azyklizität dieser totalen Ordnung.

| Rang | Lock(s) | Typ | Rolle |
|---|---|---|---|
| R0 | `CAPS` | `RwSpinLock<Caps>` | Capability-Space + PDs (Autorität) |
| R1 | `EPS[]`, `NTFNS[]`, `VSPACES`, `DMA_CTX` | `SpinLock` | Ressourcentabellen (je Objekt/global) |
| R2 | `SCHEDS[core]` | `SpinLock<Scheduler>` | Per-Kern-Runqueue |
| R2.5 | `FP_STATES` | `SpinLock<[FpState; N]>` | Lazy-FP-Kontexte — **stets UNTER `SCHEDS` genommen** (nie davor), hält selbst nichts weiter |
| R3 | `Heap.inner` | `SpinLock<HeapInner>` | prozess-lokaler Allokator (nur Region-Runtime) |
| R4 | `MEM` | `SpinLock<PhysAllocator>` | physischer Allokator — **innerster** |

**Ergänzung ext-30 (Migration):** eine Migration hält **zwei** `SCHEDS`-Locks gleichzeitig —
den des abgebenden und den des aufnehmenden Kerns. Innerhalb desselben Rangs R2 gilt dafür die
Ordnung **aufsteigende Kern-ID**: `SCHEDS[min(src,dst)]` vor `SCHEDS[max(src,dst)]`. Zwei
gleichzeitig migrierende Kerne können sich damit nicht verklemmen. Neu ist außerdem `GID_FREE`
(Freiliste der globalen Thread-Slots): ein **Leaf-Lock**, der unter `SCHEDS` genommen wird
(Spawn) bzw. ganz ohne weiteren Lock (Reap) — nie umgekehrt, und nie zusammen mit `MEM`.

**Regel:** beim Schachteln nur aufsteigend (R0 → … → R4); niemals einen Lock kleineren Rangs
nehmen, während ein größerer gehalten wird. Faustregel der bestehenden Pfade: *nie zwei grobe Locks
gleichzeitig halten, wenn ein Kopieren-und-Freigeben es vermeidet.*

**Belegte Schachtelungen (aus dem Code):**
- `delete_leaf` (Cap-Teardown): `CAPS.write` → `MEM` (`free_region`).  R0 → R4
- `SmmuV3Enforcer::attach`/`detach`: `DMA_CTX` → `MEM` (Tabellen-Alloc/Free).  R1 → R4
- `Heap::allocate` (`RegionSource::request`): `Heap.inner` → `MEM`.  R3 → R4
- `endpoint_quiesce_owner`/`purge_ipc_queues`: `EPS`/`NTFNS` **freigeben**, dann `SCHEDS`
  (`unblock_with_error`).  R1 vor R2
- `ipc_audit`: `EPS`→`SCHEDS` bzw. `NTFNS`→`SCHEDS` (Liveness-Closure, je ein Objekt).  R1 → R2
- `bind_sched_context`: `CAPS.read` (Budget lesen) **freigeben**, dann `SCHEDS[core]`.  R0 vor R2
- `reap_core`: `SCHEDS[core]` (Zombies kopieren) **freigeben**, dann `MEM`.  R2 vor R4 (bewusst
  disjunkt statt geschachtelt — Kontention)
- `dma_attach`/`dma_audit`: `CAPS.read` (DmaCap-Attribute/Snapshot) **freigeben**, dann `DMA_CTX`.
  R0 vor R1
- `fp_trap` / `fp_reset_slot` (Lazy-FP): `SCHEDS[core]` → `FP_STATES`.  R2 → R2.5 (FP_STATES wird
  **immer** unter dem gehaltenen `SCHEDS` genommen, nie davor; danach nur Atomics `FP_OWNER`/`VSPACE_OF`).

**Leaf-Locks** (halten nie einen weiteren Lock; daher deadlock-sicher unabhängig vom Aufrufkontext):
`KSTACKS`, `VIRTIO_PCI`, `LOADED_IMAGES`, `CS_STATE_REGION`/`RELOAD_INFO`, `hal::console::CONSOLE`.
(`FP_STATES` ist **kein** Leaf — es wird unter `SCHEDS` gehalten, s. R2.5 + Schachtelung oben.)

### 1a. IRQ-Sicherheit der SpinLocks (reentranter Ticket-Lock-Deadlock — Bugfix)

`SpinLock` ist ein **FIFO-Ticket-Lock**. Mehrere per-Kern-Locks werden **sowohl im IRQ-/Reschedule-
Pfad** (Timer-Tick → `reschedule` nimmt `SCHEDS[core]`; `drain_pending_irqs` nimmt `NTFNS[]`) **als
auch in Thread-/Idle-Kontext** genommen (`idle → reap_core → SCHEDS[core]`; Syscalls; Fuzzer-
`kill_remote`/`reap_core`). Ohne IRQ-Maske ist das tödlich: feuert der Timer-Tick, während Thread-/
Idle-Kontext einen solchen Lock **hält oder erwartet**, zieht der Reschedule-Hook ein **zweites
Ticket** auf denselben Lock — der erste Halter ist aber im IRQ-Handler suspendiert und gibt sein
Ticket nie frei → **Deadlock** (andere Kerne, die cross-core auf `SCHEDS[C]` warten, hängen mit).

**Invariante (erzwungen durch `SpinLock` selbst):** `lock()` maskiert IRQs am eigenen Kern (DAIF
sichern + I-Bit setzen) **vor** dem Ticket-Ziehen und der Guard stellt den vorherigen Zustand beim
`Drop` wieder her (nesting-sicher: jeder Guard sichert den Stand von vor seinem Lock; der äußerste
gibt „IRQs an" frei). Damit kann der Reschedule-/IRQ-Hook einen SpinLock-Halter **nie** unterbrechen.

**Seit ext-29 gilt dieselbe Invariante für `RwSpinLock` (`CAPS`)** — `read()`/`write()` maskieren
ebenfalls selbst. Zuvor tat es der Lock nicht, weil er nicht im IRQ-Pfad genommen wird; das war zu
schwach begründet. Der gefährliche Fall ist nicht der IRQ-Pfad, sondern der **preemptierbare
EL1-Threadkontext**: Kernel-Threads (Demos, Loader-Setup, Fuzzer-Controller) nehmen `CAPS`, und ein
Timer-Tick darf sie dabei nicht verdrängen — läuft danach auf demselben Kern ein **Syscall** an (im
Trap sind IRQs hardwareseitig maskiert), der `CAPS` nimmt, spinnt dieser Kern für immer, weil der
verdrängte Halter nie wieder eingeplant werden kann. Die Deadlockfreiheit hing damit an der
**Konvention**, dass jede dieser ~35 Aufrufstellen selbst `local_irq_disable()` klammert (sie tun
es), statt am **Mechanismus**. Jetzt trägt der Lock sie; die vorhandenen Klammern bleiben gültig
(Save/Restore ist nesting-sicher) und dokumentieren weiterhin die gewünschte Atomarität ganzer
Setup-Sequenzen.

*Befund:* dieser Deadlock war **vorbestehend** (seit der SMP-/MCS-Phase) und trat unter QEMU-TCG mit
~27 % je Lauf auf (im el0iso-/reclaim-/native-/Fuzzer-Abschnitt, der viel spawnt/faultet/reapt +
cross-core killt); er wurde fälschlich als „Host-Last-Flakiness" abgetan. Nach dem Fix: 30/30 Läufe
deadlock-frei (vorher 4/15).

## 2. DMA-Revoke-Reihenfolge (DMA-use-after-free-Sicherheit)

Eine DMA-Region wird **immer** in dieser Reihenfolge abgebaut (`revoke_dma` + `vspace_teardown`):

1. **Gerät stilllegen** (`pcie::quiesce_by_rid`, ext-35): Bus-Master löschen, dann ein
   **Config-Read vom selben Gerät**. PCIe garantiert, dass eine Completion posted Writes nicht
   überholt — die Antwort trifft also erst ein, nachdem die zuvor vom Gerät abgesetzten Writes
   zugestellt sind.
2. `enforcer.disable_dma(binding)` — die Stage-1-Region wird aus dem Übersetzungskontext entfernt
   + `CMD_TLBI`+`CMD_SYNC` (synchron: `wait_cons` wartet auf `CMDQ_CONS == PROD`).
3. VSpace-Unmap (`unmap_dma_from_thread`) + `flush_asid`.
4. **Erst dann** `free_region` (über `delete_leaf` der DmaCap bzw. `free_raw_region`).

### Was Schritt 1 und Schritt 2 je abdecken — und warum keiner den anderen ersetzt

Bis ext-34 stand hier: *„Die Stage-1-Region wird entfernt. **Danach kann kein Gerät mehr in die
Region DMAen.**"* Das war für **künftige** Übersetzungen richtig und für bereits übersetzte, noch
unterwegs befindliche (posted) Writes **falsch**. Das Entfernen eines Übersetzungseintrags sagt
nichts über Transaktionen, die die Übersetzung schon durchlaufen haben.

| | deckt ab | deckt **nicht** ab |
|---|---|---|
| **Schritt 1** (BME-Clear + Flush-Read) | das **gutartige** Gerät: bereits abgesetzte Writes sind nach dem Flush-Read zugestellt | ein **kompromittiertes** Gerät, das `BME` ignoriert |
| **Schritt 2** (Stage-1/STE entfernen) | jede **künftige** Anforderung — auch die eines kompromittierten Geräts | Transaktionen, die die Übersetzung bereits passiert haben |

**Invariante:** zwischen Schritt 4 und einem späteren Re-Alloc desselben RAM zeigt **keine**
SMMU-Stage-1 mehr auf die Region **und** ist keine vom Gerät abgesetzte Transaktion mehr in
Zustellung. Verletzung = DMA-use-after-free.

**Grenzen von Schritt 1** (gehören zur Aussage, nicht als Fußnote): Die Ordnungsgarantie hält
nur, solange *Relaxed Ordering* / *ID-Based Ordering* für diese Funktion nicht aktiv sind und der
Pfad einheitlich ist. Ein Gerät, das `BME` nicht respektiert, wird davon nicht erfasst — für das
ist Schritt 2 die Grenze. Ein vollständiges Quiesce (FLR bzw. der dokumentierte Weg des Geräts)
wäre stärker; BME + Flush-Read ist der Teil, der **ohne** gerätespezifisches Wissen auskommt.

**BME-Wiederherstellung:** Trägt der Übersetzungskontext nach dem Detach noch andere Regionen, ist
das Gerät legitim weiter in Betrieb und `restore_command` stellt den vorherigen Zustand her (die
eben entfernte Region ist ab Schritt 2 ohnehin unerreichbar). Ist es die letzte Region, bleibt
Bus-Master **aus** — ein Gerät ohne jede Zuteilung soll auch keine Requests absetzen dürfen.

**Audit:** `dma_audit()` Code `4` — *jede* in einem `DMA_CTX` gemappte Region muss einer **lebenden
DmaCap** entsprechen. Eine Kontext-Region ohne zugehörige Cap bedeutet: RAM wurde freigegeben,
während die SMMU noch darauf zeigte (Schritt 4 vor Schritt 2) → Verletzung. (Snapshot von `DMA_CTX`
ziehen, Lock freigeben, dann gegen `CAPS.read().for_each_dma` prüfen — Rangordnung R0 vor R1.)

**Was das Audit NICHT trägt:** Es findet die Verletzung, es verhindert sie nicht. Die Reihenfolge
ist eine bewiesene Vorbedingung, keine erzwungene — `free_region` ist über `PhysRegion` aufrufbar,
nicht nur über einen Token, den die Invalidierung zurückgibt. Die strukturelle Fassung (ein
`Invalidated`-Token als einziger Weg zu `free_region`) ist vorgemerkt; sie berührt die
`CapSpace`-Finalisierung und läuft über denselben Rückmeldeweg wie `ReplyFinal`. Siehe `todo.md`.

### 2a. Granularität eines DMA-Puffers (ext-35)

**Invariante:** Anfang **und** Länge einer DMA-Region liegen auf dem **Cache-Writeback-Granule**
der Architektur (`hal::mmu::dma_granule()`; `CTR_EL0.CWG` auf ARM, `1` auf x86 — dort ist DMA
hardware-kohärent, es gibt keine Wartung und damit keine Bedingung). Erzwungen an der
**Cap-Prägung** (`install_dma_cap`/`install_dma_cap_ex` → `CapError::Unaligned`), nicht als
Treiberpflicht.

**Grund:** Cache-Wartung arbeitet auf ganzen Zeilen — `dc civac` (nach einem Geräte-Write bzw. bei
bidirektionalen Puffern) **verwirft** eine komplette Zeile. Liegt in einer angebrochenen Randzeile
fremder Speicher, verliert der seine noch nicht zurückgeschriebenen Daten. Das ist ein Schaden
**außerhalb** des Puffers, den weder die Bounds-Prüfung noch die IOMMU sieht: beide betrachten den
Puffer, nicht seine Nachbarschaft.

Die Bedingung gilt **einheitlich**, nicht nur für die Richtungen, bei denen invalidiert wird
(`DeviceWrite`/`Bidirectional`). Richtungsabhängig wäre näher am tatsächlich Gefährlichen, aber
eine Cap ist langlebig und ihre Richtung ein Feld — eine später erlaubte bidirektionale Nutzung
säße sonst auf einer Ausrichtung, die nie dafür geprüft wurde.

**Test:** `dmaalign` (Boot-Selbsttest) — ausgerichtete Region wird angenommen, verschobener Anfang
und angebrochene Länge werden abgewiesen, und aus den Fehlschlägen entsteht keine Cap.

### 2b. ATS (Address Translation Services) — Entscheidung, nicht Unterlassung

**ATS wird nicht aktiviert.** Ein Gerät mit ATS darf Übersetzungen **selbst cachen** (ATC) und
Anforderungen als „bereits übersetzt" markieren — die IOMMU reicht die dann durch. Ein
kompromittiertes oder fehlerhaftes Gerät umgeht damit praktisch die gesamte Durchsetzung
(*Thunderclap*-Klasse). Für ein System, dessen DMA-Isolation genau auf dieser Durchsetzung
ruht, ist ATS deshalb standardmäßig aus.

Die Bedingungen, unter denen es je eingeschaltet werden dürfte — damit das eine überprüfbare
Entscheidung bleibt und nicht beim nächsten Durchsatzproblem stillschweigend fällt:

1. eine **explizite Geräteliste** (nicht „alles, was es kann"), begründet je Eintrag;
2. **ATC-Invalidierung im Teardown-Pfad vorhanden und getestet** — ohne sie ist Schritt 2 oben
   wirkungslos, weil das Gerät seine alte Übersetzung weiterbenutzt;
3. die Geräteliste ist Teil des Trust-Modells (ADR 0007), nicht Treiberkonfiguration.

Solange auch nur eine der drei Bedingungen fehlt, bleibt die ATS-Capability ungesetzt.

## 3. Region-Balance (kein RAM-Leck)

Jede `PhysAllocator`-Allokation hat genau einen Rückgabepfad; über einen vollständigen
Alloc-/Free-Zyklus bleibt `MEM.total_free()` unverändert.

- **DmaCap-Region:** carvt über `KernelRegionSource::request` (eine MEM-Carve-Stelle, K3), aber das
  **Besitzmodell ist (phys,len)-basiert**: die Lebensdauer hängt an der **DmaCap**, nicht an einem
  `Region`. Freigabe genau einmal über `delete_leaf`(Dma) → `free_region` (Cap-Pfad) bzw.
  `free_dma_region` (roher Pfad). Der `Region`-Wrapper aus `request` wird sofort zum reinen
  `MemoryCap`-Deskriptor aufgelöst (`into_cap`, kein Drop-Free) — bewusst **nicht** Region-besessen
  (eine DMA-Region wird nie über `RegionSource::release` zurückgegeben). Das ist kein Duplikat,
  sondern ein zweites legitimes Besitzmodell für dasselbe RAM.
- **Region-Runtime (Heap):** jede `Region` **besitzt** ihren `MemoryCap` (lineares Eigentum);
  `Heap::drop` gibt **alle** Regionen über `RegionSource::release` zurück.
- **Thread-Stacks:** `spawn` ↔ `reap_core` (`REAPED_BYTES` belegt die Rückgabe monoton).

**Audit/Test:** hwfuzz-Baseline (`total_free` balanciert je Epoche), `churn`, `dmagen`
(`balanciert`), `sasheap` (`balanciert(alle Regionen zurück)`).

## 4. `RegionView`/`Pod`-Sicherheitsvertrag (das gesamte Speicher-`unsafe`)

Das gesamte Speicher-`unsafe` der Trusted-SAS-Schicht liegt in `crates/sel4lake-region` (RegionView-
Accessoren + Allokator-Glue). Es ist begründet durch:

1. **Cap-validierter Besitz:** eine `Region` hält eine `MemoryCap` (lineares Eigentum); die Bytes
   `[base, base+len)` sind exklusiv dieser Region zugeordnet (vom Kernel ausgeschnitten, disjunkt).
2. **Bounds:** jeder `RegionView`-Zugriff (`get`/`set`/`copy_*`/`fill`/`with_bytes`/`subview`) prüft
   `offset + size_of::<T>() <= len` **vor** dem Roh-Zugriff; out-of-bounds → kein Zugriff.
3. **`Pod`-Beschränkung:** typisierte Zugriffe nur für `unsafe trait Pod` (`Copy`, keine Padding-/
   Pointer-Invarianten, jede Bitkombination gültig) — kein Erzeugen ungültiger Werte.
4. **Kein nackter Slice nach außen:** `with_bytes(|s: &mut [u8]| …)` bindet den Slice an die Closure
   (kann nicht entkommen); es gibt **keinen** öffentlichen `MemoryCap -> &mut [u8]`-Wrapper.

**Folge (ADR 0002 + 0010):** mit *no unsafe im App-Code + buglosem Compiler* kann ein safe-Rust-
Prozess nur Speicher berühren, der von seinen legitimen Referenzen erreichbar ist (Stack, Statik,
seine `Region`-Menge). Diese Schicht trägt die Isolation **ohne MMU**; ihre Korrektheit (Bounds +
cap-Besitz) ist die zentrale Verifikationsverpflichtung.

## 5. Domänen-/Cap-Policy (ext-22, Kurzfassung)

- HW-Caps (`Mmio`/`Irq`/`Dma`, `kind_is_hardware`) nur in **HardwareLand**; `PdControl` nur in
  **TrustedSas** (`install_cap_checked`).  Audit: `domain_audit()` Code 1/2.
- Eine untrusted Domäne (HardwareLand/UserLand) MUSS eine **isolierte** VSpace haben (ASID ≠ 0).
  Audit: `domain_audit()` Code 3 (nur für lebende gebundene Threads).
- HardwareLand-Backend: unveränderliche Partner-Bindung an genau einen TrustedSas + genau einen
  Kanal (bei Erzeugung fixiert).

## 6. SMMU unter QEMU (ehrlicher Befund, ADR 0008)

QEMU 11 übersetzt **emulierte** Geräte-DMA **nicht** durch die SMMUv3 (kein `smmuv3_translate`/
`smmu_ptw` selbst bei `V=0`-STE, Gerät hinter Root-Port). Daher ist die **Level-2-SMMU-Durchsetzung
unter QEMU für emulierte Geräte nicht beobachtbar** (`cj_smmu_enforced == false` ist korrekte
Telemetrie, kein Bug). Die demonstrierbare Durchsetzung ist **Level 1** (Software-Bounds,
`region_contains`): der vertrauenswürdige Treiber validiert jede Geräte-Adresse vor dem
Programmieren. Auf realer HW (z. B. STM32MP25) ist die installierte Stage-1-STE der HW-Backstop.

## 7. Audit-Katalog (aggregiert in `ipc_audit`)

| Bereich | Funktion | Codes |
|---|---|---|
| Endpoint/Notification/Scheduler | `ipc_audit` (Basis) | 1–3, 10+n |
| Cap-CDT/Refcount | `audit_cdt` | 20+n |
| Domänen-Policy | `domain_audit` | 30+n (1=HW-Cap, 2=PdControl, 3=VSpace) |
| DMA-Policy + Enforcer + Revoke-Ordnung | `dma_audit` | 40+n (1=Bounds, 2=Überlappung, 3=Enforcer, 4=Ctx-Region ohne Cap) |
| VSpace W^X / Tabellen | `vspace_audit` | separat |
| TrustedSAS-Key-DB + Trust-Gate (ext-28) | `loader::trust_audit` | separat (1=DB leer, 2=key_id≠fingerprint, 3=Dublette, 4=gültig abgelehnt, 5=manipuliert akzeptiert) |

`ipc_audit() == 0` bei jedem Quiescenz-Punkt + zwischen allen Fuzzer-Operationen = alle obigen
Invarianten halten.

## 8. Adversariale Validierung von außen (ext-27, ADR 0012)

Die Isolations-Invarianten (insbesondere #1 Hardware-Adressraumtrennung, #5 Domänen-/Cap-Policy)
werden zusätzlich durch **extern geladene Drittsoftware** geprüft: sechs adversariale EL0-Dienste
(`tests/services/`, 2 je Domäne), vom Binary-Loader geladen, greifen den Kernel + sich gegenseitig
ausschließlich über die Syscall-ABI an. Schlüsselaussagen, empirisch bestätigt:

- **Hardware-Isolation ist domänen-unabhängig.** Ein geladener Dienst **jeder** Domäne (auch
  HardwareLand und TrustedSAS) faultet beim Lesen von Kernel-RAM aus EL0 (`el0-trap FAR=0x40000000`),
  wird terminiert, der Kernel überlebt. Trust befreit **nicht** von der MMU-Trennung.
- **Trust = Cap-Autorität, nicht Privileg.** Ein TrustedSAS-Dienst ohne tatsächlich gehaltene
  `PdControl`/`Loader`-Cap erhält auf PDCTL/LOAD/KILL `ERR_BADCAP` — die Domäne allein gewährt keine
  Operationsmacht.
- **Cross-Service-Nicht-Interferenz unter Nebenläufigkeit:** drei Angreifer dreier Domänen gleichzeitig
  → keiner stört die korrekte Abweisung eines anderen, ein kernel-geschütztes Canary bleibt
  bit-genau unberührt, alle Audits 0.

Aus einem EL0-Prozess sind **nur** ABI-Operationen ausdrückbar; Cap-/CDT-Operationen (kein Syscall)
bleiben im In-Kernel-Selbsttest (`captest`/`fuzz`/`ipcfuzz`). Vollständige Matrix:
`docs/phase-reports/ext-27-adversarial-tests.md`.

## 9. TrustedSAS-Zertifikate / Trust-Gate (ext-28, ADR 0014)

`DOMAIN_TRUSTED` behält seine **Cap-Autorität** (darf `PdControl`/`Loader`-Caps halten) — daher gilt
für sie eine zusätzliche Lade-Invariante. Maßgeblich: `kernel::loader::verify_image`. Belege:
`loadhw`/`load`/`aggrt`/`intrt`/`cross` (Selbsttest) + `certfuzz`/`trust_audit` (Fuzzer/Audit).

- **Trust-Gate-Invariante.** Eine TrustedSAS-PD entsteht **nur** aus einem Image mit gültigem,
  auf genau dies Binary gebundenem Ed25519-Zertifikat. Strukturell: `load_image`/
  `load_program_into_pd` rufen `verify_image` **vor** jeder Ressourcenvergabe; ein abgelehntes Image
  erzeugt **weder** Thread **noch** PD (`LoaderError::Unverified`). UserLand/HardwareLand sind
  ausgenommen (hardware-isoliert, kein Zertifikat).
- **Bindung (alles signiert über die gesamte Nachricht).** `binary_hash == SHA-256(ELF)`,
  `manifest_hash == SHA-256(Manifest)`, `program_id`/`version == Archiv-Eintrag`,
  `version >= MIN_VERSION[program_id]`, `unsafe_status == ALL_PASS`, `key_id ∈ TRUSTED_KEYS` (nicht
  `revoked`). Bricht **eine** Bedingung → Ablehnung.
- **Schlüssel-Invariante.** Der Kernel hält **nur** öffentliche Schlüssel; die Key-DB
  (`trusted_keys.rs`) ist kompiliert + read-only, **nur** per Firmware-/Kernel-Update änderbar — es
  existiert **kein** Syscall dafür. `verify_strict` (nicht `verify`) → keine Signatur-Malleability.
- **Unsafe-Invariante (host-erzwungen).** Ein zertifiziertes TrustedSAS-Programm ist
  `#![forbid(unsafe_code)]`; `unsafe` existiert im gesamten App-Dep-Baum **nur** in der Allowlist
  `{libsel4lake}`. `tools/sign_trusted.py` verweigert sonst das Zertifikat; der Kernel verlangt
  `unsafe_status == ALL_PASS`.
- **`trust_audit()` (Laufzeit-Oracle).** Key-DB-Selbstkonsistenz (`key_id == fingerprint(pubkey)`,
  Eindeutigkeit, nicht leer) **plus** Live-Test: ein bekannt gültiges Zertifikat wird akzeptiert,
  eine manipulierte Kopie abgelehnt → das Gate setzt zur Audit-Zeit aktiv durch.

Eingefrorenes Zertifikatsformat + Sicherheitsanalyse:
`docs/phase-reports/ext-28-trusted-certificates-report.md`. Schlüssel-Runbook:
`docs/runbook-trusted-keys.md`.


## 10. Härtung ext-29: Cap-Budget, Datenremanenz, Spekulation

Vier Invarianten aus dem Sicherheits-Review (Details + Herleitung:
`docs/phase-reports/ext-29-hardening.md`).

### 10.1 Kein Cap-Leck beim IPC-Grant (Cross-PD-DoS)

Der globale `CapSpace` ist eine **systemweit geteilte** Tabelle fester Größe — jede unerreichbar
gewordene Cap darin ist ein permanenter Verlust **für alle PDs**.

- **Invariante.** Ein `REPLY`-Grant, der eine bereits im Empfangs-Slot liegende Cap verdrängt, gibt
  die verdrängte Cap frei. Nach *n* Grants derselben Quell-Cap in denselben Slot existiert **genau
  eine** lebende Ableitung. Strukturell: `grant_cap` **meldet** die verdrängte Cap zurück (es löscht
  sie nicht selbst — die Finalisierung nimmt `MEM` bzw. bricht Calls ab, unter dem dort gehaltenen
  `CAPS`+`EPS` wäre das sperrordnungswidrig), und `dispatch` löscht sie, **nachdem** beide Locks
  gefallen sind.
- **Test.** `grantlk` — 65 Grants in denselben Slot → `child_count(Quell-Cap) == 1`, Cap weiter
  nutzbar, `cap_audit_cdt() == 0`. Sensitivitätsgeprüft: mit deaktiviertem Fix meldet der Test 65.

### 10.2 Cap-Budget je PD

`NCAPS` begrenzt nur den lokalen Index-Adressraum einer PD, nicht ihren Verbrauch an globalen Slots
(`NPDS * NCAPS` ≫ Tabellengröße).

- **Invariante.** Keine PD belegt mehr als `CAP_BUDGET_PER_PD` Slots gleichzeitig; Überschreitung
  wird **abgewiesen** (kein Eintrag, keine Ableitung). Ein **Ersetzen** eines belegten Slots
  verbraucht nichts und bleibt erlaubt. Erzwungen an **allen** Eintragspfaden: `install_cap_checked`
  und `grant_cap` (sonst wäre der Grant das Schlupfloch um die Schranke).
- **Test.** `budget` (Boot-Selbsttest).

### 10.3 Keine Datenremanenz über Subjektgrenzen

- **Invariante.** Jede vom Kernel vergebene RAM-Region ist bei der Vergabe **genullt**. Erzwungen
  an genau einer Stelle: `system::mem_alloc` (alle Allokationen laufen darüber, nicht über
  `MEM.lock().alloc`). Genullt wird bei der **Vergabe**, nicht bei der Rückgabe — das deckt auch
  fabrikfrisches RAM mit Firmware-Resten ab und ist robust gegen Freigabepfade, die eine Region
  ohne `free` verlieren.
- **Test.** `zerotest` (Boot-Selbsttest): frische Allokation genullt; Muster schreiben → freigeben →
  dieselbe Region erneut allozieren → wieder genullt.

### 10.4 Spekulations-Härtung (Spectre-Klasse)

- **Invariante.** Jeder Tabellenindex, den EL0 beeinflusst (Cap-Slot, Endpoint-/Notification-Id),
  wird nicht nur architektonisch geprüft, sondern zusätzlich **datenabhängig** maskiert
  (`cpu::array_index_nospec`, Linux-Manier: arithmetische Maske + `CSDB`) — das überlebt auch eine
  falsch vorhergesagte Verzweigung. Beim VSpace-Wechsel steht eine Spekulationsbarriere (`SB`, sonst
  `dsb sy; isb`).
- **Sichtbar beim Boot.** `spec : CSV2=… CSV3=… FEAT_SB=…` meldet, was die HW von sich aus
  garantiert (CSV2 → Spectre-v2-immun, CSV3 → Meltdown-immun).
- **NICHT abgedeckt (bewusst, offen).** Cache-/Timing-Seitenkanäle zwischen PDs — dafür bräuchte es
  Cache-Partitionierung/Coloring (Architekturänderung, kein Patch). Ebenso Spectre-v2 auf HW **ohne**
  FEAT_CSV2: die Gegenmaßnahme wäre Predictor-Invalidierung per Firmware-Call
  (SMCCC_ARCH_WORKAROUND_1), den QEMU `virt` nicht anbietet.


## 11. Thread-Migration + Boot-Kapazitäten (ext-30)

Details + Herleitung: `docs/phase-reports/ext-30-migration-und-kapazitaet.md`.

### 11.1 Identität ist von der Platzierung getrennt

- **Invariante.** Die `gid` einer [`ThreadId`] ist **lebenslang stabil** und unabhängig vom
  besitzenden Kern. Alle per-Thread-Kerneltabellen (FP-Kontext, `VSPACE_OF`, Kernel-Stack-Slot)
  sind über sie indiziert und überleben eine Migration unverändert; eine Tcb-Cap bezeichnet
  nach der Migration denselben Thread.
- **Invariante (Directory-Kohärenz).** Für jeden belegten TCB gilt: der Directory-Eintrag
  seiner `gid` ist `used`, trägt dieselbe Generation und zeigt auf **genau** den Kern und den
  lokalen Slot, an dem der TCB liegt. Maschinell geprüft: `Scheduler::audit()` Code **8**,
  aggregiert über alle Kerne in `system::sched_audit_all()`.
- **Invariante (stale Handles).** Beim Thread-Ende wird der Directory-Eintrag **sofort**
  ungültig gemacht (`used = 0`, Generation +1); die `gid` kehrt erst beim **Reap** in die
  Freiliste zurück. Zwischen beiden Zeitpunkten ist sie nicht neu vergebbar — ein altes Handle
  kann also nie einen *anderen* Thread treffen.

### 11.2 Wettlauf „Besitzer nachschlagen ↔ Migration"

- **Invariante.** Jeder kernübergreifende Zugriff prüft nach dem Sperren **erneut**
  (`resolve` vergleicht Generation *und* Kern). Ein Fehlschlag mit gewechseltem Besitzer wird
  mit dem neuen Besitzer wiederholt (`system::with_owner`, begrenzt auf `MIGRATION_RETRIES`);
  ein Fehlschlag ohne Besitzerwechsel ist ein echter Fehlschlag.
- **Konsequenz:** aus einer `ThreadId` darf **nie** ein Kern abgeleitet werden (das ging bis
  ext-29 arithmetisch). Einzige Quelle ist das Directory.

### 11.3 Was nicht migriert werden darf

- Der **laufende** Thread (Zustand im aktiven Trap-Frame), ein Thread mit aktiver
  **Budget-Donation** (die Links sind lokale Slots → Donation ist intra-core), und der
  **Idle**-Thread. Strukturell erzwungen in `Scheduler::detach_for_migration`.
- **Push statt Pull:** die Migration läuft immer auf dem **abgebenden** Kern, weil nur er den
  Lazy-FP-Kontext des Migranten aus seinen eigenen FP-Registern sichern kann.
- **Kein Thread-Verlust:** scheitert die Aufnahme (Zielkern voll), wird der Migrant beim
  Quellkern wieder eingehängt.

### 11.4 Kapazität

- **Invariante.** Kerntabellen werden **einmalig beim Boot** dimensioniert
  (`system::configure`) und leben bis zum Reboot; es gibt keinen Pfad, der sie freigibt oder
  ein zweites Mal anhängt. Zugriffe sind bounds-geprüft (`Slab`/`AtomicTable` panieren bei
  Überschreitung wie ein Array — kein UB).
- **Invariante.** Die Hosting-Kapazität eines Kerns liegt über seinem Anteil
  (`MIGRATION_HEADROOM`), sonst könnte kein Kern einen Migranten aufnehmen.
- **Test.** `scale` — 1024 Threads gleichzeitig, danach vollständige Rückgabe (Slots +
  Stack-RAM), Scheduler-Audit 0.
