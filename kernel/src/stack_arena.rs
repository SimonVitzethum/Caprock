//! Stack-Arena statt gestreuter Kernel-Stacks (todo C7c).
//!
//! # Das Problem, in einer Zahl
//!
//! `hal::mmu::GUARD_BLOCKS = 16` ist statisch: jeder EL0-Kernel-Stack, der aus dem allgemeinen RAM
//! (`MEM`) kommt, streut ueber einen beliebigen 2-MiB-Block, und jeder *verschiedene* Block kostet
//! eine aufgeteilte Seitentabelle aus dem festen Vorrat. Gemessen bei 512M/64 KiB: **321 bewachte
//! Stacks ueber 16 Bloecke = rund 20 je Block**. In einen Block passten bei 4-KiB-Stack + 4-KiB-Wache
//! (Schritt 8 KiB) **256** — die Belegung liegt also bei **7,8 %**, der Verschnitt bei **92 %**.
//! `GUARD_BLOCKS` zu erhoehen kauft lineare Zahlen mit linearem BSS und laesst genau diesen
//! Verschnitt stehen.
//!
//! # Die Antwort
//!
//! Eine **einmal aufgeteilte zusammenhaengende Arena** ueber Boot-RAM: alle Stacks liegen dicht an
//! dicht, jeder Slot traegt seine Wache bei sich, und die Zahl aufgeteilter Bloecke folgt der
//! Arenameenge statt der Streuung. Dichte-Ziel auf x86: **~12x** (256 je Block statt
//! gemessener ~20). Auf aarch64 ist der Schritt 20 KiB (102 je Block) — gegen dieselbe
//! Streuung **~5x**, ehrlich kleiner; die Ableitung (`arena_plaetze`, `slots_je_block`,
//! `volle_pd_kosten`) rechnet je Architektur statt einer Zahl zu glauben.
//!
//! # Was hier steht — und was ausdruecklich nicht
//!
//! Hier steht nur die **reine Vergabe-Arithmetik**: Geometrie (Slot = Wache unten + Stack oben),
//! Bitmap-Vergabe/Freigabe mit **benannten Fehlern** ([`ArenaFehler`]) und Telemetrie
//! (vergeben/frei/Hoechststand/Abweisungen). Absichtlich **abhaengigkeitsfrei** (nur `core`),
//! damit die Logik per `rustc --test` auf dem Wirt pruefbar bleibt.
//!
//! Was der Kernel-Hook darueber hinaus noch braucht (steht hier NICHT, gehoert in `system.rs`):
//! * **einmalig Boot-RAM** fuer die Arena (`mem_alloc`, eigene Zone — sonst streut die Arena
//!   selbst wieder) und das Aufteilen der Arena-Bloecke (`guard_unmap` je Block genau einmal,
//!   statt je Stack einmal);
//! * das **Nullen** frisch vergebener Stacks (`zero_phys`) und das Fuellen der Wasserstandsmarke
//!   (`kstackmark::fuellen`) — beides `unsafe` an den bestehenden Stellen, hier faellt kein
//!   einziges `unsafe` an: **dieses Modul ist safe-only** — `unsafe` kommt nur in dieser
//!   Moduldoku vor, nie im Code (`grep -n unsafe` muss ausserhalb von `//!`-Zeilen leer sein).
//!
//! # Slot-Geometrie (gilt je Slot `i`, `0 <= i < plaetze`)
//!
//! ```text
//! basis + i*schritt ................. Wache (1 Seite, nicht abgebildet)
//! basis + i*schritt + SEITE ......... Stack-Basis (Rueckgabe von `vergeben`)
//! basis + i*schritt + SEITE + stapel  Slot-Ende (= naechste Wache)
//! ```
//!
//! Die Wache liegt **unten**, der Stack waechst von oben nach unten darauf zu — dieselbe Lage wie
//! heute (`claim_user_kstack_masked`: `wache = roh`, `base = roh + PAGE`).

// Kein `std`, nur `core`: das Modul wird in den `no_std`-Kernel kompiliert.
use core::fmt;

/// Eine Seite: Wache und Ausrichtungseinheit. Entspricht `caprock_mem::PAGE`.
pub const SEITE: usize = 4096;

/// EL0-Kernel-Stack auf x86_64 (`system::USER_KSTACK_SIZE` dort).
pub const X86_KSTACK: usize = 0x1000;
/// EL0-Kernel-Stack auf aarch64 (`system::USER_KSTACK_SIZE` dort).
pub const AARCH64_KSTACK: usize = 0x4000;
/// Kernel-Thread-Stack (`system::STACK_SIZE`) und EL0-User-Stack.
pub const KERN_STACK: usize = 64 * 1024;
/// Granularitaet der Identitaetskarte oberhalb 16 MiB (HAL: `TWO_MIB`).
pub const BLOCK_2M: usize = 2 * 1024 * 1024;

