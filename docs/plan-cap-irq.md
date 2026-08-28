# Plan — Stufe B: `CAP_IRQ`

Written 2026-08-26. Everything below is measured against this branch, not remembered.

---

## 0. The register entry is wrong in both directions — again

`docs/linux-kompatibilitaet-caprock.md` §5 lists **IRQ-Zustellung** as `fehlt — Stufe B`. That is
wrong twice over, and the shape is exactly the one K1a had:

**What already exists and RUNS.** The whole deferred delivery path is built, arch-neutral, and
**measured on aarch64 in every suite run**:

```
irq     : ALL PASS   (RTC-IRQ: IRQ-Cap + GIC-SPI-Routing + Deferred-IRQ-Zustellung
                      als Notification an HardwareLand)
```

| Piece | Where | State |
|---|---|---|
| `ObjectKind::Irq { intid }`, `install_irq_cap` | `kernel/src/system.rs` | built, used |
| `bind_irq(intid, ntfn, badge, core)` | `kernel/src/system.rs` | built — **this is B3** |
| `irq_hook` (IRQ context, lock-free: pending + mask) | `kernel/src/system.rs` | built |
| `drain_pending_irqs` → `signal_from_kernel` | reschedule path, `NTFNS < SCHEDS` | built |
| hook installed | `system.rs:1560`, arch-neutral | both architectures |
| x86 dispatch calls it with the raw vector | `x86_64/exception.rs:823` | built |
| IRTE encoding, allocation, SVT/SID, self-test | `x86_64/irte.rs` (1525 lines, 10 host tests) | built — **but the self-test reads back TABLE STATE only.** It never delivers. Corrected 2026-08-26: the delivery path had never been exercised, and every green line above it was a statement about state, not about effect. `msi_zustellprobe` is the counterpart that measures effect (self-IPI) |
| IRTE **hardware** access (`IrtHardware`, `IRTE_ALLOC`, `irte_vergib`, `irte_zieh_ein`) | `x86_64/vtd.rs` | built |
| Interrupt remapping enabled at boot (`GCMD_IRE`) | `x86_64/vtd.rs:988` | on |
| IRT entries default to *not present* (B-3.2) | | deliberate |

So the primitive is not missing. What is missing is **narrower and different**, and no register
line says it.

**What is actually missing.** Measured by grep, not by memory:

* **G1 — nothing programs the device.** `crates/caprock-hal/src/x86_64/pcie.rs` contains **zero**
  occurrences of `msi`/`MSI` and no capability-list walk at all. Without an MSI/MSI-X capability
  being written, the device never sends anything. *This* is "CAP_IRQ fehlt" on x86.
* **G2 — the IRTE allocation has no kernel caller.** `irte_vergib`/`irte_zieh_ein` are wired to
  hardware and called by **nobody** outside `vtd.rs`'s own self-test. §5 says "Kodierung und
  Vergabe stehen als reine Funktion, verdrahtet ist nichts" — understated: the hardware access
  layer exists too. What is missing is one call site.
* **G3 — `bind_irq` checks NO capability.** Its signature is
  `bind_irq(intid: u32, ntfn: usize, badge: u64, core: usize)` — raw ids. Its own doc-comment
  claims *„Nutzt das HardwareLand-Backend (über die IRQ-Cap autorisiert)"*, and the signature
  cannot support that claim. Harmless today (only the kernel calls it); it is **the hole** the
  moment it becomes a syscall. Same family as *ein Waechter prueft die EXISTENZ eines Grundes, nie
  seine WAHRHEIT*.
* **G4 — there is no syscall.** The ABI has no `BIND_IRQ`. A driver PD cannot bind; the RTC test
  binds from inside the kernel.
* **G5 — `NIRQ_BIND = 4` is an unnamed capacity.** `bind_irq` returns `bool`. As a syscall that is
  D11 verbatim: *wer eine Kapazitaet einfuehrt, muss den Ueberlauf benennen.*
* **G6 — the x86 half of masking is a stub.** `intc::mask_intid` is `{}` and `route_spi` says
  „noch nicht portiert". For **MSI that is correct** — edge-triggered, there is no distributor to
  mask — but then the re-trigger protection `irq_hook` relies on does not exist on x86, and the
  level/edge difference must become a **named decision** instead of an accident.
* **G7 — the driver polls.** `programs/hardware/virtio-blk`: `MAX_POLL = 50_000_000`.

---

## 1. The decision that makes this cheap: the driver programs its own MSI-X table

