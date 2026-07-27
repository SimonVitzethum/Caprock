---
name: sel4lake-project
description: "SEL4Lake — from-scratch Rust capability microkernel project; layout, status, key design pivot"
metadata: 
  node_type: memory
  type: project
  originSessionId: 625576c1-9165-4a91-8b4e-902623e8593b
---

SEL4Lake ist ein von Grund auf neuer, capability-basierter Rust-Microkernel
(inspiriert von seL4 + seL4-Microkit), entwickelt unter
`/home/simon/Dokumente/SEL4Lake/SEL4Lake`. Referenzquellen (nur Analyse, nicht
verändern): seL4-C-Kern unter `seL4/src`, Rust-Microkit unter `seL4/microkit_rust`
(baut Userland-PDs *auf* seL4 — NICHT die Kernelbasis; SEL4Lake ist eigenständig,
QEMU lädt via `-kernel`).

Ziel: hochsicher/-performant, capability-basiert, deterministisch, modular; ARM
aarch64, 8 Kerne, 4 GiB; Tests in QEMU `virt`. Build: `./build.sh` (rustup
nightly + `-Z build-std`, custom Target `targets/aarch64-sel4lake.json`; neuere
Nightly braucht `-Z json-target-spec`). Start: `./run-qemu.sh`.

**Zentraler Design-Pivot (ADR 0002):** „kein virtueller RAM / keine MMU“ wird
als Single-Address-Space mit EINER Identity-Map + Caches an umgesetzt — NICHT
MMU komplett aus (das wäre uncached und langsam). Adresse == Phys-Adresse,
keine per-Prozess-VSpaces. Folge: Isolation NUR durch Rust (intralingual) +
Capabilities; alle Komponenten müssen speichersicheres Rust sein.
**ÜBERHOLT (ext-11, Weg C/Hybrid):** reines SAS gilt nur noch für *trusted*
Rust-PDs; ISOLIERTE/native PDs bekommen eine eigene per-Prozess-VSpace (Identity
+ ASID/nG), weil ein kompromittierter nativer Prozess im reinen SAS fremdes
User-RAM lesen/schreiben konnte (Caps/EL0 gaten rohe Loads nicht).

Status: **Phasen 0–7 + Ausbaustufen 1–28 fertig & in QEMU verifiziert**
(`./test-qemu.sh` = ALL PASS; Kernel ruft nach Erfolg `hal::psci::system_off` -> QEMU beendet
sich, Lauf endet bei Abschluss statt Timeout). Zuletzt ext-28 (TrustedSAS-Zertifikate): Release
ohne Fuzzer = ALL PASS (certfuzz SKIP), `KERNEL_FUZZ=1` = ALL PASS inkl. certfuzz.
WICHTIG: die aktuelle Fuzzer-Config nutzt SPIN-Lauffenster (billig); ein FRÜHERER
Tick-Wait-Versuch (auf EMULIERTE Zeit warten) war unter 8-Core-TCG pathologisch langsam
-> verworfen. Host hat 20 Kerne; TCG ist single-thread -> ein freier Kern reicht, daher
robust auch unter Host-Last. Git: Repo unter dem Projektordner, Remote
origin (SSH) -> häufig nach `master` pushen, siehe [[git-workflow]].
Ausbaustufe 1: FP/SIMD-Kontext eager save/restore (TrapFrame jetzt 800B, q0-q31+
FPSR/FPCR; FPSR/FPCR-Offset 784 per `add` da GP-stp nur ±504); Bitmap-Prioritaeten
(NPRIO=8, leading_zeros) + SYS_PARK; Per-Kern-Locks ZURUECKGESTELLT (Konflikt mit
Single-SYSTEM-Lock, perf-only). Ausbaustufe 2: SYS_EXIT + Reaping (Idle ruft
system::reap -> Stack/TCB zurueck), TCBs als Caps (ObjectKind::Tcb(u64 raw),
install_tcb) + cap-kontrolliertes SYS_KILL (braucht WRITE).
Weitere Ausbaustufen (alle in QEMU verifiziert, je eigener Commit auf master):
- Notifications: ObjectKind::Notification, NotificationTable (signal/wait, Badge);
  CapSpace::lookup liefert jetzt (kind,rights,badge); SYS_SIGNAL/WAIT.
- Cap-Transfer in IPC: REPLY mit GRANT_FLAG im Tag delegiert die Cap am lokalen
  Slot an den Aufrufer (GRANT_RECV_SLOT); microkit::dispatch nimmt &mut cspace/pds.
- Stateful Hot-Reload: sel4lake-mem::peek_u64/poke_u64 (SAS-Direktzugriff, unsafe);
  Zaehler-Service v1(+1)->v2(+10), Zustand in Memory-Region bleibt erhalten.
- DTB: crate sel4lake-dtb (FDT-Parser, 0 unsafe); QEMU gibt fuer rohes ELF KEINEN
  DTB (x0=0) -> von QEMU erzeugten DTB (auf ~8.6KB gekuerzt) in kernel/src/virt.dtb
  EINGEBETTET (include_bytes!) + geparst -> RAM aus /memory.
- Lazy-FP VERWORFEN: CPACR-FP-Trap bei EL1 trappt auch Kernel-NEON (kein EL0) ->
  Hang; zurueckgerollt, eager FP bleibt. Lazy-FP braucht EL0-Userland.
- Per-Kern-parallele Scheduler (FERTIG, ext-6): sel4lake-sched::Scheduler ist jetzt
  EINE Kern-Instanz (eigene TCB-Partition [Tcb;PER_CORE], Run-Queues, current,
  Zombies). PER_CORE=32, NTHREADS=256. Globaler Slot=core*PER_CORE+local; ThreadId
  traegt globalen Slot -> tid.core()/local(). Kernel: SCHEDS:[SpinLock<Scheduler>;8]
  (je Kern eigener Lock). reschedule/syscall/fp_trap/el0_fault/reap sperren nur
  SCHEDS[core] -> paralleler heisser Pfad, kein globaler Lock mehr. bind_cores()
  (Bootkern, vor spawn) bindet alle Instanzen an Kern-ID. spawn_on_core(c,...) plant
  auf bestimmtem Kern ein. Lock-Ordnung RES->SCHEDS[*]->FP_STATES (reschedule nimmt
  nur SCHEDS[core] -> deadlockfrei). Cross-Core: gic::send_sgi(core,intid) (GICD_SGIR)
  + IPI_RESCHED_INTID=0 (handle_irq behandelt wie Timer); system::wake_remote(tid)
  sperrt Zielinstanz+unblock+IPI. unblock idempotent (nur blockierte) -> race-frei.
  Demo smp-Check: Worker auf jedem Sekundaerkern parallel + Parker auf core 1 per
  IPI von core 0 geweckt.
- Feinkoerniges IPC-Locking (FERTIG, ext-8): EIN RES-Lock -> CAPS(cspace+pds) +
  MEM(phys) + per-Endpoint EPS[i] + per-Notif NTFNS[i]. ipc: Endpoint/Notification
  sind &mut self-Einzelobjekte (NENDPOINTS/NNOTIFICATIONS pub). microkit::Caps{cspace,
  pds}; dispatch ownt das Locking (CAPS kurz fuer Cap-Aufloesung, dann per-Objekt;
  REPLY+grant haelt CAPS->EPS). Sperrordnung CAPS < {EPS[i],NTFNS[i],MEM} < SCHEDS
  < FP_STATES (Subagent-geprueft azyklisch). reap sammelt unter SCHEDS, gibt frei,
  dann MEM (keine SCHEDS+MEM-Inversion). syscall haelt kein SCHEDS mehr vor.
- EL0-Kernel-Stack-Reclaim (FERTIG, ext-9): KstackPool{free[8],slot_of[MAX_THREADS]}
  hinter eigenem KSTACKS-Lock (stets ALLEIN). spawn_user claim+record; Reclaim beim
  Thread-Ende in KernelSched::{exit_current,kill} + el0_fault. Pool MUSS EL1-only
  bleiben. user_kstack_free_count(). Demo: 16 transiente EL0-Exiter trotz 8er-Pool.
- Lastausgleich (FERTIG, ext-10): Scheduler::load() + system::least_loaded_core() +
  spawn_balanced (leichtester Kern). Per-Kern-Reaping: jeder Kern reapt in idle()
  seine eigenen Zombies (vorher nur core 0). life-Check nutzt jetzt monotonen
  system::reaped_bytes() statt total_free-baseline. KEINE Laufzeit-Migration
  (kollidiert mit TCB-Partitionierung: ThreadId kodiert Kern) -> dokumentiert.
- Per-Prozess-VSpaces / Weg C Hybrid (FERTIG, ext-11): SAS-Spur fuer trusted Rust-PDs
  + eigene VSpace fuer ISOLIERTE PDs. Beantwortet: kompromittierter nativer Prozess
  konnte im SAS fremdes User-RAM lesen/schreiben (alles UserRw; Caps/EL0 gaten rohe
  Loads NICHT). mmu: erste 2 MiB = reine Kernel-L3 (kein EL0-freies-RAM, teilbar;
  User-RAM ab USER_RAM_MIN=2MiB), UserRw jetzt nG (ASID-spez.), Code/Kernel global.
  global_root/set_user_vspace(root,asid)/build_isolated_vspace(l1,l2,region) (Kernel
  EL1-only + 1 EL0-Region in 2 frische 4KiB-Frames; Identity bleibt, Isolation via
  Mapping-Praesenz). system: VSPACE_OF[thread] (gepacktes TTBR0, 0=SAS), sync_vspace
  im Kontextwechsel (nur bei Aenderung), spawn_isolated (2MiB-Region in GiB1 + VSpace
  + ASID ab 1), el0_fault merkt iso_faulted. Kein TLB-Flush/Switch (global nG=0 +
  ASID). Demo vspace-Check: SAS- vs. isolierte Probe lesen dieselbe Adresse X -> SAS
  darf, isoliert faultet (EC=0x24) + nur-IPC. Offen: allg. VMM (map/unmap, Frame-Caps),
  Shared-Mem-IPC, untrusted nativer Code je VSpace. Drei Wege analysiert: A=per-VSpace
  (gewaehlt), B=SFI/WASM, C=Hybrid.
- Allgemeiner VMM (FERTIG, ext-12): Frame-Caps (=ObjectKind::Memory) + MAP=10/UNMAP=11
  Syscalls (cap-gated, WRITE) + VSpace-Teardown. mmu: vspace_create_base/map_block/
  unmap_block/flush_asid (TLB nach ASID). system: VSPACES[asid](L1/L2), create_vspace/
  vspace_map_region/unmap_region/teardown; spawn_isolated nutzt create+map; el0_fault
  baut VSpace der gefaulteten isol. PD ab (nach sync_vspace). SchedOps+KernelSched
  map_frame/unmap_frame. ISO_FAULTS zaehlt. Granularitaet 2 MiB (1 L2-Block/Frame;
  4 KiB = offen). Demo vmm-Check: vmm_probe MAPpt G, schreibt/liest 0xDEADBEEF, SIGNAL,
  UNMAPpt, liest erneut -> Fault.