/// Private Region einer isolierten PD in Byte.
///
/// Spiegel von `hal::mmu::PRIV_REGION_SIZE` (dort 64 KiB auf beiden Architekturen) — wie
/// `X86_KSTACK`/`AARCH64_KSTACK` ein Spiegel von `system::USER_KSTACK_SIZE`: dieses Modul ist
/// absichtlich abhaengigkeitsfrei, damit die Vergabelogik per `rustc --test` auf dem Wirt
/// pruefbar bleibt. Wer den HAL-Wert aendert, nimmt diesen hier mit (und umgekehrt).
pub const PRIV_REGION: usize = 64 * 1024;

/// Absolute Obergrenze der Arena-Plaetze — ein Deckel, kein Ziel.
///
/// Begrenzt die einzelne zusammenhaengende Frueh-Allokation (16 384 × 8 KiB = 128 MiB auf x86,
/// 16 384 × 20 KiB = 320 MiB auf aarch64) und die Bitmap (256 Worte aus Boot-RAM statt BSS).
/// Die Zahl liegt ueber der PD-Tabelle (`NPDS` = 10 000): wer sie reisst, hat mehr Faeden als
/// das 1,6-fache an Adressraeumen — Faeden jenseits des Deckels laufen ueber den Streupfad
/// (benannt abgewiesen, nicht still verloren).
pub const ARENA_DECKEL_PLAETZE: usize = 16_384;

/// Begrenzungsgruende der Platzableitung ([`arena_plaetze`]) — Telemetrie, keine Mangel-Codes.
///
/// Bewusst getrennt vom Mangel-Melder: der zaehlt seine Stellen zur Bauzeit ueber den Quelltext,
/// und jede neue Stelle dort braeche eine fremde Zusicherung (`SWEEP_KMAX`-Summe in `bringup.rs`).
/// Diese Gruende nennt der Plan, nicht der Fehlschlag.
/// Die Thread-Slots waren das Minimum — keine Kappung, der schoenste Fall.
pub const GRUND_THREADS: u32 = 0;
/// Das freie Boot-RAM traegt nicht mehr volle PDs (s. [`volle_pd_kosten`]).
pub const GRUND_RAM: u32 = 1;
/// Die Guard-Splits tragen nicht mehr Slots (nur wo Wachen echt sind).
pub const GRUND_GUARD: u32 = 2;
/// Der absolute Deckel ([`ARENA_DECKEL_PLAETZE`]) hat gegriffen.
pub const GRUND_DECKEL: u32 = 3;
/// Die zusammenhaengende Spanne war nicht allozierbar (Plan stand, RAM reichte doch nicht).
pub const GRUND_SPANNE: u32 = 4;
/// Die Bitmap war nicht allozierbar.
pub const GRUND_BITMAP: u32 = 5;
/// Die Konstruktion traegt nicht (unreachable by construction — Basis/Ausrichtung/Laenge
/// kommen aus einer einzigen Allokation; fail-closed statt Panik).
pub const GRUND_GEOMETRIE: u32 = 6;

/// Klartext zum Begrenzungsgrund — damit die Telemetrie keine Zahl bleibt.
pub const fn grund_name(grund: u32) -> &'static str {
    match grund {
        GRUND_THREADS => "Thread-Slots (keine Kappung)",
        GRUND_RAM => "Boot-RAM (nicht mehr volle PDs)",
        GRUND_GUARD => "Guard-Splits (nicht mehr bewachbare Slots)",
        GRUND_DECKEL => "absoluter Deckel",
        GRUND_SPANNE => "Spanne nicht allozierbar",
        GRUND_BITMAP => "Bitmap nicht allozierbar",
        GRUND_GEOMETRIE => "Konstruktion traegt nicht",
        _ => "unbekannt",
    }
}

/// Volle Kosten EINER isolierten PD mit Faden in Byte: private Region + Kernel-Stack mit Wache
/// (`schritt`) + drei Seitentabellen-Rahmen (L1, L2, x86-Rueckkanal).
///
/// aarch64 braucht nur zwei Rahmen — die Einheit nimmt drei, und die Richtung des Irrtums steht
/// hier: die Arena wird eher kleiner, nie groesser als das RAM traegt.
pub const fn volle_pd_kosten(schritt: usize) -> u64 {
    (PRIV_REGION + schritt + 3 * SEITE) as u64
}

