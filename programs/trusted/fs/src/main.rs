//! `fs` — **ein lesendes Dateisystem als eigene PD** (A-6.3).
//!
//! Diese PD fährt **kein Gerät**. Sie ruft den Blockdienst über dessen Kanal, liest die Bytes aus
//! der geteilten Übertragungsfläche und wertet sie mit zwei kernfreien Crates aus:
//! `sel4lake-part` (GPT) und `sel4lake-fat` (FAT16). Beide sind `forbid(unsafe_code)` und
//! host-getestet.
//!
//! ## Warum das eine eigene PD ist
//!
//! Ein Dateisystem hat mit Registern und DMA nichts zu tun, ein Treiber nichts mit
//! Verzeichniseinträgen. Den GPT-Scan hat der Blockdienst noch selbst gemacht (A-6.2) — das ist
//! bei einer Blockschicht üblich und war die kleinere Änderung. Ein **Dateisystem** dort
//! unterzubringen wäre etwas anderes: es interpretiert beliebig viel fremde Struktur, und je mehr
//! davon in der PD liegt, die das Gerät steuert, desto weniger bedeutet „Fehlereindämmung".
//!
//! ## Die Schritte
//!
//! 1. `INFO` — wie groß ist die Platte;
//! 2. `SCAN` — wo beginnt die erste Partition (GPT, geprüfte Prüfsummen);
//! 3. Bootsektor der Partition lesen → `sel4lake_fat::parse_boot`;
//! 4. Wurzelverzeichnis lesen → den gesuchten Namen finden;
//! 5. der Clusterkette folgen und die Datei lesen.
//!
//! Alle LBAs des Dateisystems sind **relativ zur Partition**; addiert wird genau einmal, an der
//! Stelle, an der die Anfrage entsteht. Zwei Schichten, die beide meinen, den Plattenanfang zu
//! kennen, sind eine Schicht zu viel.
//!
//! ## Was diese PD NICHT prüft
//!
//! Ob der Inhalt der Datei „richtig" ist. Sie legt Größe und die ersten acht Bytes in die
//! Übertragungsfläche; wer die erwartet, prüft der Kernel. Prüfte sie selbst, wäre die einzige
//! Quelle für „es hat geklappt" derselbe Code, der es behauptet.

#![no_std]
#![no_main]
// TrustedSAS: das Zertifikats-Gate (ADR 0014) verlangt Auditierbarkeit. Der rohe Zugriff auf das
// gemappte Fenster liegt deshalb im auditierten SDK (`libsel4lake::Window`), nicht hier.
#![forbid(unsafe_code)]

use libsel4lake::{call, exit, map_window, result, signal, Window};
use sel4lake_fat::{dir_entry_at, name83, parse_boot, DIR_ENTRY};

/// Slot der eigenen Notification (Manifest `ntfn`) — der Meldekanal.
const NTFN: u64 = 1;
/// Slot des Kanals zum Blockdienst (Manifest `ep`; die Instanz teilt der Kernel zu).
const BLK: u64 = 2;
/// Slot der geteilten Übertragungsfläche.
const SHARED: u64 = 6;

// Das Protokoll des Blockdienstes (muss zu `programs/hardware/virtio-blk` passen).
const OP_INFO: u64 = 0;
const OP_READ: u64 = 1;
const OP_WRITE: u64 = 3;
const OP_FLUSH: u64 = 4;
const OP_SCAN: u64 = 5;
const ST_OK: u64 = 0;

/// Die gesuchte Datei. Der Name steht hier und nicht im Kernel: ein Dateisystem sucht Namen, das
/// ist seine Aufgabe. Was in der Datei steht, prüft der Kernel.
const DATEI: &[u8] = b"HELLO.TXT";

/// Offset des Ergebnisses in der Übertragungsfläche.
///
/// Hinter dem Datenbereich (4 KiB), den der Blockdienst beschreibt — sonst überschriebe der
/// nächste Sektor das Ergebnis, und der Kernel läse den Rest eines Verzeichnisses statt eines
/// Befunds.
const OFF_ERGEBNIS: u64 = 4096;

/// Wie viele Sektoren eine Anfrage höchstens holt (muss zum Blockdienst passen).
const MAX_SECTORS: u64 = 8;

libsel4lake::entry!(run);

/// Ein Ergebniswort in die Übertragungsfläche legen. Der Bereichsschutz liegt im `Window`-Typ.
fn schreibe(shared: &Window, index: u64, wert: u64) {
    let _ = shared.write_u64(OFF_ERGEBNIS + index * 8, wert);
}

