//! **lx-shim-demo: a Linux NIC driver excerpt, mapped onto Caprock schemes.**
//!
//! Evidence that a Linux driver schema maps onto Caprock schemes automatically:
//! a fictitious but realistic PCIe network driver excerpt (ring setup, TX path
//! with `dma_map_single`, IRQ thread with completion, watchdog timer, reset
//! workqueue), written once against [`DemoTreiber`].
//!
//! Primitives used (path dependencies, all `no_std` + `forbid(unsafe_code)`):
//! - `caprock-dma`: [`DmaPool`]/[`DmaBuf`], [`map_single`],
//!   [`GeraeteGrenzen`]/[`pruefe_grenzen`], [`BounceSlot`]
//! - `caprock-wait`: [`Park`], [`Mutex`], [`Completion`], [`Clock`],
//!   [`TimerWheel`], [`Workqueue`]
//! - `caprock-region` (`page` only): [`PAGE_SIZE`], [`order_zu_len`],
//!   [`MemMap`] for ring sizing and window bookkeeping. (`kmalloc.rs` does
//!   not exist; `heap.rs` is the process allocator and is intentionally NOT
//!   used here — the rings are DMA windows, not heap objects.)
//!
//! The line-by-line Linux-to-Caprock mapping lives in `PORTIERUNG.md`
//! (named `PORTIERUNG.md` for the repo-local thread; content is English).

#![no_std]
#![forbid(unsafe_code)]

// The crate is `no_std` (it runs in a driver PD without an OS). The test
// harness needs `std` for that — test-only, the PD build never sees it
// (same pattern as `caprock-dma` / `caprock-wait`).
#[cfg(test)]
extern crate std;

use caprock_dma::{
    BounceSlot, DmaBuf, DmaPool, GeraeteGrenzen, GrenzenFehler, StromRichtung,
    bounce_einlagern, bounce_freigeben, map_single, pruefe_grenzen,
    sync_fuer_cpu, sync_fuer_geraet, unmap_single,
};
use caprock_region::page::{MemMap, PAGE_SIZE, order_zu_len};
use caprock_wait::{
    Clock, Completion, JobId, LockError, Mutex, Park, Tid, TimerWheel, Workqueue,
    msecs_to_jiffies,
};

/// Ring order: `order_zu_len(0)` is one page = 4096 bytes of backing store.
pub const RING_ORDER: u32 = 0;
/// Descriptor bookkeeping entries per ring.
pub const RING_EINTRAEGE: usize = 256;
/// What a 32-bit NIC reports: no device address above 4 GiB is reachable.
pub const MASKE_32BIT: u64 = 0xFFFF_FFFF;
/// The workqueue job id meaning "reset the device".
pub const RESET_JOB: JobId = 7;

/// Demo window: CPU view, device view below 4 GiB, 64 KiB long.
pub const DEMO_CPU_BASIS: u64 = 0x4000_0000;
pub const DEMO_DEV_BASIS: u64 = 0x1000_0000;
pub const DEMO_LAENGE: u64 = 0x10000;
/// Device view above 4 GiB: the 32-bit mask must reject everything from here.
pub const HOCH_DEV_BASIS: u64 = 0x1_0000_0000;

/// Why a transmit request did not make it onto the ring.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TxFehler {
    /// Mapped a buffer the pool never granted (the stack/heap case:
    /// `dma_map_single` on memory outside the coherent window).
    PoolFremd,
    /// Mapped, but the device cannot reach it (mask, boundary, segment size).
    Grenze(GrenzenFehler),
    /// Mapped and reachable, but no descriptor is free. Linux unmaps on this
    /// path (`dma_unmap_single` after a full queue) — so do we.
    RingVoll,
    /// The ring lock itself failed (no waiting room / self deadlock).
    Sperre(LockError),
}

/// The demo driver: TX/RX rings over one DMA pool, one TX completion the IRQ
/// thread signals, one watchdog timer, one reset workqueue.
pub struct DemoTreiber {
    pool: DmaPool,
    grenzen: GeraeteGrenzen,
    fenster: MemMap,
    tx_ring_puffer: DmaBuf,
    rx_ring_puffer: DmaBuf,
    tx_ring: [Option<DmaBuf>; RING_EINTRAEGE],
    tx_kopf: usize,
    tx_schwanz: usize,
    tx_belegt: usize,
    sperre: Mutex,
    fertig: Completion,
    watchdog: TimerWheel,
    reset_wq: Workqueue,
}

