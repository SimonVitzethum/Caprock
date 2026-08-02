//! `virtio-blk` — **ein Treiber als Dienst, außerhalb des Kerns** (A-5.1).
//!
//! Ein extern gebautes HardwareLand-Programm. Es bekommt vom System-Manifest seine Autorität,
//! löst sein Gerät **selbst** auf und **bedient dann Anfragen über seinen Kanal**. Der Kernel
//! führt dabei keinen einzigen virtio-Schritt aus; er hat enumeriert, zugeteilt und den Kanal
//! aufgesetzt, mehr nicht.
//!
//! ## Die Richtungsumkehr
//!
//! Bis hierher rief der Kernel den Treiber: er selbst fuhr den virtio-Handshake und las den
//! Sektor. Jetzt ist es umgekehrt — der Treiber **wartet** (`recv`), ein Client **fragt**
//! (`call`), der Treiber **antwortet** (`reply`). Das ist die Voraussetzung für Hot-Reload und
//! Fehlereindämmung: was der Kernel nicht selbst tut, kann er austauschen, ohne es zu verstehen.
//! Er ersetzt einen Empfänger an einem Endpoint — dass dahinter virtio steckt, weiß er nicht.
//!
//! ## Was dieses Programm hält
//!
//! | Slot | Cap | wofür |
//! |---|---|---|
//! | 0 | Notification (vom Lader delegiert) | vom Root-Task, ungenutzt |
//! | 1 | Kanal-Notification (Manifest `ntfn`) | „ich bin bereit" |
//! | 2 | Kanal-Endpoint (Manifest `ep`) | die Dienstschnittstelle |
//! | 3 | MMIO: die eigene Konfigurationsraum-**Seite** | das eigene Gerät auflösen |
//! | 4 | MMIO: das Registerfenster (BAR) | das Gerät bedienen |
//! | 5 | DMA-Region, an die eigene RID angehängt | Virtqueue + Sektorpuffer |
//! | 6 | Kopie von Slot 1 mit eigenem Badge | Stufenmeldung |
//!
//! Slot 3 ist der begründungsbedürftige. Der Einwand gegen einen Capability-Lauf im Treiber
//! lautete: der Konfigurationsraum ist **geräteweit**, wer ihn liest, sieht jedes Gerät der
//! Maschine. Das stimmt für ein globales Adressregister und für das ECAM-Fenster als Ganzes —
//! nicht für **eine Funktion**: ECAM bildet `(bus, dev, func)` auf je 4 KiB ab, also auf genau
//! eine Seite. Diese eine Seite ist mappbar, ohne die Nachbarn mitzugeben.
//!
//! ## Warum es zwei Adressen für dieselbe Region gibt
//!
//! `map_window` auf Slot 5 gibt **beides** zurück: die Adresse, unter der dieser Treiber die
//! Virtqueue beschreibt, und die Adresse, unter der **das Gerät** dieselbe Region sieht. Sie sind
//! verschieden — die zweite kommt aus dem IOVA-Fenster, das der Kernel beim Zuteilen gewählt hat,
//! und liegt oberhalb des RAM. Wer sie vermischt, programmiert dem Gerät eine Adresse, die es
//! nicht auflösen kann, oder beschreibt eine, unter der nichts liegt.
//!
//! ## Was dieses Programm NICHT prüft
//!
//! Ob im Sektor das Richtige steht. Es liefert die Bytes und den Status; ob sie stimmen, prüft
//! der Aufrufer. Prüfte der Treiber den Inhalt selbst, wäre die einzige Quelle für „es hat
//! geklappt" derselbe Code, der es behauptet.

#![no_std]
#![no_main]

use libsel4lake::{exit, map_window, recv, reply, result, signal};
use sel4lake_virtio::blk::{Op, VirtioBlk, SECTOR};

