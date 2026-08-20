# Plan: a GDB-class debugger for Caprock

Stand 2026-08-20. English per the language rule (`CLAUDE.md`, 2026-08-17).
**Every file reference below was read, not remembered.** Where a claim of an earlier draft was
disproven by reading the source, the correction is recorded in place rather than quietly removed
(appendix A lists them).

This is the complete plan: claim, capability design, ABI, kernel changes, userspace, staging,
acceptance, work list, risks. It is a plan — **nothing in it is built**.

---

## Implementation status — 2026-08-20, measured

**v1 is built and green on both architectures.** `test-qemu-x86.sh` → `== ALL PASS ==` with seven
`dbg` checks; `test-qemu.sh` (aarch64) → `== ALL PASS ==`; both kernels build, and the
`--no-default-features` build is green too.

The measured line:

```
dbg : laeuft-vorher=true vorher-undebuggbar=true (keine-Autoritaet=true alle-64-Slots-abgewiesen=true)
      abgeleitet=true gestoppt=true steht=true
dbg : zweiter-Halter-BUSY=true Ringwort-abgewiesen=true GPR-erlaubt=true krummer-PC-abgewiesen=true
      fortgesetzt=true laeuft-wieder=true
dbg : stand-vor-revoke=true revoked=true revoke-bricht-nicht=true freigaben=1
      kein-Steuerrecht-mehr=true Wurzel-lebt-noch=true
      Wurzel-geloescht-cap_delete=true danach-undebuggbar=true
dbg : ALL PASS
```

| Built | Not built |
|---|---|
| `BlockReasons` → `u16`, `DEBUG = 1 << 6`, five scheduler ops | the gdbserver PD (RSP) |
| `ObjectKind::Debuggable`, rights-derived, `Finalized` third list, release hook | hardware breakpoints, single-step, `hal::debug` (v2) |
| 5 syscalls (21..25), 2 error codes, dispatch arm | a probe for `DEBUG_READ_MEM` (the path exists, the measurement does not) |
| `POLICY_DEBUGGABLE` in the signed manifest, one minting site | the aarch64 `dbg` probe (the mechanism is arch-neutral; the probe is x86-only) |
| `vspace_resolve` on **both** architectures, gated on user accessibility | the user-window branch of `vspace_resolve` (GiB 0 is measured; `ISO_USER_VA` is not — named, not claimed) |
| `Freeze::Debugged`, migration guard, `dbg` line + **11** suite checks | the **worst case** of the stop-latency promise (see below) |
| stop latency, measured: **128/128 valid samples, p50 ≈ 6 800, p99 ≈ 13 900 cycles**, target on a foreign core | |
| `kernel/src/dbgmem.rs` — the memory probe, **arch-neutral and run by both bring-up paths** | |
| `tools/dbg-negativ.sh` — seven counter-proofs, each checked for the *right* failing conjunct | |
| `tools/berichtsgatter.sh` — the third `all_done()` trap made structural | |

### The stop latency — measured, and what the number does NOT say

```
Stopp-Latenz gemessen -- gueltige Proben 128/128 (gefordert 100)
p50=6803  p99=13886  max=21326 Zyklen   (p99 = 0 Promille eines Ticks, Schwelle 28034022)
Ziel auf Kern 1, Bericht auf Kern 0
```

p99 ≈ 14 000 cycles is roughly **5 µs** — three orders of magnitude under the ≤ 10 ms promise. That
is plausible rather than suspicious: `debug_stop` sends the reschedule IPI and a spinning target
takes it immediately.

> **And that is exactly why the number must carry its caveat.** The ≤ 10 ms bound is the *upper*
> bound for a target core that has interrupts masked or sits in a long critical section — and this
> series never reaches that state. The bound stays **derived from the tick, not measured at its
> limit**; "0 permille" attests the good case, not the bad one. Written into the report line itself,
> because a true number about the wrong question is `rx_used` all over again.

Three things had to be fixed before the row said anything, and each is its own lesson:

1. **Unbounded loops hung the suite.** 128 samples × up to 20 M spins meant the run died in the
   watchdog and the line was never printed. *A measurement that kills its own suite measures
   nothing* — and it leaves the state unobservable, since the log then shows a hang and no number.
2. **Waiting in spins measured 0/128 running targets.** A re-queued thread is only scheduled on a
   foreign core at the next tick; any spin budget below a tick misses that *structurally*. The
   lesson was already written three functions higher up in `freeze_bericht` — *gezaehlt wird in
   Ticks* — and I did not apply it.
3. **The probe measured a target on its own core, so it would have SKIPped in every run.** The
   workers sit on the boot core by design. A SKIP whose cause is structural is no longer an honest
   caveat but a line that will never say anything; the measurement now brings its **own** target on
   a foreign core, with its own PD and its own `Debuggable`, and tears both down afterwards.

### The counter-proofs — the half that proves something

`tools/dbg-negativ.sh`. Each mutation restores exactly one finding, and the script checks three
things, not one: that the mutation **applied** (a pattern that no longer matches is a silently
disabled negative case), that the **intended** conjunct fell, and — for M3 — that the *other* one
stayed green.

| | mutation | must happen |
|---|---|---|
| M1 | authority over the target PD exists at measurement time | `vorher-undebuggbar` falls |
| M2 | the release is dropped from finalisation | `revoke-bricht-nicht` falls |
| M3 | the release happens **only** on `cap_revoke` | `laeuft-wieder-nach-destroy_pd` falls **and** `revoke-bricht-nicht` stays green |
| M4 | `debug_stop` stops accepting the one-owner rule | `zweiter-Halter-BUSY` falls |
| M5a | only the *policy* ring gate is removed | `Ringwort-abgewiesen` **stays green** (depth) **and** `Ringwort-Grund-RING` falls (the reason is gone) |
| M5b | policy **and** HAL ring gates removed | both fall |
| M6 | `vspace_resolve` without the `US` check | `luecke-abgewiesen` falls |

**M3 is the one that was asked for, and it is the one that answers.** A mutation that reorders or
removes something can go red for the wrong reason; here the proof is the *asymmetry* — the teardown
row falls while the revoke row stays green. If both fell, the mutation broke something else.

**Two of these were wrong on the first attempt, and both errors are worth keeping:**

* **M1 mutated the minting site in the loader and stayed green.** Correctly so: the probe's target
  PD is created by `create_pd()` during bring-up and never goes through the loader, so the mutation
  could not reach the measured path. *A counter-proof that misses the path it tests proves nothing
  and looks like proof.* It now mints directly, before the measurement.