virtio-modern devices offer **MSI-X**, not classic MSI. The MSI-X table lives in a BAR — and the
driver PD **already holds the BAR cap** (slot 4). So the natural implementation is: the driver
writes its own table rows.

The obvious objection is that this hands the driver a free choice of interrupt vector, i.e. it
could target any vector on any core. **Interrupt remapping is exactly what removes that objection**,
and `irte.rs` was already written for it:

```
MSI-X address  =  msi_addr(handle)      // an IRTE HANDLE, not a vector
IRTE[handle]   =  { vector, apic_id, SID, SVT_SID }    // kernel-owned, driver cannot write it
```

A driver that writes a handle it was not given hits an entry that is either **not present**
(B-3.2 made that the default) or carries a **different SID** — and VT-d rejects it, because
`SVT_SID` makes the entry check the requester. Without SVT/SID, an IRTE would accept an MSI from
any device, and interrupt delivery would be precisely the channel A-5.4 closed on the DMA axis.
`Vektorform::MsiX`'s own doc already says *„Der Treiber schreibt `anzahl` Zeilen"* — the design
anticipated this; it was never wired.

**Consequence for the shape of Stufe B:** the kernel does *not* mediate MSI-X writes. It allocates
the IRTE, mints the `Irq` cap, and binds. That is three small pieces, not a new subsystem — and it
is why B1 belongs in `assign_driver_device`, next to the DMA attach, exactly as §5 says.

---

## 2. What to build

### B1 · IRTE allocation at device assignment

In `assign_driver_device`, next to `dma_attach`, **fail-closed** (a half assignment is worse than
none — the pattern the function already follows for cfg/bar/dma/shared):

1. **Walk the capability list for `CAP_ID_MSIX` (`0x11`)** — `pcie.rs` has no capability walk at
   all today. Learn the table's BAR index and offset. **Refuse the device if the table lies inside
   the BAR that is offered to the driver** (E-B2), with its own reason.
2. `sid_from_bdf` from the device's RID — already available as `DriverDevice.rid`.
3. `vtd::irte_vergib(sid, Vektorform::MsiX, 1)` → `MsiZiel { handle, vector }`.
4. **Write the MSI-X row** (address `msi_addr(handle)`, data `msi_data()`) — kernel work, because
   interrupt routing is kernel authority (E-B2).
5. Record `(handle, vector)` **and the binding slot** in `DriverAssign` — beside `dma_gewuenscht`,
   for the same reason: the report must be able to hold *granted* against *requested*, and the
   binding lives here rather than in a global table (E-B3).
6. On any later failure in the function: take the grant back — **and in that order**, see §3a.

**SVT/SID in the same step, not later.** An IRTE without a source check is the hole; adding it
afterwards would mean a window in which the entry exists and does not check.

### B2 · The `Irq` cap

`install_irq_cap(vector, Rights::READ)` into the driver's Loader-ABI slot. §5 says "Slot 7. Damit
ist ein Treiber-PD bei 7 von 8" — **that arithmetic is stale**: since 2026-08-26 the cap budget is
an account per PD and the reachable ceiling is `NCAPS = 16`, so slot 7 is 7 of up to 16.

The driver also needs the **handle** (to write into its MSI-X table) — and a handle is not a
capability, it is a number the cap already authorises. It goes in the transfer area (slot 6), not
in a new cap.

### B3 · `SYS_BIND_IRQ`

`bind_irq` exists; what is missing is the syscall and the authority check.

```
x1 = Irq-Cap-Slot · MSG0 = Notification-Cap-Slot · MSG1 = Badge
```

* Resolve **both** caps from the caller's Cspace. The `Irq` cap's `intid` is the vector — the
  caller does **not** pass a vector, it passes the cap. That closes G3 structurally rather than by
  checking a number against a table.
* The binding is looked up **through the assignment** (E-B3), so `ERR_IRQ_FULL` means *this device
  has no free vector* — a local statement, and the caller is **not blocked** (D11 verbatim, the
  same shape as `ERR_EP_FULL` and `ERR_LOAD_BUSY`).
* Rebinding the same cap replaces its entry rather than consuming a second one; otherwise a driver
  that reloads exhausts its own device.

### B4 · The driver waits instead of polling

`virtio-blk` tells the device **which vector serves the queue** (`queue_msix_vector` in the virtio
common config — that register *is* in its BAR), then `WAIT`s on its notification instead of
spinning to `MAX_POLL`. It does **not** write the MSI-X row; that happened in B1 (E-B2).

