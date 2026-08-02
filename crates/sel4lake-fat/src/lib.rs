//! **FAT16 lesen — kernfrei, ohne `unsafe`, ohne Abhaengigkeiten** (A-6.3).
//!
//! Wie `sel4lake-part` haengt diese Crate an **nichts** und wird von einer Userland-PD gelinkt.
//! Ein Dateisystem interpretiert **fremde Bytes**, die ein beliebiger Mandant geschrieben haben
//! kann; das im Kern zu tun waere die Klasse Fehler, die man dort nicht haben will. Gesetzte Regel
//! (Simon, 2026-08-02): *„moeglichst als Treiber, nicht im Kernel."*
//!
//! ## Die Form der Schnittstelle: Geometrie und Schritte, kein I/O
//!
//! Diese Crate liest **nichts**. Sie rechnet aus, **welchen Sektor** der Aufrufer als naechstes
//! braucht, und wertet aus, was er zurueckbringt. Das ist Absicht: der Aufrufer holt seine Sektoren
//! ueber einen Blockdienst per IPC, und eine Crate, die dafuer einen Callback oder ein Trait
//! verlangte, muesste dessen Fehlerfaelle mitmodellieren. So bleibt sie reine Arithmetik — und
//! damit auf dem Host in Sekunden pruefbar.
//!
//! ## Warum FAT16 und nicht FAT12
//!
//! FAT12 packt seine Eintraege in 12 Bit, also **ueber Bytegrenzen hinweg** — und die Nibble-Wahl
//! haengt an der Paritaet der Clusternummer. Das ist kein prinzipielles Problem, aber eine
//! zusaetzliche Fehlerquelle in einem Parser, der ohnehin fremde Daten liest. FAT16 hat gerade
//! Eintraege; die Grenze ist die Clusterzahl (`>= 4085`), und die wird hier **geprueft**, nicht
//! angenommen — eine als FAT16 gelesene FAT12-Tabelle liefert lauter falsche Ketten.
//!
//! ## Was diese Crate NICHT tut
//!
//! Schreiben. Lange Dateinamen (VFAT). Unterverzeichnisse. Alles drei ist eigene Arbeit mit
//! eigenen Fehlerfaellen; was hier steht, soll vollstaendig geprueft sein und nicht breit.

#![no_std]
#![forbid(unsafe_code)]

// Die Crate ist `no_std`. Der Parser ist reine Byte-Arithmetik und damit auf dem Host pruefbar --
// der Testharness braucht dafuer `std`.
#[cfg(test)]
extern crate std;

/// Groesse eines Verzeichniseintrags.
pub const DIR_ENTRY: usize = 32;
/// Kleinste Clusterzahl, ab der eine Tabelle FAT16 ist (darunter: FAT12).
pub const FAT16_MIN_CLUSTERS: u32 = 4085;
/// Groesste Clusterzahl fuer FAT16 (darueber: FAT32).
pub const FAT16_MAX_CLUSTERS: u32 = 65524;

/// Warum ein Dateisystem nicht gelesen wurde.
///
/// Bewusst **unterscheidbar**: „hier ist kein FAT" und „hier ist ein FAT32, das dieser Parser
/// nicht liest" sind verschiedene Lagen, und nur die zweite ist eine Luecke dieses Codes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FatError {
    /// Der Puffer ist kuerzer als ein Sektor.
    TooShort,
    /// Die Signatur `0x55AA` am Sektorende fehlt.
    BadSignature,
    /// Sektorgroesse ist nicht 512 — dieser Parser rechnet in 512-Byte-Sektoren.
    BadSectorSize,
    /// Sektoren je Cluster ist keine Zweierpotenz in `1..=128`.
    BadClusterSize,
    /// Reservierte Sektoren, FAT-Zahl oder FAT-Groesse sind null/unsinnig.
    BadGeometry,
    /// Die Wurzelverzeichnis-Eintragszahl passt nicht auf ganze Sektoren.
    BadRootEntries,
    /// Die Geometrie laeuft ueber die Partition hinaus.
    TooBig,
    /// Die Clusterzahl liegt ausserhalb des FAT16-Bereichs (`4085..=65524`).
    NotFat16,
}

fn rd16(b: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([b[off], b[off + 1]])
}