- Shared-Memory-IPC (FERTIG, ext-13): EIN Frame F (1 MemoryCap, 2 Kind-Caps) in ZWEI
  isolierte VSpaces gemappt (identity, gleiche Adresse); Writer schreibt+SIGNAL,
  Reader WAIT+liest -> zero-copy ueber Isolationsgrenze, cap-gewaehrt. Bug-Fix:
  SIGNAL-Cap-Badge muss !=0 sein (sonst pending|=0, verlorenes Signal vor WAIT).
- Natives Code-Laden (FERTIG, ext-14): system::spawn_isolated_native kopiert Code in
  privaten Frame, cpu::sync_code_range (dc cvau + ic ivau), mmu::vspace_map_code_block
  (EL0-RX, W^X), Thread Entry=privater Code-Frame (nicht geteilte .user_text). Beweis
  ueber Entry-Adresse. Weg-C-Reihe ext-11..14 komplett = perfekte Prozesstrennung
  (nur IPC) fuer untrusted/native PDs neben der SAS-Spur. Manager-DBG-Diagnose
  `DBG pending` (nach ~25s falls Bericht ausbleibt) bestaetigt: keine echten Hangs,
  Truncation nur unter Host-Last.
  (gewaehlt fuer isoliert), B=SFI/WASM (SAS-erhaltend), C=Hybrid (umgesetzt).
- 4-KiB-Seiten (FERTIG, ext-15): echte L3-Page-Mappings (UserPerm Rw/Rx/Ro),
  vspace_map_page/unmap_page/collect_l3s; 2 MiB bleibt Fastpath. Feingranulares W^X,
  Guard Pages, gemischte RX/RO/RW. Demo pages4k-Check.
- Ressourcen-/Teardown-Invarianten (FERTIG, ext-16, Haertung Ziel 2): destroy_isolated
  gibt ALLES frei. 2 echte Leaks gefixt: ASID-Vergabe monoton->Free-List ueber VSPACES;
  spawn_isolated[_native] riefen record_user_kstack NICHT -> kstack-Pool-Leak. Churn-Test:
  2000 spawn/destroy-Zyklen, MEM/TCB/ASID/kstack kehren exakt zur Baseline (churn-Check).
- MCS Scheduling Contexts (FERTIG, ext-17, Haertung Ziel 3): Tcb-Felder budget/period/
  remaining/next_refill/depleted; on_tick(core,frame,tick:bool) (echter Tick verbraucht
  Budget+Refill-Scan, YIELD nicht); Erschoepfung->deplanen bis Refill nach Periode.
  Autoritaet NUR ueber Cap: ObjectKind::SchedContext{budget,period} + install_sched_context;
  system::install_sched_context_cap + bind_sched_context (liest Budget aus Cap, braucht
  WRITE; CAPS<SCHEDS). budget_stats()=(depletions,refills). Demo mcs-Check: budgetierter
  vs. greedy Thread auf MCS_CORE=5, budgetiert garantiert >0 aber ~8x gedrosselt.
  Offen/optional: Budget-Donation ueber IPC, Sporadic Server (mehrere Refill-Slots).
- Sicherheits-Audit (FERTIG, ext-18): feindliche Analyse aller Subsysteme, 2 reale Bugs
  gefunden+behoben+regressionsgetestet (Tests scheitern ohne Fix): (1) IPC recv/call
  `.expect(frame_of)` auf gekilltem, in senders/receivers blockiertem Thread -> KERNEL
  PANIK (Fix: tote Eintraege ueberspringen; Test `stale`); (2) sched set_budget/
  bind_sched_context auf ERSCHOEPFTEN Thread loescht depleted ohne enqueue -> Thread
  gestrandet (Fix: war_depleted -> enqueue_ready; Test `strand`). Helfer system::
  kill_local. Verifiziert SICHER: Page-Table-Init (alle Eintraege gesetzt), EL0/EL1-
  Bits, Cap-Rechte/Generation/Bounds/Kind im Dispatch, Scheduler-Double-Enqueue (F3)
  widerlegt (per-Kern-Lock serialisiert). RESTRISIKEN (dokumentiert, nicht gefixt):
  kein Zero-on-Free (Info-Leak via Frame-Reuse; TCG-memset-Kosten), single caller/
  waiter je EP/Ntfn (Verdraengter haengt), Hot-Reload nicht atomar ggue in-flight CALL,
  SIGNAL badge=0 -> verlorener Wakeup, MAP nur cap- nicht spatial-gated (by-design),
  u32-gen-wrap, exit/block-`.expect` bei leerer Queue (nur trusted idle).
- Generativer Fuzzer (FERTIG, ext-19, Audit Bereich H): deterministischer In-Kernel-
  Fuzzer (fester Seed). Treiber-Thread core 0. Phase 1 (24 Epochen x 50 = 1200 oracle-
  gepruefte Ops): zufaellige Cap-CDT (install/copy/mint/move/delete/revoke), Frame-Map/
  Unmap, Thread-Spawn/Kill/Destroy, SchedContext-Bind; nach jeder Epoche Teardown +
  ORACLE (MEM/TCB/VSpace/kstack/cap_used_slots/cap_used_objects zurueck zur Baseline).
  Phase 2: balancierte Cap/MEM-Churn parallel auf 8 Kernen (~9k-29k Iter, SMP-Lock-
  Kontention) + Baseline-Check. Oracle-sensitiv (gepflanzter Leak -> FAILURES). Helfer:
  CapSpace::used_slots/used_objects, system::cap_used_slots/_objects/unmap_into_thread.
  IPC-Rendezvous (CALL/RECV/WAIT) NICHT gefuzzt (blockierend, kein Einzeltreiber) ->
  dafuer ipc/xipc/shm/stale-Tests; EP/Ntfn haben keinen Destroy-Pfad (nicht balancierbar).
- IPC-State-Machine-Fuzzer (FERTIG, ext-20, Audit Bereich H Teil 2): HAERTUNG eager-purge
  -- gekillte, in senders/receivers/caller/waiter blockierte Threads werden beim Tod aus
  ALLEN IPC-Queues entfernt (system::purge_ipc_queues aus allen Todespfaden; Endpoint/
  Notification::purge_thread). Vorher nur lazy-skip (ext-18) -> Corpse-Fill-DoS bei
  QCAP=32. Fuzzer: 10 nebenlaeufige Aktoren (2 Server/4 Client/Notifier/2 Waiter/1 Fast-
  Signaller) cores 1..7, Controller core 0 injiziert KILL(cross-core)/Reload/MCS-Bind/
  Cap-Churn zu unguenstigen Zeiten + respawnt. Oracle/Epoche (system::ipc_audit): keine
  toten/dup TCBs in EP/Ntfn-Queues, Scheduler::audit (bitmap/dup/dead/lost/blocked).
  Teardown STOP+kill_remote -> Ressourcen-Baseline. Sensitivitaet: ohne purge -> Oracle
  Anomalie=3 (toter Waiter) -> FAILURES. Neue Primitive: kill_remote (cross-core kill +
  IPI), reap_core, thread_alive, ipc_audit, *::audit, cap_used_slots/_objects.
  IPC-Durchsatz TCG-limitiert (globale CAPS-Sperre serialisiert Cap-Lookups).