`caprock-virtio` has **zero** MSI-X support today — this is the largest single piece of B, and it
is in the crate that is deliberately dependency-free, so it is testable on the host.

---

## 2a. The order in which a grant is taken back

Every abort path after the IRTE exists must undo it, and the order is an assurance rather than
tidiness:

> **Source before translation: still the device, then withdraw the IRTE, then release the vector.**

Reversed — withdraw first, mask second — there is a window in which the device is still armed and
its entry is already gone. An MSI arriving there hits a **not present** IRTE. That is fail-closed
as *delivery*, which is why it is easy to wave through; what it produces is a **VT-d fault with no
owner**, on an ordinary abort path, counted against nothing. Whoever later counts remapping faults
sees it without an attribution.

This is exactly the order the DMA teardown already runs, and the first step is *stilling the
device*, not clearing its enable bit: **Function Mask, not `Enable = 0`**. A function whose MSI-X
Enable is clear falls back to **INTx** (PCI 3.0 §6.8.2 — a function must not use INTx while MSI-X
is enabled), and nothing in this kernel routes INTx. Clearing Enable would trade a remapped
message for a line interrupt nobody handles.

Before `msix_enable` has run the order does not matter — but the paths are not allowed to differ
on that, because then a later change has to re-derive which side of the line it is on. All six
abort paths go through **one** revocation (`msi_revoke`), and the revocation is uniform; on the
early paths the quiesce is simply a write into a device that was never armed.

## 3. How it is measured

A line `irqmsi`, in the load suite (it is the suite with devices), arch-gated to x86:

| conjunct | says |
|---|---|
| `irte-vergeben` + `handle`/`vektor` in the report | the entry exists and the report can name it — without this every number below is unanchored |
| `svt-gesetzt` | the entry checks the requester. Read **back from the table**, not from the request — *ein Pruefer, der die gepruefte Groesse nachrechnet statt sie zu lesen, prueft eine zweite Wirklichkeit* |
| `msix-angeboten` and `msi-da` **both in the report** | the device's offer beside the grant, like C2's *requested* beside *granted*. The line's own precondition, and it is **printed**, not assumed |
| `msix-angeboten` ⟹ `msi-da` | **a device that offers MSI-X and got no vector is a FAILED GRANT.** Without this conjunct that case is indistinguishable from a device that has none — and the harmless reading hides the serious one |
| `msi-da` ⟹ (`poll-runden == 0` **and** progress) | **the point**, and it has to be conditional. The driver made a request and made progress without polling once. „Es kam an" and „er hat es selbst gemerkt" are different statements, and only the poll counter separates them — but a device without a vector polls **lawfully**, so an unconditional conjunct is red for a legitimate reason |
| `zugestellt > 0` (`IRQ_DELIVERED`) | the kernel really delivered — the counter already exists |
| `badge == erwartet` | the notification carries **this** driver's badge, not any value. Same half the IPC badge probe needed |
| `zweite-PD-unberuehrt` | the other driver PD's counter did not move. A-5.4 on the interrupt axis, and the reason SVT/SID exists |
| `kein-retrigger` | triggered twice before the drain, **at most one** delivery arrived. The assurance from E-B1 — same conjunct on both architectures, different mechanism underneath, and the only line that tells x86's empty `mask_intid` apart from a missing protection |

**Why conditional, and why not a `SKIP`.** „No interrupt is no failure" (§2, the decision that
keeps Stufe B an *extension* of A-5.1 rather than its precondition) collides head-on with
`poll-runden == 0`: a device without a vector polls, and it is right to. An unconditional conjunct
therefore has exactly two ends, and both are bad — either the line gets switched off for the
vectorless case, and then it measures **nothing** when a device silently fails to get a vector; or
it goes red for a lawful reason, and the next person removes it.

The second half is the one that carries: **`msi-da == false` must not leave the line green the
same way `msi-da == true` does.** Otherwise a failed grant is indistinguishable from one that was
never requested — the same shape as „so gewollt" against „stillschweigend gekuerzt" at C2, which
is why the answer is the same too: carry the **offer** next to the **grant** and let the report
print both. `msix-angeboten` comes from the device's capability list at offer time, `msi-da` from
the allocation; the kernel keeps them apart (`DriverAssign::msi_angeboten`, `driver_msi` returns
five numbers, not four), and `msix_cap != 0` is what survives even when the table's BAR is
unassigned — which is precisely the silent-failure case.