fn rd32(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

/// Die geprueft gelesene Geometrie eines FAT16-Dateisystems.
///
/// Alle LBAs sind **relativ zum Anfang der Partition**. Wo die liegt, weiss diese Crate nicht und
/// soll es nicht wissen — das ist die Aufgabe der Partitionstabelle (`sel4lake-part`). Zwei
/// Schichten, die beide meinen, den Plattenanfang zu kennen, sind eine Schicht zu viel.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Fat16 {
    pub sectors_per_cluster: u32,
    pub reserved_sectors: u32,
    pub num_fats: u32,
    pub fat_sectors: u32,
    pub root_entries: u32,
    pub total_sectors: u32,
    /// Erster Sektor der ersten FAT.
    pub fat_lba: u32,
    /// Erster Sektor des Wurzelverzeichnisses.
    pub root_lba: u32,
    /// Wie viele Sektoren das Wurzelverzeichnis belegt.
    pub root_sectors: u32,
    /// Erster Datensektor (Cluster 2 beginnt hier).
    pub data_lba: u32,
    /// Zahl der Datencluster.
    pub clusters: u32,
}

/// Den Bootsektor (ersten Sektor der Partition) auswerten.
pub fn parse_boot(sector: &[u8]) -> Result<Fat16, FatError> {
    if sector.len() < 512 {
        return Err(FatError::TooShort);
    }
    if sector[510] != 0x55 || sector[511] != 0xAA {
        return Err(FatError::BadSignature);
    }
    if rd16(sector, 11) != 512 {
        return Err(FatError::BadSectorSize);
    }
    let spc = sector[13] as u32;
    if spc == 0 || spc > 128 || !spc.is_power_of_two() {
        return Err(FatError::BadClusterSize);
    }
    let reserved = rd16(sector, 14) as u32;
    let num_fats = sector[16] as u32;
    let fat_sectors = rd16(sector, 22) as u32;
    if reserved == 0 || num_fats == 0 || num_fats > 4 || fat_sectors == 0 {
        return Err(FatError::BadGeometry);
    }
    let root_entries = rd16(sector, 17) as u32;
    // Das Wurzelverzeichnis muss auf ganze Sektoren aufgehen. Ein angebrochener letzter Sektor
    // waere ein halber Eintrag -- und ein halber Eintrag wird hier nicht gelesen.
    if root_entries == 0 || (root_entries as usize * DIR_ENTRY) % 512 != 0 {
        return Err(FatError::BadRootEntries);
    }
    let total16 = rd16(sector, 19) as u32;
    let total32 = rd32(sector, 32);
    let total_sectors = if total16 != 0 { total16 } else { total32 };
    if total_sectors == 0 {
        return Err(FatError::BadGeometry);
    }
    let root_sectors = (root_entries * DIR_ENTRY as u32) / 512;
    // **Jede Addition wird geprueft.** Eine Geometrie aus fremden Bytes darf nicht ueberlaufen und
    // damit eine spaetere Bereichspruefung zur Attrappe machen.
    let fat_lba = reserved;
    let root_lba = fat_lba
        .checked_add(num_fats.checked_mul(fat_sectors).ok_or(FatError::TooBig)?)
        .ok_or(FatError::TooBig)?;
    let data_lba = root_lba.checked_add(root_sectors).ok_or(FatError::TooBig)?;
    if data_lba >= total_sectors {
        return Err(FatError::TooBig);
    }
    let clusters = (total_sectors - data_lba) / spc;
    if !(FAT16_MIN_CLUSTERS..=FAT16_MAX_CLUSTERS).contains(&clusters) {
        return Err(FatError::NotFat16);
    }
    Ok(Fat16 {
        sectors_per_cluster: spc,
        reserved_sectors: reserved,
        num_fats,
        fat_sectors,
        root_entries,
        total_sectors,
        fat_lba,
        root_lba,
        root_sectors,
        data_lba,
        clusters,
    })
}

/// Ein Verzeichniseintrag (8.3).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DirEntry {
    /// Name in 8.3-Form, **ohne** Punkt, mit Leerzeichen aufgefuellt — genau wie auf der Platte.
    pub name: [u8; 11],
    pub attr: u8,
    pub first_cluster: u16,
    pub size: u32,
}

