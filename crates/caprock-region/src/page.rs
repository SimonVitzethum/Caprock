//! **Seiten-API** (`struct page`/`mem_map`-Faehigkeit, kein 1:1-Linux).
//!
//! Abbildung auf Linux-Begriffe (A-Zustand aus `docs/linux-kompatibilitaet-caprock.md`):
//!
//! | Linux            | Hier                                              |
//! |------------------|---------------------------------------------------|
//! | `struct page`    | [`PageDesc`] (Refcount + Flags + Order)           |
//! | `mem_map`        | [`MemMap`] (Basis + Seitenzahl, reine Arithmetik) |
//! | `page_address` / `page_to_phys` | [`MemMap::pfn_zu_addr`]            |
//! | `get_page`       | [`PageDesc::get_page`] (saettigend)               |
//! | `put_page`       | [`PageDesc::put_page`] (Unterlaufschutz)          |
//! | `alloc_pages(order)`-Groesse | [`order_zu_len`]                    |
//!
//! Bewusst KEIN Allokator: dies ist nur Deskriptor-Arithmetik ueber einer Arena, die der
//! Aufrufer besitzt. Kein `unsafe`, kein Zugriff auf echten Speicher.

/// Verschiebung einer 4-KiB-Seite.
pub const PAGE_SHIFT: u32 = 12;
/// Seitengroesse in Bytes.
pub const PAGE_SIZE: u64 = 1 << PAGE_SHIFT;
/// Groesste zulaessige Order (`4096 << 20` = 4 GiB, passt in `u64`).
pub const MAX_ORDER: u32 = 20;

/// Seitendeskriptor — Faehigkeit fuer Linux-`struct page` (Refcounting + Flags + Order).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageDesc {
    /// Anzahl Halter. Saettigt bei `u32::MAX` (kein Wrap).
    pub refcount: u32,
    /// Linux-`page->flags`-Faehigkeit (opake Bits, z. B. Head/Tail-Markierung).
    pub flags: u32,
    /// Buddy-Order dieser Seite (0 = eine Seite, n = `2^n` Seiten).
    pub order: u8,
}

impl PageDesc {
    /// Freier Deskriptor (kein Halter).
    pub const fn new(flags: u32, order: u8) -> Self {
        Self {
            refcount: 0,
            flags,
            order,
        }
    }

    /// Halter hinzunehmen (`get_page`-Faehigkeit). Saettigt bei `u32::MAX` statt zu
    /// wrappen; Rueckgabe ist der neue Stand.
    pub fn get_page(&mut self) -> u32 {
        self.refcount = self.refcount.saturating_add(1);
        self.refcount
    }

    /// Halter abgeben (`put_page`-Faehigkeit). Bei 0 kein Unterlauf: keine Aenderung,
    /// Rueckgabe `false` (Seite frei / unbenutzt). Sonst dekrementieren, `true`.
    pub fn put_page(&mut self) -> bool {
        if self.refcount == 0 {
            return false;
        }
        self.refcount -= 1;
        true
    }

    /// Aktueller Refcount-Stand.
    pub fn refcount(&self) -> u32 {
        self.refcount
    }
}

/// Seitentabelle ueber einem zusammenhaengenden Fenster — `mem_map`-Faehigkeit.
///
/// `basis` ist die physische Adresse von PFN 0, `seiten` die Fensterlaenge in Seiten.
/// Reine Arithmetik, kein Speicherzugriff.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemMap {
    /// Physische Basisadresse (PFN 0).
    pub basis: u64,
    /// Fensterlaenge in Seiten.
    pub seiten: u64,
}

impl MemMap {
    /// Leeres Fenster an Adresse 0.
    pub const fn new(basis: u64, seiten: u64) -> Self {
        Self { basis, seiten }
    }