* **M5 knocked out the ring check and the row stayed green — and that was a finding, not a script
  bug. Then it was a finding about the PROBE.**

  First: the rule is guarded **twice, independently** — `redirect::writeback_erlaubt` says "this
  level may not go that far", `hal::frame_wort_setzen` says "this word is never writable from
  outside the kernel". The second holds even if somebody adds a fourth authority level and forgets
  the table. So the counter-proof became two: M5a for the depth, M5b for the gating.

  Then M5b removed **both** gates and the row *still* stayed green. There is a **third** layer:
  `writeback_erlaubt` assigns word 18 to no writable class and refuses it by **default**
  (`UeberDerStufe`). Good security — three layers, default-deny — and a **blunt measurement**: the
  row read `== Err(ERR_RIGHTS)`, so "refused because ring" and "refused because unclassified"
  looked identical, and no mutation of the ring gate could move it. *Ein Pruefer, der die falsche
  Groesse liest, kann den Fehler, gegen den er gebaut ist, strukturell nicht sehen* — the `park`
  line, verbatim.

  The kernel already counts the reasons separately (`DEBUG_WR_RING` vs `DEBUG_WR_LEVEL`), so the
  probe now reads the size that actually changes. And once it did, **M5a's expectation had to flip
  as well** — removing the policy gate removes the *reason*, so the sharp row falls while the
  *outcome* row stays green. That is the two gates made measurable instead of asserted, and it took
  three runs of the counter-proofs to get there.

  **The counter-proof did its job before it ever passed.** Six green conjuncts would have carried
  the blunt row indefinitely.

### The user-accessibility hole — found by the probe, not by reading

`vspace_resolve` in its first form walked the target's page tables without checking rights. An
isolated address space holds more than its user pages: `vspace_create_base` hangs the **shared
kernel entries** in it (RAM, EL1-only). So a `DebugRead` cap — the weaker of the two rights, the one
a crash reporter gets — resolved **kernel memory**, and returned it.

That is a privilege escalation, and from outside it looks exactly like a debugger that works.

Found by `dbgmem`'s `luecke-abgewiesen` row: a read 16 MiB past the target's page resolved, although
the target has nothing there. **"Not mapped" and "not mapped FOR THE GUEST" are two statements, and
only the second carries the claim.** Fixed on both architectures — `US` on *every* level (x86 ANDs
permissions down the path, so a leaf-only check would pass a page the guest cannot reach), `AP[1]`
on the descriptor that carries it (aarch64: on the block, or on the L3 leaf — an L2 table descriptor
has no AP bits, and checking there would look like a check without being one).

**This is the row that justifies the probe.** Nothing else in the run would have gone red.

### What the measurement disproved — six corrections to this plan

Each was found by running, not by reading. They are recorded here rather than silently edited in,
because *the corrections are the measurement*.

1. **Three cap kinds do not work — the CDT derives by RIGHTS on one object.** `mint`/`copy` make a
   child pointing at the *same* object; a second `ObjectKind` would be an independent root, not a
   child. The first build did exactly that, and the probe said so: `revoke-bricht-nicht=false`,
   because `revoke` at the root never saw its "children". **One kind, `READ` vs `WRITE`.**
2. **The release trigger is per deleted control cap, not object finalisation.** `revoke` deletes the
   **subtree**, not the presented cap — so the object keeps a reference, `refcount == 0` never
   happens, and a hook there never fires. The collector now reports *every* deleted control cap; the
   drain decides, after `CAPS` is released, by asking whether any control right still lives.
3. **The root must grant nothing itself, and that needs `is_root` in the type.** Rights alone cannot
   separate root from child. Without it every revoke leaves a live "control" cap standing (the root)
   and the release never runs. §2a's *„sie gewaehrt selbst nichts"* was prose; now it is a gate.
4. **§6c's object generation is not needed — §4b's sidecar generation still is.** Holding
   `CAPS.read()` across the cap check *and* the bit-set closes the §6c window, and the nesting is
   legal and not new: `CAPS` is **R0**, `SCHEDS[core]` is **R2** (`docs/invariants.md` §1), and
   nesting must only be ascending. One fewer number for somebody to maintain.

   > **These are two different generations and the correction applies to exactly one of them.**
   > §6c's guarded a *capability* window: did the authority survive between check and use.
   > §4b's (`KOPF_GEN`) guards a *frame*: it is only true **as a whole**, and a reader that sees the
   > same value before and after read a frame that was simultaneously true. In v1 the frame is read
   > by syscall on a stopped thread, so the question does not arise — **it returns the moment the
   > sidecar is mapped read-only into the debugger**, which is the better end state and still the
   > plan. Writing "the generation is not needed" without saying which would retire the wrong one.
5. **`dbg` must NOT be a conjunct of `all_done()`** — and at the third occurrence this stopped
   being a slip and became a watchdog, `tools/berichtsgatter.sh`. With it, the run printed `dbg : ALL PASS` and
   then died in the watchdog with `bringup : offen waren: sweep dbg verif` — *what the report sets
   cannot trigger the report*, the A-6.1 trap, third occurrence. It is gated one level up, by
   `check` in the suite, exactly like `freeze`.
6. **Five of the six judging places (§7) needed no change at all** — and that is the Z24 reason set
   paying off: `audit` codes 2 and 7 read `reasons.is_empty()`, so a new reason is covered by
   construction. The **one** real gap was `freeze_thread`, which would have reported `Frozen` for a
   debug-stopped thread; it now has its own refusal, `Freeze::Debugged`, because `Frozen` would be a
   lie with consequences (the caller's later `thaw` removes `PAUSE`, `DEBUG` stays, and the
   freeze/thaw pair becomes silently asymmetric).

---

## 0. The claim this plan is built to support

> **Debug authority is a capability over exactly one protection domain — delegable, revocable and
> auditable. A PD over which no such capability was ever minted cannot be debugged, and that is a
> property of the system, not a promise.**

This is the part Unix cannot express. `ptrace` is PID-based ambient authority checked against a
UID, one tracer per process, multiplexed over signals; `root` attaches to anything, and the
mitigations (`yama`, seccomp filters) are policy bolted on afterwards. Here the authority *is* an
object in the CDT, so revocation, delegation and audit come from machinery that already exists and
is already measured.

For the multi-tenant product this is the sentence that matters: *the operator cannot look inside
the tenant's process, and that is checkable rather than asserted.*

**The whole plan stands or falls on §2b.** If `Debuggable` is minted by default, §0 describes no PD
at all and what remains is ambient authority with extra steps.

---

## 1. What already exists — measured, with references

Every primitive a debugger needs except two is in the tree, built for other reasons.