impl DemoTreiber {
    /// Backing-store size of one descriptor ring, derived from the page order
    /// (`alloc_pages(order)` capability, not a magic literal).
    pub fn ring_bytes() -> u64 {
        match order_zu_len(RING_ORDER) {
            Some(n) => n,
            None => 0,
        }
    }

    /// Default window: device view below the 4 GiB mask.
    pub fn neu() -> Option<Self> {
        Self::mit_fenster(DEMO_CPU_BASIS, DEMO_DEV_BASIS, DEMO_LAENGE)
    }

    /// Driver over an explicit window (`dma_set_mask` capability: callers that
    /// pass a device base above 4 GiB get a driver whose TX path rejects).
    pub fn mit_fenster(cpu_basis: u64, dev_basis: u64, len: u64) -> Option<Self> {
        let mut pool = DmaPool::new(cpu_basis, dev_basis, len).ok()?;
        let lang = Self::ring_bytes();
        if lang == 0 {
            return None;
        }
        // `dma_alloc_coherent` x2: one aligned grant per ring, both views.
        let tx_ring_puffer = pool.alloc(lang, PAGE_SIZE)?;
        let rx_ring_puffer = pool.alloc(lang, PAGE_SIZE)?;
        let seiten = len / PAGE_SIZE;
        Some(DemoTreiber {
            pool,
            grenzen: GeraeteGrenzen {
                maske: MASKE_32BIT,
                grenze: 0,
                max_seg: 0,
            },
            fenster: MemMap::new(cpu_basis, seiten),
            tx_ring_puffer,
            rx_ring_puffer,
            tx_ring: [None; RING_EINTRAEGE],
            tx_kopf: 0,
            tx_schwanz: 0,
            tx_belegt: 0,
            sperre: Mutex::new(),
            fertig: Completion::new(),
            watchdog: TimerWheel::new(),
            reset_wq: Workqueue::new(),
        })
    }

    /// Which page of the described window holds the TX ring (`page_to_pfn`
    /// capability: the ring is provably inside the window, not just `u64`).
    pub fn ring_pfn(&self) -> Option<u64> {
        self.fenster.addr_zu_pfn(self.tx_ring_puffer.cpu())
    }

    /// Device view of the RX ring (for the "under the mask" assertion).
    pub fn rx_ring_dev(&self) -> u64 {
        self.rx_ring_puffer.dev()
    }

    /// Pool consumption in bytes (bounce accounting observable).
    pub fn pool_verbraucht(&self) -> u64 {
        self.pool.used()
    }

    /// Occupied TX descriptors.
    pub fn tx_belegt(&self) -> usize {
        self.tx_belegt
    }

    /// TX path: `dma_map_single` + device-limit check + ring insert.
    /// Returns the device address to program into the descriptor.
    pub fn tx_einreihen(
        &mut self,
        p: &dyn Park,
        cpu: u64,
        len: u64,
    ) -> Result<u64, TxFehler> {
        // `spin_lock_irqsave` capability: exclusion between threads of this PD.
        self.sperre.lock(p).map_err(TxFehler::Sperre)?;
        let ergebnis = self.tx_einreihen_inner(cpu, len);
        self.sperre.unlock(p);
        ergebnis
    }

    fn tx_einreihen_inner(&mut self, cpu: u64, len: u64) -> Result<u64, TxFehler> {
        // `dma_map_single`: pool-external buffers get NO device address.
        let buf = map_single(&self.pool, cpu, len, StromRichtung::NachGeraet)
            .ok_or(TxFehler::PoolFremd)?;
        // `dma_set_mask` enforcement: the device cannot reach above 4 GiB.
        pruefe_grenzen(&self.grenzen, buf.dev(), buf.len()).map_err(TxFehler::Grenze)?;
        if self.tx_belegt >= RING_EINTRAEGE {
            // Full queue: unmap again, exactly like the Linux error path.
            unmap_single(buf, StromRichtung::NachGeraet);
            return Err(TxFehler::RingVoll);
        }
        // `dma_sync_single_for_device` (a no-op on coherent x86, kept call).
        sync_fuer_geraet(&buf, StromRichtung::NachGeraet);
        let idx = self.tx_schwanz % RING_EINTRAEGE;
        self.tx_ring[idx] = Some(buf);
        self.tx_schwanz = self.tx_schwanz.wrapping_add(1);
        self.tx_belegt += 1;
        Ok(buf.dev())
    }