A `SKIP` remains for one case only: **the device carries no MSI-X capability at all**. It has to
name the device (`vendor:device`) when it fires, because on q35 with virtio-modern it should never
fire — a `SKIP` that nobody can tell from an expected one is how a measurement quietly stops
measuring.

### Counter-proofs (`tools/irqmsi-negativ.sh`)

* **M1 — the IRTE is not written (`zieh_ein` before the driver starts).** Nothing arrives; the
  probe reports the timeout, `poll-runden` stays 0, and progress stops. *This is the tree of
  today.*
* **M2 — `SVT_SID` off.** The entry still delivers, so the positive conjuncts stay green — and a
  **forged MSI from the other device's SID** now arrives. Without M2, `svt-gesetzt` would be a
  field nobody checks the effect of.
* **M3 — `bind_irq` does not resolve the cap but takes the raw vector** (i.e. G3 restored). A PD
  that holds *no* `Irq` cap binds successfully → the authority conjunct falls, delivery stays
  green. This is the mutation that shows B3 is a gate and not a convenience.
* **M4 — `NIRQ_BIND` overflow returns `OK`.** The fifth bind reports success and no interrupt is
  delivered to it: a capacity without a name.
* **M5 — the badge is hardcoded to 0.** `badge == erwartet` falls, everything else stays green —
  the same positive control the IPC badge probe needed, one layer down.
* **M6 — the grant fails silently** (`msi_vektor_reservieren` returns `None`, or the readback
  check is made to fail). The device still offers MSI-X, so `msix-angeboten ⟹ msi-da` falls while
  the driver polls its way to a correct result. **Without M6 the conditional formulation is
  untested**, and a conjunct guarded by a premise that never goes false is a conjunct that never
  judges — *ein Negativtest kann eine Eigenschaft absichern, die niemand benutzt.*

---

## 4. The dependency on Stufe A — sharper than "A before B"

§5 argues: *„Ohne Fristen ist ein ausbleibender Interrupt ein Hänger und von einem langsamen Gerät
nicht zu unterscheiden. Dann ist für CAP_IRQ keine Gegenprobe schreibbar."*

Half of that holds, and the half that does not changes the order:

* The **positive** direction needs no deadline. „The driver progressed without polling" is
  measurable today: the poll counter is a number the driver already keeps.
* The **counter-proof** needs a deadline — but **the measurer's, not the ABI's.** The kernel-side
  probe waits N ticks and then judges; the driver may block in `WAIT` forever. That is exactly how
  `ckptcut` and `arena` already wait for state (`warte(ticks)`, bounded, observing the quantity
  itself).

**So Stufe B can be built and accepted before Stufe A.** What still needs A is not the measurement
but the *product*.

### The debt, by name

> **Before Stufe A: a driver PD whose interrupt fails to arrive hangs irrecoverably.**

That is not a latent shortcoming, it is a **property of the delivered state**, and it is written
here in the form in which it has to be read to someone. `WAIT` has no deadline; the driver blocks;
nothing wakes it.

**And the obvious mitigation is forbidden.** Building a poll fallback into the driver — spin for N
rounds, then fall back — would undermine `poll-runden == 0`, i.e. the very conjunct that carries
`irqmsi`. The line would stay green while measuring something else: exactly *ein Pruefer, der etwas
anderes misst, als er behauptet.*

So: **no intermediate solution between B and A.** The gap is borne and named, not papered over.


---

## 5. Explicitly out of scope

* **More than one vector per device.** One vector = single queue. Correct for virtio-blk; every
  real NIC and NVMe under load wants one vector per queue. §5 already says this is *the second
  driver, not a distant expansion stage* — `irte::vergib` takes `anzahl` and handles the MSI
  contiguity rule, so the allocator is ready and the driver side is not.
* **IRQ affinity and shared vectors.**
* **aarch64.** GICv3/ITS is a different mechanism, and the existing `irq` line already covers the
  aarch64 delivery path with a real device (RTC). Stufe B is the **x86 MSI** half.
  **Where the seam sits is part of the scope**, not an implementation detail: `msi_grant` /
  `msi_revoke` are the arch-gated pair, and the aarch64 side returns „no vector" — the state that
  architecture is in today, the driver polls. The first cut had `hal::vtd` calls directly in the
  arch-neutral body of `assign_driver_device` and broke the aarch64 build (E0433 × 4, E0425 × 4):
  third instance of *arch-neutral kernel code calls a HAL function only x86 has*, and the gate for
  it (`aarch64-bau` in `tools/abnahme.sh`) exists — it had not been run on this state.