/// Sektoren über den Blockdienst holen. `true`, wenn sie in der Fläche stehen.
fn lies(sektor: u64, anzahl: u64) -> bool {
    let r = call(BLK, [OP_READ, sektor, anzahl, 0]);
    r.result == result::OK && r.msg[0] == ST_OK
}

/// Sektoren aus der Fläche zurückschreiben.
fn schreib(sektor: u64, anzahl: u64) -> bool {
    let r = call(BLK, [OP_WRITE, sektor, anzahl, 0]);
    r.result == result::OK && r.msg[0] == ST_OK
}

/// Der Inhalt, den die Probe schreibt — deterministisch, damit ihn ein **unabhängiger** Leser
/// nachrechnen kann. Genau das tut die Suite nach dem Lauf am Abbild selbst.
const NEU_LEN: u32 = 700;
fn neu_byte(i: u32) -> u8 {
    ((i * 7 + 13) % 251) as u8
}

fn run(_arg: usize) -> ! {
    let Some(shared) = map_window(SHARED) else { exit() };
    if shared.len() < OFF_ERGEBNIS + 64 {
        exit();
    }
    // Ergebnisfelder vorbelegen — **nicht** mit Null. Eine 0 in „Dateigröße" wäre von „noch nichts
    // geschrieben" nicht zu unterscheiden, und der Kernel läse Erfolg, wo nichts geschah.
    for i in 0..4 {
        schreibe(&shared, i, u64::MAX);
    }

    // 1. Auskunft. Sie ist nicht bloß Höflichkeit: ohne Kapazität ist jede spätere LBA ungeprüft.
    let info = call(BLK, [OP_INFO, 0, 0, 0]);
    if info.result != result::OK || info.msg[0] != ST_OK {
        exit();
    }

    // 2. Wo beginnt die erste Partition?
    let sc = call(BLK, [OP_SCAN, 0, 0, 0]);
    if sc.result != result::OK || sc.msg[0] != ST_OK || sc.msg[1] == 0 {
        exit();
    }
    let part_lba = sc.msg[2];

    // 3. Bootsektor der Partition.
    if !lies(part_lba, 1) {
        exit();
    }
    let Some(boot) = shared.bytes(0, 512) else { exit() };
    let Ok(fs) = parse_boot(boot) else { exit() };

    // 4. Den Namen im Wurzelverzeichnis suchen. Es kann größer sein als eine Anfrage, also
    //    stückweise — und der Endmarker beendet die Suche, statt bis zum letzten Eintrag zu laufen.
    let Some(gesucht) = name83(DATEI) else { exit() };
    let mut gefunden = None;
    // Wo der Eintrag steht, wird **mitgeführt**: zum Schreiben muss genau dieser Sektor wieder
    // gelesen, geändert und zurückgeschrieben werden. Ihn später neu zu suchen wäre eine zweite
    // Suche, die ein anderes Ergebnis liefern könnte.
    let mut dir_lba = 0u64;
    let mut dir_index = 0u32;
    let mut gelesen = 0u32;
    'suche: while gelesen < fs.root_sectors {
        let n = MAX_SECTORS.min((fs.root_sectors - gelesen) as u64);
        if !lies(part_lba + fs.root_lba as u64 + gelesen as u64, n) {
            exit();
        }
        let Some(chunk) = shared.bytes(0, n * 512) else { exit() };
        let first = gelesen * (512 / DIR_ENTRY as u32);
        for k in 0..(n as u32 * (512 / DIR_ENTRY as u32)) {
            let Some(e) = dir_entry_at(chunk, first, first + k) else { break };
            if e.is_end() {
                break 'suche;
            }
            if e.is_file() && e.name == gesucht {
                gefunden = Some(e);
                dir_index = first + k;
                dir_lba = part_lba
                    + fs.root_lba as u64
                    + (dir_index / (512 / DIR_ENTRY as u32)) as u64;
                break 'suche;
            }
        }
        gelesen += n as u32;
    }
    let Some(datei) = gefunden else {
        schreibe(&shared, 0, 2); // gefunden: nein
        signal(NTFN, 0);
        exit();
    };

    // 5. Der Clusterkette folgen. Gelesen werden die ersten acht Bytes **und** die volle Länge
    //    wird durchlaufen — eine Kette, die nur bis zum ersten Cluster geprüft wird, ist keine
    //    geprüfte Kette.
    let mut cluster = datei.first_cluster;
    let mut rest = datei.size;
    let mut erste_acht = 0u64;
    let mut cluster_gezaehlt = 0u64;
    while rest > 0 {
        let Some(lba) = fs.cluster_lba(cluster) else {
            schreibe(&shared, 0, 3); // Kette zeigt aus dem Datenbereich
            signal(NTFN, 0);
            exit();
        };
        let n = MAX_SECTORS.min(fs.sectors_per_cluster as u64);
        if !lies(part_lba + lba as u64, n) {
            exit();
        }
        if cluster_gezaehlt == 0 {
            let Some(v) = shared.read_u64(0) else { exit() };
            erste_acht = v;
        }
        cluster_gezaehlt += 1;
        rest = rest.saturating_sub(fs.cluster_bytes());
        if rest == 0 {
            break;
        }
        // Den FAT-Sektor holen, der den Eintrag dieses Clusters traegt.
        let (fat_lba, _) = fs.fat_entry_pos(cluster);
        if !lies(part_lba + fat_lba as u64, 1) {
            exit();
        }
        let Some(fatsek) = shared.bytes(0, 512) else { exit() };
        let Some(next) = fs.next_cluster(fatsek, cluster) else { exit() };
        if fs.is_end_of_chain(next) {
            // Die Kette endet, obwohl die Groesse mehr verspricht -> das ist ein Befund, keine
            // kuerzere Datei. Still weniger zu liefern waere Datenverlust, der wie Erfolg aussieht.
            schreibe(&shared, 0, 4);
            signal(NTFN, 0);
            exit();
        }
        if fs.is_bad(next) || !fs.is_next(next) {
            schreibe(&shared, 0, 3);
            signal(NTFN, 0);
            exit();
        }
        cluster = next;
    }

    schreibe(&shared, 0, 0); // gefunden und vollstaendig gelesen
    schreibe(&shared, 1, datei.size as u64);
    schreibe(&shared, 2, erste_acht);
    schreibe(&shared, 3, cluster_gezaehlt);

    // 6. **Schreiben** (A-6.4): die Datei auf 700 Byte verlaengern — also ueber einen zweiten
    //    Cluster hinaus. Eine Schreibprobe, die in den vorhandenen Cluster passt, prueft die
    //    Kettenverlaengerung nicht; sie ist der Teil, an dem ein Schreiber schiefgeht.
    let status = schreibe_datei(&shared, &fs, part_lba, &datei, dir_lba, dir_index);
    schreibe(&shared, 4, status);
    if status == 0 {
        // 7. **Zurueck lesen** — mit einem FRISCHEN Bootsektor-Parse? Nein: die Geometrie hat sich
        //    nicht geaendert. Was sich geaendert hat, ist der Verzeichniseintrag und die Kette,
        //    und genau die werden hier neu gelesen. Ein Schreiber, der sein eigenes Ergebnis aus
        //    dem Gedaechtnis bestaetigt, bestaetigt nichts.
        let (rs, groesse, gelesen) = lies_zurueck(&shared, &fs, part_lba, dir_lba, dir_index);
        schreibe(&shared, 5, rs);
        schreibe(&shared, 6, groesse);
        schreibe(&shared, 7, gelesen);
    }
    signal(NTFN, 0);
    exit();
}

