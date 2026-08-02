//! **GPT-Partitionstabellen lesen — kernfrei, ohne `unsafe`, ohne Abhaengigkeiten** (A-6.2).
//!
//! Wie `sel4lake-virtio` haengt diese Crate an **nichts**. Sie wird von einer Userland-PD gelinkt,
//! nicht vom Kern: eine Partitionstabelle ist eine Datenstruktur auf einer Platte, die ein
//! beliebiger Mandant beschrieben haben kann. Sie im Kern zu parsen hiesse, fremde Bytes mit
//! Kernprivileg zu interpretieren — und genau das ist die Klasse Fehler, die man dort nicht haben
//! will. Gesetzte Regel (Simon, 2026-08-02): *„moeglichst als Treiber, nicht im Kernel."*
//!
//! ## Was hier geprueft wird, und warum jedes einzeln
//!
//! Ein Parser fuer fremde Daten ist Angriffsflaeche. Deshalb wird **jede** Bedingung einzeln
//! geprueft und mit eigenem Fehler gemeldet, statt in ein `Option` zusammenzufallen:
//!
//! | Bedingung | warum |
//! |---|---|
//! | Signatur `EFI PART` | ohne sie ist alles Weitere Zufall |
//! | Revision `0x00010000` | ein anderes Layout waere anders zu lesen, nicht falsch |
//! | `header_size` in `92..=512` | begrenzt die Flaeche, ueber die die Pruefsumme laeuft |
//! | Kopf-CRC32 | faengt jede stille Aenderung am Kopf |
//! | `entry_size >= 128`, Vielfaches von 8 | sonst zeigen Eintragsgrenzen ins Nichts |
//! | `num_entries * entry_size` ohne Ueberlauf | sonst wird eine Laengenpruefung zur Attrappe |
//! | Eintrags-CRC32 | faengt jede stille Aenderung an der Eintragsliste |
//!
//! **Der Kopf-CRC ist der Punkt, an dem ein Parser gern schlampt:** er wird ueber den Kopf
//! gerechnet, in dem das CRC-Feld selbst steht — und dieses Feld muss dabei als **Null** gelten.
//! Wer es mitrechnet, bekommt nie eine Uebereinstimmung; wer es ueberspringt, statt es zu nullen,
//! verschiebt alle folgenden Bytes. Beides sieht wie „Tabelle kaputt" aus.
//!
//! ## Die Eintragsliste kommt in Stuecken
//!
//! Sie ist typisch 16 KiB gross, und ein Blockdienst liefert weniger je Anfrage. Die Pruefsumme
//! muss trotzdem ueber das **Ganze** gehen — sonst prueft man ein Stueck und glaubt an die
//! Tabelle. Deshalb ist [`Crc32`] **fortschreibbar**: der Aufrufer fuettert Stueck fuer Stueck und
//! vergleicht erst am Ende.

#![no_std]
#![forbid(unsafe_code)]

// Die Crate ist `no_std` (sie laeuft in einer PD ohne Betriebssystem). Der Parser ist aber reine
// Byte-Arithmetik und damit auf dem Host pruefbar — der Testharness braucht dafuer `std`.
#[cfg(test)]
extern crate std;

/// Sektorgroesse, in der GPT rechnet.
pub const SECTOR: usize = 512;
/// Die Signatur im Kopf.
pub const SIGNATURE: [u8; 8] = *b"EFI PART";
/// Die einzige Revision, die dieser Parser liest.
pub const REVISION: u32 = 0x0001_0000;
/// Kleinstmoegliche Kopfgroesse laut Spezifikation.
pub const MIN_HEADER_SIZE: u32 = 92;
/// Kleinstmoegliche Eintragsgroesse laut Spezifikation.
pub const MIN_ENTRY_SIZE: u32 = 128;

