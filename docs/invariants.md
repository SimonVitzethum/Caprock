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
| R1 | `EPS[]`, `NTFNS[]`, `VSPACES`, `DMA_CTX`, `VIRTIO_PCI` | `SpinLock` | Ressourcentabellen (je Objekt/global) |
| R2 | `SCHEDS[core]` | `SpinLock<Scheduler>` | Per-Kern-Runqueue |
| R3 | `Heap.inner` | `SpinLock<HeapInner>` | prozess-lokaler Allokator (nur Region-Runtime) |
| R4 | `MEM` | `SpinLock<PhysAllocator>` | physischer Allokator — **innerster** |

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

**Leaf-Locks** (stets allein gehalten, keine Schachtelung): `KSTACKS`, `FP_STATES`, `VIRTIO_PCI`,
`CS_RELOAD_INFO`/`RELOAD_INFO`, `hal::console::CONSOLE`.

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
`RwSpinLock` (`CAPS`) wird **nicht** im IRQ-Pfad genommen und bleibt ohne IRQ-Maske.

*Befund:* dieser Deadlock war **vorbestehend** (seit der SMP-/MCS-Phase) und trat unter QEMU-TCG mit
~27 % je Lauf auf (im el0iso-/reclaim-/native-/Fuzzer-Abschnitt, der viel spawnt/faultet/reapt +
cross-core killt); er wurde fälschlich als „Host-Last-Flakiness" abgetan. Nach dem Fix: 30/30 Läufe
deadlock-frei (vorher 4/15).

## 2. DMA-Revoke-Reihenfolge (DMA-use-after-free-Sicherheit)

Eine DMA-Region wird **immer** in dieser Reihenfolge abgebaut (`revoke_dma` + `vspace_teardown`):

1. `enforcer.disable_dma(binding)` — die Stage-1-Region wird aus dem Übersetzungskontext entfernt
   + `CMD_TLBI`+`CMD_SYNC`. **Danach kann kein Gerät mehr in die Region DMAen.**
2. VSpace-Unmap (`unmap_dma_from_thread`) + `flush_asid`.
3. **Erst dann** `free_region` (über `delete_leaf` der DmaCap bzw. `free_raw_region`).

**Invariante:** zwischen Schritt 3 und einem späteren Re-Alloc desselben RAM zeigt **keine**
SMMU-Stage-1 mehr auf die Region. Verletzung = DMA-use-after-free.

**Audit:** `dma_audit()` Code `4` — *jede* in einem `DMA_CTX` gemappte Region muss einer **lebenden
DmaCap** entsprechen. Eine Kontext-Region ohne zugehörige Cap bedeutet: RAM wurde freigegeben,
während die SMMU noch darauf zeigte (Schritt 3 vor Schritt 1) → Verletzung. (Snapshot von `DMA_CTX`
ziehen, Lock freigeben, dann gegen `CAPS.read().for_each_dma` prüfen — Rangordnung R0 vor R1.)

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