/// Slot der Kanal-Notification. Ihr Badge steckt in der **Cap** (der Kernel hat es beim Endowment
/// gesetzt) — dieses Programm kann es nicht wählen und soll es auch nicht.
const NTFN: u64 = 1;
/// Slot des Kanal-Endpoints — die Dienstschnittstelle.
const EP: u64 = 2;
/// Slot der eigenen Konfigurationsraum-Seite.
const CFG: u64 = 3;
/// Slot des Registerfensters.
const BAR: u64 = 4;
/// Slot der DMA-Region.
const DMA: u64 = 5;
/// Slot der **geteilten Uebertragungsflaeche** (A-6.3).
///
/// Nicht dieselbe Region wie [`DMA`], und das ist der Punkt: die DMA-Region ist non-coherent
/// gemappt, damit das Geraet hineinschreiben kann. Eine zweite, gecachte Abbildung derselben
/// Seiten in einer Client-PD waere auf x86 ein Attribut-Alias. Also normales RAM, in beiden PDs
/// mit denselben Attributen — und dieser Treiber **kopiert**. Genau das tut ein echter Treiber
/// ohnehin, sobald der Puffer des Clients nicht DMA-faehig ist.
const SHARED: u64 = 6;
// --- Das Dienstprotokoll (A-6.1) --------------------------------------------------------------
//
// `msg[0]` = Operation, `msg[1]` = Sektor, `msg[2]` = Sektorzahl. Die Antwort traegt in `msg[0]`
// immer den Status.
//
// **Der Puffer des Treibers ist die Ablage.** `READ` fuellt ihn, `WRITE` schreibt ihn zurueck --
// der Client nennt also nur Sektoren, keine Adressen. Das ist kein Notbehelf, sondern das
// uebliche Staging-Modell eines Blockgeraets: der Puffer muss DMA-faehig und geraetesichtbar
// sein, und beides kann ein beliebiger Client-Puffer nicht zusagen. Erst wenn ein Client die
// Bytes SELBST sehen soll, kommt eine geteilte Region dazu -- die baut A-6.2, wo es einen
// Abnehmer dafuer gibt. Eine Uebertragungsflaeche vor ihrem ersten Benutzer waere eine
// Schnittstelle, die nur eine Vermutung belegt.

/// Auskunft: Kapazitaet, Hoechstzahl Sektoren je Anfrage, Sektorgroesse.
const OP_INFO: u64 = 0;
/// Sektoren lesen (`msg[1]` = erster Sektor, `msg[2]` = Anzahl) -> in den Puffer.
const OP_READ: u64 = 1;
/// Dienst beenden. Der Treiber antwortet **noch** und endet danach — ein Server, der ohne
/// Antwort verschwindet, lässt seinen Aufrufer in `CALL` stehen.
const OP_STOP: u64 = 2;
/// Sektoren schreiben (`msg[1]` = erster Sektor, `msg[2]` = Anzahl) <- aus dem Puffer.
const OP_WRITE: u64 = 3;
/// Alles Geschriebene dauerhaft machen.
const OP_FLUSH: u64 = 4;
/// **Partitionstabelle lesen** (A-6.2). Antwort: `[Status, Zahl belegter Eintraege, erste LBA der
/// ersten Partition, deren Sektorzahl]`.
///
/// Der Scan laeuft **hier**, im Blockdienst, und nicht im Kern: eine Partitionstabelle sind fremde
/// Bytes, die ein beliebiger Mandant geschrieben haben kann. Ein Blockdienst, der Partitionen
/// meldet, ist dabei nichts Ungewoehnliches -- das ist genau die Aufgabe einer Blockschicht.
const OP_SCAN: u64 = 5;

/// Antwortstatus: es gibt keine (lesbare) Partitionstabelle. Der genaue Grund steht in `msg[1]`
/// als [`sel4lake_part::PartError`]-Ordnungszahl -- „keine GPT" und „eine kaputte GPT" sind
/// verschiedene Lagen, und nur die zweite ist ein Datenverlust.
const ST_NOTABLE: u64 = 4;