/// Warum eine Tabelle nicht gelesen wurde.
///
/// Bewusst **unterscheidbar**: „geht nicht" ist als Diagnose wertlos. Der Unterschied zwischen
/// „hier ist gar keine GPT" (Signatur) und „hier ist eine, die nicht mehr stimmt" (CRC) ist der
/// Unterschied zwischen einer unformatierten Platte und einem Datenverlust.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PartError {
    /// Der uebergebene Puffer ist kuerzer als das, was gelesen werden muesste.
    TooShort,
    /// Keine GPT-Signatur.
    BadSignature,
    /// Andere Revision — anderes Layout, nicht dieselbe Tabelle mit Fehlern.
    BadRevision,
    /// `header_size` ausserhalb von `92..=512`.
    BadHeaderSize,
    /// Die Pruefsumme des Kopfes passt nicht.
    HeaderCrc,
    /// `entry_size` zu klein oder kein Vielfaches von 8.
    BadEntrySize,
    /// `num_entries * entry_size` laeuft ueber oder ist unsinnig gross.
    BadEntryCount,
    /// Die Pruefsumme der Eintragsliste passt nicht.
    EntriesCrc,
}

/// **CRC-32 (ISO-HDLC / zlib)** — fortschreibbar.
///
/// Bitweise gerechnet, ohne Tabelle: eine 1-KiB-Tabelle waere schneller und in einem Parser, der
/// je Boot ein paar Kilobyte prueft, reine Ziererei. Wichtiger ist, dass die Rechnung ohne
/// `unsafe` und ohne Abhaengigkeit auskommt.
#[derive(Clone, Copy)]
pub struct Crc32(u32);

impl Default for Crc32 {
    fn default() -> Self {
        Self::new()
    }
}

impl Crc32 {
    pub const fn new() -> Self {
        Self(0xFFFF_FFFF)
    }

    /// Ein weiteres Stueck einrechnen.
    pub fn update(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 ^= b as u32;
            for _ in 0..8 {
                let mask = (self.0 & 1).wrapping_neg();
                self.0 = (self.0 >> 1) ^ (0xEDB8_8320 & mask);
            }
        }
    }

    /// Das Ergebnis. Der Zustand bleibt unveraendert — weiterfuettern ist erlaubt.
    pub fn finish(&self) -> u32 {
        !self.0
    }
}

/// CRC-32 ueber einen zusammenhaengenden Puffer.
pub fn crc32(bytes: &[u8]) -> u32 {
    let mut c = Crc32::new();
    c.update(bytes);
    c.finish()
}

fn rd32(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

fn rd64(b: &[u8], off: usize) -> u64 {
    let mut v = 0u64;
    let mut i = 0;
    while i < 8 {
        v |= (b[off + i] as u64) << (8 * i);
        i += 1;
    }
    v
}

/// Der geprueft gelesene GPT-Kopf.
///
/// Es gibt ihn **nur** ueber [`parse_header`] — ein Kopf, der nicht geprueft wurde, existiert als
/// Wert gar nicht. Dieselbe Bauart wie `Verified` beim System-Manifest: die Prueffolge traegt der
/// Typ, nicht die Disziplin des Aufrufers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct GptHeader {
    /// LBA, auf der dieser Kopf liegt.
    pub my_lba: u64,
    /// LBA des Sicherungskopfes.
    pub alternate_lba: u64,
    /// Erste und letzte fuer Partitionen nutzbare LBA.
    pub first_usable_lba: u64,
    pub last_usable_lba: u64,
    /// Wo die Eintragsliste beginnt.
    pub entry_lba: u64,
    /// Wie viele Eintraege sie hat und wie gross einer ist.
    pub num_entries: u32,
    pub entry_size: u32,
    /// Pruefsumme, die die **ganze** Eintragsliste haben muss.
    pub entries_crc: u32,
}

impl GptHeader {
    /// Gesamtgroesse der Eintragsliste in Bytes.
    pub fn entries_bytes(&self) -> u64 {
        self.num_entries as u64 * self.entry_size as u64
    }