| Need | Where it already is | Read on |
|---|---|---|
| read the full register frame | `crates/caprock-sched/src/redirect.rs` (Z26/A3 sidecar) | 2026-08-20 |
| write registers back, **safely** | same file, `uebernehmbar(i, n_gpr)` — read the whole frame, write back **only GPRs**, with a counter-test over every word behind them | 2026-08-20 |
| **a generation word in the sidecar** | `redirect.rs:196 KOPF_GEN` — monotone, kernel-only, "a frame with the same or a lower counter is an **old** frame" | 2026-08-20 |
| **see the target's faults** | `ObjectKind::FaultHandler` (`crates/caprock-cap/src/object.rs`) — **already built**, and its own doc names a debugger as a consumer | 2026-08-20 |
| stop / resume a thread | `system.rs:9319 freeze_thread`, `:9361 thaw_thread`, `:9278 enum Freeze` | 2026-08-20 |
| **wake a thread whose waker went away** | `system.rs:2554 abort_finalized_replies` + `caprock_cap::Finalized` — the precedent for §6, including its overflow discipline | 2026-08-20 |
| cross-core stop kick | `system.rs:1011 kick(core)` → `hal::intc::send_sgi(core, IPI_RESCHED_INTID)`, present on both architectures (`x86_64/intc.rs:211`, `aarch64/gic.rs:90`) | 2026-08-20 |
| a named wait reason per waker | `caprock_sched::BlockReasons` (Z24) — `u8`, bits 0..5 taken | 2026-08-20 |
| reply into a blocked thread's frame | `system.rs:9385 lade_antwort` — write the frame, **then** clear the reason, both under one lock. The exact shape `debug_continue` needs | 2026-08-20 |
| a signed place to put the mint policy | `crates/caprock-loader/src/manifest.rs:132 POLICY_*` — bits 0..3 taken, `POLICY_KNOWN` rejects the rest; enforced in `kernel/src/loader.rs:1196 policy_gate` | 2026-08-20 |
| serialise thread state (core dump) | `crates/caprock-cap/src/checkpoint.rs`, including a refusal rule | earlier |
| symbols / DWARF | host GDB — **not written at all** (§9) | — |

**The hard part is already decided.** Writing registers back is the classic privilege escalation in
`ptrace`, and `redirect.rs` answers it in the type:

> Lesen darf er den ganzen Frame; zurueckschreiben nur die GPR. Die Woerter dahinter tragen den
> RING (`cs`/`ss`, `spsr`) — wer sie schreiben darf, befoerdert seinen Gast.

### 1a. What does not exist at all

**No debug surface in the HAL, on either architecture.** Grepped 2026-08-20 for `MDSCR`, `DR7`,
`DR0`, `debugctl`, `PSTATE.SS`, `single.step`: zero hits in `crates/` and `kernel/` outside SMMU
`IDR0` false positives. There is no `hal::debug` module. Hardware breakpoints and single-step (v2)
are therefore **new HAL code on both architectures**, and that is the largest single item in §12.

---

## 2. The capability design

### 2a. Four cap types — and one of them is already built

| Cap | Grants | Status |
|---|---|---|
| **`Debuggable(pd)`** | the parent: the right to derive the two below | new |
| **`DebugRead(pd)`** | read the frame, read the target's memory. **Never stops anything** | new |
| **`DebugControl(pd)`** | stop, continue, write registers | new |
| **`FaultHandler{ep, pd, sidecar, len}`** | see the target's page faults | **exists** |

`FaultHandler` is the find that shortens v1. Its doc comment in `object.rs` already separates it
from `SyscallHandler` with exactly the argument this plan needs — *„Syscalls zu beantworten heisst
‚ich bin der Kernel dieses Gastes‘, Faults zu sehen heisst ‚ich verwalte seinen Speicher‘"* — and it
names a debugger among the consumers. A crashing target therefore does not need a new mechanism to
be caught; it needs `FaultHandler` bound to the debugger's endpoint.

**But it is not the same authority as `DebugControl` and must not be folded into it.** Seeing a
fault is passive; stopping a running thread is not. Two caps, and a debugger session that wants both
holds both. *Ein Kanal mit zwei Bedeutungen* is the class this project has paid for three times, and
`object.rs` says so at that very type.

A derived cap names **one** PD. There is no PID namespace, no "root may attach to anything", no
second mechanism to keep in sync. The debugger PD itself holds **no other privilege** — a compromised
debugger can harm exactly its one target.

Three properties fall out of the derivation rather than being built:

1. **`revoke(Debuggable)` removes every debugger at once**, including delegated ones, because that is
   what the CDT does. Linux cannot revoke a `ptrace` attachment. *(And that alone would break the
   target — see §6, which is the largest single piece of new reasoning in this plan.)*
2. **Delegation is visible** — a crash reporter gets a derived cap; the CDT names every holder.
3. **"Who holds debug authority over this PD?" is a CDT walk**, not a claim.

### 2b. The mint policy IS the claim — and it belongs in the signed manifest

The first draft said "minted by the loader when the PD is created" and left the default open. That
makes §0 **vacuous**: if every PD gets a `Debuggable`, "a PD over which none was ever minted"
describes no PD at all. Same shape as the RMRR-on-q35 trap and as the `smt` line under `threads=1` —
a claim that holds because its antecedent is empty.

**The property binds only under these three rules:**

1. **Not minting is the default.** `Debuggable` is absent unless the creation parameters of the PD
   ask for it. There is no "grant it later" path that does not go through creating the parent cap.
2. **Minting is a named act** — not a side effect of a loader policy, not a flag defaulting to on.
3. **Minting is logged**, and for tenant workloads it is a customer-visible, audited action.

**Where rule 2 goes is now a measured answer rather than a design question.** `manifest.rs` has
`policy_flags` with bits 0..3 used and `POLICY_KNOWN` rejecting the rest, and `loader.rs:1196
policy_gate` is the place that enforces policy with the principle already written down:

> **was der Kernel nicht einhalten kann, wird abgewiesen, nicht ignoriert.** Ein Politikfeld, das
> nur gedruckt wird, ist schlechter als keins — es sieht konfiguriert aus.

So: **`POLICY_DEBUGGABLE = 1 << 4`**, added to `POLICY_KNOWN`, honoured in `policy_gate`. And this is
better than a runtime parameter for a reason that is a property of the format, not a convenience:
**the manifest is Ed25519-signed and bound to `kernel_hash`.** "Which PDs may be debugged" therefore
becomes a *signed, attested, anti-downgrade-protected* decision (`manifest_version` is monotone),
not a decision someone with a shell makes at runtime. For a tenant that is the difference between a
policy and an assurance.

A PD created outside the manifest (`SYS_LOAD` at runtime) needs the same named field in its creation
parameters. **Absent means absent**, per the reserved-bytes rule this repository already enforces:
`0` heisst „keine Angabe", nicht „passt auf alles".

**And minting is only half of it — custody is the other half.** Whoever holds `Debuggable` can
re-derive a `DebugControl` at any moment, so a release window enforced by time-limiting the derived
cap does not close. **The window ends by revoking `Debuggable` itself**; anything else lets the
*recoverability* of the authority outlive the release. Same distinction as *„eine Kapazitaet
einfuehren heisst, den Ueberlauf zu benennen"*, one level up: granting an authority means naming who
may re-grant it.

