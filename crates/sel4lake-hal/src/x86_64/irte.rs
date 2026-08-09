//! **Interrupt-Remapping-Einträge und MSI-Adressen kodieren** (Z22, P1) — reine Bitrechnung.
//!
//! ## Warum das eine eigene Datei ist
//!
//! Genau aus dem Grund, aus dem `dmar.rs` und `cycles.rs` eigene Dateien sind: hier passiert
//! **keine** Hardware, sondern Schieberei. Die Fallen sind ein Bit an der falschen Stelle, und ein
//! Bit an der falschen Stelle in einer IRTE äussert sich als „das Gerät unterbricht einfach nicht"
//! — ohne Fehlermeldung, ohne Fault, ohne irgendetwas, das nach einem Fehler aussieht. Ein
//! Host-Test mit Literalen trifft das; eine QEMU-Suite braucht dafür ein Gerät, einen Treiber und
//! Glück.
//!
//! `forbid(unsafe_code)`, keine `use`-Zeile ausser `super::*` im Testmodul → als **Datei**
//! prüfbar:
//! ```text
//! rustc --test --edition 2021 -O crates/sel4lake-hal/src/x86_64/irte.rs -o /tmp/t && /tmp/t
//! ```
//!
//! ## Was hier die Sicherheitsaussage trägt: `SVT` und `SID`
//!
//! Eine Treiber-PD besitzt das MMIO-Fenster ihres Geräts — also **auch dessen MSI-X-Tabelle**.
//! Sie schreibt die Adress-/Datenwerte dort selbst hinein, und das ist gewollt: es spart einen
//! Syscall je Vektor und der Kernel muss das Tabellenformat nicht kennen.
//!
//! Damit könnte sie aber den **Handle einer fremden IRTE** eintragen und so den Interrupt einer
//! anderen PD auslösen. Genau dagegen steht [`SVT_SID`]: die Einheit prüft bei jeder
//! Interrupt-Nachricht, ob die **Quell-BDF** zu der im Eintrag hinterlegten passt. Der Handle
//! einer fremden IRTE, abgeschickt vom eigenen Gerät, wird abgewiesen — nicht umgeleitet.
//!
//! Ohne diese Prüfung wäre „die PD programmiert ihre MSI-X-Tabelle selbst" ein Loch, und zwar
//! eines, das im Normalbetrieb nie auffällt.
//!
//! ## Vorbedingung, die NICHT hier steht
//!
//! `EIME` (Extended Interrupt Mode) ist im Bring-up **aus** — es gilt nur im x2APIC-Modus, und den
//! meldet nicht jede Plattform. Deshalb kodiert [`irte_build`] das Ziel im **xAPIC**-Format
//! (8-Bit-APIC-ID in Bits 47:40). Wer EIME einschaltet, muss hier mit ändern; [`irte_build`] weist
//! eine APIC-ID über 255 deshalb **ab**, statt sie abzuschneiden.

#![forbid(unsafe_code)]

/// Ein 128-Bit-Eintrag der Interrupt-Remapping-Tabelle.
///
/// Zwei Worte, und die Reihenfolge ist Teil der Aussage: `lo` **zuletzt** schreiben. Solange
/// `lo.P == 0` ist der Eintrag nicht vorhanden; wer `lo` zuerst schriebe, machte einen Eintrag
/// gültig, dessen Quellprüfung (`hi`) noch nicht darinsteht.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Irte {
    pub lo: u64,
    pub hi: u64,
}

/// `SVT` = 01: die Einheit prüft **Quell-BDF gegen `SID`**. Das ist die Zeile, die verhindert,
/// dass ein Gerät den Interrupt-Handle eines anderen benutzt.
pub const SVT_SID: u64 = 1;
/// `SQ` = 00: alle 16 Bits der `SID` müssen passen (keine Maskierung von Funktions- oder
/// Gerätenummer).
pub const SQ_ALLE_16: u64 = 0;

/// Zustellungsart „Fixed" (000) — kein Lowest-Priority. Lowest-Priority überlässt dem Chipsatz
/// die Kernwahl; ein Treiber, dessen IRQ-Thread an einen Kern gebunden ist, will das nicht.
const DLM_FIXED: u64 = 0;