/// Antwortstatus: alles gut.
const ST_OK: u64 = 0;
/// Antwortstatus: das Gerät antwortete nicht oder meldete einen Fehler.
const ST_DEVICE: u64 = 1;
/// Antwortstatus: unbekannte Operation.
const ST_BADOP: u64 = 2;
/// Antwortstatus: **Bereich**. Der verlangte Sektorbereich liegt nicht vollstaendig auf der
/// Platte, oder die Anzahl uebersteigt, was eine Anfrage fassen kann.
///
/// Ein eigener Status und nicht `ST_DEVICE`: „ich habe nicht gefragt" ist fuer den Client eine
/// andere Lage als „das Geraet hat nein gesagt". Die erste ist sein Fehler, die zweite nicht.
const ST_RANGE: u64 = 3;

/// Offset des **Bedienungszählers** in der DMA-Region.
///
/// Er liegt bewusst in der Region und nicht in einer Programmvariable: die Region überlebt den
/// Austausch des Treibers, eine Variable nicht. Nach einem Hot-Reload zählt die neue Fassung
/// damit **weiter** statt bei null anzufangen — und genau daran ist von außen zu erkennen, dass
/// sie dieselbe Region geerbt hat und nicht eine frische bekam. (Dasselbe Prinzip wie die
/// versionierte Zustandsregion aus A-4.3, nur ohne eigenen Kopf: hier ist ein Zähler alles, was
/// es zu übergeben gibt.)
const OFF_SERVED: u64 = 0x600;

/// Poll-Obergrenze für den used-Ring. Großzügig: hier wird eine Antwort **erwartet**, und ein zu
/// knappes Limit machte aus einem langsamen emulierten Gerät einen Schein-Fehlschlag.
const MAX_POLL: u64 = 50_000_000;

/// Speicherbarriere gegen **Gerätezugriffe**.
///
/// Ausdrücklich nicht `core::sync::atomic::fence(SeqCst)`. Der wäre hier zufällig richtig (auf
/// x86 ein `mfence`) und auf aarch64 falsch: dort übersetzt er zu `dmb ish`, und Device-Memory
/// liegt nicht in der inner-shareable Domäne. Eine Barriere, die auf einer Architektur trägt und
/// auf der anderen still schwächer ist, ist eine Falle mit Verfallsdatum — genau deshalb nimmt
/// `sel4lake-virtio` sie als Parameter entgegen, statt sie selbst zu wählen.
#[cfg(target_arch = "x86_64")]
fn device_fence() {
    // SAFETY: reine Barriere, kein Speicher-/Registereffekt.
    unsafe { core::arch::asm!("mfence", options(nostack, preserves_flags)) }
}
#[cfg(target_arch = "aarch64")]
fn device_fence() {
    // SAFETY: wie oben.
    unsafe { core::arch::asm!("dsb sy", options(nostack, preserves_flags)) }
}

/// Den Bedienungszaehler in der Region hochzaehlen und zurueckgeben.
///
/// # Safety-Hinweis
/// `dma_cpu` muss die Basis der eigenen DMA-Region sein; [`OFF_SERVED`] liegt darin und ausserhalb
/// von Virtqueue, Anfragekopf und Sektorpuffer.
fn bump_served(dma_cpu: u64) -> u64 {
    // SAFETY: s. o. -- eigene Region, fester Offset, ausschliesslich dieses Wort.
    unsafe {
        let p = (dma_cpu + OFF_SERVED) as *mut u64;
        let v = core::ptr::read_volatile(p).wrapping_add(1);
        core::ptr::write_volatile(p, v);
        v
    }
}