This is the single point where the product claim can tip over, and it is why the acceptance line
(§11) must carry the **negative** case — a PD that cannot be debugged **with any capability in the
system** — not merely "a debugger without a cap is refused".

### 2c. Why the right is split in two

A stop is single-owner and a read is not, and the reason is mechanical: **a reason *bit* has no
refcount**, and `BlockReasons` has no room for one. Splitting the right makes arbitrarily many
readers trivially correct and shrinks the ownership question to `DebugControl` alone (§8a).

It also matches how the tools are used: a crash reporter and continuous state inspection want to
**read and never stop**. Security-wise `DebugRead` is not the weaker cap — the price in §2e applies
unchanged, every byte is readable — but availability-wise it is the difference between "one tool at a
time" and "inspection always running".

### 2d. Two objects, two rules — and they are not interchangeable options

| | while the target runs |
|---|---|
| **memory** | **allowed.** A range has no internal consistency condition; torn is the honest, expected state, and a continuous observer needs exactly this |
| **frame** | **needs the generation** (§4b). A frame is only true *as a whole* — half of one was never simultaneously true |

The first draft offered "refuse, or return the generation" as a coin toss. It is not: the rules
differ because the **objects** differ. The consequence is worth stating rather than discovering — a
continuous observer that never stops anything sees memory and counters, and sees frames only while
somebody else has the target stopped. That is a consequence, not a limitation to argue about.

### 2e. The price, stated plainly

A `DebugRead` cap reads every byte of the target's memory. That is a **complete confidentiality
break** for the target and it is unavoidable — a debugger that cannot read memory is not a debugger.
The gain is not less access; it is that the access is **named, delegable, revocable and visible**
instead of ambient.

---

## 3. The ABI

New syscall numbers start at **21** — `SPAWN = 20` is the last (`crates/caprock-abi/src/lib.rs:181`).

### 3a. Five syscalls

| № | Name | Cap | Args | Returns |
|---|---|---|---|---|
| 21 | `DEBUG_ATTACH` | `Debuggable` | slot, rights mask (read / control) | derived cap slot |
| 22 | `DEBUG_STOP` | `DebugControl` | slot, target tid | `OK` / `ERR_DEBUG_BUSY` / `ERR_BADCAP` |
| 23 | `DEBUG_CONTINUE` | `DebugControl` | slot, target tid | `OK` / `ERR_BADCAP` |
| 24 | `DEBUG_READ_MEM` | `DebugRead` | slot, target va, len, own buffer va | bytes transferred |
| 25 | `DEBUG_WRITE_REGS` | `DebugControl` | slot, target tid, mask, values | `OK` / `ERR_RIGHTS` |

Frame **reading** deliberately has no syscall — it is the sidecar mapping (§4). Reading via a
syscall is an acceptable v1 shortcut if the mapping slips, but it is not the design.

Each operation is in the kernel for a reason worth naming:

| Operation | in the kernel because |
|---|---|
| `DEBUG_STOP` / `DEBUG_CONTINUE` | scheduler state — userspace cannot set a `BlockReasons` bit |
| `DEBUG_WRITE_REGS` | **the mask is the entire security statement** (§3b) |
| `DEBUG_READ_MEM` | it walks the **target's** page tables, not the debugger's |
| `DEBUG_ATTACH` | it derives a CDT child, which is a `CAPS.write()` |

New error codes continue from `ERR_INUSE = 18`: **`ERR_DEBUG_BUSY = 19`** (a second `DebugControl`
holder tried to stop an already-stopped target — §8a) and **`ERR_NOT_DEBUGGABLE = 20`** (no
`Debuggable` was ever minted for that PD; distinct from `ERR_BADCAP`, which means *you* do not hold
one — the distinction is the whole of §0 and must be visible at the ABI).