impl DirEntry {
    /// Ist das ein normaler Dateieintrag?
    ///
    /// Aussortiert werden: freie und geloeschte Eintraege, Datentraegerbezeichnungen und
    /// VFAT-Fragmente (Attribut `0x0F`). Die VFAT-Fragmente sind der Grund, warum das eine eigene
    /// Funktion ist: sie stehen **vor** dem 8.3-Eintrag, sehen wie Muell aus und wuerden einen
    /// naiven Leser aus dem Tritt bringen.
    pub fn is_file(&self) -> bool {
        self.name[0] != 0x00 && self.name[0] != 0xE5 && self.attr & 0x08 == 0 && self.attr != 0x0F
    }

    /// Ist das der Endmarker des Verzeichnisses? Danach kommt nichts mehr.
    pub fn is_end(&self) -> bool {
        self.name[0] == 0x00
    }
}

/// Den Eintrag `index` aus einem **Stueck** des Verzeichnisses lesen, das bei `chunk_first` beginnt.
///
/// `None`, wenn der Eintrag nicht in diesem Stueck liegt oder es zu kurz ist — ein Eintrag, der
/// halb im Puffer liegt, wird nicht halb gelesen.
pub fn dir_entry_at(chunk: &[u8], chunk_first: u32, index: u32) -> Option<DirEntry> {
    if index < chunk_first {
        return None;
    }
    let off = (index - chunk_first) as usize * DIR_ENTRY;
    let end = off.checked_add(DIR_ENTRY)?;
    if end > chunk.len() {
        return None;
    }
    let e = &chunk[off..end];
    let mut name = [0u8; 11];
    name.copy_from_slice(&e[..11]);
    Some(DirEntry {
        name,
        attr: e[11],
        first_cluster: rd16(e, 26),
        size: rd32(e, 28),
    })
}

impl Fat16 {
    /// Erster Sektor eines Clusters (relativ zur Partition). `None` fuer Clusternummern ausserhalb
    /// des Datenbereichs — Cluster 0 und 1 gibt es nicht, sie sind in der FAT als Marker belegt.
    pub fn cluster_lba(&self, cluster: u16) -> Option<u32> {
        let c = cluster as u32;
        if c < 2 || c - 2 >= self.clusters {
            return None;
        }
        Some(self.data_lba + (c - 2) * self.sectors_per_cluster)
    }

    /// In welchem FAT-Sektor der Eintrag fuer `cluster` steht, und an welchem Byte darin.
    pub fn fat_entry_pos(&self, cluster: u16) -> (u32, usize) {
        let byte = cluster as u32 * 2;
        (self.fat_lba + byte / 512, (byte % 512) as usize)
    }

    /// Den Nachfolger von `cluster` aus dem passenden FAT-Sektor lesen.
    ///
    /// `None`, wenn der Puffer zu kurz ist. Der Rueckgabewert ist der **rohe** Eintrag; ob er ein
    /// Kettenende ist, sagt [`Self::is_end_of_chain`] — die beiden zu vermengen hiesse, einen
    /// defekten Cluster (`0xFFF7`) wie ein Ende zu behandeln und stillschweigend eine kuerzere
    /// Datei zu liefern.
    pub fn next_cluster(&self, fat_sector: &[u8], cluster: u16) -> Option<u16> {
        let (_, off) = self.fat_entry_pos(cluster);
        if off + 2 > fat_sector.len() {
            return None;
        }
        Some(rd16(fat_sector, off))
    }

    /// Ist `entry` ein regulaeres Kettenende (`>= 0xFFF8`)?
    pub fn is_end_of_chain(&self, entry: u16) -> bool {
        entry >= 0xFFF8
    }

    /// Ist `entry` ein **defekter** Cluster (`0xFFF7`)? Kein Ende — ein Fehler.
    pub fn is_bad(&self, entry: u16) -> bool {
        entry == 0xFFF7
    }

    /// Ist `entry` eine gueltige Fortsetzung (zeigt auf einen echten Datencluster)?
    pub fn is_next(&self, entry: u16) -> bool {
        let c = entry as u32;
        c >= 2 && c - 2 < self.clusters
    }

    /// Bytes je Cluster.
    pub fn cluster_bytes(&self) -> u32 {
        self.sectors_per_cluster * 512
    }
}