/// **Einen IRTE bauen: vorhanden, flankengetriggert, fixed, physisch adressiert, quellgeprüft.**
///
/// * `vector` — der IDT-Vektor, unter dem der Kernel ihn erwartet.
/// * `apic_id` — Ziel-LAPIC (xAPIC-Format, also ≤ 255; s. Modul-Doku zu `EIME`).
/// * `sid` — die BDF des berechtigten Geräts: `(bus << 8) | (dev << 3) | func`.
///
/// `None`, wenn Vektor oder APIC-ID nicht darstellbar sind. **Abweisen statt abschneiden**: eine
/// abgeschnittene APIC-ID zeigt auf einen anderen Kern, und der Interrupt käme still am falschen
/// Ort an — schlimmer als gar keiner, weil er wie ein Erfolg aussieht.
///
/// Vektoren unter 32 sind CPU-Ausnahmen und werden ebenfalls abgewiesen; ein Gerät, das Vektor 14
/// auslöst, sähe für den Kernel wie ein Seitenfehler aus.
pub fn irte_build(vector: u8, apic_id: u32, sid: u16) -> Option<Irte> {
    if vector < 32 {
        return None;
    }
    if apic_id > 0xFF {
        return None; // s. Modul-Doku: ohne EIME passt nur eine 8-Bit-ID
    }
    // lo:
    //   0    P   = 1  (vorhanden)
    //   1    FPD = 0  (Faults werden gemeldet -- eine stumme Einheit sieht aus wie eine heile)
    //   2    DM  = 0  (physisch)
    //   3    RH  = 0
    //   4    TM  = 0  (flankengetriggert -- MSI IST flankengetriggert; das Maskieren, das ein
    //                  level-getriggerter SPI braucht, entfaellt damit)
    //   7:5  DLM = 000 (fixed)
    //  23:16 Vector
    //  47:40 Destination (xAPIC-ID)
    let lo = 1 | (DLM_FIXED << 5) | ((vector as u64) << 16) | ((apic_id as u64) << 40);
    // hi:
    //  15:0  SID
    //  17:16 SQ
    //  19:18 SVT
    let hi = (sid as u64) | (SQ_ALLE_16 << 16) | (SVT_SID << 18);
    Some(Irte { lo, hi })
}

/// Eine BDF zu einer `SID` zusammensetzen. `None` bei unmöglichen Werten — `dev` hat 5 Bit,
/// `func` 3.
pub fn sid_from_bdf(bus: u8, dev: u8, func: u8) -> Option<u16> {
    if dev > 31 || func > 7 {
        return None;
    }
    Some(((bus as u16) << 8) | ((dev as u16) << 3) | (func as u16))
}

/// **Die MSI-Adresse im „Remappable"-Format** — das, was die PD in ihre MSI-X-Tabelle schreibt.
///
/// ```text
/// 31:20  0xFEE
/// 19:5   Handle[14:0]
/// 4      SHV (SubHandle Valid) = 0
/// 3      Interrupt Format = 1  <-- DIESES Bit unterscheidet remapped von compatibility
/// 2      Handle[15]
/// 1:0    0
/// ```
///
/// Bit 3 ist der ganze Unterschied: ohne es wäre die Nachricht im **Compatibility**-Format, und
/// das ist genau das, was der Bring-up mit `CFI` abgeschaltet hat. Eine Adresse ohne dieses Bit
/// führt also nicht zu einem falschen Interrupt, sondern zu **gar keinem** — und das sieht aus wie
/// ein stummes Gerät.
pub fn msi_addr(handle: u16) -> u32 {
    let h = handle as u32;
    0xFEE0_0000 | ((h & 0x7FFF) << 5) | (1 << 3) | ((h >> 15) << 2)
}