`caprock-microkit` resolves slot → cap → PD and hands over; the decision is taken in **one** kernel
function, following `SYS_SPAWN`'s callback shape (`crates/caprock-microkit/src/lib.rs:1215
dispatch`). *Eine Pruefung, die an zwei Stellen halb passiert, ist zwei Pruefungen, und die zweite
altert* (K1a).

### 3b. The write mask — a **third** authority level, and this is what will bite

The existing redirect predicate is **too narrow for a debugger**. Without writing `PC` there is no
`jump`, no `return`, and no way to resume after a breakpoint; without `SP` no stack manipulation.

| Role | may write |
|---|---|
| redirect handler (today, `uebernehmbar`) | GPRs |
| **debugger** | **GPRs + PC + SP + `rflags`/`pstate` through a mask** |
| nobody, ever | `cs` / `ss` (x86), the EL bits of `spsr` (aarch64) — that is the ring |

`PC` and `SP` cannot change the ring, but they are not free: `redirect.rs:345` already records why
they were left out — *„jedes von ihnen braucht eine eigene Gueltigkeitspruefung (Kanonizitaet,
Ausrichtung), und drei Entscheidungen in einer Zeile zu treffen ist die Form, die dieses Projekt
schon bezahlt hat"*. A non-canonical `rip` faults **in the kernel** at `iretq`. So: canonicality
check on `PC`, alignment check on `SP`, and both are named refusals, not silent clamps.

`rflags` needs a **mask, not a yes/no** — `TF` turns on single-stepping (which is v2 and must not
arrive early through the back door) and `IOPL` grants port access. Linux does exactly this masking,
and it is where implementations usually get sloppy.

**Extend `uebernehmbar`; do not put a second predicate beside it.** Two copies of one rule are the
drift this project already paid for in the colour arithmetic (`MASK_BITS` / `stripe`).

### 3c. The mask may not come from the sidecar header — measured, and it is not obvious

`KOPF_NGPR` (word 5) says how many frame words may be written back. It sits in a page the handler
**writes**. Read on 2026-08-20, `kopf_pruefen` does the right thing: it compares
`slot[KOPF_NGPR]` against the *reader's own* `n_gpr` and rejects `FremdeBreite` on mismatch. **The
header value is a consistency check, never the authority.**

That has a direct consequence for the debugger, and it would be easy to get wrong: since the debugger
may write more registers than a handler may, its expected `n_gpr` **differs**, and `kopf_pruefen`
demands equality. **The writeback limit must therefore come from the capability presented at the
syscall, not from the slot.** Reusing the header word would hand a debugger's slot to a handler's
check, or invert it — either way a security decision would be taken by an allocation layout.

---

## 4. Frame access — the sidecar mapping

Map the Z26/A3 sidecar **read-only** into the debugger. Register reading then costs no kernel entry
at all, and the mechanism exists.

### 4a. Page granularity is the enforcement

`SLOT_BYTES = 512` (`redirect.rs:113`), so **eight slots share a 4 KiB page.** If several threads'
sidecars share a page, mapping one hands the debugger frames it holds no capability for — a
capability check defeated by an allocation layout. Two ways out:

1. **One page per debugged thread** — wastes 3.5 KiB per thread, trivially correct.
2. **The kernel copies the slot into a per-debugger page** — costs a copy per stop, keeps density.

**Recommendation: (1), and decided before the mapping exists**, because the layout *is* the
enforcement. `fenster_deckt`/`slots_in` already exist to express the window arithmetic and are
host-tested; what they do not express is *whose* slots are in a page. That is the new condition.

> Recorded from the Z26/A3 review: the sidecar arithmetic (`slot_offset`, `slot_gueltig`, `slots_in`,
> `fenster_deckt`) once had **no caller outside its own test module** for three days while two
> mutations dutifully proved the functions correct. `grep` for the callers is part of accepting this
> item, not an afterthought.

### 4b. The stop generation already exists

The first draft asked for "a generation word in the sidecar, seqlock-shaped". **It is already
there** — `KOPF_GEN`, word 2, monotone, kernel-only, with the rationale already written:

> Der Handler merkt sich den zuletzt gesehenen Stand; ein Frame mit gleichem oder kleinerem Zaehler
> ist ein **alter** Frame, kein neuer. Ohne diese Zahl koennte er eine ausgebliebene Zustellung nicht
> von einer wiederholten unterscheiden — genau die Verwechslung `rx_used` gegen „Daten sind
> angekommen".

**Decision: a debug stop is a delivery.** It increments `KOPF_GEN` under the same rule, in the same
word, and the debugger reads it before and after the frame. No new field, no second counter, no
second rule to age. Without it, "the frame is mapped read-only" holds while stopped and is **silently
false** while running.

---

## 5. The wait reason

**`BlockReasons::DEBUG = 1 << 6`.** Bits 0..5 are taken (`IPC`, `BUDGET`, `PAUSE`, `PARK`, `HANDLER`,
`LOAD` — `crates/caprock-sched/src/lib.rs:287`).

Z24 rules apply unchanged and are load-bearing: **only the debugger removes this reason** — `resume`,
`unpark`, `unblock`, `handler_reply` and `load_reply` must not touch it, and a thread is re-queued
**only when the reason set is empty**. Without the second half the set is just another spelling of
the old bits.

This is the **seventh** instance of the class the `BlockReasons` doc comment enumerates, and like
`HANDLER` and `LOAD` it is predicted rather than found.

> **Widen `BlockReasons` to `u16` in this change, not later.** It is a `u8`; after `DEBUG` exactly
> one bit remains. The widening is mechanical **now**, while there is a consumer at hand and the
> counter-proofs are being run anyway. At bit 8, under deadline pressure, it is not — and the six
> judging places of §7 would have to be re-verified a second time.
>
> **Checked first, because it decides whether "mechanical" is true:** the reason set is **not** part
> of the checkpoint format. `crates/caprock-cap/src/checkpoint.rs` uses the word *Grund* only for
> `Refusal` reasons, which are a different thing. **Had it been serialised, this would not be a
> widening but a persistent format change** — version at the format, N and N+1 side by side, separate
> release. One grep decided which of the two changes this is.

---

## 6. Release — revoke and teardown

**`revoke(Debuggable)` on a stopped target would brick it.** Only the debugger clears `DEBUG` (§5).
A target that is stopped when the revoke lands — or whose debugger PD simply dies — keeps a set
reason bit that **nobody is allowed to clear**. It never runs again. *A revoke that bricks the target
is worse than no revoke*, and it is the same class as the four D9 findings: a wakeup whose waker went
away.

### 6a. The mechanism already exists, and it is not the one the earlier draft invented

Read on 2026-08-20: `system.rs:2519 cap_delete` and `:2536 cap_revoke` are **structurally
identical**. Both build a `caprock_cap::Finalized`, both take `CAPS.write()` + `MEM.lock()`, and both
then call, in this order and with **no lock held**:

```
abort_finalized_replies(&rf);      // CAPS < EPS < SCHEDS — unblocks threads whose waker died
dma_finalize(&rf, …);
note_finalize_overflow(&rf);
```

`abort_finalized_replies` is **precisely this problem, already solved, for the exactly analogous
case**: finalising a Reply cap would leave a `CALL`-blocked caller waiting forever, so it is unblocked
with `ERR_SERVER_GONE`. Its own comment names the lock-order reason it runs where it runs.

**So the release is not new machinery.** It is a third list in `Finalized` — beside `items`
(`(ep, caller)` pairs) and `dma` (`(phys, len)` regions) — carrying the threads whose `DEBUG` must be
cleared, drained by a `release_finalized_debug(&rf)` sitting next to `abort_finalized_replies`.

This answers, at one stroke, three things the earlier draft handled separately and worse:

* **Both paths are covered.** The earlier draft correctly measured that a dying PD deletes its caps
  one at a time through `cap_delete` (`system.rs:4226`), not through `cap_revoke`, and warned that
  writing the rule twice means the `cap_delete` copy ages (K1a). Written *here*, there is one copy.
* **The lock order is the one that already survives.** No `CAPS → SCHEDS[core]` edge is created:
  `CAPS` is released before the hook runs, exactly as for reply aborts.
* **The overflow is already named.** `Finalized::overflowed()` + `note_finalize_overflow` exist and
  print loudly, because *„ein Fehler dieser Klasse ist im Nachhinein an nichts mehr zu erkennen — man
  sieht nur einen Thread, der steht"*. That sentence was written about blocked `CALL` callers; it
  describes a stuck debug target word for word. The capacity is `CapSpace::finalize_capacity()`
  (`space.rs:343`) — **derived from the object table**, not a second number kept beside it. The
  constant `MAX_FINALIZED` was exactly such a number and was removed for that reason (`space.rs:84`).

### 6b. The ordering rule, mirrored — named in advance this time

`docs/invariants.md` §1b covers this as a general rule. Its two directions both appear here:

> **The revocation must be visible before the release runs.** If the release ran first, a
> `DEBUG_STOP` still holding a valid-looking cap could set the bit again behind it, and the thread
> would stand forever. Under the `Finalized` design this is free: the cap is already gone from the
> cspace when `CAPS.write()` is dropped, and the hook runs after.

> **And the notification must be armed before a stop is possible** (§8a) — otherwise a stop that
> lands in the window is lost. D0 and the C8 job queue, both times the same ordering.

### 6c. The race the syscall split does not cover

A `DEBUG_STOP` that saw a valid cap at syscall entry must not still be allowed to set the bit *after*
the revoke has landed. **Cap check and bit-set must be the same critical section.** Checking on entry
and setting later is precisely the bug nobody sees, because it never occurs in normal operation.

Concretely: `DEBUG_STOP` resolves the cap under `CAPS.read()` and sets `DEBUG` under
`SCHEDS[core]` — two different locks, so "the same critical section" needs a mechanism, not a wish.
The cheapest correct one is a **generation on the `Debuggable` object**, read with the cap and
re-checked under `SCHEDS[core]` before the bit is set; a revoke bumps it. This is the `KOPF_GEN`
shape again and the same shape as the object-table `gen` field that already guards stale object
indices (`object.rs:188`).

### 6d. Migration must be made structural

**Can a DEBUG-stopped thread migrate away between the cap check and the bit-set?** Measured
2026-08-20: `migration_candidate` walks only the **ready queues**, and a thread with a non-empty
reason set is not enqueued (Z24). `migrate_to` has exactly two callers: `balance_once` (through that
candidate) and one cross-core test. **So it cannot happen today — incidentally, not structurally.**

> `migrate_to(tid, dst)` takes an arbitrary `tid` and does **not** check the thread's state. The
> property therefore rests on every caller's discipline, which is the shape this project keeps paying
> for. **Make it structural:** `detach_for_migration` refuses a thread that is not in a ready queue.
> Then *"a DEBUG-stopped thread does not change cores"* is enforced rather than observed.

This is a **prerequisite of the debugger, not part of it** — it is a five-line change with its own
counter-proof, and it should land first, on its own.

### 6e. Two corrections to the earlier draft, recorded

The earlier draft rejected clearing inside `revoke` for three reasons. One was false:

* ~~Two `SCHEDS` are never held simultaneously, so this would be a new class.~~ **False.**
  `migrate_to` holds `SCHEDS[lo]` and `SCHEDS[hi]` **at the same time**, and already with the
  ascending-index total order such a class needs. The two source comments saying *„nie zwei
  gleichzeitig"* are **scoped** — one describes the `KernelSched` facade for the IPC/dispatch path
  (`:924`), the other `least_loaded_core` (`:2720`). Two function-level promises were read as a
  tree-level invariant.
* **`CAPS` would be held for a data-dependent, unbounded duration.** That stands, and on its own it
  is sufficient — C9 measures exactly this.

And the draft's own answer ("kick the target core, clear it there") is **superseded by §6a**: it
would have been a fourth mechanism next to a third one that already does this job, at the same place,
for the same reason.

### 6f. The alternative, recorded so it does not come back

Make "blocked for debug" a *derived* predicate that checks for a live `DebugControl` on every query.
Then revoke touches nothing — but all six judging places of §7 gain a CDT access, and the lock-order
edge reappears in the other direction. Worse.

> **This still couples CDT finalisation to scheduler state** — at a place where the lock order
> survives, but it is a new fact. It belongs in `docs/invariants.md` §1 as a named edge, not in a
> comment: **a cap operation can now make a thread runnable.** (It already could, via
> `abort_finalized_replies`; the entry documents both.)

---

## 7. The six places that judge thread state — they must all learn `DEBUG`

*Wer einen neuen Zustand einfuehrt, muss jede Stelle mitnehmen, die ueber Zustaende URTEILT — nicht
nur die, die ihn erzeugen.* The D0 rebuild paid for this once (audit code 7: "runnable and in no
list" was literally the state a parked thread was in).

| # | Where | What goes wrong without it |
|---|---|---|
| 1 | `caprock-ipc/src/lib.rs:204 is_quiescent` | **D11 rebuilt.** A debug-stopped thread has no open IPC relation, so it reports *quiet* — and a hot reload would be released while someone sits in the debugger |
| 2 | `system.rs:9261 thread_quiescence` | same, one level up — `freeze_thread` reads it |
| 3 | `caprock-ipc/src/lib.rs:521 purge_thread` | a stopped thread must not be torn down as unreferenced |
| 4 | `caprock-sched/src/lib.rs:1776 audit` | "runnable and in no list" — the D0 regression, exactly |
| 5 | `caprock-ipc/src/lib.rs:596 audit` | IPC-side accounting of a thread that is neither running nor waiting on IPC |
| 6 | `system.rs` audit implementations | the aggregate verdict |

This table is the part of the plan most likely to be skipped, and it is the part that leaves the
suite **green while something is broken**.

---

## 8. Stop semantics

`freeze_thread` is **not** the debugger's stop. It halts at a *nameable boundary* — not on a core
**and** no open IPC relation — and returns `Freeze::Busy` otherwise (`system.rs:9278`). That is right
for a checkpoint and wrong for a debugger, which must stop mid-IPC and at an arbitrary instruction.

`DEBUG_STOP` therefore sets the reason and, if the target runs on another core, calls the existing
`kick(core)` (`system.rs:1011`).

> **The target stops at the next kernel entry, not mid-instruction.** At 100 Hz that is ≤ 10 ms. This
> belongs in the promise; otherwise `stop` claims something the mechanism cannot do. And §11 measures
> it, because *a promise that is never measured is a promise, not a property*.

### 8a. Who owns the stop

`DEBUG` is a bit without a refcount, so the stop has exactly one owner: the first `DEBUG_STOP`
establishes it, and `DEBUG_CONTINUE` or the loss of that `DebugControl` cap releases it (§6). A second
`DebugControl` holder attempting `DEBUG_STOP` on an already-stopped target is **refused with
`ERR_DEBUG_BUSY`** — not queued, not silently accepted. *Wer eine Kapazitaet einfuehrt, muss den
Ueberlauf benennen* (D11), and "one" is a capacity.

Readers are unaffected: any number of `DebugRead` holders may look at a target somebody else stopped.

### 8b. Faults — and the two states a debugger must not confuse

A crashing target arrives through `FaultHandler`, which already exists. **A faulted thread and a
debug-stopped thread are different states**, and folding them would be this project's favourite bug
in a new place: the first is *the target did something*, the second is *the debugger did something*,
and only the second may be cleared by `DEBUG_CONTINUE`. The bound `FaultHandler` sees the fault; the
debugger must then take its own `DEBUG_STOP` if it wants to hold the thread.

---

## 9. What stays in userspace

**Do not port GDB — build a `gdbserver`-equivalent PD.** It speaks RSP over a channel and real GDB
runs on the host. RSP is small and text-based, and it keeps a libc out of the picture.

An earlier version of this section listed DWARF, disassembly and expression evaluation as work to be
done here. That is wrong, and it made the plan look larger than it is. **Those live in host GDB and
are not written at all.**

| Runs as a Caprock PD (write this) | Runs in host GDB (write nothing) |
|---|---|
| RSP packet framing, checksums, `qSupported` | DWARF, source display |
| register-number mapping (GDB order ↔ frame order) | disassembly |
| memory read/write packets | expression evaluation, value printing |
| breakpoint and thread packets (`Z`/`z`, `H`, `T`) | backtrace reconstruction |

An **unprivileged** PD holding one or two capabilities, with **no proof obligation**. The register-
number mapping is the only fiddly part, and `KOPF_ABI` (words 16..23, the ABI index table) already
exists so that a reader *„findet ihre Argumente, ohne die Registerlage der Architektur nachzubilden —
ein Leser, der die gepruefte Groesse nachrechnet, prueft eine zweite Wirklichkeit"*. Use it.

---

## 10. Staging

### 10a. v1 — attach, stop, look

`DEBUG_ATTACH`, `DEBUG_STOP`, `DEBUG_CONTINUE`, `DEBUG_READ_MEM`, sidecar frame read; `Debuggable` /
`DebugRead` / `DebugControl`; `FaultHandler` bound for crash catching. **No writing, no single-step,
no breakpoints.** This never touches W^X and is already a usable post-mortem and attach debugger.

Prerequisite that lands first, alone: §6d (`detach_for_migration`).

### 10b. v2 — control

`DEBUG_WRITE_REGS` (§3b mask), hardware breakpoints, single-step.

- **Hardware breakpoints need per-thread save/restore of the debug registers** in the context switch
  — the same class as the FP state (`FP_OWNER`, `NM_TRAPS`). Skipping it is not a missing feature but
  a **channel**: thread A's watchpoint would fire in thread B, across the PD boundary.
- **Single-step** needs `TF` (x86) / `MDSCR_EL1.SS` + `PSTATE.SS` (aarch64) and the debug exception
  vector wired to the stop path.
- **`hal::debug` must exist on BOTH architectures from day one.** Twice an x86-only function in
  arch-neutral kernel code broke the aarch64 build (`guard_unmap`, `irq_tiefe`) — the second time
  *after* the acceptance run was supposedly hardened against that class. The aarch64 side may be a
  stub, but a **named** one that reports `false`, not a missing symbol.

### 10c. v2 design point, not an open question: a thread inside an IPC call

For platform threads this reads like a corner case. **For the workloads that matter it is the common
one.** Most targets are Linux binaries whose state, mid-syscall, lives partly in the syscall-server
PD rather than in their own frame — a stopped Postgres thread halfway through `fsync` is the ordinary
situation, not the rare one.

Three candidate answers, to be decided **before** v2 is built:

1. **Refuse**: a target in an IPC relation cannot be stopped. Honest, and it makes the debugger
   useless for exactly the targets it is for.
2. **Show the caller's frame and mark the thread as *in a call***, naming the server. The debugger
   sees a truthful but incomplete picture and says so. *(Recommended.)*
3. **Follow into the server** — requires authority over the server PD, which the debugger does not
   have and must not silently acquire.

The redirect machinery has an answer for *handlers*; a debugger is not a handler, and inheriting its
answer without checking would be the shape this project keeps recording. **If v2 is built without
settling this, it will not be usable for the targets that count.**

### 10d. Deferred, and it is a decision — software breakpoints

`int3` patching is a **write to program text**, and W^X is not merely a report line here: it is
`vspace_audit` (`system.rs:3579`, code 1 = W^X violation) **and a Verus proof**
(`verus/wx_invariant.rs`, "`map_page` erhaelt die W^X-Invariante"). Linux gets around this with
`FOLL_FORCE`, a documented hole.

Copying that would devalue a property this project actually proves. Three honest routes:

1. **No software breakpoints** (hardware only — 4 per core). *Carries v1 and v2.*
2. A distinct "write text" capability, with the `wx` line carrying open debug-write windows as a
   **counted, named exception** and gating on their being closed.
3. A kernel operation that flips the page briefly and takes the audit along.

> **The deferral is right, and it is coupled to a product decision — write the coupling down now.**
> Four hardware breakpoints are ample for platform processes, so route 1 carries v1 and v2. The
> **customer-debugging path** (Velve architecture, ch. 16) is different: fifty breakpoints in a Node
> application is an ordinary session, and hardware registers cannot carry it. On the day that path is
> built, **route 2 is the only one that does not devalue the measurement** — and note that route 2
> now also has a **proof** obligation, not just a report line: the Verus statement is about
> `map_page`, and a second mapping path that bypasses it would have to be proven or excluded.
> Recording the coupling here is what keeps it from being rediscovered in a year as a surprise.

---

## 11. Acceptance — what has to be measured, and what would falsify it

A debugger claiming *"I read the target's true state"* needs a check that could say otherwise.

Report line **`dbg`**, gated in `all_done()` on **both** architectures — x86 `DONE_FLAGS` 43 → 44
(`kernel/src/arch/x86_64/bringup.rs:4451`), aarch64 `DONE_FLAGS_ARM` 60 → 61.

| Claim | How it is measured | Counter-proof that must go red |
|---|---|---|
| the target really ran | baton counter advances (K1a pattern) | — |
| the stop really held | counter stands across an observation window longer than a tick (Z4a pattern — through *effect*, not a state bit) | remove the reason on `resume` ⇒ target keeps running |
| the register read is true | the target writes a known value into a GPR itself; the debugger reads it back | break the read path ⇒ line red |
| the memory read is true | target writes a magic word; debugger reads it at the same VA | read the *debugger's* page tables instead ⇒ line red |
| the frame is not torn | `KOPF_GEN` before == after across the read | drop the generation re-read ⇒ a read while running must be detected |
| a foreign PD is refused | second debugger without a cap gets `ERR_BADCAP` | — |
| `revoke` bites | revoke `Debuggable`, next operation fails | — |
| **§0 itself** — a PD for which `Debuggable` was **never minted** is undebuggable by a debugger PD holding **every other capability in the system** | the probe is handed the full cap set; every debug operation returns `ERR_NOT_DEBUGGABLE` | **mint `Debuggable` for that PD ⇒ the line must go red.** Without this row §0 is the only claim with no measurement, and it is the one the product rests on |
| **`revoke` does not brick the target** (§6) | revoke while the target is DEBUG-stopped; **the baton must advance within N ticks** — Z4a form, through *effect*, not a timeout on a negative | **two** mutations, two red conjuncts: (a) drop the `Finalized` debug list drain; (b) drop the object-generation re-check of §6c so a stop can land behind the revoke |
| **teardown does not brick the target** (§6a) | kill the debugger PD (→ `cap_delete` path, not `cap_revoke`) while the target is stopped; baton must advance | route the drain through `cap_revoke` only ⇒ this row red, the row above still green — **that asymmetry is the point** |
| **the stop arrives inside the promised window** (§8) | measured stop latency, **p99 over ≥ 100 stops**, against one tick | — |
| the finalisation buffer did not overflow | `finalize_overflow_count()` unchanged across the run | — a silent overflow is a permanently stopped thread |

**Speaking test:** the line must fail when nothing was debugged at all. `0 stops` is not a pass —
that is `pprobe`'s `SKIP` discipline and `PlacementStats::speaking`.

**The counter-proofs must isolate.** Mutation (b) in both revoke rows is a reordering, and depending
on how the code sits it can break a memory ordering at the same time and then go red for the *wrong*
reason. The first D9 counter-proof did exactly that. **After it goes red, check that the conjuncts
which fell are the ones that should have.** One mutation, one failing conjunct.

---

## 12. Work list, file by file

Line counts are **estimates**, marked as such; the file references are read.

### Prerequisite (lands alone, before v1)

| File | Change | est. |
|---|---|---|
| `crates/caprock-sched/src/lib.rs` | `detach_for_migration` refuses a thread not in a ready queue (§6d) | ~10 |
| — | counter-proof: allow it again ⇒ a migrated stopped thread must be detectable | ~20 |

### v1

| File | Change | est. |
|---|---|---|
| `crates/caprock-abi/src/lib.rs` | `sys::DEBUG_* = 21..25`, `ERR_DEBUG_BUSY = 19`, `ERR_NOT_DEBUGGABLE = 20` | ~40 |
| `crates/caprock-cap/src/object.rs` | `ObjectKind::{Debuggable, DebugRead, DebugControl}` + object generation for §6c | ~60 |
| `crates/caprock-cap/src/space.rs` | `Finalized` third list + `push_debug`/`iter_debug`, same overflow rule | ~30 |
| `crates/caprock-sched/src/lib.rs` | `BlockReasons` → `u16`, `DEBUG = 1 << 6`, `debug_stop`/`debug_release` | ~80 |
| `crates/caprock-sched/src/redirect.rs` | bump `KOPF_GEN` on a debug stop; per-thread page rule (§4a) | ~40 |
| `crates/caprock-microkit/src/lib.rs` | five dispatch arms, cap resolution, callbacks | ~200 |
| `kernel/src/system.rs` | `release_finalized_debug` beside `abort_finalized_replies`; `debug_read_mem` (target page-table walk); §6c generation re-check | ~180 |
| `kernel/src/loader.rs` | `POLICY_DEBUGGABLE` in `policy_gate`, mint + log | ~50 |
| `crates/caprock-loader/src/manifest.rs` | `POLICY_DEBUGGABLE = 1 << 4`, into `POLICY_KNOWN` | ~10 |
| six judging places (§7) | learn `DEBUG` | ~60 |
| `programs/trusted/gdbserver/` | the RSP PD | ~1500 |
| `kernel/src/…` probe + `dbg` line | the acceptance probe of §11 | ~250 |
| `test-qemu*.sh` ×3 | `check` lines, `DONE_FLAGS` +1 both arches | ~30 |
| `tools/dbg-negativ.sh` | the counter-proofs of §11 | ~150 |
| `docs/invariants.md` | the §6f edge entry | ~15 |

**v1 kernel-side ≈ 750 lines + ~1500 userspace.** For comparison: SMT (Z6 stages 0+1) and NUMA (Z8
N0–N4) together came to a comparable size on 2026-08-17, and both landed inside a day.

### v2 (not costed in detail)

`hal::debug` on **both** architectures (x86 `DR0..7`, aarch64 `MDSCR_EL1`/`DBGBVR`/`DBGBCR`),
per-thread save/restore in the context switch, the debug exception vector, `DEBUG_WRITE_REGS` with
canonicality/alignment checks. **This is the larger half of the whole project** and is where the
"no debug surface anywhere" finding of §1a is paid for.

---

## 13. Scope

Kernel-side: **3 new cap types + 1 existing (`FaultHandler`) · 5 syscalls (21..25) · 2 error codes ·
1 wait reason with `BlockReasons` widened to `u16` · 1 extended write mask · 1 third list in
`Finalized` · 6 judging places · 1 object generation · 1 report line.**

Userspace: one PD with **no privilege at all**, holding one or two capabilities.

**What is deliberately not in v1:** register writing, breakpoints, single-step, software breakpoints,
following into a server across IPC.

---

## 14. Risks I can name today

| Risk | Why it is real | Mitigation in this plan |
|---|---|---|
| `Debuggable` becomes default | convenience, one line in a loader | §2b, and §11's negative row is the guard |
| the sidecar page shares slots | the layout is not visible at the call site | §4a — decide before the mapping exists; grep for callers |
| the write mask drifts from `uebernehmbar` | two predicates for one rule | §3b — extend, do not duplicate |
| a stopped target is never released | overflow, or a path that does not drain | §6a rides on `note_finalize_overflow`, which already shouts |
| `cap_delete` copy ages | K1a's exact shape | §6a — one copy at the shared point; §11 measures the two paths **separately** |
| v2 arrives before the IPC design point is settled | it looks like a detail until the first real target | §10c names it as a decision, not a question |
| the `dbg` line passes vacuously | this is `pprobe` under KVM and `smt` under `threads=1` | speaking test in §11 |

---

## 15. Open questions this plan does not hide

- **Does the debugger's memory read authority come from `DebugRead`, or from separate memory caps?**
  Coupling them is convenient and is *ein Parameter, der zwei Bedeutungen traegt* (D9, `spawn_user`).
  Separating them makes every attach a two-cap dance. **Undecided** — but if coupled, it must be
  written down as a coupling, not discovered later.
- **Does `DEBUG_READ_MEM` respect the target's own W^X and mapping rights, or does it read raw
  physical frames behind them?** The first is more honest and cannot show unmapped memory; the second
  shows everything and is what a debugger usually wants. **Undecided**, and it changes what §11's
  memory row proves.
- ~~Multiple debuggers on one target?~~ **Answered by the `DebugRead`/`DebugControl` split** (§2c,
  §8a): unlimited readers, one stop owner, refused with `ERR_DEBUG_BUSY`.
- ~~What does a debugger see of a thread inside an IPC call?~~ **Not an open question — a named v2
  design point** (§10c).

---

## Appendix A — corrections made while writing this plan

Kept because *the corrections are the measurement*, and because a plan that hides them teaches the
next reader to trust its unmeasured parts equally.

| Claim in an earlier draft | What reading the source showed |
|---|---|
| "Two `SCHEDS` are never held simultaneously" | **False.** `migrate_to` holds two, with ascending-index order. The two comments cited were scoped to `KernelSched` (`:924`) and `least_loaded_core` (`:2720`) |
| "The release needs a new kick on the target core" | **Superseded.** `abort_finalized_replies` already does exactly this job, at the right point, for both `cap_delete` and `cap_revoke` (§6a) |
| "A stop generation must be added to the sidecar" | **Already there.** `KOPF_GEN`, word 2, with the rationale already written (§4b) |
| "Catching a crash needs a new mechanism" | **Already there.** `ObjectKind::FaultHandler`, whose doc names a debugger as a consumer (§2a) |
| "The mint policy is a loader parameter" | **Better place found.** `policy_flags` in the Ed25519-signed, kernel-hash-bound manifest makes it attested (§2b) |
| "§5 also costs DWARF, disassembly, expression evaluation" | **Wrong.** Those live in host GDB and are not written (§9) |
| "W^X is a measured assurance" | **Understated.** It is also a Verus proof (`verus/wx_invariant.rs`), which raises the bar for route 2 (§10d) |