/// Fehlergruende als Zahl -- damit der Aufrufer sie unterscheiden kann, ohne den Typ zu kennen.
fn fehlercode(e: sel4lake_part::PartError) -> u64 {
    use sel4lake_part::PartError as E;
    match e {
        E::TooShort => 1,
        E::BadSignature => 2,
        E::BadRevision => 3,
        E::BadHeaderSize => 4,
        E::HeaderCrc => 5,
        E::BadEntrySize => 6,
        E::BadEntryCount => 7,
        E::EntriesCrc => 8,
    }
}

/// Die Partitionstabelle lesen: Kopf von LBA 1, dann die Eintragsliste **in Stuecken**.
///
/// Die Liste ist typisch 16 KiB gross und passt nicht in eine Anfrage. Die Pruefsumme muss
/// trotzdem ueber das **Ganze** gehen — deshalb wird `Crc32` fortgeschrieben und erst am Ende
/// verglichen. Wer je Stueck prueft, prueft ein Stueck und glaubt an die Tabelle.
///
/// Gibt `(Zahl belegter Eintraege, erste LBA der ersten Partition, deren Sektorzahl)`.
fn scan_partitions(blk: &VirtioBlk, dma_cpu: u64, dma_dev: u64) -> Result<(u64, u64, u64), u64> {
    use sel4lake_part::{entry_at, parse_header, verify_entries, Crc32, PartError};
    let buf = dma_cpu + sel4lake_virtio::blk::OFF_DATA;
    // 1. Kopf von LBA 1.
    // SAFETY: MMIO im gemappten BAR; der Puffer gehoert dieser PD allein.
    let r = unsafe { blk.request(dma_cpu, dma_dev, Op::Read, 1, 1, MAX_POLL) };
    if !r.completed() {
        return Err(fehlercode(PartError::TooShort));
    }
    let hdr = parse_header(sektor(buf, SECTOR as usize)).map_err(fehlercode)?;

    // 2. Eintragsliste, stueckweise. Der letzte Happen darf **nicht** auf die Sektorgrenze
    //    aufgerundet in die Pruefsumme gehen: sie gilt fuer `entries_bytes`, nicht fuer die
    //    gelesenen Sektoren. Genau das ist der haeufigste Grund fuer eine "kaputte" Tabelle,
    //    die in Ordnung ist.
    let gesamt = hdr.entries_bytes();
    let mut crc = Crc32::new();
    let mut belegt = 0u64;
    let mut erste = (0u64, 0u64);
    let mut gelesen = 0u64;
    let stueck = sel4lake_virtio::blk::MAX_SECTORS as u64 * SECTOR as u64;
    while gelesen < gesamt {
        let lba = hdr.entry_lba + gelesen / SECTOR as u64;
        let rest = gesamt - gelesen;
        let jetzt = if rest > stueck { stueck } else { rest };
        let sektoren = jetzt.div_ceil(SECTOR as u64) as u32;
        // SAFETY: wie oben; `sektoren` ist durch `MAX_SECTORS` begrenzt.
        let rr = unsafe { blk.request(dma_cpu, dma_dev, Op::Read, lba, sektoren, MAX_POLL) };
        if !rr.completed() {
            return Err(fehlercode(PartError::TooShort));
        }
        let chunk = sektor(buf, jetzt as usize);
        crc.update(chunk);
        let first = (gelesen / hdr.entry_size as u64) as u32;
        let n = jetzt / hdr.entry_size as u64;
        for k in 0..n {
            let idx = first + k as u32;
            if let Some(p) = entry_at(&hdr, chunk, first, idx) {
                if p.is_used() && p.within(&hdr) {
                    if belegt == 0 {
                        erste = (p.first_lba, p.sectors());
                    }
                    belegt += 1;
                }
            }
        }
        gelesen += jetzt;
    }
    verify_entries(&hdr, &crc).map_err(fehlercode)?;
    Ok((belegt, erste.0, erste.1))
}

