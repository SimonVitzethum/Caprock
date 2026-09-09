# Porting map: Linux NIC excerpt → Caprock (`lx-shim-demo`)

One fictitious but realistic PCIe network driver excerpt, mapped line by line.
Left: the Linux original (as a comment — this file documents, it does not
compile against Linux). Right: the stencil replacement as written in
`src/lib.rs` against [`DemoTreiber`].

Conventions: `pool` is the granted DMA window (`DmaPool`), `grenzen` carries
the 32-bit device mask (`GeraeteGrenzen { maske: 0xFFFF_FFFF, .. }`), `p`
is the `Park` contract (sleep/wake), `clk` the driver clock.

## 1. Ring setup — `dma_alloc_coherent` → `DmaPool::alloc`

```c
/* Linux (probe): */
ring->desc = dma_alloc_coherent(&pdev->dev, RING_LEN * sizeof(*ring->desc),
                                &ring->dma_handle, GFP_KERNEL);
if (!ring->desc)
        return -ENOMEM;
```

```rust
// Caprock stencil (DemoTreiber::mit_fenster):
let bytes = DemoTreiber::ring_bytes(); // order_zu_len(RING_ORDER), no magic literal
if bytes == 0 { return None; }
let tx_ring_puffer = pool.alloc(bytes, PAGE_SIZE)?; // one grant, both views
let rx_ring_puffer = pool.alloc(bytes, PAGE_SIZE)?;
```

Why it maps 1:1: `dma_alloc_coherent` hands out CPU view + device view that
belong together by convention (two loose `u64`). `DmaBuf` carries both and
cannot be unpacked the wrong way round; the pool rejects the identity map at
construction (`PoolError::Identitaet`).

## 2. TX path — `dma_map_single` + mask → `map_single` + `pruefe_grenzen`

```c
/* Linux (ndo_start_xmit): */
dma = dma_map_single(&pdev->dev, skb->data, len, DMA_TO_DEVICE);
if (dma_mapping_error(&pdev->dev, dma))
        return NETDEV_TX_BUSY;
if (dma + len - 1 > dev->dma_mask) { /* 32-bit card, high IOVA */
        dma_unmap_single(&pdev->dev, dma, len, DMA_TO_DEVICE);
        return NETDEV_TX_BUSY;
}
desc->addr = dma; desc->len = len;
dma_sync_single_for_device(...);
spin_unlock_irqrestore(...);
```

```rust
// Caprock stencil (DemoTreiber::tx_einreihen_inner):
let buf = map_single(&pool, cpu, len, StromRichtung::NachGeraet)
    .ok_or(TxFehler::PoolFremd)?;                       // pool-external: no IOVA, by design
pruefe_grenzen(&grenzen, buf.dev(), buf.len()).map_err(TxFehler::Grenze)?; // 32-bit mask
if self.tx_belegt >= RING_EINTRAEGE {
    unmap_single(buf, StromRichtung::NachGeraet);        // full queue: unmap again
    return Err(TxFehler::RingVoll);
}
sync_fuer_geraet(&buf, StromRichtung::NachGeraet);       // no-op on coherent x86, kept call
// ... store buf, return buf.dev() for the descriptor
self.sperre.lock(p) / unlock(p)                         // spin_lock_irqsave capability
```

Two deliberate differences: the mask check is a typed `GrenzenFehler`
(`MaskeVerletzt { dev, len, maske }`) instead of an open-coded comparison,
and the direction (`NachGeraet` = `DMA_TO_DEVICE`) is mandatory but
unevaluated on coherent x86 — so a future non-coherent mapping can evaluate
it without touching callers.

## 3. RX path, foreign buffer — copy to bounce → `bounce_einlagern`

```c
/* Linux (RX copy-break / stack buffer case): */
if (!dma_capable(...) || object_is_on_stack(skb->data))
        /* no dma_map_single on this memory: copy into the coherent pool */;
```

```rust
// Caprock stencil (DemoTreiber::rx_bounce_holen / rx_bounce_fertig):
let slot = bounce_einlagern(&mut pool, bedarf, 64)?; // mark + grant, atomically paired
// ... driver memcpy foreign -> slot.buf (CPU view), device works on slot.buf (IOVA)
sync_fuer_cpu(&slot.buf, StromRichtung::VomGeraet);
bounce_freigeben(&mut pool, slot) // back to the stored mark: bounce is not a leak
```

