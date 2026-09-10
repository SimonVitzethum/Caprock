//! irq+NTFN je Vektor (Multi-Vektor-MSI-X): die reine Paar-Buchhaltung.
//!
//! Ein MSI-X-Gerät mit `n` Vektoren braucht `n` `Irq`-Caps und `n` Notifications — je Vektor
//! ein Paar, weil `SYS_BIND_IRQ` genau ein Paar bindet (eine Cap nennt genau einen `intid`,
//! und die Bindung trägt genau ein Badge). Was hier steht, ist die Prüfung des ganzen
//! Satzes **vor** der ersten Bindung: halbe Sätze scheitern, bevor etwas steht — „eine halbe
//! Zuteilung ist schlimmer als keine" gilt für Vektoren wie für Geräte.
//!
//! Abhaengigkeitsfrei (`core` nur) und per `rustc --test` als DATEI pruefbar — die
//! Entscheidung steht genau einmal hier, der Kernel (Patch-Text) ruft sie auf, statt sie
//! nachzubauen (dieselbe Teilung wie `cspace.rs`/`proc.rs`).
//!
//! ## Der Cspace ist variabel — LESEN wie
//!
//! Jede Schranke nimmt die Lauf-Länge als Parameter (`cspace_len`), statt `16`
//! anzunehmen: Slot 20 ist in einem 32er-Lauf adressierbar und in einem 16er nicht. Wer
//! hier eine Konstante annähme, wiese Treiber-PDs mit grossem Cspace ab, was der Cspace
//! gerade erlaubt hat — dieselbe unerreichbare Zusage wie Budget 20 bei 16 Plätzen.
//!
//! ## Was diese Datei NICHT tut: binden
//!
//! Die Bindung selbst (`SYS_BIND_IRQ` je Paar, Tabelle im Kernel, `irq_hook` im
//! IRQ-Kontext) bleibt, wo sie ist. Diese Datei antwortet nur auf „darf dieser Satz
//! gebunden werden" — mit einem Namen je Absage (D11-Form).

/// Höchstzahl Vektoren je Gerät auf der Bindungsseite.
///
/// **Policy-Schranke, keine Maskenbreite**: was darüber liegt, wird mit
/// [`VektorAbweisung::ZuVieleVektoren`] abgewiesen — gerätelokal, die anderen Geräte
/// stört es nicht. Der Dispatch bildet das auf `ERR_IRQ_FULL` ab (Nr. 23, „dieses Gerät
/// hat keinen freien Vektor"), nicht auf einen globalen Code.
///
/// Die Zahl spiegelt `caprock_hal::irte::VEKTOREN_JE_GERAET_MAX` (die Breite der
/// Pending-Maske im Re-Trigger-Schutz): beide begrenzen „Vektoren je Gerät", die eine als
/// Zusage an den Treiber, die andere als Darstellung im Kernel. Driftet eine Seite, passt
/// ein gewährter Satz nicht in den Schutz — deshalb steht die Beziehung hier, nicht nur
/// in den Werten.
pub const VEKTOREN_JE_GERAET_MAX: usize = 64;

/// Ein Vektor-Paar: die beiden Slots im Cspace des Aufrufers, die `SYS_BIND_IRQ` zusammen
/// binden wird — `irq_slot` hält die `Irq`-Cap (welcher Interrupt), `ntfn_slot` die
/// Notification-Cap (wohin).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct VektorPaar {
    /// Slot der `Irq`-Cap (braucht `READ`).
    pub irq_slot: usize,
    /// Slot der Notification-Cap (braucht `WRITE` — der Kernel signalisiert sie).
    pub ntfn_slot: usize,
}