/// Einen 8.3-Namen aus der ueblichen Schreibweise bauen (`"TEST    TXT"` aus `"TEST.TXT"`).
///
/// `None`, wenn der Name nicht in 8.3 passt. Bewusst **kein** stilles Abschneiden: ein Name, der
/// nicht passt, bezeichnet keine Datei, und ein abgeschnittener bezeichnet die falsche.
pub fn name83(s: &[u8]) -> Option<[u8; 11]> {
    let mut out = [b' '; 11];
    let punkt = s.iter().position(|&c| c == b'.');
    let (stamm, endung): (&[u8], &[u8]) = match punkt {
        Some(i) => (&s[..i], &s[i + 1..]),
        None => (s, &[]),
    };
    if stamm.is_empty() || stamm.len() > 8 || endung.len() > 3 {
        return None;
    }
    for (i, &c) in stamm.iter().enumerate() {
        out[i] = c.to_ascii_uppercase();
    }
    for (i, &c) in endung.iter().enumerate() {
        out[8 + i] = c.to_ascii_uppercase();
    }
    Some(out)
}

// --- Schreibseite (A-6.4) ---------------------------------------------------------------------
//
// Auch hier: diese Crate schreibt **nichts**. Sie aendert Bytes in einem Puffer, den der Aufrufer
// gelesen hat und danach zurueckschreibt. Read-Modify-Write bleibt beim Aufrufer, weil nur er
// weiss, wie er an Sektoren kommt.

/// Ein FAT-Eintrag in einem gelesenen FAT-Sektor **setzen**.
///
/// `None`, wenn der Eintrag nicht in diesem Sektor liegt — ein halb getroffener Eintrag wird nicht
/// halb geschrieben.
pub fn set_fat_entry(fat_sector: &mut [u8], off: usize, wert: u16) -> Option<()> {
    if off + 2 > fat_sector.len() {
        return None;
    }
    fat_sector[off..off + 2].copy_from_slice(&wert.to_le_bytes());
    Some(())
}

/// Den ersten **freien** Cluster in einem gelesenen FAT-Sektor suchen.
///
/// `first_cluster` ist die Clusternummer, die zum ersten Eintrag dieses Sektors gehoert. Frei
/// heisst `0x0000`. Cluster 0 und 1 sind Marker und werden uebersprungen — sie als frei zu melden
/// waere der klassische Weg, eine FAT zu zerstoeren.
pub fn find_free(fat_sector: &[u8], first_cluster: u16, clusters: u32) -> Option<u16> {
    let n = fat_sector.len() / 2;
    for i in 0..n {
        let c = first_cluster as u32 + i as u32;
        if c < 2 || c - 2 >= clusters {
            continue;
        }
        if u16::from_le_bytes([fat_sector[i * 2], fat_sector[i * 2 + 1]]) == 0 {
            return Some(c as u16);
        }
    }
    None
}

/// Groesse und Startcluster eines Verzeichniseintrags in einem gelesenen Verzeichnis-Stueck
/// **setzen**.
pub fn set_dir_entry(
    chunk: &mut [u8],
    chunk_first: u32,
    index: u32,
    first_cluster: u16,
    size: u32,
) -> Option<()> {
    if index < chunk_first {
        return None;
    }
    let off = (index - chunk_first) as usize * DIR_ENTRY;
    let end = off.checked_add(DIR_ENTRY)?;
    if end > chunk.len() {
        return None;
    }
    chunk[off + 26..off + 28].copy_from_slice(&first_cluster.to_le_bytes());
    chunk[off + 28..off + 32].copy_from_slice(&size.to_le_bytes());
    Some(())
}

