//! **Protokoll, das nie parkt** (LogFlush-Strang) — bounded Ring-Buffer-Senke für
//! `dev_err`/`printk`-Schemen.
//!
//! ## Wofür das da ist
//!
//! Ein Linux-Treiber protokolliert mit `dev_err`/`printk` — jederzeit, auch aus dem IRQ-Thread,
//! auch wenn niemand liest. In einer PD heisst das: der Aufruf darf **nie parken**. Wer beim
//! Protokollieren auf einen Leser wartete, hielte im IRQ-Thread die ganze PD an — und ein
//! Überlauf, der still verwerfete, äusserte sich als fehlende Zeile, während jeder Prüfer
//! Ordnung meldet.
//!
//! Deshalb [`LogRing`]: begrenzt ([`LOG_PLAETZE`] Einträge), FIFO, und `push` verwirft bei
//! vollem Ring das **ÄLTESTE** und zählt mit ([`LogRing::verworfen`]) — statt zu blockieren.
//! Der Zähler ist die Telemetrie: `> 0` heisst „der Ring ist zu kurz oder der Leser zu langsam",
//! und das steht als Zahl da, nicht als fehlende Zeile.
//!
//! ## Stufen und Filter
//!
//! Drei Stufen ([`Stufe`]) — die `printk`-Abbildung `KERN_INFO`/`KERN_WARNING`/`KERN_ERR`, ohne
//! deren Nummern zu übernehmen: was zählt, ist die Ordnung `Info < Warn < Err`. Der Filter
//! ([`LogRing::set_filter`]) nennt die **leichteste** Stufe, die noch aufbewahrt wird; alles
//! Leichtere wird abgewiesen — und zählt ebenfalls als verworfen (nicht aufbewahrt ist nicht
//! aufbewahrt, gleich aus welchem Grund).
//!
//! ## Was hier NICHT steht
//!
//! Kein Formatieren: der Aufrufer legt Bytes in einen stapellokalen Puffer (`core::fmt` gehört
//! dem Aufrufer, nicht der Senke) und reicht die Scheibe herein; was über [`LOG_TEXT`] hinaus
//! geht, wird **begrenzt, nicht abgewiesen** — eine überlange Zeile gekürzt zu sehen ist mehr
//! wert als gar keine Zeile plus Zähler. Kein Leser-Weckruf: wer Sätze braucht statt Schecks,
//! legt eine `caprock-wait`-Completion daneben (Liste hier, Weckruf dort — dieselbe Teilung wie
//! dort bei der Workqueue).
//!
//! Abhängigkeitsfrei und `forbid(unsafe_code)`.

#![no_std]
#![forbid(unsafe_code)]

/// Wie viele Sätze der Ring fasst. **Benannt statt still**: wer mehr protokolliert, als der
/// Ring hält, sieht das am [`LogRing::verworfen`]-Zähler — nicht an einer Lücke.
pub const LOG_PLAETZE: usize = 64;

/// Wie viele Bytes ein Satz fasst. Längeres wird **begrenzt** (Präfix bleibt stehen), nicht
/// abgewiesen — s. Modul-Doku.
pub const LOG_TEXT: usize = 128;

/// Die Stufe eines Satzes — die `printk`-Abbildung in Caprock-Ordnung: `Info < Warn < Err`.
/// Die Ordnung ist tragend, nicht bequem: der Filter vergleicht damit (`>=` heisst aufbewahren).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Stufe {
    /// `KERN_INFO`: Betrieb, kein Handlungsbedarf.
    Info,
    /// `KERN_WARNING`: auffällig, aber weiter.
    Warn,
    /// `KERN_ERR`: Fehler — was schiefging, steht im Text.
    Err,
}

/// Ein Satz im [`LogRing`]: Stufe plus bis zu [`LOG_TEXT`] Bytes Text.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Eintrag {
    stufe: Stufe,
    laenge: usize,
    text: [u8; LOG_TEXT],
}

impl Eintrag {
    const fn leerer() -> Eintrag {
        Eintrag {
            stufe: Stufe::Info,
            laenge: 0,
            text: [0; LOG_TEXT],
        }
    }

    pub fn stufe(&self) -> Stufe {
        self.stufe
    }