/// Die Sicht auf einen Cspace-Slot, die die Prüfung braucht — injiziert, nicht nachgeschlagen.
///
/// Der Aufrufer (Kernel) liest sie aus `PdTable` (`cap_at` + `cspace.lookup`); was hier als
/// Zahl steht, ist die Entscheidung danach. Eine Prüfung, die an zwei Stellen halb passiert,
/// ist zwei Prüfungen — deshalb reicht der Kernel die Sicht herein, statt dass diese Datei
/// sie selbst holt.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SlotSicht {
    /// Liegt hier eine auflösbare Cap?
    pub belegt: bool,
    /// Welche Sorte — nur die beiden, die ein Paar bilden, der Rest ist `Sonst`.
    pub rolle: CapRolle,
    /// Trägt die Cap `READ`?
    pub lesen: bool,
    /// Trägt die Cap `WRITE`?
    pub schreiben: bool,
}

/// Die Cap-Sorte, soweit ein Vektor-Paar sie unterscheidet.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CapRolle {
    /// Eine `Irq`-Cap (Geräte-Interrupt).
    Irq,
    /// Eine Notification-Cap (Signalziel).
    Mitteilung,
    /// Alles andere (Endpoint, Memory, …) — kein Paar-Bestandteil.
    Sonst,
}

/// Warum ein Vektor-Satz nicht gebunden werden darf — jede Absage mit Vektor-Index.
/// Der Index ist die Behebung („Paar 3 prüfen"), nicht nur die Diagnose.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum VektorAbweisung {
    /// Kein einziges Paar angeboten — ein Satz ohne Vektoren ist kein Satz.
    Leer,
    /// Mehr Vektoren verlangt, als ein Gerät trägt ([`VEKTOREN_JE_GERAET_MAX`]).
    /// Gerätelokal: der Dispatch meldet `ERR_IRQ_FULL`.
    ZuVieleVektoren {
        /// Verlangte Paar-Zahl.
        verlangt: usize,
        /// Geltende Schranke.
        max: usize,
    },
    /// Ein Slot liegt jenseits des Cspace-Laufs dieser PD (der Lauf ist variabel).
    AusserhalbDesCspace {
        /// Welches Paar (Index in `paare`).
        vektor: usize,
        /// Der beanstandete Slot.
        slot: usize,
        /// Die Lauf-Länge dieser PD.
        laenge: usize,
    },
    /// Der Slot hält keine auflösbare Cap.
    LeererSlot {
        /// Welches Paar.
        vektor: usize,
        /// Der beanstandete Slot.
        slot: usize,
    },
    /// Die Cap im Slot ist von der falschen Sorte (keine `Irq`- / Notification-Cap).
    FalscheSorte {
        /// Welches Paar.
        vektor: usize,
        /// Der beanstandete Slot.
        slot: usize,
    },
    /// Das nötige Recht fehlt (`READ` auf der `Irq`-Cap, `WRITE` auf der Notification).
    RechtFehlt {
        /// Welches Paar.
        vektor: usize,
        /// Der beanstandete Slot.
        slot: usize,
    },
}

/// Wie viele Cspace-Plätze `n` Vektoren kosten: je Vektor ein `Irq`- + ein
/// Notification-Slot. Die Zahl, mit der eine Treiber-PD ihren Cspace bemisst
/// (`Budget`/`Plätze` in `cspace::anforderung_aufloesen`) — steht hier und nicht beim
/// Aufrufer, damit es **eine** Quelle gibt.
pub const fn plaetze_fuer_vektoren(n: usize) -> usize {
    n.saturating_mul(2)
}