    /// PFN -> physische Adresse (`page_address`/`page_to_phys`-Faehigkeit).
    /// `None`, wenn `pfn` ausserhalb des Fensters liegt oder die Arithmetik
    /// ueberlaeuft (`checked_mul`/`checked_add`).
    pub fn pfn_zu_addr(&self, pfn: u64) -> Option<u64> {
        if pfn >= self.seiten {
            return None;
        }
        let off = pfn.checked_mul(PAGE_SIZE)?;
        self.basis.checked_add(off)
    }

    /// Physische Adresse -> PFN (abgerundet auf die Seite). `None`, wenn `addr`
    /// ausserhalb des Fensters `[basis, basis + seiten*4096)` liegt oder die
    /// Fensterlaenge ueberlaeuft.
    pub fn addr_zu_pfn(&self, addr: u64) -> Option<u64> {
        let len = self.seiten.checked_mul(PAGE_SIZE)?;
        let ende = self.basis.checked_add(len)?;
        if addr < self.basis || addr >= ende {
            return None;
        }
        Some((addr - self.basis) / PAGE_SIZE)
    }
}

/// Order -> Byte-Laenge (`alloc_pages(order)`-Groessenfaehigkeit: `4096 << order`).
/// `None` bei `order > 20` oder Shift-Overflow.
pub const fn order_zu_len(order: u32) -> Option<u64> {
    if order > MAX_ORDER {
        return None;
    }
    PAGE_SIZE.checked_shl(order)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pfn_null_ergibt_basis() {
        let m = MemMap::new(0x1_0000, 16);
        assert_eq!(m.pfn_zu_addr(0), Some(0x1_0000));
    }

    #[test]
    fn addr_rundweg() {
        let m = MemMap::new(0x10_0000, 64);
        for pfn in [0u64, 1, 7, 63] {
            let addr = m.pfn_zu_addr(pfn).expect("pfn im Fenster");
            assert_eq!(addr % PAGE_SIZE, 0);
            assert_eq!(m.addr_zu_pfn(addr), Some(pfn));
        }
    }

    #[test]
    fn pfn_ausserhalb_ergibt_none() {
        let m = MemMap::new(0x0, 4);
        assert_eq!(m.pfn_zu_addr(4), None);
        assert_eq!(m.pfn_zu_addr(u64::MAX), None);
    }

    #[test]
    fn addr_ausserhalb_ergibt_none() {
        let m = MemMap::new(0x10_0000, 4);
        // Vor dem Fenster.
        assert_eq!(m.addr_zu_pfn(0x10_0000 - 1), None);
        // Erstes Byte hinter dem Fenster.
        assert_eq!(m.addr_zu_pfn(0x10_0000 + 4 * 4096), None);
        // Weit ausserhalb.
        assert_eq!(m.addr_zu_pfn(u64::MAX), None);
    }

    #[test]
    fn refcount_saettigt() {
        let mut p = PageDesc::new(0, 0);
        p.refcount = u32::MAX;
        assert_eq!(p.get_page(), u32::MAX);
        assert_eq!(p.refcount(), u32::MAX);
        // put baut danach genau einen Halter ab.
        assert!(p.put_page());
        assert_eq!(p.refcount(), u32::MAX - 1);
    }

    #[test]
    fn put_bei_null_ohne_unterlauf() {
        let mut p = PageDesc::new(0, 0);
        assert_eq!(p.refcount(), 0);
        assert!(!p.put_page());
        assert_eq!(p.refcount(), 0);
        // get/put-Runde.
        assert_eq!(p.get_page(), 1);
        assert!(p.put_page());
        assert!(!p.put_page());
    }

    #[test]
    fn order_basiswerte() {
        assert_eq!(order_zu_len(0), Some(4096));
        assert_eq!(order_zu_len(1), Some(8192));
        assert_eq!(order_zu_len(20), Some(4096 << 20));
    }

    #[test]
    fn order_overflow_ergibt_none() {
        assert_eq!(order_zu_len(21), None);
        assert_eq!(order_zu_len(u32::MAX), None);
    }
}