* **Classic MSI.** `Vektorform::Msi` is modelled (with its alignment caveat written down) and
  unused; virtio-modern is MSI-X.

---

## 6. The three decisions — settled 2026-08-26

They were listed as open in the first draft. All three now have an answer, and two of them turned
out to be smaller than they looked.

### E-B1 · The asymmetry is an ASSURANCE, not an implementation

The accident can be removed without unifying anything — the two architectures really are different
here, and pretending otherwise would be the mistake.

> **The named property: after delivery and before the drain, the same vector must not fire again.**

* **aarch64** establishes it with `mask_intid` at the distributor.
* **x86/MSI** establishes it through the delivery form: MSI is edge-triggered, there is no
  level-triggered case to re-assert.

Both satisfy the same assurance by different means. `intc::mask_intid` on x86 is then no longer
"empty" but **"satisfied by the delivery form"** — documented at the place where `irq_hook` relies
on it, not in the HAL where nobody reads it.

**And the check becomes writable, identically on both architectures:** trigger twice before the
drain, at most **one** delivery may arrive. Different mechanism underneath, same conjunct on top.
Without that line, x86's empty function and aarch64's mask are indistinguishable from each other
*and* from a missing protection.

This matters more than it looks, because it returns with MSI-X: there the question is **per
vector**, not per device.

### E-B2 · MEASURED — the driver holds exactly one BAR, and that is an accident today

`bringup.rs:6795` picks the BAR **that contains the virtio common config**
(`common >= b && common < b + len`) and offers only that one. The MSI-X table lives in whichever
BAR the MSI-X capability names — for virtio-pci usually a different one. So today the driver
**cannot** write MSI-X address or data words at all.

That is the good variant. It is not a decision, it is a property of one device's BAR layout, and
*eine Eigenschaft, die aus einer Groessenrelation folgt statt aus der Struktur, verschwindet beim
naechsten Messwert.* So it becomes a rule:

1. At offer time the kernel walks the capability list for `CAP_ID_MSIX` (`0x11`) and learns the
   table's BAR index and offset.
2. **If the table lies inside the offered BAR, the device is refused** — fail-closed, with its own
   reason. Carving a hole out of a mapped BAR is the other option and is worse: it needs
   page-granular exclusion inside a region the driver otherwise owns whole.
3. **The kernel writes the MSI-X row**, the driver picks only the queue index
   (`queue_msix_vector` in the common config, which *is* in its BAR).

The rule behind it is the one the rest of the construction already follows: **the handle is a
number, not an authority — and so is the vector.** A driver that could choose its own IRTE index
would be choosing where its interrupt lands; SVT/SID catches the *delivery*, but there is no reason
to rely on a second line of defence when the first one is free.

**This moves the capability-list walk from B4 into B1.** It is kernel work either way — interrupt
routing is kernel authority, exactly like the IRTE — and it is not a driver in the HAL: `pcie.rs`
already reads BARs and the config page at bringup.

### E-B3 · No account needed — the binding hangs on the assignment

`NIRQ_BIND = 4` global is shape-identical to `CAP_BUDGET_PER_PD` before it became an account. But
unlike the cap budget there is **already a natural ceiling**: one vector per device (Stufe B's own
non-goal), hence implicitly **one binding per assigned device**.

So the binding hangs on `DriverAssign`, not on a global counter. *Wer ein Geraet hat, hat genau die
Bindungen dieses Geraets* — a driver PD cannot take the resource from another, because the
assignment is already the place where it is handed out. No second bookkeeping, no ratchet, no
exhaustion path to measure.

`ERR_IRQ_FULL` stays, but it means something **local**: *this device has no free vector left*. That
is also what carries into the MSI-X expansion, where the bound is "vectors of this device" rather
than a global number that has to be renegotiated with the second driver.

## 7. Order

1. **B1 + B2** (IRTE allocation and the cap at assignment) — no ABI change, measurable on its own
   as "the entry exists, carries SVT/SID, and is read back from the table".
2. **B3** (`SYS_BIND_IRQ` with the cap check) — small, and it closes G3 and G5 together.
3. **B4** (MSI-X in `caprock-virtio` + the driver) — the largest piece, host-testable.
4. The counter-proofs. **M1 first**: it is the tree of today, and if it does not go red, nothing
   below it means anything.