/// Einen Vektor-Satz prüfen — alle Paare, vor der ersten Bindung.
///
/// Reihenfolge je Paar, und sie ist festgelegt: erst die Adresse (`AusserhalbDesCspace`),
/// dann der Inhalt (`LeererSlot`), dann die Sorte, dann das Recht. Wer das Recht vor der
/// Adresse prüfte, läse Rechte aus einem Slot, den es nicht gibt.
///
/// `cspace` ist die injizierte Sicht auf den Lauf des Aufrufers (Länge = Lauf-Länge, s.
/// Modul-Doku „variabel"); `cspace_len` ist überflüssig, weil `cspace.len()` es ist — eine
/// zweite Zahl daneben wäre die zweite Wirklichkeit.
pub fn paare_pruefen(paare: &[VektorPaar], cspace: &[SlotSicht]) -> Result<(), VektorAbweisung> {
    if paare.is_empty() {
        return Err(VektorAbweisung::Leer);
    }
    if paare.len() > VEKTOREN_JE_GERAET_MAX {
        return Err(VektorAbweisung::ZuVieleVektoren {
            verlangt: paare.len(),
            max: VEKTOREN_JE_GERAET_MAX,
        });
    }
    for (i, p) in paare.iter().enumerate() {
        paar_slot_pruefen(i, p.irq_slot, CapRolle::Irq, true, cspace)?;
        paar_slot_pruefen(i, p.ntfn_slot, CapRolle::Mitteilung, false, cspace)?;
    }
    Ok(())
}