    /// Reap up to `hoechstens` completed TX descriptors (`dma_unmap_single`
    /// per descriptor). Returns how many were reaped.
    pub fn tx_abschliessen(&mut self, hoechstens: usize) -> usize {
        let mut n = 0;
        while n < hoechstens && self.tx_belegt > 0 {
            let idx = self.tx_kopf % RING_EINTRAEGE;
            if let Some(buf) = self.tx_ring[idx].take() {
                unmap_single(buf, StromRichtung::NachGeraet);
            }
            self.tx_kopf = self.tx_kopf.wrapping_add(1);
            self.tx_belegt -= 1;
            n += 1;
        }
        n
    }

    /// IRQ thread body, one step: completions for all TX waiters.
    /// Returns how many waiters were woken.
    pub fn irq_behandeln(&mut self, p: &dyn Park) -> usize {
        self.fertig.complete(p)
    }

    /// TX side: one waiting step on the completion (`wait_for_completion`).
    /// `Ok(true)` means "transmit done" (consumed), `Ok(false)` means
    /// "parked, call again".
    pub fn tx_warten_schritt(&mut self, p: &dyn Park) -> Result<bool, LockError> {
        self.fertig.warten_schritt(p)
    }

    /// Uncollected completions (a counter, not a flag: more completions than
    /// collections is a statement about the device).
    pub fn fertig_offen(&self) -> u32 {
        self.fertig.offen()
    }

    /// RX path for a pool-external buffer: stage a bounce piece in the pool.
    /// The caller copies foreign -> bounce (driver memcpy), the device works
    /// on the pool piece, then [`Self::rx_bounce_fertig`] releases it.
    pub fn rx_bounce_holen(&mut self, bedarf: u64) -> Option<BounceSlot> {
        bounce_einlagern(&mut self.pool, bedarf, 64)
    }

    /// RX done: `dma_sync_single_for_cpu`, then release back to the mark.
    pub fn rx_bounce_fertig(&mut self, slot: BounceSlot) -> bool {
        sync_fuer_cpu(&slot.buf, StromRichtung::VomGeraet);
        bounce_freigeben(&mut self.pool, slot)
    }

    /// Watchdog arm: `mod_timer` capability — "wake `tid` in `ms`".
    /// Ticks come from the driver's clock (`msecs_to_jiffies` capability).
    pub fn watchdog_start<C: Clock>(&mut self, clk: &C, ms: u64, tid: Tid) -> bool {
        let ticks = msecs_to_jiffies(ms, clk.hz());
        self.watchdog.after(clk.now(), ticks, tid)
    }

    /// Timer thread body: expire everything due at `now` (`timer_list`
    /// capability). Returns how many were woken (a fired watchdog resets
    /// the device via [`Self::reset_anfordern`] — caller policy).
    pub fn watchdog_ablauf(&mut self, now: u64, p: &dyn Park) -> usize {
        self.watchdog.expire(now, p)
    }

    /// Armed watchdog timers.
    pub fn watchdog_len(&self) -> usize {
        self.watchdog.len()
    }

    /// Reset path, submit side: `schedule_work` capability.
    pub fn reset_anfordern(&mut self, p: &dyn Park) -> Result<(), LockError> {
        self.reset_wq.queue(p, RESET_JOB)
    }

    /// Reset path, worker side: run one job. `true` means there was work.
    pub fn reset_ausfuehren(&mut self, p: &dyn Park, f: impl Fn(JobId)) -> bool {
        self.reset_wq.run_ein_job(p, f)
    }