    /// Der Text — genau die übergebenen Bytes (begrenzt auf [`LOG_TEXT`]).
    pub fn text(&self) -> &[u8] {
        &self.text[..self.laenge]
    }

    pub fn len(&self) -> usize {
        self.laenge
    }
    pub fn is_empty(&self) -> bool {
        self.laenge == 0
    }
}

/// **Der Ring** — begrenzt, FIFO, verwirft das Älteste statt zu blockieren.
///
/// Der Schreibstand ist (`anfang`, `n`): die Sätze `0..n` in Aufbewahrungsfolge liegen an
/// `(anfang + i) % LOG_PLAETZE`. Überschreiben heisst: an `anfang` schreiben und `anfang`
/// vorrücken — der Zeiger wandert, der Zähler (`n`) bleibt bei vollen Ringen stehen.
pub struct LogRing {
    ring: [Eintrag; LOG_PLAETZE],
    anfang: usize,
    n: usize,
    filter: Stufe,
    /// Wie viele Sätze nicht aufbewahrt wurden — überschrieben (Ring voll) ODER per Filter
    /// abgewiesen. Sättigend (`u64::MAX` statt Überlauf): ein Zähler, der auf 0 umschlüge,
    /// meldete einen übergelaufenen Verlust als frischen Ring — dieselbe Fehlerform wie ein
    /// verlorener Weckruf, nur in der Telemetrie.
    verworfen: u64,
}

impl Default for LogRing {
    fn default() -> Self {
        Self::new()
    }
}

impl LogRing {
    pub const fn new() -> LogRing {
        LogRing {
            ring: [Eintrag::leerer(); LOG_PLAETZE],
            anfang: 0,
            n: 0,
            filter: Stufe::Info, // alles aufbewahren, bis jemand enger stellt
            verworfen: 0,
        }
    }

    /// Ablegen. Kehrt **immer sofort** zurück — blockiert nie, parkt nie.
    ///
    /// * Stufe unter dem Filter → `false` (abgewiesen, zählt als verworfen).
    /// * Sonst → `true` (aufbewahrt; bei vollem Ring wurde dafür das Älteste verworfen und
    ///   zählt ebenfalls als verworfen).
    pub fn push(&mut self, stufe: Stufe, nachricht: &[u8]) -> bool {
        if stufe < self.filter {
            self.verworfen = self.verworfen.saturating_add(1);
            return false;
        }
        let mut e = Eintrag::leerer();
        e.stufe = stufe;
        let k = if nachricht.len() < LOG_TEXT {
            nachricht.len()
        } else {
            LOG_TEXT
        };
        let mut i = 0;
        while i < k {
            e.text[i] = nachricht[i];
            i += 1;
        }
        e.laenge = k;
        if self.n < LOG_PLAETZE {
            self.ring[(self.anfang + self.n) % LOG_PLAETZE] = e;
            self.n += 1;
        } else {
            // Voll: das ÄLTESTE verwerfen statt zu blockieren — Logging darf nie parken.
            self.ring[self.anfang] = e;
            self.anfang = (self.anfang + 1) % LOG_PLAETZE;
            self.verworfen = self.verworfen.saturating_add(1);
        }
        true
    }

    /// Die leichteste Stufe, die noch aufbewahrt wird (`>=` bleibt, `<` wird abgewiesen).
    /// Voreinstellung: [`Stufe::Info`] (alles).
    pub fn set_filter(&mut self, stufe: Stufe) {
        self.filter = stufe;
    }
    pub fn filter(&self) -> Stufe {
        self.filter
    }

    /// Der i-te Satz in Aufbewahrungsfolge (0 = ältester). `None` bei `i >= len`.
    pub fn eintrag(&self, i: usize) -> Option<&Eintrag> {
        if i >= self.n {
            return None;
        }
        Some(&self.ring[(self.anfang + i) % LOG_PLAETZE])
    }

    /// Den ältesten Satz ziehen (FIFO). `None`, wenn leer.
    pub fn pop(&mut self) -> Option<Eintrag> {
        if self.n == 0 {
            return None;
        }
        let e = self.ring[self.anfang];
        self.anfang = (self.anfang + 1) % LOG_PLAETZE;
        self.n -= 1;
        Some(e)
    }

    /// Alles verwerfen — der Leser fängt neu an. Zähler und Filter bleiben: `verworfen` ist
    /// Geschichte, kein Stand, und der Filter ist Politik, kein Inhalt.
    pub fn leeren(&mut self) {
        self.anfang = 0;
        self.n = 0;
    }