- Reply-Liveness + CDT-Property-Oracle (FERTIG, ext-21):
  (A) FIX: stirbt der Server (Reply-Owner) nach RECV vor REPLY, wird der blockierte
  CALL-Aufrufer mit ERR_SERVER_GONE (neuer ABI-Code 5) entblockt statt ewig zu haengen.
  Endpoint.reply_owner (gesetzt in call/recv, konsumiert in reply); Endpoint::owner_died;
  purge_ipc_queues sammelt verwaiste Caller + unblock_with_error aus allen Todespfaden.
  Test `rgone` (Server RECVt+parkt, Manager killt, Client muss ERR_SERVER_GONE sehen);
  Sensitivitaet: owner_died deaktiviert -> Client haengt, rgone=false. KEIN voller
  first-class Reply-Cap (eigene ObjectKind im Cspace / per-PD-Reply-Slot / Budget-
  Donation) -> dokumentierter naechster Schritt; Reply-nach-reiner-Cap-Revocation (ohne
  Thread-Tod) bleibt Restrisiko. (C) CapSpace::audit_cdt (system::cap_audit_cdt):
  Property-Oracle -- keine verlorenen Objekte / negativen Refcounts / toten CDT-Knoten /
  Ableitung auf fremdes Objekt / Zyklen. In captest (3 Checks) + Ressourcen-Fuzzer
  (mid-epoch 40+, post-teardown 20+) + IPC-Fuzzer (ipc_audit 20+). Sensitivitaet: copy
  ohne refcount++ -> Code 2 erkannt. (B) Langzeit-Stress: die zwei Fuzzer + cross-core/
  MCS/reload-Tests SIND die Stress-Harness mit Per-Epoche-Oracles + festem Seed; skaliert
  ueber die Epochen-Konstanten (FUZZ_EPOCHS/IPCF_EPOCHS/FUZZ_SMP_ITERS); echte Mio-Ops/
  Stunden nur Konstanten-Frage, durch TCG-Wall-Clock begrenzt. test-qemu.sh = 32 Checks.
  FOLGE-HAERTUNG (ext-21 erweitert): #2 Reload-/Quiesce-Reply-Liveness -- system::
  endpoint_quiesce_owner (in reload_swap), entblockt ausstehenden Caller mit
  ERR_SERVER_GONE wenn Reply-Owner via Reload zurueckgezogen wird (Server lebt); rgone
  Runde 2 prueft es. #5 VMM-Property-Walker -- mmu::vspace_wx_ok / system::vspace_audit
  (W^X: keine EL0-Seite schreibbar+ausfuehrbar + PT-Struktur), im Ressourcen-Fuzzer
  mid-epoch (Code 60+); sensitivitaetsgeprueft (UXN aus user_block -> erkannt). #6
  (64-bit-gen gegen ABA) bewusst NICHT gemacht (2^32 unerreichbar, Repack-Risiko).
  FOLGE-HAERTUNG 2 (Empfehlung #1, FERTIG): #1a Budget-Donation (Commit aeed8e3) --
  intra-core CALL teilt den Scheduling-Context des Aufrufers (Tcb.sc_donor/sc_donee,
  switch_to setzt Links); on_tick belastet das Aufrufer-Konto fuer Server-Arbeit,
  Refill leitet auf Donee um, end_donation loest die Spende bei reply(). Inert bei
  unbeschr. Budget (budget=0 -> keine Belastung -> Altpfade unveraendert). Test ddon
  (14 Konto-Erschoepfungen, >=6 erwartet); Sensitivitaet: switch_to-Spendenlink raus
  -> erkannt. Cross-core CALL spendet nicht (kein switch_to). #1b First-class Reply-Cap
  (Commit 2d50d42) -- ObjectKind::Reply{ep,caller} ist eigene Capability, pro CALL
  gepraegt (reply_cap_for/install_reply), in CDT/Refcount/Finalisierung integriert.
  Revocation: cap_delete/cap_revoke finalisieren -> ReplyFinal-Kollektor sammelt
  (ep,caller); da Cap-Schicht den Scheduler NICHT erreichen darf (CAPS<EPS<SCHEDS),
  bricht der Kernel die Calls erst NACH Lock-Freigabe via abort_finalized_replies ab
  -> ausstehender CALL kehrt mit ERR_SERVER_GONE zurueck. Test rcap (Reply-Cap fuer
  ausstehenden Call praegen+loeschen -> Ergebnis=5); Sensitivitaet: Finalisierungs-Push
  in delete_leaf deaktiviert -> Client haengt -> == FAILURES ==. #1c Reply-Cap-Server-
  Migration (Commit d9737c9) -- Endpoint::migrate_owner / system::endpoint_migrate_owner
  reiht den wartenden Aufrufer beim Hot-Reload WIEDER als Sender ein + loescht caller/
  reply_owner -> die naechste RECV-Instanz (v2) uebernimmt dieselbe Nachricht aus dem
  weiterhin blockierten Aufrufer-Frame und wird neuer Reply-Owner -> v2 schliesst den
  Call ab, statt den Client mit ERR_SERVER_GONE abzubrechen (Reply-Cap ueberlebt den
  Server-Wechsel). Reine EPS-Operation (kein Entblocken -> kein SCHEDS-Lock). NPDS 48->64
  (3 zusaetzliche statische Test-PDs v1/v2/client haetten sonst den Pool erschoepft ->
  ipcf-Setup paniert "ipcf pd"). Test rmig (v1 empfaengt+parkt, Manager migriert auf v2,
  Client-CALL kehrt mit OK + 5*7=35 zurueck); Sensitivitaet: migrate durch quiesce
  ersetzt -> Client abgebrochen -> == FAILURES ==. Damit Empfehlung #1 vollstaendig.
  FOLGE-HAERTUNG 3 (Empfehlung #3, FERTIG, Commit b649b4f): CAPS-Reader-Writer-Lock.
  Die globale CAPS-Sperre serialisierte ALLE Cap-Lookups; der heisse IPC-Pfad ist aber
  rein lesend (slot->CapPtr->(kind,rights,badge)). NEU: sel4lake-sync::RwSpinLock<T>
  (writer-bevorzugend, keine Schreiber-Aushungerung; Writer-Drop via fetch_and(!WRITER)
  statt store(0) -> kein Unterlauf bei transient optimistischen Lesern; RwReadGuard
  Deref, RwWriteGuard Deref+DerefMut). CAPS = RwSpinLock<Caps>; 21 Aufrufstellen
  klassifiziert (6 read: used_slots/used_objects/audit_cdt/inspect/bind_sched_context-
  lookup/ipc_audit; 15 write: install*/copy/mint/move/delete/revoke/reply_cap_for/PD-Ops).
  microkit::dispatch loest unter read() auf (alle Pfade ausser REPLY+GRANT brauchen danach
  kein CAPS); REPLY+GRANT (einzige Dispatch-Mutation, grant_cap) nimmt write() VOR dem
  EPS-Lock -> CAPS<EPS bleibt gewahrt, KEIN read->write-Upgrade. Sperrordnung sonst
  unveraendert (read/write an derselben Position wie zuvor lock()). Test caplk: 2 Sonden
  auf Kern 1+2 halten via zweiphasiger Barriere GLEICHZEITIG den CAPS-Read-Lock ->
  beobachteter Hoechststand gleichzeitiger Leser=2 (mit exklusivem Lock strukturell 1).
  Sonden brauchen keine PD (rufen system::caps_read_concurrency_probe direkt + PARK),
  spawn_on_core nutzt kein CAPS -> sauberes Fenster; gegate auf rmig+fuzz+ipcfuzz fertig.
  Sensitivitaet: Sonde nimmt write() statt read() -> serialisiert -> Hoechststand 1 ->
  == FAILURES ==. Korrektheit zusaetzlich via 8-Kern-Fuzzer+CDT-Oracle unter neuem Lock.
  WICHTIG: Durchsatz-Gewinn selbst nur auf echter Multicore-HW messbar (single-threaded
  TCG serialisiert Kerne) -> verknuepft mit offenem #4; Read-Parallelitaet hier dennoch
  deterministisch belegt. Damit Audit-Empfehlungen #1/#2/#3/#5 erledigt; offen nur #4
  (echter Mehrstunden-Lauf auf realer HW/KVM + vereinheitlichter Mega-Fuzzer).
  WICHTIG: test-qemu.sh ist unter konkurrierender HOST-LAST (z.B. nativer Paketbau)
  intermittierend flaky (TCG-Starvation, KEIN Kernel-Bug -> bei freier CPU 36/36 in ~5s).
- AUSBAUSTUFE ext-22: DREI SICHERHEITSDOMAENEN (FERTIG, P1-P6, 42/42 ALL PASS).
  ADR docs/adr/0007-security-domains.md (Variante B), Bericht
  docs/phase-reports/ext-22-security-domains.md. Modell: jeder Pd traegt einen
  UNVERAENDERLICHEN Domain-Tag { TrustedSas | HardwareLand | UserLand } (nur bei
  Erzeugung setzbar, kein Setter; EMPTY-Default=TrustedSas -> Altbestand unveraendert).
  GRUNDSATZ: Domaene = POLICY (erlaubte Cap-Typen + Kommunikationsbeziehungen),
  VSpace = nur Isolationsmechanismus. Regeln: TrustedSas -> keine HW-Caps (Treiber-/
  Protokoll-Logik, KEIN unsafe); HardwareLand -> HW-Caps (Mmio/Irq) + GENAU EIN
  Trusted-Partner, kleiner unsafe-HW-Kern; UserLand -> keine HW-Caps, keine direkte
  Kommunikation mit HardwareLand. Zentraler Enforcement-Punkt
  Caps::install_cap_checked; Oracle Caps::domain_audit (Codes 1=HW-Cap ausserhalb
  HardwareLand, 2=PdControl ausserhalb TrustedSas, 3=untrusted PD mit globalem LIVE-
  Thread, 4=HardwareLand-Backend ohne TrustedSas-Partner, 5=Backend haelt Fremd-Comm-
  Cap) -> in ipc_audit (Code 30+) aggregiert; plus vspace_device_wx_ok (W^X Device-Seiten).
  Neue ObjectKinds: PdControl{pd}, Mmio{phys,len}, Irq{intid}; kind_is_hardware =
  matches!(Mmio|Irq) (generische Kategorie, DmaCap spaeter ohne ABI-Aenderung
  einhaengbar, NICHT implementiert). WICHTIG: delete_leaf von Mmio/Irq/PdControl ruft
  NIEMALS alloc.free_region (Geraet != RAM) -> sonst Allokator-Korruption (hwfuzz
  total_free-Baseline greift, sensitivitaetsbelegt).
  P2: SYS_PDCTL=12 + pdctl::{START,STOP,PAUSE,RESUME}, gegated auf PdControl-Cap +
  caller==TrustedSas + target==UserLand (Scheduler::pause + on_tick-Fix).
  P3: PAARWEISER KANAL create_hardware_backend(partner, backend_id, ep, ntfn) setzt
  partner+chan_ep+chan_ntfn am Pd (kernelseitig unveraenderlich), Kardinalitaet
  1 Trusted : N HardwareLand, nie umgekehrt; Backend haelt NUR seine eigenen Kanal-
  Comm-Caps (install_cap_checked erzwingt). P4: GENERISCHE Device-MMIO-Infra
  mmu::vspace_map_device(l1,phys,len,ro,alloc) (KEINE RTC-Annahmen!) splittet den
  GiB-0-1-GiB-Device-Block in Device-L2/L3, mappt EL0-Device (Device-nGnRnE, PXN|UXN,
  nG) nur fuer die Zielseiten, Rest bleibt EL1 (Kernel-UART/GIC erhalten);
  + vspace_collect_device_tables (Teardown) + vspace_device_wx_ok (Audit). RTC = erste
  Referenz-Instanz (PL031, QEMU virt base 0x0901_0000, RTC_DR off 0x000, IRQ SPI 2 =
  INTID 34): Backend liest RTC_DR, sendet NUR ueber den Partner-Kanal. P5: IRQ-Caps +
  gic::route_spi (GICD_ITARGETSR, fehlte!) + mask_intid (GICD_ICENABLER); DEFERRED-IRQ-
  ZUSTELLUNG deadlock-frei: irq_hook im IRQ-Kontext LOCK-FREI (Atomics + GIC-Mask + EOI,
  setzt pending); reschedule() ruft drain_pending_irqs() VOR dem SCHEDS-Lock ->
  signal_from_kernel auf die gebundene Notification (Lock-Ordnung NTFNS<SCHEDS, IRQs im
  Trap maskiert -> keine Reentranz; DAIF erst beim eret). P6: hwfuzz-Fuzzer (32 Epochen
  HW-/Management-Cap-Churn gegen Domaenen-Policy + CDT/VSpace-Oracle + total_free-
  Baseline). NPDS jetzt 96.
- AUSBAUSTUFE ext-23: DMA-CAPABILITIES mit SMMUv3 (FERTIG, D0-D5, 47/47 ALL PASS).
  ADR docs/adr/0008-dma-smmu.md, Bericht docs/phase-reports/ext-23-dma-smmu.md.
  GRUNDSATZ: DMA-Mechanismus (DmaCap) entkoppelt vom Enforcement-Treiber (DmaEnforcer-
  Trait); "SMMUv3 ist die Implementierung, nicht die Architektur". ObjectKind::Dma{phys,
  len} = kernel-ausgeschnittene KONTIGUIERLICHE RAM-Region (alloc_dma_region, GiB 1),
  nur HardwareLand (kind_is_hardware schliesst Dma ein). ANDERS als Mmio/Irq ist DMA
  echtes RAM -> delete_leaf gibt es via free_region frei (DMA-use-after-free-sicher durch
  Revoke-Reihenfolge enforcer.disable_dma -> Unmap -> free). mmu::vspace_map_dma = EL0-RW
  Normal-Non-Cacheable (neuer MAIR-Index 2), GiB 1, bestehende L3-Maschinerie. dma_audit
  (Bounds/Disjunktheit, in ipc_audit Code 40+). DmaEnforcer-Trait (init/enable_dma/
  disable_dma/audit) in system.rs; SmmuV3Enforcer (einzige Impl) kapselt ALLES SMMU-Wissen
  (+ hal::smmu); ein kuenftiger NullIommuEnforcer ist Drop-in ohne Aenderung am oeffentl.
  Pfad. Neue HAL-Module: pcie.rs (ECAM-Enum @0x40_1000_0000, BAR-Zuweisung, PCIe-Bridge-
  Enum/Root-Ports, RID=StreamID), smmu.rs (@0x0905_0000: Command-/Event-Queue + lineare
  Stream-Tabelle, Default-Abort, CR0, CMD_SYNC; STE->CD->Stage-1 bildet NUR die DmaCap-
  Region ab), virtio.rs (virtio-pci-modern-RNG-Treiber: Handshake + Split-Virtqueue in der
  DmaCap-Region). mmu::map_device_block_global haengt ECAM @256 GiB global ein.
  ZWEISTUFIGE DURCHSETZUNG (Nutzer-Entscheidung nach QEMU-Befund): Level 1 = Software-
  Disziplin (system::dma_addr_in_region — der Treiber validiert JEDE Geraete-DMA-Adresse
  gegen die DmaCap VOR dem Programmieren; demonstrierbar in QEMU); Level 2 = SMMUv3-Stage-1
  als Hardware-Backstop (greift auf realer HW, z.B. STM32MP25).
  WICHTIGER QEMU-BEFUND (per smmu-Trace belegt, 8 Laeufe): QEMU 11 routet den DMA
  EMULIERTER Geraete NICHT durch smmuv3_translate (null translate/ptw-Events, selbst mit
  V=0-STE, integriert wie hinter pcie-root-port) -> die SMMU-Hardware-Erzwingung ist unter
  QEMU fuer emulierte Geraete NICHT beobachtbar (cj_smmu_enforced=false, ehrlich); die
  Stage-1-STE ist dennoch korrekt installiert + greift auf realer HW. test-qemu.sh-Maschine:
  -machine virt,iommu=smmuv3 -net none -device pcie-root-port,id=rp0 -device
  virtio-rng-pci,bus=rp0 (Endpunkt MUSS hinter Root-Port; integrierte Bus-0-Endpunkte
  umgehen die SMMU). Echter Bus-Master-DMA (64 Zufallsbytes) + hwfuzz-Dma-Churn (balanciert).
  FALLE: `if let Some(x) = MEM.lock().alloc(..)` haelt den MEM-Lock ueber den ganzen Block
  (Rust-Temporary) -> free_region darin = Selbst-Deadlock; Lock vor dem if-let freigeben.
- AUSBAUSTUFE ext-24: GENERISCHE DMA-INFRASTRUKTUR (FERTIG, E0-E5, 48/48 ALL PASS).
  ADR docs/adr/0009-generic-dma-infrastructure.md, Bericht docs/phase-reports/
  ext-24-generic-dma.md. ADDITIV (keine API gebrochen; ext-23 + Tests gelten weiter).
  (1) Richtung + Kohaerenz als DmaCap-ATTRIBUTE (cap-rein): ObjectKind::Dma{phys,len,dir,
  coherence} + DmaDir{DeviceRead|DeviceWrite|Bidirectional} + DmaCoherence{Coherent|
  NonCoherent}; install_dma bleibt (Defaults Bidir/NonCoherent), + install_dma_ex/
  install_dma_cap_ex. Richtungsminimales SMMU-AP: DeviceRead -> Stage-1 READ-ONLY
  (schreibgeschuetzt gegen das Geraet, Sicherheitsgewinn); Kohaerenz -> Normal-WB
  (Coherent) vs NC. (2) DmaHandle{iova,len}: gerätesichtbare Adresse von der PA entkoppelt
  (iova=PA heute; spaeter Bounce/Remap/SG-Kompaktierung ohne API-Bruch). (3) DmaContext
  (system.rs, ersetzt die ext-23-1:1-Bindung): je StreamID-GRUPPE 1 STE-Gruppe -> 1 CD ->
  1 Stage-1-Tabelle mit MEHREREN Regionen; dma_attach/dma_detach (additiv), dma_group_add
  (N StreamIDs/Kontext = IOMMU-Stream-Gruppen); enable_dma/disable_dma darauf refactored
  (rueckwaertskompatibel). hal::smmu: stage1_create/map_region/unmap_region/read_leaf +
  tlbi_sync (ersetzt build_stage1_identity). (4) Scatter-Gather: DmaSgEntry{handle,offset,
  len} + dma_sg_validate (Level-1, jedes Segment in angehaengter Region); Geraete-Deskriptor
  bleibt im Backend. (5) DmaPool: Bump-Sub-Allokator ueber eine angehaengte Region.
  (6) Cache-Maintenance: hal::mmu::dma_cache_clean/invalidate + system::dma_prepare/
  dma_complete (richtungs-abhaengig); vspace_map_dma kohaerenz-aware. Test `dmagen`
  (strukturell: Stage-1-Leaves zurueckgelesen fuer AP/Attr + Level-1 funktional); hwfuzz
  churnt zusaetzlich dir/coherence/attach/SG. WIE ext-23: SMMU-HW-Erzwingung unter QEMU
  fuer emulierte Geraete nicht beobachtbar -> strukturell + Level-1 verifiziert.
- AUSBAUSTUFE ext-25: REGION-RUNTIME + PROZESS-LOKALER HYBRID-HEAP (FERTIG, R0-R4,
  49/49 ALL PASS). ADR docs/adr/0010-region-runtime-and-process-heap.md, Bericht
  docs/phase-reports/ext-25-region-runtime-process-heap.md. ZIEL: echter dynamischer
  Heap (Box/Vec/BTreeMap) fuer safe-Rust-Trusted-SAS-Prozesse auf REALEN Physadressen,
  ohne unsafe im App-Code. NEUES CRATE crates/sel4lake-region = DIE EINZIGE STELLE mit
  Speicher-unsafe: Region (cap-besessen, haelt MemoryCap + RegionTag{id,Purpose}),
  RegionView<'a> (nur sichere Ops: get<T:Pod>/set/copy_from/copy_to/fill + scoped
  with_bytes(|&mut [u8]| ...) das den Slice NICHT entkommen laesst + split_at/subview).
  KEIN oeffentlicher MemoryCap->&mut [u8]-Wrapper. heap::RegionSource (grow/shrink:
  request/release) + Heap<S> = HYBRID-ALLOKATOR (Groessenklassen-Slabs 16..2048 mit
  intrusiven Free-Listen + Bump-Carving; dedizierte Regionen fuer Large-Allok), impl
  core::alloc::Allocator, arbeitet auf RegionViews, Drop gibt alle Regionen zurueck.
  Multi-Region von Anfang an (ein Prozess = Menge von Regionen, nicht "ein Heap").
  ARCHITEKTURWAHL nach Vergleich: Hybrid (Slabs+Bump) — klassischer Free-List-Malloc
  verworfen (SAS hat KEINE Kompaktierung -> dauerhafte Fragmentierung + schwerste
  Verifikation). kernel: extern crate alloc + allocator_api/btreemap_alloc;
  KernelRegionSource (system.rs, grow/shrink ueber MEM); NoGlobalHeap als
  #[global_allocator]-WAECHTER (paniert -> prozess-lokale Heaps via *_in(&heap) Pflicht,
  kein impliziter globaler Heap). Test `sasheap` (run_sasheap in threads.rs): echte
  Box/Vec(4096,Realloc/grow)/BTreeMap(256)/Large-Alloc(16KiB,dedizierte Region)/Drop/
  Balance, Testcode 100% SAFE. WICHTIG: SAS = SIP-Modell (language-based isolation),
  no-unsafe-App + bugloser Compiler garantiert, dass ein Prozess nur seinen eigenen
  RAM (Region-Menge) erreicht, OHNE MMU. nightly-Features im Kernel: allocator_api +
  btreemap_alloc.