/// Arena-Plaetze aus (Faeden, RAM, Guard-Vorrat, Deckel) ableiten — statt einer Fixzahl.
///
/// * `threads`: Thread-Slots des Systems — mehr Plaetze als Faeden sind unbindbar.
/// * `freie_bytes`/`kosten_je_pd`: das RAM traegt so viele VOLLE PDs (s. [`volle_pd_kosten`]) —
///   die Arena haelt Kernel-Stacks fuer Faeden, und jeder Faden kostet im schlimmsten Fall
///   (isolierte PD) eine volle PD. Wer mehr Stacks versprach, als PDs je entstehen koennen,
///   legte RAM fuer Faeden zurueck, deren Adressraum nie existiert.
/// * `guard_plaetze`: `Some(n)` wo Wachen echt sind (x86) — ein Slot ohne legbare Wache wird
///   beim Bestuecken gesperrt und waere belegtes, nie nutzbares RAM. `None` wo `guard_unmap`
///   nur zaehlt (aarch64) — dort begrenzt kein Split.
/// * `deckel`: absolute Obergrenze (s. [`ARENA_DECKEL_PLAETZE`]).
///
/// Gibt `(plaetze, grund)`; `grund` nennt das Minimum. Bei Gleichstand gilt die Reihenfolge
/// `THREADS` vor `GUARD` vor `RAM` vor `DECKEL` — der schoenste Fall zuerst, der Deckel zuletzt.
pub const fn arena_plaetze(
    threads: usize,
    freie_bytes: u64,
    kosten_je_pd: u64,
    guard_plaetze: Option<usize>,
    deckel: usize,
) -> (usize, u32) {
    let ram = if kosten_je_pd == 0 {
        usize::MAX
    } else {
        let n = freie_bytes / kosten_je_pd;
        if n > usize::MAX as u64 {
            usize::MAX
        } else {
            n as usize
        }
    };
    let guard = match guard_plaetze {
        Some(n) => n,
        None => usize::MAX,
    };
    if threads <= ram && threads <= guard && threads <= deckel {
        (threads, GRUND_THREADS)
    } else if guard <= ram && guard <= deckel {
        (guard, GRUND_GUARD)
    } else if ram <= deckel {
        (ram, GRUND_RAM)
    } else {
        (deckel, GRUND_DECKEL)
    }
}

/// Benannte Absage statt `None`/`bool`: der Aufrufer (`system.rs`) meldet sie als Mangel weiter.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ArenaFehler {
    /// Alle Plaetze vergeben — der heutige `MANGEL_GUARD_TABELLE`-Fall, nur ehrlicher:
    /// hier ist die Arena voll, nicht der Blockvorrat zu klein gedacht.
    Erschoepft,
    /// Freigabe einer Adresse, die zu keinem vergebenen Slot gehoert: fremde Adresse,
    /// un pelletierte Ausrichtung oder **Doppel-Frei** (Bit war schon frei).
    Ungueltig,
    /// Arena-Basis nicht seitenausgerichtet — die Wachen waeren es dann auch nicht.
    Unausgerichtet,
    /// Ungueltige Geometrie: `stapel == 0`, nicht seitengranular, `plaetze == 0` oder die
    /// mitgegebene Bitmap ist zu kurz (`woerter(plaetze)` Worte noetig).
    Geometrie,
}

impl fmt::Display for ArenaFehler {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            ArenaFehler::Erschoepft => "Arena erschoepft",
            ArenaFehler::Ungueltig => "kein vergebener Slot (fremd/unaligned/doppelt-frei)",
            ArenaFehler::Unausgerichtet => "Arena-Basis nicht seitenausgerichtet",
            ArenaFehler::Geometrie => "ungueltige Arena-Geometrie",
        };
        f.write_str(s)
    }
}

/// Wie viele `u64`-Worte die Bitmap fuer `plaetze` Slots braucht.
pub const fn woerter(plaetze: usize) -> usize {
    (plaetze + 63) / 64
}

/// Wie viele Arena-Slots in einen 2-MiB-Kartenblock passen (Packungsdichte).
/// x86: Schritt 8 KiB → 256; aarch64: Schritt 20 KiB → 102 (ganzzahlig abgerundet).
pub const fn slots_je_block(schritt: usize) -> usize {
    if schritt == 0 {
        return 0;
    }
    BLOCK_2M / schritt
}

/// Hergeleiteter Dichtegewinn gegen die gemessene Streuung (`je_block_gestreut`, z. B. ~20).
/// Gibt `(tragfaehig, gewinn_fach)`: `tragfaehig` ist genau dann falsch, wenn die Rechnung
/// nichts traegt (`je_block_gestreut == 0` oder kein Slot je Block) — eine Kennzahl, die
/// schweigend 0 meldete, waere dieselbe Falle wie ein stiller Erfolgsmarker.
pub const fn dichte_gewinn(schritt: usize, je_block_gestreut: usize) -> (bool, usize) {
    let je_block = slots_je_block(schritt);
    if je_block == 0 || je_block_gestreut == 0 {
        return (false, 0);
    }
    (true, je_block / je_block_gestreut)
}

