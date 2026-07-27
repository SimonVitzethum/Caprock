//! **Arch-neutrale Fault-Beobachtung einer IOMMU.**
//!
//! SMMUv3 und VT-d melden Übersetzungsfehler völlig verschieden: die eine über eine Ringpuffer-
//! Event-Queue mit `PROD`/`CONS`, die andere über eine Handvoll Fault-Recording-Register mit
//! `FSTS.PPF` und einem Index. Auch die Fehlerklassen sind anders geschnitten.
//!
//! Diese Datei existiert, weil das **vor** der x86-Zuteilung eine gemeinsame Form braucht. Der
//! Negativtest liest heute `F_TRANSLATION`, StreamID und Eingangsadresse. Bekäme er stattdessen
//! eine `cfg`-Verzweigung, wäre das Abnahmekriterium („die vorhandenen Tests hören auf zu
//! skippen, ohne x86-Sonderpfade") formal erfüllt und inhaltlich verfehlt — an genau dieser
//! Stelle entstünde der zweite Entwurf, den die arch-neutrale Cap-Seite vermeiden soll.

/// Klasse eines Fehlers, so grob wie nötig und so genau wie belastbar.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FaultKind {
    /// Es gibt keine gültige Übersetzung für die angeforderte Adresse. **Der** Typ, den eine
    /// Anforderung auf eine nicht zugeteilte Adresse erzeugt.
    Translation,
    /// Eine Übersetzung existiert, erlaubt aber die Zugriffsart nicht (Schreiben auf RO).
    Permission,
    /// Die Eingangsadresse liegt oberhalb der konfigurierten Breite.
    AddressSize,
    /// **Die Einheit lehnt die Tabellen des Kernels ab.** Immer ein Kernel-Fehler, nie das
    /// Verschulden eines Geräts — und, entscheidend, der Stream übersetzt dann *gar nicht*.
    /// Jede spätere Aussage der Form „keine Faults" ist danach bedeutungslos.
    Config,
    /// Alles andere, mit dem rohen Code der Architektur.
    Other(u8),
}

/// Ein Fehlereintrag in gemeinsamer Form.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FaultRecord {
    pub kind: FaultKind,
    /// StreamID (ARM) bzw. Requester-ID (x86) der verursachenden Transaktion.
    pub requester: u32,
    /// Die **Eingangs**adresse — die IOVA, die das Gerät angefordert hat.
    pub input_addr: u64,
    /// Der rohe architekturspezifische Code, für Diagnose.
    pub raw: u8,
}
