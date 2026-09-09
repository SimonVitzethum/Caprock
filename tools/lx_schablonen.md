# S0 A-Header-Vertrag: Linux-Symbol → Caprock-Schema

Stand: 2026-09-09. Quelle: `docs/linux-kompatibilitaet-caprock.md`
Abschnitt 2 (A/B-Kante, Z. 34–182), Abschnitt 5 (Caprock-Lücke, Z. 331–366),
M1/M2 (Z. 622–641); Primitive geprüft gegen
`crates/caprock-dma/src/lib.rs`, `crates/caprock-wait/src/lib.rs`,
`crates/caprock-region/src/{lib,page,heap}.rs`, `crates/caprock-sync/src/lib.rs`.

## Lesart

- **Klasse A** wird beim Übersetzen durch Schablonen auf Caprock abgebildet.
  Eine Zeile ohne Schablone ist ein **Übersetzungsfehler**, kein leerer Stub.
- **Klasse B** wird aus dem echten Linux-Quellbaum übersetzt und bekommt
  bewusst **keine** Schablone (`fehlt:bewusst`).
- **Art A-rein** = reine Ersetzungsregel, nichts bleibt im Binary zurück.
  **Art A-Zustand** = die Regel erzeugt Code mit Laufzeitzustand (das Risiko
  der Schicht).

## Statuswerte

| Status | Heißt |
|---|---|
| `vorhanden:formell benannt` | Schema existiert und ist benannt (Regel oder PD-Crate) |
| `vorhanden:noch ohne Frist` | Schema existiert, blockierende Form wartet auf die PARK-Frist (A2-Rest) |
| `fehlt:Kernel` | Nicht gebaut; braucht Caprock-seitige Arbeit (Kernel oder PD-Crate) |
| `fehlt:bewusst` | Bewusst keine Schablone (Klasse B: wird übersetzt, nicht ersetzt) |

## Vertragstabelle (maschinenlesbar: `lx_symbols.py` parst sie)

| Linux-Symbol | Klasse | Caprock-Schema | Art | Status |
|---|---|---|---|---|
| kmalloc | A | caprock-region Heap (Slab-Klassen, heap.rs) | A-Zustand | vorhanden:formell benannt |
| kfree | A | Kmaloc::kfree (Vorrat/Recycling; Heap-dealloc ist unsafe und bleibt drin) | A-Zustand | vorhanden:formell benannt |
| ksize | A | caprock-region kmalloc::ksize (Klassengroesse, truesize) | A-Zustand | vorhanden:formell benannt |
| krealloc | A | caprock-region Kmaloc::krealloc (Umzug ohne Kopie — kopieren obliegt dem Shim) | A-Zustand | vorhanden:formell benannt |
| GFP_KERNEL, GFP_ATOMIC | A | caprock-region Gfp (ATOMIC erreicht den Wachstums-Pfad gar nicht: Modul-Vorrat) | A-Zustand | vorhanden:formell benannt |
| alloc_pages | A | caprock-region page: order_zu_len + MemMap, Vergabe via Heap-Large-Pfad | A-Zustand | vorhanden:formell benannt |
| free_pages | A | Region-Release (Heap-Large-shrink) | A-Zustand | vorhanden:formell benannt |
| get_page | A | PageDesc::get_page (saettigend, page.rs) | A-Zustand | vorhanden:formell benannt |
| put_page | A | PageDesc::put_page (Unterlaufschutz) | A-Zustand | vorhanden:formell benannt |
| page_address | A | MemMap::pfn_zu_addr (reine Arithmetik) | A-rein | vorhanden:formell benannt |
| mutex_lock | A | caprock-wait Mutex (Park-Vertrag, null Syscalls unbestritten) | A-Zustand | vorhanden:formell benannt |
| spin_lock | A | caprock-wait Spinlock + Preempt-Gatter je Thread (E3: nur Kernwechsel derselben PD) | A-rein | vorhanden:formell benannt |
| spin_lock_irqsave | A | caprock-sync irq_save_disable + SpinLock; PD-seitig threaded-IRQ-Modell (Mutex) | A-rein | vorhanden:formell benannt |
| atomic_* | A | core::sync::atomic (Sprachmittel, kein Caprock-Schema noetig) | A-rein | vorhanden:formell benannt |
| msleep | A | timeout::msleep ueber TimeoutPark + Clock (SYS_PARK_TIMEOUT, ABI 29) | A-Zustand | vorhanden:formell benannt |
| jiffies | A | caprock-wait jiffies() ueber Clock | A-rein | vorhanden:formell benannt |
| msecs_to_jiffies | A | caprock-wait msecs_to_jiffies (saettigend, stutzt ab) | A-rein | vorhanden:formell benannt |
| timer_setup | A | TimerWheel::after | A-Zustand | vorhanden:formell benannt |
| mod_timer | A | TimerWheel::after (erneutes Eintragen) | A-Zustand | vorhanden:formell benannt |
| del_timer | A | TimerWheel::absagen (Austragen ohne Wecken, idempotent) | A-Zustand | vorhanden:formell benannt |
| queue_work | A | Workqueue::queue | A-Zustand | vorhanden:formell benannt |
| queue_delayed_work | A | Komposition msleep + Workqueue::queue (kein eigener Typ noetig) | A-Zustand | vorhanden:formell benannt |
| flush_workqueue | A | Workqueue::flush (nur Leere-Nachweis, kein blockierendes Flush) | A-Zustand | vorhanden:noch ohne Frist |
| wait_event | A | caprock-wait wait_event (pruefen, einreihen, parken) | A-Zustand | vorhanden:formell benannt |
| wait_event_timeout | A | timeout::wait_event_timeout (absolute Frist, Austragen bei Ablauf) | A-Zustand | vorhanden:formell benannt |
| complete | A | Completion::complete (Zaehler, kein Flag) | A-Zustand | vorhanden:formell benannt |
| wait_for_completion | A | Completion::warten | A-Zustand | vorhanden:formell benannt |
| wait_for_completion_timeout | A | Completion::warten_timeout (Zaehler bleibt bei Ablauf stehen) | A-Zustand | vorhanden:formell benannt |
| ioremap | A | Kernel/HAL-MMU-Mapping (gemessen, §5) | A-Zustand | vorhanden:formell benannt |
| readl | A | Load auf gemapptes BAR + Barriere | A-rein | vorhanden:formell benannt |
| writel | A | Store auf gemapptes BAR + Barriere | A-rein | vorhanden:formell benannt |
| ioread32 | A | wie readl (32-Bit-Form) | A-rein | vorhanden:formell benannt |
| iowrite32 | A | wie writel (32-Bit-Form) | A-rein | vorhanden:formell benannt |
| wmb | A | Fence-Intrinsic | A-rein | vorhanden:formell benannt |
| rmb | A | Fence-Intrinsic | A-rein | vorhanden:formell benannt |
| dma_alloc_coherent | A | DmaPool::alloc (Name abweichend, Faehigkeit gleich) | A-Zustand | vorhanden:formell benannt |
| dma_map_single | A | map_single (Pool-Arithmetik, None ausserhalb des Pools) | A-rein | vorhanden:formell benannt |
| dma_unmap_single | A | unmap_single (No-op, kohärent auf x86) | A-rein | vorhanden:formell benannt |
| dma_map_sg | A | map_sg (nie verschmelzend: Eingabezahl oder 0) | A-rein | vorhanden:formell benannt |
| dma_sync_single_for_cpu | A | sync_fuer_cpu (No-op, kohärent) | A-rein | vorhanden:formell benannt |
| dma_set_mask | A | GeraeteGrenzen::maske + pruefe_grenzen | A-rein | vorhanden:formell benannt |
| request_irq | A | IRQ-Cap + bind_irq (B1-B3 gemessen; B4-Warten und Multi-Vektor offen) | A-Zustand | vorhanden:formell benannt |
| request_threaded_irq | A | Threaded-IRQ-Modell (Completion + WAIT, Warteweg) | A-Zustand | vorhanden:formell benannt |
| pci_enable_device | B | aus Linux-Quellen uebersetzt (drivers/base, PCI) | — | fehlt:bewusst |
| pci_request_regions | B | aus Linux-Quellen uebersetzt | — | fehlt:bewusst |
| dev_err | A | keines (Senke in Klassenschicht/Router offen) | A-Zustand | fehlt:Kernel |
| printk | A | keines (Senke in Klassenschicht/Router offen) | A-Zustand | fehlt:Kernel |