- KONSOLIDIERUNGSPHASE ext-22..ext-25 (FERTIG, K1-K6 + O-A/B/C, 49/49 ALL PASS).
  Bericht docs/phase-reports/consolidation-ext22-25.md, Referenz docs/invariants.md.
  Befund: Schichtung tragfaehig; Reibung weil DMA-Pfad (ext-23/24) VOR der Region-
  Runtime (ext-25) entstand. Kleine strukturelle Begradigungen (KEINE Features):
  K1=docs/invariants.md (Sperrordnung-Rang R0 CAPS..R4 MEM innerster, Revoke-Ordnung,
  Region-Balance, RegionView/Pod-Vertrag, SMMU-QEMU, Audit-Katalog) + dma_audit Code 4
  (keine SMMU-gemappte Region ueberlappt freies RAM = use-after-free-Audit, via
  PhysAllocator::overlaps_free); K4=kanonisches region_contains (Bounds-Formel 3x->1);
  K6=map_region_into_thread(MappingKind{Device{ro},Dma{coherent}}) ersetzt
  map_mmio/map_dma/_ex; O-C=Caps::dma_audit->dma_bounds_audit; K5=Test-/Telemetrie-API
  (dma_ctx_*, smmu_*, peek_dma_words, dma_audit_with_floor) in pub(crate) mod testsupport;
  K2=DmaPool ENTFERNT (redundant zum SG-Pfad DmaSgEntry+dma_sg_validate); O-A=DmaEnforcer
  enable_dma/disable_dma->attach/detach; K3=alloc_dma_region carvt ueber KernelRegionSource
  (Purpose::Dma); O-B=Hot-Reload-Zustand als Region (Purpose::HotReloadState,
  system::CS_STATE_REGION + hotreload_state_get/set ueber RegionView, CS_STATE_BASE weg).
  BEWUSST NICHT: DMA-Puffer voll auf Region-Besitz (DmaCap-(phys,len)-Modell ist 2.
  legitimes Besitzmodell, Umstellung waere Feature-Scale; in invariants.md §3 begruendet).
- KRITISCHER BUGFIX (waehrend ext-26): VORBESTEHENDER intermittierender SMP-Deadlock
  (~27%/Lauf), die ganze SMP-/MCS- Aera als "Host-Last-Flakiness" fehlgedeutet (Nutzer
  bemerkte: CPU idle <6% -> "muss was anderes sein"). Root Cause: SpinLock ist FIFO-
  TICKET-Lock + maskierte KEINE IRQs; SCHEDS[core]/NTFNS[] werden im Timer-Reschedule
  UND in Thread/Idle-Kontext (idle->reap_core, Syscalls, Fuzzer-kill_remote) genommen
  -> Timer-Tick zieht zweites Ticket auf gehaltenen Lock -> Halter im IRQ-Handler
  suspendiert -> Deadlock. Forensik: QEMU-Monitor info-registers-a aller Kerne +
  addr2line; auf ext-25 reproduziert (nicht durch juengere Aenderungen). FIX: IRQ-
  sichere SpinLocks (crates/sel4lake-sync: DAIF beim lock() sichern+maskieren, beim
  Drop restaurieren; nesting-sicher; RwSpinLock/CAPS unberuehrt, nicht im IRQ-Pfad).
  Verifikation: tools/hang-stress.sh -> 30/30 (vorher 4/15). invariants.md §1a.
  WICHTIG: "Flakiness" frueherer Phasen war IMMER dieser Bug, nie der Host.