    /// Wie viele Sektoren die Eintragsliste belegt (aufgerundet).
    pub fn entries_sectors(&self) -> u64 {
        self.entries_bytes().div_ceil(SECTOR as u64)
    }
}

/// Den GPT-Kopf aus dem Sektor lesen, den er belegt (LBA 1).
pub fn parse_header(sector: &[u8]) -> Result<GptHeader, PartError> {
    if sector.len() < MIN_HEADER_SIZE as usize {
        return Err(PartError::TooShort);
    }
    if sector[..8] != SIGNATURE {
        return Err(PartError::BadSignature);
    }
    if rd32(sector, 8) != REVISION {
        return Err(PartError::BadRevision);
    }
    let header_size = rd32(sector, 12);
    if header_size < MIN_HEADER_SIZE || header_size as usize > sector.len() || header_size as usize > SECTOR {
        return Err(PartError::BadHeaderSize);
    }
    // **Das CRC-Feld gilt beim Rechnen als Null**, steht aber an seiner Stelle — nicht
    // uebersprungen, sonst verschoebe sich alles danach.
    let want = rd32(sector, 16);
    let mut c = Crc32::new();
    c.update(&sector[..16]);
    c.update(&[0, 0, 0, 0]);
    c.update(&sector[20..header_size as usize]);
    if c.finish() != want {
        return Err(PartError::HeaderCrc);
    }
    let entry_size = rd32(sector, 84);
    if entry_size < MIN_ENTRY_SIZE || entry_size % 8 != 0 {
        return Err(PartError::BadEntrySize);
    }
    let num_entries = rd32(sector, 80);
    // Die Multiplikation wird **geprueft**, nicht gehofft: eine Laengenpruefung hinter einem
    // uebergelaufenen Produkt ist eine Attrappe. Die Obergrenze ist grosszuegig und trotzdem eine
    // Grenze -- 1 MiB Eintragsliste ist mehr als jedes reale Werkzeug schreibt.
    let total = (num_entries as u64).checked_mul(entry_size as u64);
    match total {
        Some(t) if t > 0 && t <= 1024 * 1024 => {}
        _ => return Err(PartError::BadEntryCount),
    }
    Ok(GptHeader {
        my_lba: rd64(sector, 24),
        alternate_lba: rd64(sector, 32),
        first_usable_lba: rd64(sector, 40),
        last_usable_lba: rd64(sector, 48),
        entry_lba: rd64(sector, 72),
        num_entries,
        entry_size,
        entries_crc: rd32(sector, 88),
    })
}

/// Ein Partitionseintrag.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Partition {
    /// Typ-GUID. Lauter Nullen heisst **unbenutzter Eintrag** — die Liste ist ueblicherweise
    /// deutlich laenger als die Zahl der Partitionen.
    pub type_guid: [u8; 16],
    pub first_lba: u64,
    pub last_lba: u64,
    pub attributes: u64,
}

impl Partition {
    /// Ist der Eintrag belegt?
    pub fn is_used(&self) -> bool {
        self.type_guid != [0u8; 16]
    }

    /// Groesse in Sektoren. `0`, wenn die Grenzen unsinnig sind (`last < first`) — ein
    /// **umgekehrter** Bereich ist kein Grund zu rechnen, sondern einer, nichts zu liefern.
    pub fn sectors(&self) -> u64 {
        if self.last_lba < self.first_lba {
            0
        } else {
            self.last_lba - self.first_lba + 1
        }
    }

    /// Liegt die Partition vollstaendig im nutzbaren Bereich des Kopfes?
    ///
    /// Getrennt von [`Self::is_used`], weil das verschiedene Fragen sind: ein Eintrag kann belegt
    /// **und** unsinnig sein, und wer beides in einem `bool` zusammenfasst, kann eine kaputte
    /// Tabelle nicht mehr von einer leeren unterscheiden.
    pub fn within(&self, h: &GptHeader) -> bool {
        self.sectors() > 0
            && self.first_lba >= h.first_usable_lba
            && self.last_lba <= h.last_usable_lba
    }
}