/// Die Datei neu schreiben: Daten in zwei Cluster, Kette in **alle** FAT-Kopien, Eintrag
/// aktualisieren, flushen. `0` = gut, sonst ein Grund.
fn schreibe_datei(
    shared: &Window,
    fs: &sel4lake_fat::Fat16,
    part_lba: u64,
    datei: &sel4lake_fat::DirEntry,
    dir_lba: u64,
    dir_index: u32,
) -> u64 {
    let c1 = datei.first_cluster;
    // Einen freien Cluster suchen — im ersten FAT-Sektor. Reicht fuer die Probe; ein
    // Produktivtreiber muesste weitersuchen, und dass er das NICHT tut, steht hier statt in
    // einer Fussnote.
    if !lies(part_lba + fs.fat_lba as u64, 1) {
        return 10;
    }
    let Some(fatsek) = shared.bytes(0, 512) else { return 11 };
    let Some(c2) = sel4lake_fat::find_free(fatsek, 0, fs.clusters) else { return 12 };
    if c2 == c1 {
        return 13;
    }

    // Daten. Cluster 1 traegt die ersten 512 Byte, Cluster 2 den Rest.
    for (k, c) in [c1, c2].into_iter().enumerate() {
        let von = k as u32 * 512;
        let n = (NEU_LEN - von).min(512);
        for i in 0..512u32 {
            let b = if i < n { neu_byte(von + i) } else { 0 };
            if shared.write_u8(i as u64, b).is_none() {
                return 14;
            }
        }
        let Some(lba) = fs.cluster_lba(c) else { return 15 };
        if !schreib(part_lba + lba as u64, 1) {
            return 16;
        }
    }

    // Kette — in **jede** FAT-Kopie. Nur die erste fortzuschreiben hinterliesse ein Dateisystem,
    // das jedes Pruefwerkzeug als beschaedigt meldet.
    for kopie in 0..fs.num_fats {
        for (c, next) in [(c1, c2), (c2, 0xFFFFu16)] {
            let Some(lba) = fs.fat_copy_lba(kopie, c) else { return 17 };
            if !lies(part_lba + lba as u64, 1) {
                return 18;
            }
            let (_, off) = fs.fat_entry_pos(c);
            if shared.write_u16(off as u64, next).is_none() {
                return 19;
            }
            if !schreib(part_lba + lba as u64, 1) {
                return 20;
            }
        }
    }

    // Verzeichniseintrag: neue Groesse, gleicher Startcluster.
    if !lies(dir_lba, 1) {
        return 21;
    }
    let off = (dir_index % (512 / sel4lake_fat::DIR_ENTRY as u32)) as u64
        * sel4lake_fat::DIR_ENTRY as u64;
    if shared.write_u16(off + 26, c1).is_none() || shared.write_u32(off + 28, NEU_LEN).is_none() {
        return 22;
    }
    if !schreib(dir_lba, 1) {
        return 23;
    }

    // **Flush.** Ohne ihn ist „geschrieben" eine Aussage ueber einen Puffer, nicht ueber die
    // Platte — und genau diese Verwechslung ist der Unterschied zwischen einem Dateisystem, das
    // einen Stromausfall ueberlebt, und einem, das es meistens tut.
    let f = call(BLK, [OP_FLUSH, 0, 0, 0]);
    if f.result != result::OK || f.msg[0] != ST_OK {
        return 24;
    }
    0
}