- AUSBAUSTUFE ext-26: GENERISCHER BINARY-LOADER (L0-L6 FERTIG, 55/55). ADR 0011,
  Plan + Bericht docs/phase-reports/ext-26-binary-loader*.md. ZIEL: Uebergang von
  EINGEBETTETEN Prozessen (.user_text im Kernel-Image) zu EXTERN gebauten/geladenen.
  Entscheidung (Nutzer): In-Kernel-Loader CAP-GEGATET (Arch A, nicht Userspace-Server),
  Minimal-ELF64 (nur PT_LOAD, keine Relok). NEUES CRATE sel4lake-loader
  (#![forbid(unsafe_code)], host-getestet): archive::Archive (Boot-Archiv-Parser) +
  elf::ElfImage (ET_EXEC/AArch64/PT_LOAD) + quellen-agnostischer Program-Deskriptor
  (program_id/version/domain/hash/elf/manifest). Boot-Delivery: QEMU -device loader ->
  reserviertes 16-MiB-RAM-Fenster MOD_BASE=0x1_3F00_0000 (vom PhysAllocator ausgenommen,
  init_mem). tools/mkarchive.py (Host). EXTERNE Programme: programs/ = EIGENER Cargo-
  Workspace (aarch64-sel4lake-user.json + user.ld, festgelinkt VA 0x4100_0000, W^X-
  PT_LOAD) + SDK libsel4lake (Syscall-Stubs) + hello. Laden: hal/mmu vspace_map_page_at
  (va->pa nicht-identity), sched spawn_user_at (EL0-SP getrennt von Reap-Region),
  system::load_elf (Segmente kopieren=EINZIGE unsafe-Stelle, W^X mappen, PD in Domaene,
  Cap-Endowment via install_cap_checked, EL0-Spawn IRQ-maskiert), loader::load_image
  (kleine quellen-agnostische API). Test `load`: extern gebautes hello laeuft + signalt
  HELLO_BADGE. 4 Nutzer-Review-Verfeinerungen eingebaut (kleine API, ELF-Safe-Rust,
  Quelle austauschbar, stabile program_id). L2 FERTIG: Laden zur LAUFZEIT cap-gegatet.
  ObjectKind::Loader{source} (Autoritaets-Cap, nur TrustedSAS wie PdControl) +
  sys::LOAD=13. microkit::dispatch SYS_LOAD-Case: Loader-Cap+WRITE pruefen, Caller-Cap
  (x3) KOPIEREN (CDT), CAPS freigeben, dann Kernel-Loader-Callback (fn-Pointer-Param
  load_by_index). Geladener Prozess erhaelt NUR delegierte Caps. load_elf nutzt
  hal::cpu::local_irq_save/restore (korrekt im Syscall-Trap UND In-Kernel). Test
  sysload: TrustedSAS-Caller laedt hello via Syscall + delegiert Notification-Cap,
  Negativ ohne Loader-Cap -> ERR_BADCAP. hello macht jetzt exit() (gibt Pool-Slot
  zurueck; sonst FAILt reclaim-Test free>=4).
  L3 FERTIG: load_elf aufgeteilt in load_into_pd (Laden in VOR-erstellte PD) + load_elf-
  Wrapper (erzeugt UserLand-PD). HardwareLand-Laden: hwhello in eine Backend-PD (Partner+
  Kanal via create_hardware_backend; Kanal-Cap-Policy gilt). KRITISCHE NUTZER-DIREKTIVE
  ("TrustedSAS soll auch in EL0"): ALLE geladenen Prozesse laufen EL0-ISOLIERT (load_into_pd
  spawnt STETS EL0) -> ein geladenes TrustedSAS-Programm ist EL0-isoliert (NICHT EL1),
  behaelt nur die TRUST-STUFE (darf PdControl/Loader-Caps halten; domain_audit erlaubt
  isolierte TrustedSAS-PDs). KEIN privileg-basiertes Lade-Verbot mehr; verify_image immer
  true (prog.hash = Integritaets-/Signatur-Hook). load_image: DOMAIN_USERLAND+DOMAIN_TRUSTED
  -> beide EL0; HardwareLand via load_program_into_pd. Die bestehenden EL1-globalen TrustedSAS-
  PDs (in-kernel SAS) bleiben unveraendert. Test loadhw: hwhello in Backend-PD signalt Kanal;
  trusted-x laedt als EL0-isolierte TrustedSAS-PD (domain_audit ok), dann destroy_loaded.
  L4 FERTIG: geladene PT_LOAD-Segment-Frames NICHT cap-getrackt (Eigentum -> VSpace) ->
  Segment-Register LOADED_IMAGES (je ASID), vspace_teardown gibt sie mit frei (sonst Leck).
  destroy_loaded(tid,pd): PD-Caps loeschen + Thread/VSpace/Segmente/Kstack (destroy_isolated)
  + PdTable::free. Test loadstop: laden+abbauen -> MEM/VSpace/Kstack-Baseline (kein Leck).
  L5 FERTIG: loader_audit (ipc_audit Code 60+): kein registriertes Segment ueberlappt FREIES
  RAM (use-after-free). Test loaderfuzz: 8 fehlerhafte ELFs durch vollen load_image-Pfad ->
  alle bei parse abgelehnt, kein Crash/OOB, Ressourcen-Baseline, loader_audit==0. L6 FERTIG:
  programs/ nach Domaene (userland/hardware/trusted), hello->userland/hello, READMEs.
  Stress 12/12 OK nach EL0-TrustedSAS. WICHTIG: kernel-only Cap-Ops aus KEINEM geladenen Prozess
  aufrufbar -> Cap-/CDT-Angriffe bleiben In-Kernel. tools/hang-stress.sh = Deadlock-
  Regressionstest (nach IRQ-Safe-Lock-Fix).
- AUSBAUSTUFE ext-27: ADVERSARIALE EXTERNE TESTDIENSTE (FERTIG, T0-T5, 62/62). ADR 0012 +
  docs/phase-reports/ext-27-adversarial-tests*.md (inkl. Angriffsmatrix). 6 extern gebaute EL0-
  Dienste (2 je Domaene), vom Binary-Loader wie Drittsoftware geladen, greifen Kernel + sich
  GEGENSEITIG ueber alle Domaenen-Kombinationen an -> empirischer Isolationsbeweis von aussen.
  Kernel UNVERAENDERT (nur Testdienste + Idle-Manager-Harness-Schritte). NEUER WORKSPACE tests/
  (sibling zu programs/, eigene Target-Spec/Linker kopiert, dep nur programs/libsel4lake; .gitignore
  /tests/build/). SDK libsel4lake erweitert (result-Codes, pdctl-Sub-Ops, Wrapper map/unmap/pdctl/
  load/kill). Dienste tests/services/{userland,hardware,trusted}/{aggressor,intruder}: aggressor-*
  = Cap-Confusion (leerer Slot/falscher Typ/falsche Rechte -> BADCAP/RIGHTS/BADSYS) + Eskalation
  (PDCTL/LOAD/KILL ohne Cap -> BADCAP); intruder-* = Speicher-Isolation (liest Kernel-RAM 0x4000_0000
  aus EL0 -> Translation-Fault, sichtbar el0-trap FAR=0x40000000 -> Kernel terminiert+ueberlebt).
  PROTOKOLL "Dienst ist sein eigener Richter": jeder Dienst signalisiert SUCCESS-Badge GENAU DANN,
  wenn ALLE Angriffe korrekt abgewiesen -> sonst Timeout=FAIL; fatale Batterie meldet PRE, dann
  Fault (Harness prueft PRE + el0_fault_count++ + Survival). Harness prueft nach JEDER Attacke
  domain/vspace/cap_cdt/loader/ipc_audit==0. KERNERKENNTNISSE: (1) "Trust != Privileg" -- ein
  TrustedSAS-Dienst hat hoechste Cap-Autoritaet, doch OHNE PdControl/Loader-Cap scheitert PDCTL/LOAD/
  KILL als BADCAP (aggressor-t). (2) Speicher-Isolation DOMAENEN-UNABHAENGIG -- auch HardwareLand-
  Backend + TrustedSAS-Dienst faulten auf Kernel-RAM (alle geladen EL0-isoliert). (3) SIGNAL nutzt
  das GEMINTE Cap-Badge (nicht x2) -> Dienste vollstaendig kernel-gesteuert wiederverwendbar.
  T4 cross = 3 Angreifer 3er Domaenen NEBENLAEUFIG (2 Aggressoren melden unabh. SUCCESS, 1 Intruder
  faultet) + kernel-geschuetztes Canary (mem::poke/peek) bit-genau unberuehrt. HardwareLand-Dienste
  via run_hw_service_start (Backend+Partner+Kanal; install_cap_checked erlaubt NUR eigene Kanal-Cap).
  generische run_el0_aggressor/run_el0_intruder (UserLand+TrustedSAS, Domaene aus Archiv). NPDS=96
  traegt geladene PDs (exit/fault gibt PD-Slot NICHT frei, nur destroy_loaded). WICHTIG: hang-stress.sh
  + test-qemu.sh MUESSEN identisches Archiv bauen (tests/-Build + alle Dienste) -- sonst scheitert
  ext-27-Setup -> Selbsttest erreicht NIE SELFTEST COMPLETE = falscher "Hang" (10/10 deterministisch
  + Einzellauf gruen -> Archiv-Mismatch, KEIN Deadlock). intruder-* nutzen 1 dokumentiertes unsafe
  (read_volatile, BEWUSST illegaler Zugriff = Testzweck). Cap-/CDT-Angriffe NICHT ABI-ausdrueckbar
  -> bleiben In-Kernel (captest/fuzz/ipcfuzz). Check-Zahl 55->62.
- FUZZER HINTER FEATURE `kernel-fuzz` (FERTIG, ADR 0013, vor dem Langzeittest). Ziel: Release-Kernel
  enthaelt KEINERLEI Fuzzer-Code; Fuzzer = optionales Verifikationsmodul; AUDITS bleiben IMMER im
  Kernel. 4 Fuzzer (fuzz/ipcfuzz/hwfuzz/loaderfuzz), alle waren in threads.rs. threads.rs ->
  threads/mod.rs + neues Kindmodul threads/fuzz.rs (#[cfg(feature="kernel-fuzz")] mod fuzz; sonst
  Stub-Modul mit identischer No-Op-API). fuzz.rs nutzt `use super::*` -> erreicht alle (auch
  privaten) Harness-Items OHNE pub-Oeffnung (Kernel-API unveraendert). Verschoben: alle Fuzzer-fns/
  -statics/-konstanten + generische Helfer (frand/frand_rights/pick_live/free_slot) + die 4
  Idle-Treiberschritte (jetzt fuzz::drive() je Tick). Kette: genau 2 Nicht-Fuzzer-Tests gaten auf
  Fuzzer-DONE -> Pass-Through-Gates fuzz::loaderfuzz_gate(pred)/fuzzers_gate(pred) (ON: Fuzzer-DONE;
  OFF/Stub: pred=Vorgaenger -> Kette fliesst durch). report()/all_done()/DBG nutzen fuzz::report()/
  all_passed()/dbg_*(). 4 nur-Fuzzer system.rs-fns (cap_used_slots/cap_used_objects/unmap_into_thread/
  free_dma_region) bleiben als API, Dead-Code-Warnung im Release per #[cfg_attr(not(feature="kernel-
  fuzz"),allow(dead_code))] unterdrueckt. test-qemu.sh/hang-stress.sh: Default = Release OHNE Fuzzer
  (Langzeit-/Produktivkonfig); KERNEL_FUZZ=1 baut --features kernel-fuzz; fcheck() prueft die 4
  Fuzzerzeilen nur dann (sonst SKIP). VALIDIERT: mit Feature 62/62 ALL PASS (Fuzzer laufen exakt wie
  zuvor), ohne Feature 58/58 ALL PASS + 4 SKIP (kein Fuzzer-Code, ALLE Audits gruen); beide Builds
  17 Warnungen (Bestand, KEINE Fuzzer-Warnung). WICHTIG (set -u): in fcheck `${KERNEL_FUZZ:-}` statt
  `$KERNEL_FUZZ` (sonst unbound-var-Crash). Extraktion per Python-Skript (Funktionsende=`^}`,
  Kommentar-/Attribut-Kopf hochexpandiert). docs/phase-reports/fuzzers-feature-gate.md.
  DANACH: LANGZEITTEST laeuft auf dem Release-Kernel OHNE Fuzzer.
- BURN-IN / REIFEPRUEFUNG (Nutzer will ZWEI komplementaere Stabilitaetsnachweise vor naechster
  Ausbaustufe). #1 REBOOT/POWER-CYCLE (FERTIG): tools/burn-in.py (Host-Orchestrator, NULL Kernel-
  aenderung; bootet Release-ohne-Fuzzer wiederholt, Kernel faehrt nach SELFTEST COMPLETE per PSCI
  system_off selbst herunter; parst je Lauf Audits/Balancen/Faults, aggregiert, report.md). Lauf:
  8h, 4384 Reboots, 99.886% CLEAN, 5 Hangs (~0.1%). Balancen bit-identisch (freies RAM 4077 MiB,
  domain_audit=0). Bericht docs/phase-reports/burn-in-1-reboot-report.md (inkl. 8-Dim-Analyse: stark
  = Kaltstart-Robustheit + deterministische Per-Lauf-Korrektheit; NICHT belegt = Dauerbetrieb einer
  Instanz, Langzeit-Speicher, Zufallsfolgen, anhaltende Last, reale HW). #2 CONTINUOUS-SOAK (GEPLANT,
  noch NICHT gebaut, Nutzer hat verschoben): EINE Instanz viele Stunden ohne Neustart via neuem
  Feature `soak` (Harness-Treiber, Kernel-Kern byte-identisch) + tools/soak.py; Plan
  docs/phase-reports/burn-in-2-soak-plan.md.
  #2 CONTINUOUS-SOAK JETZT IMPLEMENTIERT + GESTARTET (nach ext-28): Feature `soak` (nicht in default,
  analog kernel-fuzz) -> kernel/src/threads/soak.rs (reiner Harness/Testcode, Kernel-Kern byte-
  identisch). Nach dem Selbsttest faehrt der Idle-Manager statt system_off (cfg-gegatet) eine
  Endlos-Epochenschleife: jede Epoche ruft NUR vorhandene balance-neutrale Ops -- check_loadtrusted_el0
  (Zertifikat-Gate+ELF-Loader+EL0-PD+Teardown), run_dmagen (DMA-Round-Trip/SMMU attach/detach),
  region_churn (direkter PhysAllocator alloc/free) -- dann trust_audit + ext27_audits_ok +
  Ressourcen-Baseline (free RAM/cap_obj/cap_slots) GEGEN DEN SOAK-START; Drift=Anomalie. SOAK-Heartbeat
  (~60s, SOAK_HEARTBEAT_TICKS=6000) mit Uptime/freiem RAM/Cap-Anzahl/Op-Zaehlern/Audit. tools/soak.py:
  baut --features soak + signiertes Archiv (svc-demo als trusted-x id12 + aggressor-t id24 signiert),
  bootet EINE lange QEMU-Instanz (KEIN system_off), liest seriellen Strom KONTINUIERLICH (select),
  verfolgt Kurven (freies RAM=Langzeit-Konsistenz-Indikator), erkennt Anomalien/Panics/FAILURES +
  Heartbeat-Luecken (Hang) -> build/soak/report.md (+state.json/serial.log/progress.log, gitignored).
  VALIDIERT (Heartbeat temporaer 300 Ticks=3s): >10000 Epochen in ~90s, freies RAM BYTE-IDENTISCH
  konstant (4250103808 B = 4053 MiB), cap_obj=49/cap_slots=121 konstant, loads==dmas==Epochen
  (run_dmagen voll wiederholbar -> kein Binding-Leak), faults eingefroren bei 7 (selftest-intruder),
  alle Audits 0, 0 Anomalien. Default- + soak-Build kompilieren beide; Default-Release byte-identisch.
  LANGER LAUF gestartet: tools/soak.py --hours 8 --gap 600 (detached nohup; Host-CPU war frei). BEFUND #1 (5 Hangs) = TEST-/HARNESS-Flakes, KEIN Kernel-
  Kern-Bug (rigoros aus Logs verifiziert: in jeder Anomalie lief der Test durch -> "fertig aber
  OK=false" -> all_done() nie true -> Hang; dmagen=true [auf VRNG_DONE gegatet] beweist Durchlauf,
  DBG-pending beweist core0 lebte). 3 BUGFIXES (committet, Kernel-Kern unveraendert): (1) Harness-
  WATCHDOG in demo_report_then_idle(): wird ein synchroner Test rare DONE-aber-OK=false, blieb
  all_done() ewig false -> Idle-Hang; neu: hal::timer::ticks(0)>6000 (TICK_HZ=100 -> 10ms/Tick ->
  6000=60s; Normal ~6s; ticks ist 100/s NICHT 10/s!) -> report()+system_off, Flake wird gemeldeter
  FAILURE statt Hang. (2) churn SMP-robust: mass globale Zaehler mit nur core-0-IRQs-aus -> reclaim/
  balance-Threads ANDERER Kerne perturbierten sie (Schein-Leak); neu: gegate auf reclaim+balance
  fertig + vor Baseline ueber ALLE Kerne quieszieren (system::reap_core(c) ist kern-uebergreifend!
  reap() nur eigener Kern) bis Zaehler stabil. (3) virtiorng: hal/virtio.rs used-Ring-Poll 2M->50M
  Spins (TCG-Jitter). burn-in.py: watchdog-FAILURE != HANG. Verifiziert: test-qemu 58/58, hang-stress
  20/20, Sanity-Burn-in 300/300 CLEAN. Volle Re-Validierung der 0.1%-Rate bewusst spaeter.