/// Die Arena: Vergabe ueber einer vom Aufrufer gehaltenen Bitmap.
///
/// Die Bitmap gehoert dem Aufrufer (statisch im Kernel-BSS oder Boot-RAM), nicht der Arena —
/// so braucht die Arena weder Allokator noch `unsafe`, und die Wirt-Tests speisen ein
/// schlichtes Array ein. Die Lebensdauer bindet beides aneinander.
pub struct StackArena<'a> {
    /// Physische Basis des zusammenhaengenden Bereichs (seitenausgerichtet).
    basis: usize,
    /// Nutzbarer Stack je Slot in Byte (seitengranular, ohne Wache).
    stapel: usize,
    /// Slot-Schritt = `SEITE` (Wache) + `stapel`.
    schritt: usize,
    /// Zahl der Slots.
    plaetze: usize,
    /// Ein Bit je Slot: 1 = vergeben.
    bits: &'a mut [u64],
    /// Gerade vergebene Slots.
    vergeben: usize,
    /// Hoechststand seit Arena-Start (Ratsche, faellt nie — wie `pt_peak`).
    hoechststand: usize,
    /// Abgewiesene Vergaben: Arena voll.
    abgewiesen_erschoepft: u64,
    /// Abgewiesene Freigaben: fremd/unaligned/doppelt-frei.
    abgewiesen_ungueltig: u64,
}

impl<'a> StackArena<'a> {
    /// Eine Arena ueber `[basis, basis + plaetze*schritt)` errichten. Der Speicher selbst
    /// (Boot-RAM) und das Aufteilen der Kartenbloecke bleiben Sache des Hooks — hier wird
    /// nur gerechnet und gebucht.
    pub fn neu(
        basis: usize,
        stapel: usize,
        plaetze: usize,
        bits: &'a mut [u64],
    ) -> Result<Self, ArenaFehler> {
        if !basis.is_multiple_of(SEITE) {
            return Err(ArenaFehler::Unausgerichtet);
        }
        if stapel == 0 || !stapel.is_multiple_of(SEITE) || plaetze == 0 {
            return Err(ArenaFehler::Geometrie);
        }
        if bits.len() < woerter(plaetze) {
            return Err(ArenaFehler::Geometrie);
        }
        // Sauberer Start: kein Bit darf stehen — sonst wuerde die erste Vergabe einen
        // angeblich belegten Slot ueberspringen und die Telemetrie luegen.
        for w in bits.iter_mut() {
            *w = 0;
        }
        let schritt = SEITE + stapel;
        Ok(StackArena {
            basis,
            stapel,
            schritt,
            plaetze,
            bits,
            vergeben: 0,
            hoechststand: 0,
            abgewiesen_erschoepft: 0,
            abgewiesen_ungueltig: 0,
        })
    }

    /// Wachenadresse von Slot `i` (nicht abgebildete Seite, Slot-Unterkante).
    pub fn wache_von_slot(&self, slot: usize) -> Option<usize> {
        if slot >= self.plaetze {
            return None;
        }
        Some(self.basis + slot * self.schritt)
    }

    /// Stack-Basis von Slot `i` (Rueckgabewert von `vergeben`, entspricht `roh + PAGE` heute).
    pub fn stapelbasis_von_slot(&self, slot: usize) -> Option<usize> {
        Some(self.wache_von_slot(slot)? + SEITE)
    }

    /// Slot-Index zu einer vergebenen Stack-Basis — oder `None` bei fremder/unaligned Adresse.
    /// Die Bereichspruefung laeuft ueber den Slot-Index, nicht ueber `adresse < ende`: ein
    /// `ende`, das ueberlaeuft, duerfte nie „innen" melden.
    pub fn slot_von_stapelbasis(&self, stapelbasis: usize) -> Option<usize> {
        if !stapelbasis.is_multiple_of(SEITE) {
            return None;
        }
        let relativ = stapelbasis.checked_sub(self.basis)?;
        let (slot, rest) = (relativ / self.schritt, relativ % self.schritt);
        if slot >= self.plaetze || rest != SEITE {
            return None;
        }
        Some(slot)
    }