`DmaPool::map` returns `None` for pool-external addresses — that refusal IS
the feature (a stack buffer must never gain an IOVA onto foreign memory).
The mark travels *with* the slot (`BounceSlot { buf, marke }`) so release
cannot use a foreign or stale mark.

## 4. IRQ thread — `kthread` + `completion` → `Completion`

```c
/* Linux (threaded IRQ + waiter): */
irqreturn_t nic_irq_thread(int irq, void *data) { complete(&tx_done); return IRQ_HANDLED; }
wait_for_completion(&tx_done); /* ... transmit done ... */
```

```rust
// Caprock stencil:
self.fertig.complete(p)              // IRQ thread: "done", wakes ALL waiters
self.fertig.warten_schritt(p)?       // TX side: Ok(true) = done (consumed)
```

`fertig` is a counter, not a flag: a `complete()` arriving *before* anyone
waits is preserved (the normal case — the IRQ thread is faster than the
waiter), and surplus completions stay observable via `fertig_offen()`.
The check-then-park order plus the kernel wake mark close the lost-wakeup
window, exactly as in the `caprock-wait` contract.

## 5. Watchdog timer — `mod_timer` → `TimerWheel`

```c
/* Linux (tx watchdog): */
mod_timer(&priv->watchdog, jiffies + msecs_to_jiffies(100));
/* ... timer fires: netif_tx_timeout -> reset ... */
```

```rust
// Caprock stencil:
let ticks = msecs_to_jiffies(ms, clk.hz()); // saturating, never wraps to 0
self.watchdog.after(clk.now(), ticks, tid)  // arm: "wake tid in <ticks>"
self.watchdog.expire(now, p)                // timer thread: wake all due, FIFO
```

Saturation runs through the whole chain: `msecs_to_jiffies` saturates
instead of overflowing (an overflow-to-zero would let the timeout expire
instantly), `after` saturates the due tick toward the end of time. Who turns
the wheel (`expire` with a fresh `now`) is the driver's business — typically
the timer thread; the crate guards the list, not the tick.

## 6. Reset workqueue — `schedule_work` → `Workqueue`

```c
/* Linux (reset deferred work): */
schedule_work(&priv->reset_work);
/* ... worker: reset_work_fn() runs the reset ... */
```

```rust
// Caprock stencil:
self.reset_wq.queue(p, RESET_JOB)?       // submit: Err(KeinWarteplatz), never silent drop
self.reset_wq.run_ein_job(p, |job| { /* reset */ }) // worker: oldest first (FIFO)
```

`queue` wakes nobody — the list does not know who pulls, and a wakeup to
nobody would be a mark to nobody. Who needs a sleeping worker pairs a
`Completion` next to it (list here, wakeup there, one job each). `flush()`
is a proof (`len == 0`), not a blockade: blocking would mean parking
without a wakeup.

## What this demo does NOT show (honest list)

- **Multi-vector MSI-X.** One `Completion`, one IRQ thread skeleton. Vector
 -to-queue affinity, per-queue completions, and affinity masks are not mapped.
- **Real BAR / MMIO.** No register is touched; descriptors are bookkeeping
  (`Option<DmaBuf>` arrays), not device-visible memory. MMIO mapping and
  doorbells belong to a HAL/loader grant and are out of scope.
- **Variable Cspace.** `Tid`/`JobId` are plain `u64` constants (`TEST_TID`,
  `RESET_JOB`); no capability slots are minted, delegated, or revoked.
- **NAPI polling.** No poll budget, no `napi_complete`/`napi_schedule`
  equivalent — the RX path is bounce accounting, not a poll loop.
- **`dma_map_sg` path.** `map_sg` exists in `caprock-dma` but the demo maps
  single fragments only; scatter-gather merging rules are not exercised.
- **Non-coherent DMA.** `sync_*`/`unmap_*` are documented no-ops on coherent
  x86; cache maintenance on non-coherent mappings is a call site, not code.
- **Error interrupts / AER, suspend-resume, statistics/ethtool.** No
  equivalent is written; the watchdog Expiry → `reset_anfordern` policy is a
  caller decision shown only as a comment, not wired in.
- **Heap allocation.** `caprock-region::heap` is intentionally unused (rings
  are DMA windows, not heap objects); only `page` (`PAGE_SIZE`,
  `order_zu_len`, `MemMap`) is exercised. `kmalloc.rs` does not exist.