- HARDWARE-PORT STM32MP257F-DK -- M0 "first light" ERREICHT (2026-06-29), alles in ARMTest/ (NICHT im
  git-Repo; Mainline unveraendert). Port-Arbeitsbereich ARMTest/stm32mp25-port/: minimaler Bare-Metal-
  BL33-Payload (aarch64-stm32mp25.json cortex-a35; linker.ld Link 0x84000000; src/main.rs = _start +
  USART2-Treiber (st,stm32h7-uart @0x400e_0000) + Banner; nightly+build-std). Flash-Weg (KEIN TF-A-
  Neubau noetig -- die DDR-PHY-FW ist proprietaerer ST-Blob nur im FIP): aus einem funktionierenden
  OpenSTLinux-Vollabbild (ARMTest/backup-openstl.bin.gz) GPT selbst geparst, fip-a extrahiert, mit
  tools/fip_tool.py das BL33 (U-Boot) gegen SEL4Lake getauscht (7/8 FIP-Komponenten byte-identisch),
  via flash-full-sel4lake.sh auf microSD (Backup-Image + fip-a-Tausch). HW-Boot bestaetigt: TF-A-BL2
  laedt SEL4Lake als image id 5 @0x84000000, BL31->BL33-Sprung, Banner auf USART2 (STLINK-V3-VCP
  115200 8N1, /dev/ttyACM0). WICHTIGE HW-Fakten: Eintritt real **EL1-NS** (SPSR 0x3c5, NICHT EL2 --
  OP-TEE belegt Secure-EL1; gut, SEL4Lake laeuft ohnehin EL1); PSCI-Provider BL31 -> SMP via **smc**
  (Conduit hvc->smc); GICv2 @0x4ac1_0000/0x4ac2_0000; 2x cortex-a35; 4 GiB LPDDR4 @0x80000000.
  Beweis: ARMTest/stm32mp25-port/first-light-SUCCESS.md. Zugriff: Assistent NICHT in disk/uucp-Gruppe
  + kein passwortloses sudo -> Flash/serielle Schritte fuehrt der Nutzer aus. NAECHSTER SCHRITT M1:
  Kernel-Kern (unveraendert) einbinden, HAL-Deltas (RAM_BASE 0x80000000, NUM_CORES=2, USART2-println,
  GIC-Basen, Eintritt EL1, PSCI-smc) -> HW-agnostische Selbsttest-Suite auf echtem cortex-a35.