    /// Einen Slot vergeben. Gibt die **Stack-Basis** zurueck (Wache = Basis − SEITE).
    /// Vergabereihenfolge: niedrigster freier Slot zuerst — deterministisch und damit
    /// testbar (kein Zufall, keine Zeitquelle, kein Allokator).
    pub fn vergeben(&mut self) -> Result<usize, ArenaFehler> {
        let worte = self.bits.len();
        for (wi, wort) in self.bits.iter_mut().enumerate() {
            // Slots jenseits `plaetze` im letzten Wort sind von der Vergabe
            // ausgeschlossen (Maske); die Gueltigkeitspruefung steht hier, nicht in `neu`.
            let mut maske = !*wort;
            if wi == worte - 1 {
                let rest = self.plaetze % 64;
                if rest != 0 {
                    maske &= (1u64 << rest) - 1;
                }
            }
            if maske == 0 {
                continue;
            }
            let bit = maske.trailing_zeros() as usize;
            let slot = wi * 64 + bit;
            debug_assert!(slot < self.plaetze);
            *wort |= 1u64 << bit;
            self.vergeben += 1;
            if self.vergeben > self.hoechststand {
                self.hoechststand = self.vergeben;
            }
            // `slot < plaetze` gilt per Maske oben; die Adresse folgt der Geometrie.
            return Ok(self.basis + slot * self.schritt + SEITE);
        }
        self.abgewiesen_erschoepft += 1;
        Err(ArenaFehler::Erschoepft)
    }

    /// Einen Stack zurueckgeben. **Doppel-Frei ist `Ungueltig`, nicht still ok**: ein zweites
    /// Frei desselben Slots wuerde sonst zwei lebende Vergaben auf dieselbe Region legen.
    pub fn freigeben(&mut self, stapelbasis: usize) -> Result<(), ArenaFehler> {
        let Some(slot) = self.slot_von_stapelbasis(stapelbasis) else {
            self.abgewiesen_ungueltig += 1;
            return Err(ArenaFehler::Ungueltig);
        };
        let (wi, bit) = (slot / 64, slot % 64);
        if self.bits[wi] & (1u64 << bit) == 0 {
            // Bit steht nicht: nie vergeben oder schon zurueckgegeben.
            self.abgewiesen_ungueltig += 1;
            return Err(ArenaFehler::Ungueltig);
        }
        self.bits[wi] &= !(1u64 << bit);
        self.vergeben -= 1;
        Ok(())
    }

    /// Ist `stapelbasis` gerade vergeben? Reine Auskunft, keine Buchung.
    pub fn ist_vergeben(&self, stapelbasis: usize) -> bool {
        let Some(slot) = self.slot_von_stapelbasis(stapelbasis) else {
            return false;
        };
        self.bits[slot / 64] & (1u64 << (slot % 64)) != 0
    }

    /// Einen Slot **dauerhaft sperren** (Init-Pfad des Kernels): Bit setzen wie bei
    /// [`Self::vergeben`], aber ohne Basis zurueckzugeben — der Slot verlaesst den
    /// Vergabepool fuer immer (Wache liess sich nicht legen). Ausserhalb des Bereichs
    /// oder doppelt gesperrt → `false`, kein Zaehler (Init-Eigenschaft, kein Laufzeitfehler).
    pub fn sperren(&mut self, slot: usize) -> bool {
        if slot >= self.plaetze {
            return false;
        }
        let (wi, bit) = (slot / 64, slot % 64);
        if self.bits[wi] & (1u64 << bit) != 0 {
            return false;
        }
        self.bits[wi] |= 1u64 << bit;
        self.vergeben += 1;
        if self.vergeben > self.hoechststand {
            self.hoechststand = self.vergeben;
        }
        true
    }

    // --- Telemetrie (alle Zaehler monoton bzw. exakt, keine Schaetzwerte) ---