## Ehrliche Lücken (Querschnitt, nicht je Symbol)

Stand: PARK-Frist (SYS_PARK_TIMEOUT, ABI 29, Dispatch in microkit), Präemptions-Gatter
(Spinlock + Preempt in caprock-wait), kmalloc/GFP/ksize/krealloc (caprock-region kmalloc),
del_timer (TimerWheel::absagen) und delayed_work (msleep+queue-Komposition) sind seit diesem
Zug vorhanden und host-getestet. Übrig bleibt:

1. **Multi-Vektor:** ein Vektor je Gerät heißt Single-Queue; MSI-X mit einem
   Vektor je Queue ist offen (§5, B-Rand) — braucht zusätzlich variablen Cspace.
2. **Variabler Cspace:** `NCAPS = 16`, hart; für mehr Caps je PD braucht es
   einen variablen Cspace (TODO0 K1c). Trifft `request_irq` (Cap je Vektor)
   und jede Treiber-PD mit großem Cap-Bedarf.
3. **Logging:** `dev_err`/`printk` brauchen eine Senke in Klassenschicht oder
   Router; heute keine Schablone.
4. **Blockierendes `flush_workqueue`:** `Workqueue::flush` belegt nur die Leere;
   ein Flush, der auf laufende Jobs wartet, ist mit Completion baubar, aber nicht
   als Form ausgeschrieben.
5. **MMIO-Barrieren als HAL-Sache:** readl/writel-Semantik (volatile + Fence) liegt
   im arch-spezifischen HAL (B-Besitz) und ist hier nur als Schablonen-Regel benannt,
   nicht als geprüftes Primitiv.

## Messung

- `python3 tools/lx_symbols.py` — Quote aus dieser Tabelle (M1-Näherung).
- `python3 tools/lx_symbols.py --nm <datei>` — `nm -u`-Ausgabe (oder eine
  Symbol-pro-Zeile-Datei) gegen die Tabelle prüfen, Unbekannte listen.
- `tools/lx_messen.sh` — M1/M2-Stand einzeilig, Exit 0.