/// Das MSI-Datenwort im Remappable-Format bei `SHV = 0`: **null**.
///
/// Es steht hier als Funktion und nicht als „schreib halt 0 hin", weil die Null eine Bedeutung
/// hat: Vektor und Ziel stehen in der IRTE, nicht in der Nachricht. Wer hier den Vektor
/// hineinschriebe (wie im Compatibility-Format üblich), bekäme einen Sub-Handle, den niemand
/// vergeben hat.
pub fn msi_data() -> u32 {
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eintrag_ist_vorhanden_und_flankengetriggert() {
        let e = irte_build(0x40, 0, 0x0100).unwrap();
        assert_eq!(e.lo & 1, 1, "P muss gesetzt sein");
        assert_eq!(e.lo & (1 << 1), 0, "FPD aus -- Faults sollen gemeldet werden");
        assert_eq!(e.lo & (1 << 2), 0, "DM = physisch");
        assert_eq!(e.lo & (1 << 4), 0, "TM = flankengetriggert (MSI)");
        assert_eq!((e.lo >> 5) & 0b111, 0, "DLM = fixed, nicht lowest-priority");
    }

    #[test]
    fn vektor_und_ziel_stehen_an_ihrem_platz() {
        let e = irte_build(0x42, 3, 0).unwrap();
        assert_eq!((e.lo >> 16) & 0xFF, 0x42);
        assert_eq!((e.lo >> 40) & 0xFF, 3);
    }

    #[test]
    fn quellpruefung_ist_eingeschaltet() {
        // **Die Sicherheitsaussage dieser Datei.** Ohne SVT=01 duerfte jedes Geraet jeden
        // Handle benutzen -- und die PD schreibt ihre MSI-X-Tabelle selbst.
        let sid = sid_from_bdf(0x00, 0x1f, 3).unwrap();
        let e = irte_build(0x40, 0, sid).unwrap();
        assert_eq!((e.hi >> 18) & 0b11, SVT_SID);
        assert_eq!((e.hi >> 16) & 0b11, SQ_ALLE_16);
        assert_eq!(e.hi & 0xFFFF, sid as u64);
    }

    #[test]
    fn bdf_wird_richtig_gepackt() {
        assert_eq!(sid_from_bdf(0, 0, 0), Some(0x0000));
        assert_eq!(sid_from_bdf(0, 1, 0), Some(0x0008));
        assert_eq!(sid_from_bdf(0, 0x1f, 7), Some(0x00FF));
        assert_eq!(sid_from_bdf(0x12, 3, 1), Some(0x1219));
        // Abweisen statt abschneiden: eine ueberlaufende Geraetenummer wuerde in die Busnummer
        // hineinlaufen und den Eintrag einem FREMDEN Geraet zuordnen.
        assert_eq!(sid_from_bdf(0, 32, 0), None);
        assert_eq!(sid_from_bdf(0, 0, 8), None);
    }

    #[test]
    fn cpu_ausnahmevektoren_werden_abgewiesen() {
        // Ein Geraet auf Vektor 14 saehe fuer den Kernel wie ein Seitenfehler aus.
        assert!(irte_build(14, 0, 0).is_none());
        assert!(irte_build(31, 0, 0).is_none());
        assert!(irte_build(32, 0, 0).is_some());
    }

    #[test]
    fn zu_grosse_apic_id_wird_abgewiesen_statt_abgeschnitten() {
        // Abgeschnitten zeigte sie auf einen ANDEREN Kern -- der Interrupt kaeme still am
        // falschen Ort an, und das sieht aus wie Erfolg.
        assert!(irte_build(0x40, 0x100, 0).is_none());
        assert!(irte_build(0x40, 0xFF, 0).is_some());
    }

    #[test]
    fn msi_adresse_traegt_das_remappable_bit() {
        // Ohne Bit 3 ist die Nachricht im Compatibility-Format -- und das hat der Bring-up
        // ueber CFI abgeschaltet. Ergebnis waere GAR KEIN Interrupt, nicht ein falscher.
        assert_eq!(msi_addr(0) & (1 << 3), 1 << 3);
        assert_eq!(msi_addr(0) & 0xFFF0_0000, 0xFEE0_0000);
    }

    #[test]
    fn handle_wird_geteilt_wie_die_spezifikation_es_verlangt() {
        // Bits 14:0 nach 19:5, Bit 15 nach Bit 2. Die zweite Haelfte wird beim Schreiben von Hand
        // gern vergessen -- und faellt erst ab Handle 32768 auf.
        assert_eq!((msi_addr(1) >> 5) & 0x7FFF, 1);
        assert_eq!((msi_addr(0x7FFF) >> 5) & 0x7FFF, 0x7FFF);
        assert_eq!(msi_addr(0x7FFF) & (1 << 2), 0);
        assert_eq!(msi_addr(0x8000) & (1 << 2), 1 << 2);
        assert_eq!((msi_addr(0x8000) >> 5) & 0x7FFF, 0);
        assert_eq!((msi_addr(0xFFFF) >> 5) & 0x7FFF, 0x7FFF);
        assert_eq!(msi_addr(0xFFFF) & (1 << 2), 1 << 2);
    }

    #[test]
    fn shv_ist_aus_und_das_datenwort_ist_null() {
        assert_eq!(msi_addr(7) & (1 << 4), 0);
        // Vektor und Ziel stehen in der IRTE. Wer hier den Vektor hineinschriebe, erzeugte einen
        // Sub-Handle, den niemand vergeben hat.
        assert_eq!(msi_data(), 0);
    }

    #[test]
    fn zwei_eintraege_fuer_verschiedene_geraete_sind_unterscheidbar() {
        // Die Aussage, auf der die Isolation ruht: gleicher Vektor, gleicher Kern, aber
        // verschiedene Quellen -> verschiedene `hi`.
        let a = irte_build(0x40, 0, sid_from_bdf(0, 4, 0).unwrap()).unwrap();
        let b = irte_build(0x40, 0, sid_from_bdf(0, 5, 0).unwrap()).unwrap();
        assert_eq!(a.lo, b.lo);
        assert_ne!(a.hi, b.hi);
    }
}