    /// Zahl der Slots insgesamt.
    pub fn plaetze(&self) -> usize {
        self.plaetze
    }
    /// Gerade vergebene Slots.
    pub fn vergeben_anzahl(&self) -> usize {
        self.vergeben
    }
    /// Freie Slots (`plaetze - vergeben`, RAM-begrenzt wie `user_kstack_free_count`).
    pub fn frei(&self) -> usize {
        self.plaetze - self.vergeben
    }
    /// Hoechststand seit Arena-Start (Ratsche).
    pub fn hoechststand(&self) -> usize {
        self.hoechststand
    }
    /// Abgewiesene Vergaben (Arena voll).
    pub fn abgewiesen_erschoepft(&self) -> u64 {
        self.abgewiesen_erschoepft
    }
    /// Abgewiesene Freigaben (fremd/unaligned/doppelt-frei).
    pub fn abgewiesen_ungueltig(&self) -> u64 {
        self.abgewiesen_ungueltig
    }
    /// Auslastung in Promille (0..=1000) — ganzzahlig, kein Float im Kernel.
    pub fn auslastung_promille(&self) -> usize {
        if self.plaetze == 0 {
            return 0;
        }
        self.vergeben * 1000 / self.plaetze
    }
    /// Slot-Schritt in Byte (Wache + Stack) — die Zahl, an der die Dichte haengt.
    pub fn schritt(&self) -> usize {
        self.schritt
    }
    /// Nutzbarer Stack je Slot in Byte (ohne Wache).
    pub fn stapel(&self) -> usize {
        self.stapel
    }
    /// Arena-Basis (fuer den Bericht).
    pub fn basis(&self) -> usize {
        self.basis
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Testbett: Bitmap auf dem Stapel des Wirts, Geometrie wie x86 (4 KiB + Wache).
    fn arena8<'a>(bits: &'a mut [u64; 1]) -> StackArena<'a> {
        StackArena::neu(0x10_0000, X86_KSTACK, 8, bits).unwrap()
    }

    #[test]
    fn vergabe_liefert_stapelbasis_mit_wache_darunter() {
        let mut bits = [0u64; 1];
        let mut a = arena8(&mut bits);
        let b0 = a.vergeben().unwrap();
        assert_eq!(b0, 0x10_0000 + SEITE);
        // Naechster Slot: genau einen Schritt weiter, wieder Wache darunter.
        let b1 = a.vergeben().unwrap();
        assert_eq!(b1, b0 + SEITE + X86_KSTACK);
        assert_eq!(a.wache_von_slot(0).unwrap(), b0 - SEITE);
        assert_eq!(a.stapelbasis_von_slot(1).unwrap(), b1);
        assert_eq!(a.vergeben_anzahl(), 2);
        assert_eq!(a.frei(), 6);
    }

    #[test]
    fn freigabe_gibt_slot_zurueck_und_ist_wiedervergebbar() {
        let mut bits = [0u64; 1];
        let mut a = arena8(&mut bits);
        let b0 = a.vergeben().unwrap();
        a.freigeben(b0).unwrap();
        assert!(!a.ist_vergeben(b0));
        assert_eq!(a.vergeben_anzahl(), 0);
        // Hoechststand ist eine Ratsche: Freigabe senkt ihn nicht.
        assert_eq!(a.hoechststand(), 1);
        let b = a.vergeben().unwrap();
        assert_eq!(b, b0); // niedrigster freier Slot zuerst
    }

    #[test]
    fn doppel_frei_wird_abgewiesen() {
        let mut bits = [0u64; 1];
        let mut a = arena8(&mut bits);
        let b0 = a.vergeben().unwrap();
        a.freigeben(b0).unwrap();
        assert_eq!(a.freigeben(b0), Err(ArenaFehler::Ungueltig));
        assert_eq!(a.abgewiesen_ungueltig(), 1);
        // Der Slot blieb frei: keine zweite Buchung verloren.
        assert_eq!(a.vergeben_anzahl(), 0);
    }

    #[test]
    fn erschoepfung_ist_benannt_und_zaehlt() {
        let mut bits = [0u64; 1];
        let mut a = StackArena::neu(0x20_0000, X86_KSTACK, 3, &mut bits).unwrap();
        a.vergeben().unwrap();
        a.vergeben().unwrap();
        a.vergeben().unwrap();
        assert_eq!(a.vergeben(), Err(ArenaFehler::Erschoepft));
        assert_eq!(a.abgewiesen_erschoepft(), 1);
        assert_eq!(a.vergeben(), Err(ArenaFehler::Erschoepft));
        assert_eq!(a.abgewiesen_erschoepft(), 2);
        assert_eq!(a.auslastung_promille(), 1000);
    }

    #[test]
    fn guard_rechnung_je_slot() {
        let mut bits = [0u64; 1];
        let a = arena8(&mut bits);
        for slot in 0..8 {
            let wache = a.wache_von_slot(slot).unwrap();
            let stapel = a.stapelbasis_von_slot(slot).unwrap();
            // Wache seitenausgerichtet, Stack genau eine Seite darueber, Slots lueckenlos.
            assert!(wache.is_multiple_of(SEITE));
            assert_eq!(stapel, wache + SEITE);
            assert_eq!(wache, 0x10_0000 + slot * (SEITE + X86_KSTACK));
            // Rueckrechnung: jede Stack-Basis findet ihren Slot, jede Wache keinen
            // (eine Wache ist kein Stack — wer sie freigaebe, gaebe den Nachbarn frei).
            assert_eq!(a.slot_von_stapelbasis(stapel), Some(slot));
            assert_eq!(a.slot_von_stapelbasis(wache), None);
        }
        assert_eq!(a.wache_von_slot(8), None);
    }

    #[test]
    fn fremde_und_unaligned_freigaben_werden_abgewiesen() {
        let mut bits = [0u64; 1];
        let mut a = arena8(&mut bits);
        let b0 = a.vergeben().unwrap();
        // Fremde Adresse (ausserhalb der Arena).
        assert_eq!(a.freigeben(0x30_0000), Err(ArenaFehler::Ungueltig));
        // Unaligned (Mitte eines Stacks).
        assert_eq!(a.freigeben(b0 + 1), Err(ArenaFehler::Ungueltig));
        // Wachenadresse statt Stack-Basis.
        assert_eq!(a.freigeben(b0 - SEITE), Err(ArenaFehler::Ungueltig));
        // Nie vergebener, aber gueltig aussehender Slot.
        let fremd = a.stapelbasis_von_slot(5).unwrap();
        assert_eq!(a.freigeben(fremd), Err(ArenaFehler::Ungueltig));
        assert_eq!(a.abgewiesen_ungueltig(), 4);
        // Die echte Vergabe steht unversehrt.
        assert!(a.ist_vergeben(b0));
    }

    #[test]
    fn init_weist_schlechte_geometrie_benannt_ab() {
        let mut bits = [0u64; 1];
        // Unausgerichtete Basis.
        assert_eq!(
            StackArena::neu(0x10_0001, X86_KSTACK, 8, &mut bits).err(),
            Some(ArenaFehler::Unausgerichtet)
        );
        // Null-Stapel, nicht seitengranularer Stapel, null Plaetze.
        assert_eq!(
            StackArena::neu(0x10_0000, 0, 8, &mut bits).err(),
            Some(ArenaFehler::Geometrie)
        );
        assert_eq!(
            StackArena::neu(0x10_0000, 1000, 8, &mut bits).err(),
            Some(ArenaFehler::Geometrie)
        );
        assert_eq!(
            StackArena::neu(0x10_0000, X86_KSTACK, 0, &mut bits).err(),
            Some(ArenaFehler::Geometrie)
        );
        // Bitmap zu kurz: 65 Plaetze brauchen 2 Worte.
        let mut kurz = [0u64; 1];
        assert_eq!(
            StackArena::neu(0x10_0000, X86_KSTACK, 65, &mut kurz).err(),
            Some(ArenaFehler::Geometrie)
        );
    }

    #[test]
    fn bitmap_ueber_wortgrenze_traegt() {
        let mut bits = [0u64; 2];
        let mut a = StackArena::neu(0x10_0000, X86_KSTACK, 65, &mut bits).unwrap();
        let mut letzte = 0;
        for _ in 0..65 {
            letzte = a.vergeben().unwrap();
        }
        assert_eq!(a.vergeben(), Err(ArenaFehler::Erschoepft));
        // Slot 64 liegt im zweiten Wort; Freigabe dort gibt genau ihn zurueck.
        a.freigeben(letzte).unwrap();
        assert_eq!(a.vergeben().unwrap(), letzte);
        assert_eq!(a.hoechststand(), 65);
    }

    #[test]
    fn dichte_rechnung_gibt_zwoelf_fach_her() {
        // x86-Schritt 8 KiB: 256 je Block; gemessen ~20 je Block → ~12x.
        assert_eq!(slots_je_block(SEITE + X86_KSTACK), 256);
        let (ok, gewinn) = dichte_gewinn(SEITE + X86_KSTACK, 20);
        assert!(ok);
        assert_eq!(gewinn, 12);
        // aarch64-Schritt 20 KiB: 102 je Block (abgerundet) — ehrlich kleiner.
        assert_eq!(slots_je_block(SEITE + AARCH64_KSTACK), 102);
        // Entartete Eingaben tragen nichts und sagen es.
        assert_eq!(dichte_gewinn(0, 20), (false, 0));
        assert_eq!(dichte_gewinn(8192, 0), (false, 0));
    }

    #[test]
    fn woerter_rechnung() {
        assert_eq!(woerter(1), 1);
        assert_eq!(woerter(64), 1);
        assert_eq!(woerter(65), 2);
    }

    #[test]
    fn sperren_nimmt_genau_diesen_slot_aus_dem_pool() {
        let mut bits = [0u64; 1];
        let mut a = arena8(&mut bits);
        assert!(a.sperren(3));
        assert!(!a.ist_vergeben(a.stapelbasis_von_slot(0).unwrap()));
        // Vergabe ueberspringt den gesperrten Slot.
        let b0 = a.vergeben().unwrap();
        assert_eq!(a.slot_von_stapelbasis(b0), Some(0));
        let b1 = a.vergeben().unwrap();
        assert_eq!(a.slot_von_stapelbasis(b1), Some(1));
        let b2 = a.vergeben().unwrap();
        assert_eq!(a.slot_von_stapelbasis(b2), Some(2));
        let b4 = a.vergeben().unwrap();
        assert_eq!(a.slot_von_stapelbasis(b4), Some(4));
        // Doppel-Sperren und ausserhalb: Absage, kein Zaehler-Schaden.
        assert!(!a.sperren(3));
        assert!(!a.sperren(8));
        assert!(!a.sperren(9000));
        // Freigeben eines gesperrten Slots ist moeglich (Init-Rollback), danach
        // ist er wieder ein normaler Slot.
        let s3 = a.stapelbasis_von_slot(3).unwrap();
        assert!(a.freigeben(s3).is_ok());
        assert!(!a.ist_vergeben(s3));
    }

    #[test]
    fn volle_pd_kosten_beider_architekturen() {
        // x86: 64 KiB privat + 8 KiB Schritt + 3 × 4 KiB Tabellen = 84 KiB.
        assert_eq!(volle_pd_kosten(SEITE + X86_KSTACK), 84 * 1024);
        // aarch64: 64 KiB privat + 20 KiB Schritt + 3 × 4 KiB Tabellen = 96 KiB.
        assert_eq!(volle_pd_kosten(SEITE + AARCH64_KSTACK), 96 * 1024);
    }

    #[test]
    fn plaetze_threads_binden_ohne_kappung() {
        // Genug von allem: die Faeden sind das Minimum, keine Kappung.
        let (p, g) = arena_plaetze(100, u64::MAX, 84 * 1024, None, ARENA_DECKEL_PLAETZE);
        assert_eq!((p, g), (100, GRUND_THREADS));
        // Gleichstand: der schoenste Fall wird genannt.
        let (p, g) = arena_plaetze(100, 100 * 84 * 1024, 84 * 1024, Some(100), 100);
        assert_eq!((p, g), (100, GRUND_THREADS));
    }

    #[test]
    fn plaetze_ram_kappt_volle_pds() {
        // 512-MiB-Lage (x86): ~496 MiB frei, volle PD 84 KiB → ~6046 Plaetze.
        let frei = 496 * 1024 * 1024u64;
        let (p, g) = arena_plaetze(10_000, frei, volle_pd_kosten(SEITE + X86_KSTACK), None, ARENA_DECKEL_PLAETZE);
        assert_eq!(p as u64, frei / (84 * 1024));
        assert_eq!(g, GRUND_RAM);
        assert!(p < 10_000, "bei 512M traegt das RAM keine 10 000 vollen PDs");
    }

    #[test]
    fn plaetze_guard_kappt_auf_x86() {
        // x86-Lage bei 6G: RAM reicht, 16 Splits × 256 Slots = 4096 Plaetze.
        let frei = 6 * 1024 * 1024 * 1024u64;
        let guard = 16 * slots_je_block(SEITE + X86_KSTACK);
        assert_eq!(guard, 4096);
        let (p, g) = arena_plaetze(10_000, frei, volle_pd_kosten(SEITE + X86_KSTACK), Some(guard), ARENA_DECKEL_PLAETZE);
        assert_eq!((p, g), (4096, GRUND_GUARD));
    }

    #[test]
    fn plaetze_deckel_faengt_riesenmaschinen() {
        // 256 Kerne → 65 536 Faeden: der Deckel greift vor dem RAM.
        let (p, g) = arena_plaetze(65_536, u64::MAX, 1, None, ARENA_DECKEL_PLAETZE);
        assert_eq!((p, g), (ARENA_DECKEL_PLAETZE, GRUND_DECKEL));
    }

    #[test]
    fn plaetze_null_ist_benannt() {
        // Keine Faeden, kein RAM, keine Splits: 0 Plaetze mit dem Grund, der zuerst bindet.
        assert_eq!(arena_plaetze(0, 0, 84 * 1024, Some(0), ARENA_DECKEL_PLAETZE).0, 0);
        assert_eq!(arena_plaetze(0, 1 << 40, 1, None, ARENA_DECKEL_PLAETZE).1, GRUND_THREADS);
        assert_eq!(arena_plaetze(10, 0, 84 * 1024, Some(100), ARENA_DECKEL_PLAETZE).1, GRUND_RAM);
        assert_eq!(arena_plaetze(10, 1 << 40, 1, Some(0), ARENA_DECKEL_PLAETZE).1, GRUND_GUARD);
        // Kosten 0 heisst „keine RAM-Schranke" (Division durch null faellt aus).
        assert_eq!(arena_plaetze(7, 0, 0, None, ARENA_DECKEL_PLAETZE), (7, GRUND_THREADS));
    }

    #[test]
    fn gruende_haben_namen() {
        assert_eq!(grund_name(GRUND_THREADS), "Thread-Slots (keine Kappung)");
        assert_eq!(grund_name(GRUND_RAM), "Boot-RAM (nicht mehr volle PDs)");
        assert_eq!(grund_name(GRUND_GUARD), "Guard-Splits (nicht mehr bewachbare Slots)");
        assert_eq!(grund_name(GRUND_DECKEL), "absoluter Deckel");
        assert_eq!(grund_name(GRUND_SPANNE), "Spanne nicht allozierbar");
        assert_eq!(grund_name(GRUND_BITMAP), "Bitmap nicht allozierbar");
        assert_eq!(grund_name(GRUND_GEOMETRIE), "Konstruktion traegt nicht");
        assert_eq!(grund_name(999), "unbekannt");
    }
}