    pub fn len(&self) -> usize {
        self.n
    }
    pub fn is_empty(&self) -> bool {
        self.n == 0
    }
    /// Wie viele Sätze nicht aufbewahrt wurden — s. Feld-Doku. `> 0` heisst: der Ring ist zu
    /// kurz bemessen oder der Leser zu langsam.
    pub fn verworfen(&self) -> u64 {
        self.verworfen
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_around_behaelt_die_neuesten() {
        let mut ring = LogRing::new();
        for i in 0..(LOG_PLAETZE + 3) {
            assert!(ring.push(Stufe::Info, &[i as u8]));
        }
        assert_eq!(ring.len(), LOG_PLAETZE); // begrenzt, nicht gewachsen
        // Die drei Ältesten (0, 1, 2) sind weg, der Älteste ist jetzt 3, der Neueste zuletzt.
        assert_eq!(ring.eintrag(0).unwrap().text(), &[3u8]);
        assert_eq!(
            ring.eintrag(LOG_PLAETZE - 1).unwrap().text(),
            &[(LOG_PLAETZE + 2) as u8]
        );
        assert_eq!(ring.eintrag(LOG_PLAETZE), None); // hinter dem Ende ist nichts
    }

    #[test]
    fn verworfen_zaehlt_jedes_verdraengte() {
        let mut ring = LogRing::new();
        for i in 0..(LOG_PLAETZE + 5) {
            assert!(ring.push(Stufe::Warn, &[i as u8]));
        }
        assert_eq!(ring.verworfen(), 5); // genau die Verdrängten, kein Gefühl, eine Zahl
        assert_eq!(ring.len(), LOG_PLAETZE);
    }

    #[test]
    fn filter_laesst_nur_schwere_durch_und_zaehlt_leichtes() {
        let mut ring = LogRing::new();
        ring.set_filter(Stufe::Warn);
        assert_eq!(ring.filter(), Stufe::Warn);
        assert!(!ring.push(Stufe::Info, b"betrieb")); // abgewiesen, nicht aufbewahrt
        assert_eq!(ring.len(), 0);
        assert_eq!(ring.verworfen(), 1); // nicht aufbewahrt ist nicht aufbewahrt
        assert!(ring.push(Stufe::Warn, b"auffaellig"));
        assert!(ring.push(Stufe::Err, b"fehler"));
        assert_eq!(ring.len(), 2);
        assert_eq!(ring.verworfen(), 1); // Aufbewahrtes zählt nicht
        assert_eq!(ring.pop().unwrap().stufe(), Stufe::Warn); // FIFO auch hier
        assert_eq!(ring.pop().unwrap().stufe(), Stufe::Err);
        assert_eq!(ring.pop(), None);
    }

    #[test]
    fn text_wird_begrenzt_statt_abgewiesen() {
        let mut ring = LogRing::new();
        let lang = [7u8; LOG_TEXT + 10];
        assert!(ring.push(Stufe::Err, &lang)); // kein `false` wegen Länge
        let e = ring.eintrag(0).unwrap();
        assert_eq!(e.len(), LOG_TEXT); // Präfix bleibt stehen
        assert!(e.text().iter().all(|&b| b == 7));
        assert_eq!(ring.verworfen(), 0); // Begrenzen ist kein Verwerfen
    }

    #[test]
    fn pop_liefert_fifo_und_leeren_setzt_nur_den_inhalt_zurueck() {
        let mut ring = LogRing::new();
        assert!(ring.is_empty());
        assert!(ring.push(Stufe::Info, b"eins"));
        assert!(ring.push(Stufe::Err, b"zwei"));
        assert_eq!(ring.pop().unwrap().text(), b"eins");
        assert_eq!(ring.len(), 1);
        ring.leeren();
        assert!(ring.is_empty());
        assert_eq!(ring.pop(), None);
        assert_eq!(ring.filter(), Stufe::Info); // Politik bleibt
        assert_eq!(ring.verworfen(), 0); // Geschichte bleibt
        assert!(ring.push(Stufe::Warn, b"drei")); // und danach geht es weiter
        assert_eq!(ring.eintrag(0).unwrap().text(), b"drei");
    }
}