    /// Queued (not yet run) reset jobs.
    pub fn reset_offen(&self) -> usize {
        self.reset_wq.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::cell::{Cell, RefCell};

    const TEST_TID: Tid = 1;

    /// Stand-in for the kernel: a wake mark that survives even when the
    /// thread is still awake (the property everything here hinges on).
    struct FakePark {
        ich: Tid,
        marken: RefCell<[u32; 8]>,
        blockiert: RefCell<u32>,
        weckrufe: RefCell<u32>,
    }

    impl FakePark {
        fn neu(ich: Tid) -> Self {
            FakePark {
                ich,
                marken: RefCell::new([0; 8]),
                blockiert: RefCell::new(0),
                weckrufe: RefCell::new(0),
            }
        }
        fn blockierte(&self) -> u32 {
            *self.blockiert.borrow()
        }
        fn weckrufe(&self) -> u32 {
            *self.weckrufe.borrow()
        }
    }

    impl Park for FakePark {
        fn me(&self) -> Tid {
            self.ich
        }
        fn park(&self) {
            let mut m = self.marken.borrow_mut();
            if m[self.ich as usize] > 0 {
                m[self.ich as usize] -= 1;
            } else {
                *self.blockiert.borrow_mut() += 1;
            }
        }
        fn unpark(&self, t: Tid) {
            self.marken.borrow_mut()[t as usize] += 1;
            *self.weckrufe.borrow_mut() += 1;
        }
    }

    /// A clock to put on the table: resolution plus a settable stand.
    struct FakeClock {
        hz: u64,
        stand: Cell<u64>,
    }

    impl Clock for FakeClock {
        fn hz(&self) -> u64 {
            self.hz
        }
        fn now(&self) -> u64 {
            self.stand.get()
        }
    }

    #[test]
    fn tx_unter_4gib_maske_ok() {
        let park = FakePark::neu(TEST_TID);
        let mut t = DemoTreiber::neu().expect("demo driver");
        let cpu = DEMO_CPU_BASIS + 0x8000;
        let dev = t.tx_einreihen(&park, cpu, 64).expect("under 4 GiB mask");
        assert_eq!(dev, DEMO_DEV_BASIS + 0x8000);
        assert_eq!(t.tx_belegt(), 1);
        // Uncontended lock on the hot path: no park, no syscall shape.
        assert_eq!(park.blockierte(), 0);
    }

    #[test]
    fn tx_ueber_maske_abgewiesen() {
        // A 32-bit card (mask 0xFFFF_FFFF) cannot reach a device view above
        // 4 GiB — the TX path must name it, not silently program it.
        let park = FakePark::neu(TEST_TID);
        let mut t =
            DemoTreiber::mit_fenster(DEMO_CPU_BASIS, HOCH_DEV_BASIS, DEMO_LAENGE)
                .expect("demo driver");
        let err = t.tx_einreihen(&park, DEMO_CPU_BASIS, 64).err();
        assert!(matches!(
            err,
            Some(TxFehler::Grenze(GrenzenFehler::MaskeVerletzt { .. }))
        ));
        assert_eq!(t.tx_belegt(), 0); // rejected, not queued
    }

    #[test]
    fn tx_pool_fremd_abgewiesen() {
        // `dma_map_single` on a stack/heap buffer: no device address exists.
        let park = FakePark::neu(TEST_TID);
        let mut t = DemoTreiber::neu().expect("demo driver");
        assert_eq!(
            t.tx_einreihen(&park, 0xDEAD_0000, 64).err(),
            Some(TxFehler::PoolFremd)
        );
        assert_eq!(t.tx_belegt(), 0);
    }

    #[test]
    fn ring_voll_weist_ab_und_schliesst_ab() {
        let park = FakePark::neu(TEST_TID);
        let mut t = DemoTreiber::neu().expect("demo driver");
        for i in 0..RING_EINTRAEGE as u64 {
            let cpu = DEMO_CPU_BASIS + 0x8000 + i * 64;
            t.tx_einreihen(&park, cpu, 64).expect("ring has room");
        }
        assert_eq!(t.tx_belegt(), RING_EINTRAEGE);
        // Mapped and reachable — but no descriptor left: unmapped again.
        assert_eq!(
            t.tx_einreihen(&park, DEMO_CPU_BASIS + 0x8000, 64).err(),
            Some(TxFehler::RingVoll)
        );
        assert_eq!(t.tx_abschliessen(RING_EINTRAEGE), RING_EINTRAEGE);
        assert_eq!(t.tx_belegt(), 0);
    }

    #[test]
    fn bounce_rundweg() {
        let mut t = DemoTreiber::neu().expect("demo driver");
        let vorher = t.pool_verbraucht();
        let slot = t.rx_bounce_holen(256).expect("bounce room");
        assert_eq!(slot.buf.len(), 256);
        // The bounce piece IS pool memory: it maps and passes the 32-bit mask.
        let sicht = map_single(
            &t.pool,
            slot.buf.cpu(),
            slot.buf.len(),
            StromRichtung::VomGeraet,
        )
        .expect("bounce maps");
        pruefe_grenzen(&t.grenzen, sicht.dev(), sicht.len()).expect("under mask");
        assert!(t.rx_bounce_fertig(slot));
        // Released back to the mark: bounce is not a leak.
        assert_eq!(t.pool_verbraucht(), vorher);
    }

    #[test]
    fn irq_complete_weckt() {
        let park = FakePark::neu(TEST_TID);
        let mut t = DemoTreiber::neu().expect("demo driver");
        // Threaded-IRQ order: the waiter arrives first, the IRQ completes.
        assert_eq!(t.tx_warten_schritt(&park), Ok(false));
        assert_eq!(park.blockierte(), 1);
        assert_eq!(t.irq_behandeln(&park), 1); // one waiter woken
        assert_eq!(t.fertig_offen(), 1);
        assert!(t.tx_warten_schritt(&park).expect("room")); // consumes it
        assert_eq!(t.fertig_offen(), 0);
        // And the fast path: complete-before-wait is never lost either.
        assert_eq!(t.irq_behandeln(&park), 0); // nobody waited
        assert_eq!(park.blockierte(), 1); // still 1: no new park happened
        assert!(t.tx_warten_schritt(&park).expect("room"));
    }

    #[test]
    fn watchdog_feuert() {
        let park = FakePark::neu(TEST_TID);
        let clk = FakeClock {
            hz: 1000,
            stand: Cell::new(1000),
        };
        let mut t = DemoTreiber::neu().expect("demo driver");
        assert!(t.watchdog_start(&clk, 100, TEST_TID)); // 100 ms = 100 ticks
        assert_eq!(t.watchdog_len(), 1);
        assert_eq!(t.watchdog_ablauf(1050, &park), 0); // not due yet
        assert_eq!(t.watchdog_len(), 1);
        assert_eq!(t.watchdog_ablauf(1100, &park), 1); // due: fired
        assert_eq!(park.weckrufe(), 1);
        assert_eq!(t.watchdog_len(), 0);
    }

    #[test]
    fn reset_job_laeuft() {
        let park = FakePark::neu(TEST_TID);
        let mut t = DemoTreiber::neu().expect("demo driver");
        assert_eq!(t.reset_offen(), 0);
        t.reset_anfordern(&park).expect("queue has room");
        assert_eq!(t.reset_offen(), 1);
        let lief = Cell::new(false);
        assert!(t.reset_ausfuehren(&park, |job| {
            assert_eq!(job, RESET_JOB);
            lief.set(true);
        }));
        assert!(lief.get());
        assert_eq!(t.reset_offen(), 0);
        assert!(!t.reset_ausfuehren(&park, |_| {})); // empty: nothing runs
    }

    #[test]
    fn ring_liegt_im_beschriebenen_fenster() {
        // `page` capability: the ring is provably inside the described window.
        let t = DemoTreiber::neu().expect("demo driver");
        assert_eq!(DemoTreiber::ring_bytes(), 4096);
        let pfn = t.ring_pfn().expect("ring lives in the window");
        assert_eq!(t.fenster.addr_zu_pfn(t.tx_ring_puffer.cpu()), Some(pfn));
        assert_eq!(t.rx_ring_dev() % PAGE_SIZE, 0);
    }
}