- FORMALE VERIFIKATION (MEHRSCHICHTIG, FERTIG bis Phase 6 + Kat-A-unsafe; je eigener Commit). Vier
  EBENEN ergaenzen sich (ersetzen sich NICHT): Runtime-Audits (*_audit im Kernel) + Fuzzer (kernel-fuzz)
  + KANI (Tier 1, bounded model checking, Speichersicherheit) + VERUS (Tier 2, deduktiv/SMT, funktionale
  Korrektheit fuer ALLE Zustaende). Strategie/Aufwand: ARMTest/formale-verifikation-aufwand.md +
  unsafe-memory-safety-aufwand.md (ARMTest liegt AUSSERHALB des git-Repos). Pipeline: docs/verification.md.
  WICHTIG build-std-Workaround: workspace .cargo/config erzwingt Custom-Target -> Kani/Verus/Host-Tests
  laufen auf STANDALONE-Kopien in /tmp via tools/{kani,verus}-verify.sh (sonst "duplicate lang item").
  VERUS: Release 0.2026.06.20.911e4e7, braucht rustc 1.96.0, installiert ~/.verus/verus (+z3). Syntax:
  verus!{}, spec/proof fn, requires/ensures/decreases, Seq/Set, =~= (extensionale Gleichheit),
  opt->Some_0 + opt is Some, #[verifier::rlimit(N)]. spec fn in ensures nutzbar, proof fn NICHT.
  TIER-2-PILOT (verus/*.rs, 9 Dateien, 32 verified): cap_cdt_{refcount,tree,structure,acyclic},
  domain_policy, wx_invariant, dma_disjoint, loader_disjoint, trust_keydb -- decken 6 Laufzeit-Audits ab.
  KOMPONENTENWEISE FUNKTIONALE VERIFIKATION (Verification/<komp>/, jede EIGENSTAENDIG verstaendlich OHNE
  Quellcode; Methodik Analyse->Variantenvergleich->ADR->Plan->Impl->Verifikation->Doku->Validierung,
  je Phase committet). ABSTRAKTE, GETREUE Verus-MODELLE werden bewiesen; realer Kernel-Code UNVERAENDERT;
  Modell<->Code-Treue = dokumentierte TCB, abgesichert durch Audits+Fuzzer (mehrschichtig). HAL/SMP/
  Locking/Nebenlaeufigkeit bewusst AUSSERHALB (Hardware-/Concurrency-Vertrauensgrenze; Verus single-
  threaded -> braeuchte Loom/TLA+). 7 Phasen FERTIG (Verus gesamt 90 verified ueber 16 Dateien):
  P1 Capability-System (cap_space.rs, volle cap_inv Codes1-7 + install/copy/mint/delete, 10 verified;
  Acyclicitaet via ghost `rank`=well-founded measure); P2 Loader (load_gate.rs, Cert-Gate-Soundness/
  Revocation/Atomaritaet, 7); P3 Region-Runtime (conservation.rs, kein Leak/keine Doppel-Freigabe/
  Balance, 7); P4 IPC (endpoint.rs, Rendezvous-Ausschluss/kein Verlust-Duplikat/Fortschritt, 6);
  P5 Scheduler (runqueue.rs, Runqueue-Kopplung audit-Codes1-4,7 + MCS-Budget-Schranke/keine
  Aushungerung/Fortschritt, 13); P6 Notifications (notification.rs, kein Signalverlust/genau-einmal-
  Konsum/kein Lost-Wakeup, 8); P7 DMA-Lifetime (dma_revoke.rs, Revoke-Ordnung detach->free =
  dma_audit Code 4, kein DMA-use-after-free, 7). ADRs 0015-0020+0022. OFFENE Ausbaustufen
  (dokumentiert): die HARTE WAND =
  CDT-Reachability (Code 4r) via ghost `sib_pos` -> erst dann move-general/revoke beweisbar (P1);
  Endowment(P2), RegionSource/Zero-Copy(P3), Reply-Caps/Cap-Transfer(P4), Prio-Auswahl/Budget-
  Donation(P5), Multi-Waiter/Binding(P6); durchgaengig Nebenlaeufigkeit (Loom/TLA+).
  KANI-SPEICHERSICHERHEIT der unsafe-Stellen (Verification/unsafe-safety/, ADR 0021, tools/kani-verify.sh
  Ziel `unsafe`, im DEFAULT_TARGETS + CI). Aufwandsanalyse teilt ~200 unsafe in Kat-A (reines RAM, Kani-
  beweisbar) vs Kat-B (Pagetables mmu.rs/MMIO/Assembly = Maschinenmodell, Forschungsklasse, HAL-TCB).
  region(RegionView)+sync VORAB Kani-bewiesen. EIGENSTAENDIGES Artefakt: GETREUE Kopien der Kernel-unsafe-
  Glue (Zeilenverweis), Kani fuehrt die ECHTEN core::ptr-Ops auf modelliertem Puffer aus (CAP=4 +
  kani::unwind fuer schnelles BMC; sonst Timeout). 7 Harnesses/4 Stellen bewiesen: copy_segment
  (system.rs:1214, einzige unsafe-Stelle des Ladepfads) + Slab-Free-Liste (heap.rs:106/175) + Code-Kopie
  (system.rs:1154, Guard code_len<=clen) + alloc_zeroed (system.rs:1676). Kat-A damit VOLLSTAENDIG; Rest
  = Trust-Primitive (mem::peek/poke, MOD-Fenster, DMA-Sentinel: Adressgueltigkeit aus Cap-/Boot-/DMA-
  Vertrag) + MMIO + Funktionszeiger-transmute (HAL) -> als Kontrakt dokumentiert, kein Kani-Ziel.
  CI-GATES: .gitea/workflows/verus.yml + kani.yml + loom.yml (installieren Verus/Kani/loom,
  laufen alle Beweise, scheitern bei Regression). Siehe [[unsafe-allowed-domains]] + [[autonomous-execution]].
- BUG-JAGD (auf Nutzerwunsch, KEIN Agent): systematischer Review aller code-lesbaren Kernel-Pfade ->
  14 reale Befunde gefixt (8 Commits, je build+test KERNEL_FUZZ=1 ALL PASS, gepusht). Roter Faden =
  Fehlerpfad-Schwaeche "Ressourcen allozieren -> fallible Schritte -> Fehlerpfad raeumt nicht auf":
  Loader F1(>8 PT_LOAD-Segmente lecken)/F2(Mid-Load-Fail leckt Segment+Stack)/F3(load_elf leckt PD),
  create_hardware_backend(ep/ntfn), spawn_isolated_native(Code-Frame via loaded_register), DMA
  SmmuV3Enforcer::attach G1-G3(Stage-1/CD bei Fehler frei), SYS_LOAD I(endow-Cap-Kopie bei Ladefehler);
  Allokator M1(checked_add-Abbruch->skip)/M2(Suffix-Verlust bei voller Free-Liste->skip); microkit M3
  (partner u8->u16); Liveness H1(purge_ipc_queues orphans [;8]->NENDPOINTS)/H2(ReplyFinal [;8]->NOBJECTS)
  -- sonst haengen verworfene CALL-Aufrufer; Scheduler K(record_zombie NZOMBIES 16->PER_CORE, sonst
  Stack-Leak; reclaim-Test lag exakt an 16). SICHERHEIT J: grant_cap (REPLY+GRANT) nutzte install_cap
  (ungeprueft) statt install_cap_checked -> Domaenen-Policy-Bypass (Server schleust HW-/Loader-Cap in
  fremde Domaene); FIX: cap_allowed-Vorabpruefung. AUDIT-ORACLE L: audit_cdt an Verus-Invariante
  angeglichen (first_child.prev==None + Geschwister teilen Parent). DANACH (Nutzerwunsch) zwei
  Vertiefungen: LOOM (s.o., Sync-Primitive concurrency-verifiziert + Sensitivitaet) + KRYPTO-REVIEW
  (Trust-/Cert-Flow KEIN Befund: Signatur deckt alle Felder, Reihenfolge ok, verify_strict, bounds-safe).
  SAUBER befunden (kein Bug): RwSpinLock(fetch_and-Release), Trap/FP-Save-Restore+Frame-Layout,
  MMU-W^X-Bits+vspace_wx_ok, dma_sg_validate, Dispatch-Rechte, Archiv-/DTB-Parser, domain_audit, MCS.
  FINDING M (Lock-Ordnung-Doku): invariants.md §1 listete FP_STATES als "Leaf (allein gehalten)" +
  VIRTIO_PCI doppelt (R1 UND Leaf) -- falsch: FP_STATES wird real unter SCHEDS gehalten (fp_trap/
  fp_reset_slot -> Rang R2.5), VIRTIO_PCI haelt nie einen weiteren Lock (echter Leaf). Code korrekt
  (Graph SCHEDS[*]->FP_STATES azyklisch, deadlock-frei), nur Doku gefixt (latente Inversionsgefahr).
  LOCK-ORDERING-SWEEP (systematisch): MEM(R4) nie mit weiterem Lock; SCHEDS(R2) nimmt nur FP_STATES;
  EPS/NTFNS(R1) geben vor SCHEDS frei; CAPS(R0) kein read->write-Upgrade; Cross-Core-IPC je Op genau
  EIN SCHEDS-Lock -> Hierarchie ueberall eingehalten. (Gesamt: 14 Code-Bugs + Finding M Doku.)
  LOOM-VERIFIKATION (Verification/concurrency/loom/, ADR 0023, tools/loom-verify.sh + CI loom.yml):
  8 Modelle ueber ALLE Interleavings, je sensitivitaets-gegengeprueft (injizierter Bug -> loom faengt
  ihn): (a) RwSpinLock (lib.rs) Mutual-Exclusion/kein-Lost-Update/transienter-Reader (store(0)-Bug
  gefangen), (b) Ticket-SpinLock (ticket.rs), (c) GLOBALE Lock-Hierarchie (hierarchy.rs: aufsteigende
  Schachtelungen deadlock-frei; Inversion MEM->CAPS deadlockt), (d) CROSS-CORE-IPC (crosscore.rs:
  one-lock-per-op deadlock-frei; zwei gehaltene SCHEDS deadlocken). Loom braucht Host-std+crates.io ->
  standalone nach $TMPDIR (wie kani-verify.sh). KRYPTO-REVIEW (sel4lake-trust + verify_trusted_cert):
  KEIN Befund (Signatur deckt alle Felder, Reihenfolge ok, verify_strict, bounds-safe, kein TOCTOU).
- AUSBAUSTUFE ext-28: TRUSTEDSAS-ZERTIFIKATE (FERTIG, C0-C5, == ALL PASS == in Release + kernel-fuzz).
  ADR docs/adr/0014-trusted-sas-certificates.md, Bericht docs/phase-reports/ext-28-trusted-
  certificates-report.md, Runbook docs/runbook-trusted-keys.md, invariants.md §9. ZIEL: ein als
  TrustedSAS (Domaene 0) deklariertes Image laedt NUR mit gueltigem, kryptographisch auf genau dies
  Binary gebundenem Ed25519-Zertifikat (TrustedSAS behaelt Trust-Stufe -> darf PdControl/Loader-Caps
  halten). UserLand/HardwareLand UNVERAENDERT (hardware-isoliert, kein Cert). Kernel haelt NUR
  oeffentliche Keys; private Keys NIE im Kernel/Repo; Key-DB kompiliert + read-only, NUR per Firmware-/
  Kernel-Update aenderbar, KEIN Syscall. Etablierte Krypto (keine Eigenentwicklung): Ed25519
  (ed25519-dalek v2, default-features=false, verify_strict -> keine Malleability) + SHA-256 (sha2),
  no_std + no-alloc (kompiliert sauber in den NoGlobalHeap-Kernel). NEUES CRATE sel4lake-trust
  (verify_sig/sha256/fingerprint/TrustedKey; 5 RFC-8032-Tests). EINGEFRORENES Cert-Format (Commit
  41ece2d) in crates/sel4lake-loader/src/cert.rs (TrustedCert, #![forbid(unsafe_code)], panik-frei,
  152-B-Header + variable Signatur): magic TSC1, cert/sig-Formatversion, signature_algorithm_id
  (Ed25519=1), certificate_policy_id, build/audit/unsafe/allowlist_rules_version, program_id, version,
  binary_hash, manifest_hash, key_id (=SHA-256(pubkey)[..16]), unsafe_status (Bitflags, muss ALL_PASS),
  unsafe_audit_hash, build_info; die GESAMTE Nachricht wird signiert. Archiv-Format v2 (cert_off/
  cert_len in frueher reservierten Entry-Feldern; mkarchive.py 7. Spec-Feld :CERT). KERNEL-GATE
  loader::verify_image (DOMAIN_TRUSTED): parse -> alg==Ed25519 & |sig|==64 -> key_id in TRUSTED_KEYS
  (nicht revoked, key_id==fingerprint(pubkey)) -> verify_strict ueber message -> binary_hash==
  sha256(elf) & manifest_hash==sha256(manifest) -> program_id/version==Archiv-Eintrag -> version>=
  MIN_VERSION -> unsafe_status==ALL_PASS; sonst LoaderError::Unverified (KEIN Thread/PD, vor jeder
  Ressourcenvergabe). kernel/src/trusted_keys.rs = autogenerierte read-only Key-DB (nur PubKeys),
  mod trusted_keys in main.rs, Kernel-Dep sel4lake-trust. UNSAFE-AUDIT + ALLOWLIST: ein zertifiziertes
  TrustedSAS-Programm ist vollstaendig #![forbid(unsafe_code)]; einzige unsafe-Quelle im App-Dep-Baum
  ist die auditierte Syscall-ABI libsel4lake (Allowlist). tools/sign_trusted.py: cargo metadata ->
  transitiver Dep-Baum (Sysroot core/alloc ausser Scope), je Crate unsafe-Scan, Programm muss forbid
  + 0 unsafe, unsafe NUR in {libsel4lake}; Verletzung -> ABBRUCH, KEIN Cert; Audit-Bericht (<cert>.
  audit.txt) je Crate, SHA-256 als unsafe_audit_hash verankert; dann SHA-256(ELF)+SHA-256(Manifest)
  + Ed25519-Signatur via python-cryptography. tools/gen_trusted_key.py: Keypair (privat keys/*.ed25519,
  gitignored 0600) + Key-DB-Gen. WICHTIGE SPANNUNG GELOEST: #[no_mangle] ist in aktuellem Rust ein
  *unsafe* Attribut -> von forbid(unsafe_code) BLOCKIERT; der ELF-Entry _start gehoert daher in die
  Allowlist-Schicht: libsel4lake::entry!(run)-Makro erzeugt die #[no_mangle]-Glue (Makro-Hygiene der
  externen Crate traegt das durch forbid hindurch), das Programm liefert nur sichere fn run(arg)->!
  und bleibt forbid-rein (KEIN "Trampolin zum Verstecken von Programm-unsafe", sondern Standard-
  Runtime-Support). programs/trusted/svc-demo = sauberes zertifiziertes Demo. TEST-UMWIDMUNG: aggressor-t
  jetzt forbid-clean + entry! -> zertifiziert (laedt+attackiert, "Trust!=Privileg" weiter belegt);
  intruder-t traegt absichtlich unsafe (nicht zertifizierbar) -> liegt OHNE Cert im Archiv -> intrt-Test
  UMGEWIDMET (war "laedt+faultet") zu "unzertifiziertes TrustedSAS abgewiesen" (trusted_load_rejected
  prueft Unverified, KEIN Thread/PD); EL0-Isolation der Trusted-Domaene weiter via intruder-u/-h belegt.
  trusted-x nutzt jetzt svc-demo (program_id 12) statt hello. AUDIT loader::trust_audit (IMMER im
  Kernel, kein Feature): Key-DB-Selbstkonsistenz (key_id==fingerprint, Eindeutigkeit, nicht leer) +
  Live-Oracle (gueltiges Archiv-Cert akzeptiert, manipulierte Kopie abgelehnt) -> Codes 1..5; in
  check_loadtrusted_el0 (loadhw) verdrahtet. FUZZER certfuzz (hinter kernel-fuzz, loader::verify_only
  nur dann kompiliert): 603 Cert-Varianten (Byte-Mutation/Truncation/Feld-Korruption/Zufallsmuell +
  Identitaets-/Binary-Transplantation eines gueltigen Certs auf falsche program_id/version/ELF) durch
  verify_image -> ALLE abgelehnt, echtes Cert akzeptiert, kein Crash/OOB (no-alloc Krypto), trust_audit+
  loader_audit==0, total_free unveraendert. Round-Trip host-verifiziert (python-cryptography <->
  ed25519-dalek interoperabel). Methodik wie gefordert: 1.Analyse 2.Variantenvergleich (Spike BEIDER
  ed25519-dalek + ed25519-compact gegen die custom no_std build-std-Target -> beide bauten; gewaehlt
  dalek wg. Audit-Pedigree) 3.ADR 4.Plan 5.Impl 6.Fuzzer+Audits 7.Doku. WICHTIG: Host-Tests der
  workspace-Crates (loader/trust) MUESSEN STANDALONE in /tmp laufen (workspace .cargo/config erzwingt
  build-std -> "duplicate lang item in core" sonst). BEWUSST OFFEN: mehrere Root-Keys/Cross-Signing,
  certificate_policy_id-ERZWINGUNG (heute signiert aber nicht geprueft), HSM/Secure-Boot statt keys/.
- Kern-uebergreifende synchrone IPC (FERTIG, ext-7): sched::SchedOps-Trait
  abstrahiert Scheduler-Ops; ipc/microkit nehmen &mut dyn SchedOps statt
  &mut Scheduler. call() verzweigt: Empfaenger gleicher Kern -> switch_to-Fastpath;
  anderer Kern -> Nachricht in dessen blockierten Frame + unblock(+IPI), Aufrufer
  blockiert lokal. Kernel-Facade KernelSched (system.rs) impl SchedOps ueber
  SCHEDS[]: je Op GENAU EIN Lock (auch fremder Kern), nie zwei -> deadlockfrei
  (RES serialisiert IPC, Reschedule nimmt nie RES). syscall() haelt RES + Facade
  (kein pre-lock von SCHEDS mehr); danach SCHEDS[core] fuer sync_fp_trap. Demo
  xipc-Check: Client core0 ruft Server core2 (CALL+REPLY je ueber Kerngrenze per
  IPI), 3->21/5->35/7->49.
- EL0-Userland (FERTIG, Privileg-Grenze im SAS, NICHT Adressraum): MMU AP-Bits
  (Kernel-Image EL1-only; .user_text EL0-RX+PXN; User-RAM > __kernel_end EL0+EL1-RW
  +PXN/UXN). TrapFrame traegt SP_EL0 (Offset 264; Save/Restore im Trap-Pfad);
  init_thread_frame(el0,user_sp) setzt SPSR=EL0t; hal::frame_from_el0. sched.spawn_user
  + system::spawn_user (Kernel-Stack aus EL1-only Linker-Pool __user_kstacks 4x16KiB,
  User-Stack aus phys.alloc; REAPT wird der User-Stack, Kernel-Pool-Slot wird geleakt).
  Isolation: handle_exception routet SYNC-Fault AUS EL0 an einen Fault-Hook (system::
  el0_fault -> exit_current toetet NUR den Thread, Kernel laeuft weiter); EL1-Faults
  halten weiter an. Demo: user_entry (EL0, nur inline-svc) CALLt EL1-Server=0xC0DE;
  bad_user (EL0) liest 0x40080000 -> Data Abort EC=0x24 -> isoliert. el0/el0iso PASS.
- Lazy-FP (FERTIG, ext-5; loest den ext-3-Hang dank EL0/EL1): Target ist jetzt
  SOFT-FLOAT (targets/aarch64-sel4lake.json: "rustc-abi":"softfloat" + features
  "+v8a,+strict-align,-neon") -> EL1-Kernel emittiert kein FP/SIMD. Boot CPACR_EL1.
  FPEN=0b01 (FP trappt nur an EL0). hal::fp: FpState(q0-31+FPSR/FPCR), save/restore
  (eigener global_asm mit ".arch armv8-a", damit q-Regs trotz -neon assemblieren),
  set_el0_trap(FPEN 0b01<->0b11). TrapFrame jetzt 272B (KEIN eager FP mehr; q-Save/
  Restore aus __trap_dispatch entfernt). FP-Trap EC 0x07 -> fp_hook. system.rs:
  per-Kern FP_OWNER[8] (Atomic) + per-Slot FP_STATES[64] (unter SCHED), fp_trap
  (alten Owner sichern/Trapper laden/Owner setzen/FPEN frei), sync_fp_trap am Ende
  jedes Hooks, fp_reset_slot bei spawn. fp_switch_count(). Demo fp-Check = echter
  EL0-Lazy-FP-Test: 2 EL0-Threads (user_fp_entry, .user_text, global_asm) halten
  Muster in d0-d3, YIELD-Ping-Pong (hoechste Prio FP_PRIO=5), SIGNAL an EL1-Kollektor;
  399 Owner-Wechsel ohne Korruption. WICHTIG: #[target_feature(neon)] auf softfloat
  ist future-hard-error -> stattdessen global_asm fuer FP-User-Code.
Crates jetzt auch: sel4lake-dtb. test-qemu.sh prueft 27 Checks (Default-Timeout 45s,
wegen SMP-Last + isolierten VSpaces unter single-threaded TCG; bei vielen parallelen
QEMUs truncaten Laeufe durch Host-Last -> einzeln/gespacet testen).
Crates: `kernel`, `sel4lake-sync`, `sel4lake-abi`, `sel4lake-hal`, `sel4lake-mem`,
`sel4lake-cap` (CapSpace+CDT, ObjectKind::Memory+Endpoint, lookup/install_endpoint),
`sel4lake-sched` (Round-Robin + block/unblock/switch_to), `sel4lake-ipc` (Endpoints
Call/Recv/Reply), `sel4lake-microkit` (PdTable + cap-gated dispatch). Alle neuen
Crates 0 unsafe. Kernel: `system.rs` (EIN SYSTEM-Lock = phys+cspace+sched+eps+pds,
beide Hooks, alle Wrapper), `selftest.rs`, `threads.rs` (PD/Cap-Setup + Demo). mm.rs
 entfernt (-> system.rs).
- Phase 1 (HAL): 8 Kerne, Identity-MMU+Caches, Exception-Vektoren, GICv2,
  Per-Kern-Timer (PPI 30, 10 Hz), SMP via PSCI/HVC (CPU_ON 0xC4000003).
- Phase 2 (Speicher): W^X-MMU (mehrstufig L1→L2→L3; .text=RX, .rodata=RO, Rest
  RW+XN), capability-basierter Allokator (lineare MemoryCap = Rust-Ownership).
- Phase 3 (Caps): CapSpace + CDT + Objekt-Refcount; copy/mint/move/delete(blatt-
  only)/revoke; Generations-Handles; Finalisierung via free_region. Nur Memory.
- Phase 4 (Sched): Kontextwechsel = SP-Tausch im Trap-Pfad (handle_exception gibt
  *mut TrapFrame zurück, __trap_dispatch `mov sp,x0`); init_thread_frame; Per-Kern-
  Round-Robin; Boot-Kontext=Idle; Thread-Stacks aus mm::alloc.
- Phase 5 (IPC): SVC (ESR.EC=0x15) -> Syscall-Hook; Register-ABI (x0=nr,x1=cap-idx,
  x2..x5=msg,x6=tag); Endpoints Call/Recv/Reply; block_current/switch_to/unblock;
  Transfer via hal::exception::frame_reg/set_reg zwischen Frames.
- Phase 6 (Microkit/cap-gated IPC): Endpoints sind Caps (ObjectKind::Endpoint im
  globalen CapSpace, KEIN zweites Cap-System); PD = Thread + cspace ([lokaler Index
  -> globaler CapPtr]); microkit::dispatch löst lokale Cap auf, prüft Objekttyp +
  Rechte (CALL=WRITE, RECV/REPLY=READ); ohne Cap=ERR_BADCAP, falsches Recht=ERR_RIGHTS,
  keine PD=ERR_NOPD. ALLER Kernzustand in EINEM SYSTEM-Lock (system.rs).
WICHTIG: (1) MMU vor Atomics/Spinlock/SMP. (2) Kernel ist SOFT-FLOAT (kein FP/SIMD
im EL1-Code); FP nur an EL0, lazy verwaltet (s.o.). (3) Kontextwechsel = SP-Tausch,
integer-only (272B TrapFrame); FP wird lazy via FP-Trap (EC 0x07) gewechselt.
Offen: keine Notifications/Send/Cap-Transfer-in-IPC; Zero-Copy via Memory-Cap; statische
PDs (kein .system-Format); feste Tabellengrößen; DTB=0; ein globaler SYSTEM-Lock
(per-Kern=Opt); Round-Robin eine Priorität (Bitmap TODO); kein Thread-Exit/Join; TCB
nicht im Cap-System.
- Phase 7 (Hot-Reload): Server-PD im laufenden System ersetzt (Quiesce: eps.retire_receiver
  + pds.clear_cap -> Swap: v2 spawn + bind, irq-disabled). Stabile Endpoint-Cap; Client
  unverändert. v1 verdoppelt, v2 verdreifacht -> Beweis. Reload-Manager = Idle-Thread.
Offene Ausbaustufen (über Roadmap hinaus): Lazy-FP, Bitmap-Prio, Per-Kern-Locks;
feinere IPC-Granularitaet (Endpoint-/Notification-Tabellen unter EINEM RES-Lock ->
IPC global serialisiert; per-Endpoint-Locks waeren Skalierungs-Opt); Reclaim der
EL0-Kernel-Stack-Pool-Slots (8 feste, beim Thread-Ende geleakt); Thread-Migration/
Lastausgleich (Threads fest kern-gebunden); statische PDs (kein .system-Format);
feste Tabellengroessen. Roadmap & Phasenberichte in `docs/`. Arbeitsweise (vom Nutzer gefordert): auf Deutsch,
iterativ je Phase, mehrere Ansätze vergleichen + dokumentieren (ADRs), `unsafe`
strikt minimieren. Siehe [[unsafe-allowed-domains]].