impl Fat16 {
    /// Der Sektor der **`n`-ten Kopie** der FAT, der den Eintrag fuer `cluster` traegt.
    ///
    /// **Der Grund, warum es diese Funktion gibt:** ein FAT-Dateisystem hat ueblicherweise ZWEI
    /// Kopien der Tabelle. Wer nur die erste fortschreibt, hinterlaesst ein Dateisystem, das jedes
    /// Pruefwerkzeug als beschaedigt meldet — und das ein Leser, der die zweite Kopie benutzt,
    /// anders sieht als der Schreiber. Die Zahl der Kopien steht im Bootsektor
    /// ([`Fat16::num_fats`]); sie zu ignorieren ist der haeufigste Schreibfehler ueberhaupt.
    pub fn fat_copy_lba(&self, kopie: u32, cluster: u16) -> Option<u32> {
        if kopie >= self.num_fats {
            return None;
        }
        let (lba, _) = self.fat_entry_pos(cluster);
        Some(lba + kopie * self.fat_sectors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec;
    use std::vec::Vec;

    /// Ein **gueltiges** FAT16 bauen — der Bezugspunkt, gegen den die Mutationen laufen.
    /// Ohne Positivfall waere jede Abweisung wertlos: ein Parser, der alles ablehnt, besteht
    /// jeden Negativtest.
    ///
    /// Gibt `(Partition als Bytes, Fat16-Geometrie)`.
    fn build(total: u32, dateien: &[(&str, &[u8])]) -> Vec<u8> {
        let spc = 1u32;
        let reserved = 1u32;
        let num_fats = 2u32;
        let root_entries = 512u32;
        let root_sectors = root_entries * DIR_ENTRY as u32 / 512;
        // FAT-Groesse so waehlen, dass sie zur Clusterzahl passt (Fixpunkt in einem Schritt).
        let mut fat_sectors = 1u32;
        for _ in 0..8 {
            let data = total - reserved - num_fats * fat_sectors - root_sectors;
            fat_sectors = (data / spc * 2).div_ceil(512).max(1);
        }
        let fat_lba = reserved;
        let root_lba = fat_lba + num_fats * fat_sectors;
        let data_lba = root_lba + root_sectors;
        let clusters = (total - data_lba) / spc;

        let mut img = vec![0u8; (total * 512) as usize];
        // Bootsektor
        let b = &mut img[..512];
        b[11..13].copy_from_slice(&512u16.to_le_bytes());
        b[13] = spc as u8;
        b[14..16].copy_from_slice(&(reserved as u16).to_le_bytes());
        b[16] = num_fats as u8;
        b[17..19].copy_from_slice(&(root_entries as u16).to_le_bytes());
        b[19..21].copy_from_slice(&(total as u16).to_le_bytes());
        b[22..24].copy_from_slice(&(fat_sectors as u16).to_le_bytes());
        b[510] = 0x55;
        b[511] = 0xAA;

        // Dateien anlegen: Cluster fortlaufend ab 2, FAT-Kette + Verzeichniseintrag.
        let mut naechster = 2u16;
        for (i, (name, inhalt)) in dateien.iter().enumerate() {
            let n = ((inhalt.len() as u32).max(1)).div_ceil(spc * 512).max(1);
            let start = naechster;
            for k in 0..n {
                let c = start + k as u16;
                let next = if k + 1 == n { 0xFFFFu16 } else { c + 1 };
                for f in 0..num_fats {
                    let o = ((fat_lba + f * fat_sectors) * 512) as usize + c as usize * 2;
                    img[o..o + 2].copy_from_slice(&next.to_le_bytes());
                }
                let lba = data_lba + (c as u32 - 2) * spc;
                let von = (k as usize) * (spc as usize * 512);
                let bis = (von + spc as usize * 512).min(inhalt.len());
                if von < inhalt.len() {
                    let o = (lba * 512) as usize;
                    img[o..o + (bis - von)].copy_from_slice(&inhalt[von..bis]);
                }
            }
            let o = (root_lba * 512) as usize + i * DIR_ENTRY;
            img[o..o + 11].copy_from_slice(&name83(name.as_bytes()).unwrap());
            img[o + 11] = 0x20; // Archiv
            img[o + 26..o + 28].copy_from_slice(&start.to_le_bytes());
            img[o + 28..o + 32].copy_from_slice(&(inhalt.len() as u32).to_le_bytes());
            naechster = start + n as u16;
        }
        let _ = clusters;
        img
    }

    /// Eine Datei ueber die Kette lesen — so, wie es die PD tut: Sektor holen, Schritt rechnen.
    fn read_file(fs: &Fat16, img: &[u8], e: &DirEntry) -> Vec<u8> {
        let mut out = Vec::new();
        let mut c = e.first_cluster;
        while out.len() < e.size as usize {
            let lba = fs.cluster_lba(c).expect("Cluster im Datenbereich");
            let o = (lba * 512) as usize;
            let n = (e.size as usize - out.len()).min(fs.cluster_bytes() as usize);
            out.extend_from_slice(&img[o..o + n]);
            let (fat_lba, _) = fs.fat_entry_pos(c);
            let fat_sector = &img[(fat_lba * 512) as usize..(fat_lba * 512) as usize + 512];
            let next = fs.next_cluster(fat_sector, c).unwrap();
            if fs.is_end_of_chain(next) {
                break;
            }
            assert!(!fs.is_bad(next), "defekter Cluster in der Kette");
            assert!(fs.is_next(next), "Kette zeigt aus dem Datenbereich");
            c = next;
        }
        out
    }

    #[test]
    fn gueltiges_fat16_wird_gelesen() {
        let img = build(20000, &[("HELLO.TXT", b"hallo")]);
        let fs = parse_boot(&img[..512]).expect("Bootsektor");
        assert_eq!(fs.sectors_per_cluster, 1);
        assert_eq!(fs.num_fats, 2);
        assert!(fs.clusters >= FAT16_MIN_CLUSTERS, "clusters={}", fs.clusters);
        assert!(fs.clusters <= FAT16_MAX_CLUSTERS);
        // Wurzelverzeichnis: erster Eintrag ist HELLO.TXT.
        let root = &img[(fs.root_lba * 512) as usize..][..512];
        let e = dir_entry_at(root, 0, 0).unwrap();
        assert!(e.is_file());
        assert_eq!(&e.name, &name83(b"HELLO.TXT").unwrap());
        assert_eq!(e.size, 5);
        assert_eq!(read_file(&fs, &img, &e), b"hallo");
    }

    #[test]
    fn datei_ueber_mehrere_cluster() {
        // Der eigentliche Test der FAT: eine Datei, die nicht in einen Cluster passt. Ohne sie
        // prueft man nur, dass der erste Cluster gefunden wird -- die KETTE bliebe ungeprueft.
        let inhalt: Vec<u8> = (0..2000u32).map(|i| (i % 251) as u8).collect();
        let img = build(20000, &[("BIG.BIN", &inhalt)]);
        let fs = parse_boot(&img[..512]).unwrap();
        let root = &img[(fs.root_lba * 512) as usize..][..512];
        let e = dir_entry_at(root, 0, 0).unwrap();
        assert_eq!(e.size, 2000);
        assert_eq!(read_file(&fs, &img, &e), inhalt);
    }

    #[test]
    fn zwei_dateien_werden_unterschieden() {
        let img = build(20000, &[("A.TXT", b"eins"), ("B.TXT", b"zwei")]);
        let fs = parse_boot(&img[..512]).unwrap();
        let root = &img[(fs.root_lba * 512) as usize..][..512];
        let a = dir_entry_at(root, 0, 0).unwrap();
        let b = dir_entry_at(root, 0, 1).unwrap();
        assert_eq!(read_file(&fs, &img, &a), b"eins");
        assert_eq!(read_file(&fs, &img, &b), b"zwei");
        assert!(dir_entry_at(root, 0, 2).unwrap().is_end());
    }

    #[test]
    fn fehlende_signatur_wird_abgewiesen() {
        let mut img = build(20000, &[("A.TXT", b"x")]);
        img[511] = 0x00;
        assert_eq!(parse_boot(&img[..512]), Err(FatError::BadSignature));
    }

    #[test]
    fn fremde_sektorgroesse_wird_abgewiesen() {
        let mut img = build(20000, &[("A.TXT", b"x")]);
        img[11..13].copy_from_slice(&4096u16.to_le_bytes());
        assert_eq!(parse_boot(&img[..512]), Err(FatError::BadSectorSize));
    }

    #[test]
    fn unsinnige_clustergroesse_wird_abgewiesen() {
        for spc in [0u8, 3, 5, 200] {
            let mut img = build(20000, &[("A.TXT", b"x")]);
            img[13] = spc;
            assert_eq!(parse_boot(&img[..512]), Err(FatError::BadClusterSize), "spc={spc}");
        }
    }

    #[test]
    fn nullgeometrie_wird_abgewiesen() {
        for (off, len) in [(14usize, 2usize), (16, 1), (22, 2)] {
            let mut img = build(20000, &[("A.TXT", b"x")]);
            for k in 0..len {
                img[off + k] = 0;
            }
            assert_eq!(parse_boot(&img[..512]), Err(FatError::BadGeometry), "off={off}");
        }
    }

    #[test]
    fn angebrochenes_wurzelverzeichnis_wird_abgewiesen() {
        let mut img = build(20000, &[("A.TXT", b"x")]);
        img[17..19].copy_from_slice(&5u16.to_le_bytes()); // 5*32 = 160, kein ganzer Sektor
        assert_eq!(parse_boot(&img[..512]), Err(FatError::BadRootEntries));
    }

    #[test]
    fn zu_wenige_cluster_sind_kein_fat16() {
        // **Die Falle, gegen die `clusters` geprueft wird:** eine kleine Partition ist FAT12, und
        // als FAT16 gelesen liefert sie lauter falsche Ketten -- ohne dass irgendetwas kaputt
        // aussieht.
        let img = build(2000, &[("A.TXT", b"x")]);
        assert_eq!(parse_boot(&img[..512]), Err(FatError::NotFat16));
    }

    #[test]
    fn geometrie_jenseits_der_partition_wird_abgewiesen() {
        let mut img = build(20000, &[("A.TXT", b"x")]);
        img[22..24].copy_from_slice(&30000u16.to_le_bytes()); // FAT groesser als die Partition
        assert_eq!(parse_boot(&img[..512]), Err(FatError::TooBig));
    }

    #[test]
    fn zu_kurzer_puffer_wird_abgewiesen() {
        let img = build(20000, &[("A.TXT", b"x")]);
        assert_eq!(parse_boot(&img[..511]), Err(FatError::TooShort));
    }

    #[test]
    fn cluster_ausserhalb_des_datenbereichs_liefert_nichts() {
        let img = build(20000, &[("A.TXT", b"x")]);
        let fs = parse_boot(&img[..512]).unwrap();
        assert_eq!(fs.cluster_lba(0), None);
        assert_eq!(fs.cluster_lba(1), None);
        assert!(fs.cluster_lba(2).is_some());
        assert_eq!(fs.cluster_lba(u16::MAX), None);
        assert!(!fs.is_next(0));
        assert!(!fs.is_next(1));
        assert!(fs.is_next(2));
    }

    #[test]
    fn kettenende_und_defekter_cluster_sind_verschieden() {
        let img = build(20000, &[("A.TXT", b"x")]);
        let fs = parse_boot(&img[..512]).unwrap();
        assert!(fs.is_end_of_chain(0xFFFF));
        assert!(fs.is_end_of_chain(0xFFF8));
        assert!(!fs.is_end_of_chain(0xFFF7));
        assert!(fs.is_bad(0xFFF7));
        // Ein defekter Cluster als Ende zu lesen hiesse, stillschweigend eine kuerzere Datei zu
        // liefern -- Datenverlust, der wie Erfolg aussieht.
        assert!(!fs.is_bad(0xFFFF));
    }

    #[test]
    fn halber_verzeichniseintrag_wird_nicht_gelesen() {
        let img = build(20000, &[("A.TXT", b"x")]);
        let fs = parse_boot(&img[..512]).unwrap();
        let root = &img[(fs.root_lba * 512) as usize..][..512];
        assert_eq!(dir_entry_at(&root[..20], 0, 0), None);
        assert_eq!(dir_entry_at(root, 0, 16), None); // 16*32 = 512, genau hinter dem Puffer
        assert_eq!(dir_entry_at(root, 4, 0), None); // vor dem Stueckanfang
    }

    #[test]
    fn schreiben_setzt_beide_fat_kopien() {
        // Der Test, der den haeufigsten Schreibfehler faengt: nur die erste Kopie fortschreiben.
        let mut img = build(20000, &[("A.TXT", b"x")]);
        let fs = parse_boot(&img[..512]).unwrap();
        assert_eq!(fs.num_fats, 2);
        let c = 100u16;
        for kopie in 0..fs.num_fats {
            let lba = fs.fat_copy_lba(kopie, c).unwrap();
            let o = (lba * 512) as usize;
            let (_, off) = fs.fat_entry_pos(c);
            set_fat_entry(&mut img[o..o + 512], off, 0xABCD).unwrap();
        }
        for kopie in 0..fs.num_fats {
            let lba = fs.fat_copy_lba(kopie, c).unwrap();
            let sek = &img[(lba * 512) as usize..][..512];
            assert_eq!(fs.next_cluster(sek, c), Some(0xABCD), "Kopie {kopie}");
        }
        assert_eq!(fs.fat_copy_lba(2, c), None); // es gibt nur zwei
    }

    #[test]
    fn freier_cluster_ueberspringt_0_und_1() {
        let img = build(20000, &[("A.TXT", b"x")]);
        let fs = parse_boot(&img[..512]).unwrap();
        let sek = &img[(fs.fat_lba * 512) as usize..][..512];
        // Cluster 2 ist von A.TXT belegt -> der erste freie ist 3. Waeren 0/1 nicht
        // uebersprungen, kaeme 0 zurueck -- und ein Schreiber wuerde die Marker ueberschreiben.
        assert_eq!(find_free(sek, 0, fs.clusters), Some(3));
    }

    #[test]
    fn geschriebene_kette_wird_wieder_gelesen() {
        // Ende zu Ende auf dem Puffer: eine Datei auf zwei Cluster verlaengern, dann lesen.
        let mut img = build(20000, &[("A.TXT", b"x")]);
        let fs = parse_boot(&img[..512]).unwrap();
        let inhalt: Vec<u8> = (0..700u32).map(|i| ((i * 7 + 13) % 251) as u8).collect();
        let c1 = 2u16;
        let c2 = {
            let sek = &img[(fs.fat_lba * 512) as usize..][..512];
            find_free(sek, 0, fs.clusters).unwrap()
        };
        // Daten
        for (k, c) in [c1, c2].iter().enumerate() {
            let lba = fs.cluster_lba(*c).unwrap();
            let von = k * 512;
            let bis = (von + 512).min(inhalt.len());
            let o = (lba * 512) as usize;
            img[o..o + (bis - von)].copy_from_slice(&inhalt[von..bis]);
        }
        // Kette in BEIDE Kopien
        for kopie in 0..fs.num_fats {
            for (c, next) in [(c1, c2), (c2, 0xFFFFu16)] {
                let lba = fs.fat_copy_lba(kopie, c).unwrap();
                let (_, off) = fs.fat_entry_pos(c);
                let o = (lba * 512) as usize;
                set_fat_entry(&mut img[o..o + 512], off, next).unwrap();
            }
        }
        // Verzeichniseintrag
        let ro = (fs.root_lba * 512) as usize;
        set_dir_entry(&mut img[ro..ro + 512], 0, 0, c1, inhalt.len() as u32).unwrap();

        let e = dir_entry_at(&img[ro..ro + 512], 0, 0).unwrap();
        assert_eq!(e.size, 700);
        assert_eq!(read_file(&fs, &img, &e), inhalt);
    }

    #[test]
    fn halber_eintrag_wird_nicht_halb_geschrieben() {
        let mut puffer = [0u8; 511];
        assert_eq!(set_fat_entry(&mut puffer, 510, 1), None);
        let mut dir = [0u8; 40];
        assert_eq!(set_dir_entry(&mut dir, 0, 1, 2, 3), None); // Eintrag 1 braucht 64 Byte
        assert_eq!(set_dir_entry(&mut dir, 4, 0, 2, 3), None); // vor dem Stueckanfang
    }

    #[test]
    fn name83_baut_und_weist_ab() {
        assert_eq!(&name83(b"HELLO.TXT").unwrap(), b"HELLO   TXT");
        assert_eq!(&name83(b"a.b").unwrap(), b"A       B  ");
        assert_eq!(&name83(b"README").unwrap(), b"README     ");
        assert_eq!(name83(b"ZUVIELDRIN.TXT"), None); // Stamm > 8
        assert_eq!(name83(b"A.LANG"), None); // Endung > 3
        assert_eq!(name83(b".TXT"), None); // leerer Stamm
    }

    #[test]
    fn geloeschte_und_vfat_eintraege_sind_keine_dateien() {
        let e = |n0: u8, attr: u8| DirEntry {
            name: [n0, b' ', b' ', b' ', b' ', b' ', b' ', b' ', b' ', b' ', b' '],
            attr,
            first_cluster: 2,
            size: 1,
        };
        assert!(!e(0xE5, 0x20).is_file()); // geloescht
        assert!(!e(0x00, 0x20).is_file()); // frei
        assert!(!e(b'A', 0x0F).is_file()); // VFAT-Fragment
        assert!(!e(b'A', 0x08).is_file()); // Datentraegerbezeichnung
        assert!(e(b'A', 0x20).is_file());
    }
}