/// Einen einzelnen Eintrag aus einem **Stueck** der Eintragsliste lesen.
///
/// `chunk` beginnt bei Eintrag `chunk_first`. `None`, wenn `index` nicht in diesem Stueck liegt
/// oder das Stueck zu kurz ist — ein Eintrag, der halb im Puffer liegt, wird **nicht** halb
/// gelesen.
pub fn entry_at(h: &GptHeader, chunk: &[u8], chunk_first: u32, index: u32) -> Option<Partition> {
    if index >= h.num_entries || index < chunk_first {
        return None;
    }
    let off = (index - chunk_first) as usize * h.entry_size as usize;
    let end = off.checked_add(h.entry_size as usize)?;
    if end > chunk.len() {
        return None;
    }
    let e = &chunk[off..end];
    let mut type_guid = [0u8; 16];
    type_guid.copy_from_slice(&e[..16]);
    Some(Partition {
        type_guid,
        first_lba: rd64(e, 32),
        last_lba: rd64(e, 40),
        attributes: rd64(e, 48),
    })
}

/// Die Pruefsumme der **vollstaendig** eingefuetterten Eintragsliste gegen den Kopf halten.
///
/// Der Aufrufer fuettert [`Crc32`] Stueck fuer Stueck mit genau [`GptHeader::entries_bytes`] Bytes
/// — nicht mit ganzen Sektoren, wenn die Liste nicht auf einer Sektorgrenze endet. Genau dieser
/// Unterschied ist der haeufigste Grund fuer eine „kaputte" Tabelle, die in Ordnung ist.
pub fn verify_entries(h: &GptHeader, c: &Crc32) -> Result<(), PartError> {
    if c.finish() == h.entries_crc {
        Ok(())
    } else {
        Err(PartError::EntriesCrc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec;
    use std::vec::Vec;

    /// Den Kopf neu versiegeln: CRC-Feld nullen, Pruefsumme rechnen, eintragen.
    ///
    /// Genau die Falle aus der Modul-Doku, und der Test ist beim ersten Anlauf hineingetappt: wer
    /// `crc32(&h[..92])` rechnet, waehrend im CRC-Feld noch die ALTE Pruefsumme steht, rechnet sie
    /// mit. Ergebnis war `HeaderCrc` statt des Fehlers, den der Test eigentlich ausloesen wollte —
    /// der Test haette also etwas anderes belegt, als er behauptet.
    fn reseal(h: &mut [u8]) {
        h[16..20].copy_from_slice(&0u32.to_le_bytes());
        let crc = crc32(&h[..92]);
        h[16..20].copy_from_slice(&crc.to_le_bytes());
    }

    /// Eine **gueltige** GPT bauen — der Bezugspunkt, gegen den die Mutationen laufen.
    ///
    /// Ohne einen selbst gebauten Positivfall waere jede Abweisung wertlos: ein Parser, der alles
    /// ablehnt, besteht jeden Negativtest.
    fn build(disk_sectors: u64, parts: &[(u64, u64)]) -> (Vec<u8>, Vec<u8>) {
        let num_entries: u32 = 128;
        let entry_size: u32 = 128;
        let mut entries = vec![0u8; (num_entries * entry_size) as usize];
        for (i, &(first, last)) in parts.iter().enumerate() {
            let o = i * entry_size as usize;
            entries[o..o + 16].copy_from_slice(&[0x0Fu8; 16]); // irgendein Typ != 0
            entries[o + 32..o + 40].copy_from_slice(&first.to_le_bytes());
            entries[o + 40..o + 48].copy_from_slice(&last.to_le_bytes());
        }
        let entries_crc = crc32(&entries);

        let mut h = vec![0u8; SECTOR];
        h[..8].copy_from_slice(&SIGNATURE);
        h[8..12].copy_from_slice(&REVISION.to_le_bytes());
        h[12..16].copy_from_slice(&92u32.to_le_bytes());
        // 16..20 = CRC, spaeter
        h[24..32].copy_from_slice(&1u64.to_le_bytes()); // my_lba
        h[32..40].copy_from_slice(&(disk_sectors - 1).to_le_bytes()); // alternate
        h[40..48].copy_from_slice(&34u64.to_le_bytes()); // first usable
        h[48..56].copy_from_slice(&(disk_sectors - 34).to_le_bytes()); // last usable
        h[72..80].copy_from_slice(&2u64.to_le_bytes()); // entry lba
        h[80..84].copy_from_slice(&num_entries.to_le_bytes());
        h[84..88].copy_from_slice(&entry_size.to_le_bytes());
        h[88..92].copy_from_slice(&entries_crc.to_le_bytes());
        reseal(&mut h);
        (h, entries)
    }

    #[test]
    fn crc32_kennt_den_bekannten_wert() {
        // Der Standardvektor. Ohne ihn prueft der Rest nur, dass zwei eigene Rechnungen
        // uebereinstimmen -- was auch bei einem falschen Polynom der Fall waere.
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn crc32_stueckweise_gleich_am_stueck() {
        let data: Vec<u8> = (0u8..=255).cycle().take(3000).collect();
        let mut c = Crc32::new();
        c.update(&data[..1000]);
        c.update(&data[1000..2500]);
        c.update(&data[2500..]);
        assert_eq!(c.finish(), crc32(&data));
    }

    #[test]
    fn gueltige_tabelle_wird_gelesen() {
        let (h, e) = build(2048, &[(34, 1000), (1001, 2000)]);
        let hdr = parse_header(&h).expect("Kopf");
        assert_eq!(hdr.num_entries, 128);
        assert_eq!(hdr.entry_size, 128);
        assert_eq!(hdr.entries_bytes(), 16384);
        assert_eq!(hdr.entries_sectors(), 32);
        let mut c = Crc32::new();
        c.update(&e);
        verify_entries(&hdr, &c).expect("Eintrags-CRC");
        let p0 = entry_at(&hdr, &e, 0, 0).expect("Eintrag 0");
        assert!(p0.is_used());
        assert_eq!(p0.first_lba, 34);
        assert_eq!(p0.sectors(), 967);
        assert!(p0.within(&hdr));
        assert!(!entry_at(&hdr, &e, 0, 2).expect("Eintrag 2").is_used());
    }

    #[test]
    fn eintraege_stueckweise_gelesen_ergeben_dasselbe() {
        // Der Fall, den der Blockdienst wirklich fuehrt: die Liste passt nicht in eine Anfrage.
        let (h, e) = build(2048, &[(34, 1000)]);
        let hdr = parse_header(&h).unwrap();
        let chunk = 4096usize; // 8 Sektoren, wie MAX_SECTORS im Treiber
        let mut c = Crc32::new();
        let mut gefunden = 0;
        let mut off = 0usize;
        while off < hdr.entries_bytes() as usize {
            let end = (off + chunk).min(hdr.entries_bytes() as usize);
            c.update(&e[off..end]);
            let first = (off / hdr.entry_size as usize) as u32;
            let n = (end - off) / hdr.entry_size as usize;
            for k in 0..n {
                let idx = first + k as u32;
                if entry_at(&hdr, &e[off..end], first, idx).unwrap().is_used() {
                    gefunden += 1;
                }
            }
            off = end;
        }
        verify_entries(&hdr, &c).expect("CRC ueber die ganze Liste");
        assert_eq!(gefunden, 1);
    }

    #[test]
    fn falsche_signatur_wird_abgewiesen() {
        let (mut h, _) = build(2048, &[(34, 100)]);
        h[0] = b'X';
        assert_eq!(parse_header(&h), Err(PartError::BadSignature));
    }

    #[test]
    fn fremde_revision_wird_abgewiesen() {
        let (mut h, _) = build(2048, &[(34, 100)]);
        h[8..12].copy_from_slice(&0x0002_0000u32.to_le_bytes());
        assert_eq!(parse_header(&h), Err(PartError::BadRevision));
    }

    #[test]
    fn kaputte_kopf_pruefsumme_wird_abgewiesen() {
        let (mut h, _) = build(2048, &[(34, 100)]);
        h[40] ^= 0xff; // first_usable_lba veraendern, CRC nicht nachziehen
        assert_eq!(parse_header(&h), Err(PartError::HeaderCrc));
    }

    #[test]
    fn kopfgroesse_ausserhalb_der_grenzen_wird_abgewiesen() {
        for size in [0u32, 91, 513, u32::MAX] {
            let (mut h, _) = build(2048, &[(34, 100)]);
            h[12..16].copy_from_slice(&size.to_le_bytes());
            // Die CRC nachziehen, damit wirklich die GROESSE den Ausschlag gibt und nicht
            // nebenbei die Pruefsumme -- sonst belegte der Test etwas anderes, als er behauptet.
            if size >= MIN_HEADER_SIZE && size as usize <= SECTOR {
                reseal(&mut h);
            }
            assert_eq!(parse_header(&h), Err(PartError::BadHeaderSize), "size={size}");
        }
    }

    #[test]
    fn unsinnige_eintragsgroesse_wird_abgewiesen() {
        for size in [0u32, 64, 127, 132] {
            let (mut h, _) = build(2048, &[(34, 100)]);
            h[84..88].copy_from_slice(&size.to_le_bytes());
            reseal(&mut h);
            assert_eq!(parse_header(&h), Err(PartError::BadEntrySize), "size={size}");
        }
    }

    #[test]
    fn ueberlaufende_eintragszahl_wird_abgewiesen() {
        // `num_entries * entry_size` laeuft in 64 Bit nicht ueber, ist aber absurd gross --
        // und genau so sieht der Angriff aus, der eine Laengenpruefung aushebeln soll.
        let (mut h, _) = build(2048, &[(34, 100)]);
        h[80..84].copy_from_slice(&u32::MAX.to_le_bytes());
        reseal(&mut h);
        assert_eq!(parse_header(&h), Err(PartError::BadEntryCount));
    }

    #[test]
    fn kaputte_eintrags_pruefsumme_wird_abgewiesen() {
        let (h, mut e) = build(2048, &[(34, 100)]);
        let hdr = parse_header(&h).unwrap();
        e[64] ^= 0x01; // ein Bit in einem UNBENUTZTEN Eintrag
        let mut c = Crc32::new();
        c.update(&e);
        // Auch eine Aenderung, die keine Partition betrifft, muss auffallen -- sonst waere die
        // Pruefsumme eine Zusage ueber einen Teil der Liste statt ueber die Liste.
        assert_eq!(verify_entries(&hdr, &c), Err(PartError::EntriesCrc));
    }

    #[test]
    fn zu_kurzer_puffer_liefert_keinen_halben_eintrag() {
        let (h, e) = build(2048, &[(34, 100)]);
        let hdr = parse_header(&h).unwrap();
        assert_eq!(entry_at(&hdr, &e[..100], 0, 0), None); // 100 < entry_size
        assert_eq!(entry_at(&hdr, &e, 0, 128), None); // jenseits von num_entries
        assert_eq!(entry_at(&hdr, &e, 4, 0), None); // vor dem Stueckanfang
    }

    #[test]
    fn zu_kurzer_kopfpuffer_wird_abgewiesen() {
        let (h, _) = build(2048, &[(34, 100)]);
        assert_eq!(parse_header(&h[..91]), Err(PartError::TooShort));
    }

    #[test]
    fn umgekehrter_bereich_liefert_null_sektoren() {
        let p = Partition { type_guid: [1; 16], first_lba: 100, last_lba: 50, attributes: 0 };
        assert!(p.is_used());
        assert_eq!(p.sectors(), 0);
        let (h, _) = build(2048, &[(34, 100)]);
        assert!(!p.within(&parse_header(&h).unwrap()));
    }
}
