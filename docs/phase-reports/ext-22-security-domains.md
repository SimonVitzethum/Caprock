# ext-22 — Drei Sicherheitsdomänen (Trusted-SAS / HardwareLand / UserLand)

Siehe [ADR 0007](../adr/0007-security-domains.md) für die Architekturentscheidung und die
IRQ-Lock-Sicherheitsanalyse.

**Ergebnis:** `./test-qemu.sh` = **ALL PASS (42 Checks)**. Das dreidomänige Modell ist
funktional komplett, kernel-erzwungen und durchgehend auditiert. Jede neue Invariante ist
einzeln **sensitivitätsgeprüft** (Fix brechen → Oracle/Test greift → wiederherstellen).

## Phasen (je eigener Commit auf master)

- **P1 `domain`** — Domänen-Tag (unveränderlich) am `Pd` (microkit); `Caps::install_cap_checked`
  als zentraler Policy-Enforcement-Punkt; `domain_audit()`-Oracle, in `ipc_audit` aggregiert
  (Code 30+) → beide Fuzzer prüfen es. Regel: Domäne = Policy (Cap-Typen + Kommunikation),
  VSpace = Isolationsmechanismus; untrusted Domänen müssen isoliert sein (nur LEBENDE Threads
  geprüft). Sensitivität: HardwareLand an globalen Thread gebunden → audit==3.
- **P2 `pdctl`** — `ObjectKind::PdControl{pd}` + `SYS_PDCTL` (START/STOP/PAUSE/RESUME); nur
  TrustedSas steuert nur UserLand, jede Op cap-gated. `Scheduler::pause` (gezieltes Blockieren)
  + `on_tick`-Korrektur (blockierter `current` deplaniert sauber). Sensitivität: Cap-Typ-Policy
  aus → PdControl in UserLand erlaubt → pdctl FAILURES.
- **P3 `chan`** — unveränderliche paarweise HardwareLand↔Trusted-Bindung bei `create_hardware_
  backend` (1:N, stabile `backend_id`, dedizierter Endpoint+Notification); ein Backend hält
  NUR Kanal-Comm-Caps (install_cap_checked + domain_audit Regel 4/5). Sensitivität: Kanal-
  Enforcement aus → Fremd-Cap ins Backend → chan + ipcfuzz FAILURES. (Verifizierte nebenbei:
  EL1-Client ↔ isolierter EL0-Backend CALL/REPLY.)
- **P4 `rtc`** — generisches `vspace_map_device` (beliebige MMIO-Region EL0-Device,
  Device-nGnRnE, PXN|UXN; GiB-0-Block-Split, Rest EL1-only); `MmioCap` (kernel-only,
  HardwareLand-only, Delete fasst RAM nicht an); RTC-Backend liest echtes PL031-`RTC_DR` über
  den Kanal. Audits: vspace_audit prüft Device-W^X. Sensitivität: Mapping weggelassen →
  Backend faultet (FAR=0x09010000); MMIO-Cap in UserLand abgelehnt.
- **P5 `irq`** — `IrqCap`; GIC `route_spi` (GICD_ITARGETSR — fehlte) + `mask_intid`; **Deferred-
  IRQ-Zustellung** (lock-freier IRQ-Hook → Reschedule-Drain → `signal_from_kernel`, deadlock-
  frei dank In-Trap-IRQ-Maskierung). RTC-Backend armiert den PL031-Match-IRQ, der Kernel
  stellt ihn als Notification zu. Sensitivität: `route_spi` weggelassen → IRQ erreicht keinen
  Kern → irq FAILURES.
- **P6 `hwfuzz`** — Domänen/HW-Fuzzer (32 Epochen, deterministischer LCG): churnt MMIO/IRQ/
  PdControl-Caps gegen die Domänen-Policy (Negativfälle abgelehnt), stresst den CDT mit
  Kopien, prüft je Epoche `domain_audit/cap_audit_cdt/vspace_audit == 0` + Ressourcen-
  Baseline — insbesondere bleibt `total_free` über MMIO/IRQ-Cap-Delete **unverändert**
  (Gerät != RAM). Plus ADR 0007 + dieser Bericht.

## Neue Kernel-Primitive (Überblick)
- `microkit`: `Domain`-Enum + `Pd{domain, partner, backend_id, chan_ep, chan_ntfn}` (alle
  unveränderlich); `install_cap_checked`, `domain_audit`, `create_hardware_backend`.
- `cap::ObjectKind`: `PdControl{pd}`, `Mmio{phys,len}`, `Irq{intid}` (+ `install_*`). Delete
  fasst den RAM-Allokator NIE für diese an.
- `abi`: `SYS_PDCTL=12` + `pdctl`-Sub-Ops (13.. für künftige HW-Syscalls/DMA reserviert).
- `hal::mmu`: `vspace_map_device`, `vspace_collect_device_tables`, `vspace_device_wx_ok`.
- `hal::gic`: `route_spi` (GICD_ITARGETSR), `mask_intid` (GICD_ICENABLER).
- `hal::exception`: `set_irq_hook` (Geräte-IRQ-Hook).
- `ipc::Notification`: `signal_from_kernel`.
- `sched`: `SchedOps::pause`/`stop`; `Scheduler::pause`.
- `system`: `create_pd_in_domain`, `domain_audit`, `install_pd_control_cap`, `install_mmio_cap`/
  `map_mmio_into_thread`, `install_irq_cap`/`bind_irq`/`irqs_delivered`, `drain_pending_irqs`.

## Bewusst aufgeschoben
- **`DmaCap`**: als HW-Cap-Kategorie vorgehalten (nachrüstbar ohne ABI-/Strukturänderung);
  echtes Geräte-DMA braucht ein DMA-fähiges Gerät/Virtio.
- Reale NVMe/WLAN/USB/Ethernet/GPU-Stacks (Treiberlogik bliebe ohnehin im Trusted-SAS).

## Hinweis
`test-qemu.sh`-Default-Timeout 360 s (längere Demo); die ext-22-Tests laufen VOR den Fuzzern
(frisches Regime). Unter konkurrierender Host-Last bleibt der Lauf intermittierend flaky
(TCG-Starvation, kein Kernel-Bug) — bei freier CPU 42/42 in wenigen Sekunden.