/// Ein Slot eines Paares: `irq == true` verlangt `Irq` + `READ`, sonst `Mitteilung` + `WRITE`.
/// Die Recht-Richtung steht in genau einem `bool` statt in zwei Funktionen — zwei Funktionen
/// mit je einer Richtung wären zwei Stellen, die auseinanderlaufen können.
fn paar_slot_pruefen(
    vektor: usize,
    slot: usize,
    rolle: CapRolle,
    irq: bool,
    cspace: &[SlotSicht],
) -> Result<(), VektorAbweisung> {
    let Some(s) = cspace.get(slot) else {
        return Err(VektorAbweisung::AusserhalbDesCspace { vektor, slot, laenge: cspace.len() });
    };
    if !s.belegt {
        return Err(VektorAbweisung::LeererSlot { vektor, slot });
    }
    if s.rolle != rolle {
        return Err(VektorAbweisung::FalscheSorte { vektor, slot });
    }
    let recht_ok = if irq { s.lesen } else { s.schreiben };
    if !recht_ok {
        return Err(VektorAbweisung::RechtFehlt { vektor, slot });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn irq(lesen: bool) -> SlotSicht {
        SlotSicht { belegt: true, rolle: CapRolle::Irq, lesen, schreiben: false }
    }

    fn ntfn(schreiben: bool) -> SlotSicht {
        SlotSicht { belegt: true, rolle: CapRolle::Mitteilung, lesen: false, schreiben }
    }

    fn leer() -> SlotSicht {
        SlotSicht { belegt: false, rolle: CapRolle::Sonst, lesen: false, schreiben: false }
    }

    /// Ein Cspace-Lauf der Länge `n` mit einem gültigen Paar auf (0, 1) — und, wenn der
    /// Lauf lang genug ist, einem zweiten auf (20, 21) für den Längenvergleich.
    fn lauf(n: usize) -> [SlotSicht; 64] {
        let mut c = [leer(); 64];
        let m = if n > 64 { 64 } else { n };
        let mut i = 0;
        while i < m {
            c[i] = SlotSicht {
                belegt: true,
                rolle: CapRolle::Sonst,
                lesen: true,
                schreiben: true,
            };
            i += 1;
        }
        if m > 1 {
            c[0] = irq(true);
            c[1] = ntfn(true);
        }
        if m > 21 {
            c[20] = irq(true);
            c[21] = ntfn(true);
        }
        c
    }

    #[test]
    fn gueltiges_paar_besteht() {
        let c = lauf(16);
        let paare = [VektorPaar { irq_slot: 0, ntfn_slot: 1 }];
        assert_eq!(paare_pruefen(&paare, &c[..16]), Ok(()));
    }

    #[test]
    fn leere_anfrage_wird_benannt() {
        let c = lauf(16);
        assert_eq!(paare_pruefen(&[], &c[..16]), Err(VektorAbweisung::Leer));
    }

    #[test]
    fn zu_viele_vektoren_sind_geraetelokal_benannt() {
        // 65 Paare: die Absage nennt verlangt/max — der Dispatch meldet dafür
        // `ERR_IRQ_FULL` („dieses Gerät"), nicht einen globalen Code.
        let c = lauf(16);
        let viele = [VektorPaar { irq_slot: 0, ntfn_slot: 1 }; 65];
        assert_eq!(
            paare_pruefen(&viele, &c[..16]),
            Err(VektorAbweisung::ZuVieleVektoren { verlangt: 65, max: 64 })
        );
        // Genau 64 gehen durch die Zählung (die Slots selbst sind hier egal — es gibt
        // nur ein Paar-Muster; die Prüfung der Vielzahl steht vor der der Slots).
        let voll = [VektorPaar { irq_slot: 0, ntfn_slot: 1 }; 64];
        assert_eq!(paare_pruefen(&voll, &c[..16]), Ok(()));
    }

    #[test]
    fn cspace_laenge_entscheidet_nicht_die_konstante() {
        // **Der variable Cspace als Test.** Slot 20 ist in einem 32er-Lauf adressierbar
        // und in einem 16er nicht — dieselbe Anfrage, zwei Läufe, zwei Ausgänge.
        let klein = lauf(16);
        let gross = lauf(32);
        let paare = [VektorPaar { irq_slot: 20, ntfn_slot: 21 }];
        assert_eq!(
            paare_pruefen(&paare, &klein[..16]),
            Err(VektorAbweisung::AusserhalbDesCspace { vektor: 0, slot: 20, laenge: 16 })
        );
        assert_eq!(paare_pruefen(&paare, &gross[..32]), Ok(()));
    }

    #[test]
    fn leere_slots_falsche_sorten_fehlende_rechte_tragen_den_index() {
        // Leerer Irq-Slot in Paar 1 (Paar 0 ist gültig — die Prüfung läuft der Reihe nach).
        let mut c = lauf(16);
        c[2] = leer();
        c[3] = ntfn(true);
        let paare = [
            VektorPaar { irq_slot: 0, ntfn_slot: 1 },
            VektorPaar { irq_slot: 2, ntfn_slot: 3 },
        ];
        assert_eq!(
            paare_pruefen(&paare, &c[..16]),
            Err(VektorAbweisung::LeererSlot { vektor: 1, slot: 2 })
        );
        // Falsche Sorte: Notification, wo die Irq-Cap hingehört.
        let mut c = lauf(16);
        c[0] = ntfn(true);
        let paare = [VektorPaar { irq_slot: 0, ntfn_slot: 1 }];
        assert_eq!(
            paare_pruefen(&paare, &c[..16]),
            Err(VektorAbweisung::FalscheSorte { vektor: 0, slot: 0 })
        );
        // Fehlendes READ auf der Irq-Cap …
        let mut c = lauf(16);
        c[0] = irq(false);
        let paare = [VektorPaar { irq_slot: 0, ntfn_slot: 1 }];
        assert_eq!(
            paare_pruefen(&paare, &c[..16]),
            Err(VektorAbweisung::RechtFehlt { vektor: 0, slot: 0 })
        );
        // … und fehlendes WRITE auf der Notification (der Kernel signalisiert sie —
        // dieselbe Richtung wie `SYS_BIND_IRQ` selbst).
        let mut c = lauf(16);
        c[1] = ntfn(false);
        let paare = [VektorPaar { irq_slot: 0, ntfn_slot: 1 }];
        assert_eq!(
            paare_pruefen(&paare, &c[..16]),
            Err(VektorAbweisung::RechtFehlt { vektor: 0, slot: 1 })
        );
    }

    #[test]
    fn plaetze_zaehlen_je_vektor_zwei() {
        // irq+NTFN je Vektor: vier Vektoren kosten acht Plätze — die Zahl, mit der die
        // Treiber-PD bemessen wird (s. `cspace`-Test „acht Plätze Reserve").
        assert_eq!(plaetze_fuer_vektoren(0), 0);
        assert_eq!(plaetze_fuer_vektoren(1), 2);
        assert_eq!(plaetze_fuer_vektoren(4), 8);
        assert_eq!(plaetze_fuer_vektoren(64), 128);
    }
}