/// Die Datei neu von der Platte lesen und pruefen, dass jedes Byte stimmt.
/// Gibt `(Status, gemeldete Groesse, geprueft gelesene Bytes)`.
fn lies_zurueck(
    shared: &Window,
    fs: &sel4lake_fat::Fat16,
    part_lba: u64,
    dir_lba: u64,
    dir_index: u32,
) -> (u64, u64, u64) {
    if !lies(dir_lba, 1) {
        return (30, 0, 0);
    }
    let Some(chunk) = shared.bytes(0, 512) else { return (31, 0, 0) };
    let first = dir_index - (dir_index % (512 / sel4lake_fat::DIR_ENTRY as u32));
    let Some(e) = dir_entry_at(chunk, first, dir_index) else { return (32, 0, 0) };
    let mut cluster = e.first_cluster;
    let mut gelesen = 0u32;
    while gelesen < e.size {
        let Some(lba) = fs.cluster_lba(cluster) else { return (33, e.size as u64, gelesen as u64) };
        if !lies(part_lba + lba as u64, 1) {
            return (34, e.size as u64, gelesen as u64);
        }
        let n = (e.size - gelesen).min(512);
        let Some(b) = shared.bytes(0, n as u64) else { return (35, e.size as u64, gelesen as u64) };
        for (i, &c) in b.iter().enumerate() {
            if c != neu_byte(gelesen + i as u32) {
                return (36, e.size as u64, (gelesen + i as u32) as u64);
            }
        }
        gelesen += n;
        if gelesen >= e.size {
            break;
        }
        let (fat_lba, _) = fs.fat_entry_pos(cluster);
        if !lies(part_lba + fat_lba as u64, 1) {
            return (37, e.size as u64, gelesen as u64);
        }
        let Some(fatsek) = shared.bytes(0, 512) else { return (38, e.size as u64, gelesen as u64) };
        let Some(next) = fs.next_cluster(fatsek, cluster) else { return (39, e.size as u64, gelesen as u64) };
        if !fs.is_next(next) {
            return (40, e.size as u64, gelesen as u64);
        }
        cluster = next;
    }
    (0, e.size as u64, gelesen as u64)
}