/// Den Datenpuffer als Slice ansehen.
///
/// # Safety-Hinweis
/// `buf` muss auf den Datenbereich der eigenen DMA-Region zeigen und `len` innerhalb davon liegen.
fn sektor<'a>(buf: u64, len: usize) -> &'a [u8] {
    // SAFETY: s. o. -- eigene Region, Laenge durch `MAX_SECTORS * SECTOR` begrenzt, nur lesend.
    unsafe { core::slice::from_raw_parts(buf as *const u8, len) }
}

libsel4lake::entry!(run);

fn run(_arg: usize) -> ! {
    // 1. Die drei Fenster mappen. Jeder Fehlschlag beendet den Treiber **ohne** Bereit-Meldung:
    //    ein Treiber, der ohne sein Gerät in `recv` ginge, nähme Anfragen an, die er nicht
    //    beantworten kann — und der Fehler fiele beim Client an, nicht bei der Zuteilung.
    let Some(cfg_win) = map_window(CFG) else { exit() };
    let cfg = cfg_win.base();
    if map_window(BAR).is_none() {
        exit(); // das Registerfenster wird nicht direkt adressiert, muss aber gemappt sein
    }
    let Some(dma) = map_window(DMA) else { exit() };
    let (dma_cpu, dma_len, dma_dev) = (dma.base(), dma.len(), dma.iova());
    let Some(shared_win) = map_window(SHARED) else { exit() };
    let (shared, shared_len) = (shared_win.base(), shared_win.len());
    // Die Gerätesicht MUSS eine eigene Achse sein. Wäre sie gleich der CPU-Sicht, liefe dieser
    // Treiber auf einer identity-Abbildung -- und genau die soll es nicht mehr geben.
    if dma_dev == 0 || dma_len < sel4lake_virtio::blk::REGION_BYTES {
        exit();
    }

    // 2. Das eigene Gerät auflösen — auf der eigenen Seite, ohne den Kernel.
    // SAFETY: `cfg` ist die gemappte Konfigurationsraum-Seite genau dieser Funktion, und das darin
    // genannte BAR ist über Slot 4 in dieser VSpace erreichbar.
    let Some(transport) = (unsafe { sel4lake_virtio::probe_ecam(cfg, device_fence) }) else {
        exit();
    };
    let blk = VirtioBlk::from_transport(transport);

    // 3. „Ich bin bereit." Erst **nach** dem Auflösen: ein Bereit-Signal vor dem Gerät wäre eine
    //    Zusage, die der Treiber nicht einhalten kann.
    //    Gemeldet wird ueber die **endowte** Cap: ihr Badge hat der Kernel gesetzt und **je
    //    Fassung verschieden** gewaehlt. Daran ist ablesbar, WELCHE Fassung bereit ist -- und das
    //    ist die Frage, auf die es beim Austausch ankommt.
    signal(NTFN, 0);

    // 4. Dienstschleife. **Hier liegt die Richtungsumkehr**: der Treiber wartet, der Client fragt.
    //
    //    Die Kapazitaet wird EINMAL beim ersten Zugriff festgehalten und danach fuer die
    //    Bereichspruefung benutzt. Sie jedes Mal neu zu lesen waere nicht sicherer, sondern
    //    unklarer: eine Platte, deren Groesse sich unter einem laufenden Dienst aendert, ist ein
    //    eigenes Problem und keins, das eine Wiederholung des Registerlesens loesen wuerde.
    let mut capacity = 0u64;
    loop {
        let m = recv(EP);
        if m.result != result::OK {
            // Kein gültiger Endpoint mehr (stillgelegt/entzogen) -> geordnet enden. Genau das
            // passiert beim Hot-Reload mit der alten Fassung.
            break;
        }
        let (op, sector, count) = (m.msg[0], m.msg[1], m.msg[2].max(1));
        match op {
            OP_INFO => {
                // SAFETY: MMIO im gemappten BAR, Region der eigenen PD.
                let r = unsafe {
                    blk.request(dma_cpu, dma_dev, Op::Flush, 0, 0, MAX_POLL)
                };
                capacity = r.capacity_sectors;
                reply(
                    EP,
                    [ST_OK, capacity, sel4lake_virtio::blk::MAX_SECTORS as u64, SECTOR as u64],
                );
            }
            OP_READ | OP_WRITE => {
                if capacity == 0 {
                    // SAFETY: wie oben. Ein Flush ohne Nutzdaten ist der billigste Weg, die
                    // Kapazitaet zu erfahren, ohne etwas zu veraendern.
                    capacity = unsafe { blk.request(dma_cpu, dma_dev, Op::Flush, 0, 0, MAX_POLL) }
                        .capacity_sectors;
                }
                // **Bereichspruefung vor dem Geraet, nicht danach.** Ein Geraet, das ueber sein
                // Ende hinaus gefragt wird, darf antworten, wie es will; der Dienst hat vorher
                // nein zu sagen. Und `count` wird gegen die Puffergroesse geprueft, sonst
                // schriebe das Geraet hinter das Ende der Region.
                let zu_gross = count > sel4lake_virtio::blk::MAX_SECTORS as u64;
                let ueber_ende = capacity == 0
                    || sector >= capacity
                    || count > capacity - sector;
                if zu_gross || ueber_ende {
                    reply(EP, [ST_RANGE, 0, 0, capacity]);
                    continue;
                }
                let o = if op == OP_READ { Op::Read } else { Op::Write };
                if o == Op::Write {
                    let n = (count * SECTOR as u64).min(shared_len) as usize;
                    // SAFETY: wie oben.
                    unsafe {
                        core::ptr::copy_nonoverlapping(
                            shared as *const u8,
                            (dma_cpu + sel4lake_virtio::blk::OFF_DATA) as *mut u8,
                            n,
                        );
                    }
                }
                // SAFETY: wie oben; `count` ist geprueft und passt in die Region.
                let r = unsafe {
                    blk.request(dma_cpu, dma_dev, o, sector, count as u32, MAX_POLL)
                };
                // Gelesene Sektoren in die geteilte Flaeche kopieren, damit der Client sie
                // SEHEN kann. Beim Schreiben umgekehrt -- dort hat er sie vorher hineingelegt.
                if r.completed() {
                    let n = (r.sectors as u64 * SECTOR as u64).min(shared_len) as usize;
                    // SAFETY: beide Bereiche gehoeren dieser PD; `n` ist durch die kleinere der
                    // beiden Laengen begrenzt.
                    unsafe {
                        let von = (dma_cpu + sel4lake_virtio::blk::OFF_DATA) as *const u8;
                        let nach = shared as *mut u8;
                        if op == OP_READ {
                            core::ptr::copy_nonoverlapping(von, nach, n);
                        }
                    }
                }
                let served = bump_served(dma_cpu);
                reply(
                    EP,
                    [
                        if r.completed() { ST_OK } else { ST_DEVICE },
                        r.first_word,
                        r.sectors as u64,
                        served,
                    ],
                );
            }
            OP_FLUSH => {
                // SAFETY: wie oben.
                let r = unsafe { blk.request(dma_cpu, dma_dev, Op::Flush, 0, 0, MAX_POLL) };
                reply(
                    EP,
                    [if r.completed() { ST_OK } else { ST_DEVICE }, 0, 0, bump_served(dma_cpu)],
                );
            }
            OP_SCAN => {
                match scan_partitions(&blk, dma_cpu, dma_dev) {
                    Ok((n, first_lba, sectors)) => reply(EP, [ST_OK, n, first_lba, sectors]),
                    Err(code) => reply(EP, [ST_NOTABLE, code, 0, 0]),
                }
            }
            OP_STOP => {
                reply(EP, [ST_OK, 0, 0, 0]);
                break;
            }
            _ => reply(EP, [ST_BADOP, 0, 0, 0]),
        }
    }
    exit();
}
