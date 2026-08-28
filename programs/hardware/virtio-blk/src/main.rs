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
// **T5: eine echte thread-lokale Variable.** Nicht als Selbstzweck -- sie ist der Abnahmefall des
// TLS-Strangs: *ein Treiber laeuft mit TLS*. Ohne `has-thread-local` im Ziel und `.tdata`/`.tbss`
// im Linkerskript (T3) schlaegt schon diese Zeile im UEBERSETZER fehl, nicht erst im Lauf.
#![feature(thread_local)]

use libcaprock::{exit, map_window, recv, reply, result, signal};
use caprock_virtio::blk::{Op, VirtioBlk, SECTOR};

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
/// Slot der **`Irq`-Cap** dieses Geraets (Stufe B, B2). Leer, wenn das Geraet keinen Vektor
/// bekommen hat — dann pollt dieser Treiber wie vor Stufe B, und das ist zulaessig.
const IRQ: u64 = 7;
/// Slot der **Notification, auf der der Interrupt ankommt** (B4).
///
/// Ein eigenes Objekt, nicht [`NTFN`]: eine Notification rastet ein, und auf [`NTFN`] liegen die
/// eigenen Bereit-Signale. Ein `wait` darauf kaeme sofort mit einem alten pending-Bit zurueck --
/// von einem zugestellten Interrupt nicht zu unterscheiden.
const IRQ_NTFN: u64 = 8;
/// Das Etikett, unter dem der Kernel den Interrupt zustellt. **Der Treiber waehlt es**, denn er
/// ist der, der es wiedererkennen muss.
const IRQ_BADGE: u64 = 0x1;

/// **Faehrt dieser Treiber den Warteweg?** (B4)
///
/// Steht auf `false`, und das ist ein **benannter offener Punkt**, kein Rueckfall: gebunden wird
/// weiterhin (B3 laeuft ueber die ABI, aus einer echten Treiber-PD), gewartet noch nicht.
///
/// Der Grund ist gemessen und nicht vermutet: IRTE praesent mit `SVT/SID`, MSI-X-Zeile im Geraet
/// zurueckgelesen (`addr=0xfee00008`, unmaskiert, Handle korrekt), `queue_msix_vector` vom Geraet
/// angenommen -- und `IRQ_DELIVERED` bleibt **0**. Der Treiber blockierte damit fuer immer, und
/// weil `WAIT` keine Frist hat (Stufe A fehlt), nahm er die ganze Suite mit.
///
/// **Ein dynamischer Rueckfall waere hier verboten** (Plan §4): ein Treiber, der nach N
/// vergeblichen Weckrufen doch pollte, machte `poll-runden == 0` wertlos und die Zeile gruen fuer
/// etwas anderes. Deshalb ein **Bauschalter**, kein Laufzeitpfad -- und die Berichtszeile fuehrt
/// ihn als `b4-aktiv`, damit die fehlende Haelfte in der Zeile steht statt in einer kuerzeren
/// Liste von Konjunkten.
const B4_WARTEN: bool = false;

/// Offsets der B4-Zahlen in der DMA-Region — der Kernel liest sie dort.
///
/// In der Region und nicht in Programmvariablen, aus demselben Grund wie [`OFF_SERVED`]: sie
/// ueberlebt den Austausch, eine Variable nicht.
const OFF_POLLED: u64 = 0x608;
const OFF_WAKEUPS: u64 = 0x610;
const OFF_BADGE: u64 = 0x618;
const OFF_MSIX_OK: u64 = 0x620;
/// **Die Marke, an der der Kernel erkennt, dass hier ueberhaupt jemand meldet.**
///
/// Ohne sie las der Bericht die vier Offsets aus **jeder** DMA-Region -- auch aus der von
/// `virtio-net`, wo dort Virtqueue-Bytes liegen. Die Zahlen sahen aus wie Messwerte
/// (`weckrufe=9223372037261623427`) und waren Ringdaten, und eine davon machte `wartende=1` wahr.
/// *Eine Zahl, die niemand als solche ausweist, ist ein Byte.*
const OFF_MAGIC: u64 = 0x628;
/// Faehrt dieser Treiber den Warteweg? -- s. [`B4_WARTEN`].
const OFF_B4_AKTIV: u64 = 0x630;
/// „B4MELDER" -- willkuerlich, aber unverwechselbar gegen Ringdaten.
const B4_MAGIC: u64 = 0x4234_4D45_4C44_4552;
/// T5: Marke und Wert der thread-lokalen Variablen, dort wo der Kernel sie liest.
const OFF_TLS_MAGIC: u64 = 0x638;
const OFF_TLS_WERT: u64 = 0x640;
/// T5-Diagnose: der gesetzte Zeiger und das, was ein ROHER `fs:[0]` liefert.
const OFF_TLS_TP: u64 = 0x648;
const OFF_TLS_FS0: u64 = 0x650;
/// „TLSDRV!" — unverwechselbar gegen Ringdaten, s. [`B4_MAGIC`].
const TLS_MAGIC: u64 = 0x544C_5344_5256_2121;
/// Der Wert, den der Treiber in seine thread-lokale Variable schreibt.
const TLS_WERT: u64 = 0xC0FFEE_4711;

/// **Der TLS-Puffer dieses Threads** — `static mut`, nicht Stack: er muss den Thread ueberleben.
///
/// **Ein Puffer, ein Thread.** Dieser Treiber hat heute einen; ein zweiter braeuchte einen
/// zweiten, und wer beiden denselben gaebe, haette kein TLS, sondern zwei Namen fuer eine
/// Variable (Gegenprobe M3 in `docs/plan-tls.md`).
/// **8 KiB, nicht 512** — `libcaprock::TLS_ALIGN` ist eine Seite (s. dort, gemessen), also braucht
/// ein Block Ausrichtung plus aufgerundete Groesse. `tls_bedarf()` sagt die Zahl; hier steht
/// grosszuegig das Doppelte, damit eine zusaetzliche thread-lokale Variable nicht sofort wieder
/// eine stille Verschiebung ist.
static mut TLS_PUFFER: [u8; 8192] = [0; 8192];

/// **Die thread-lokale Variable.** Der ganze Punkt von T5.
#[thread_local]
static mut TLS_MARKE: u64 = 0;

/// **Der Warteweg** — als Funktionszeiger, weil `caprock-virtio` abhaengigkeitsfrei ist.
///
/// `false` heisst „der Warteweg ist unbrauchbar geworden"; die Crate bricht dann ab und pollt
/// **nicht**. Ein `wait`, das ein fremdes Badge liefert, ist kein Fehler -- Notifications
/// akkumulieren, und die Crate prueft danach ohnehin den used-Ring.
fn warte_auf_irq() -> bool {
    let b = libcaprock::wait(IRQ_NTFN);
    WECKRUFE.fetch_add(1, core::sync::atomic::Ordering::AcqRel);
    // Das zuletzt gesehene Badge festhalten: der Kernel prueft, dass **sein** Etikett ankam und
    // nicht irgendein Wert. Ohne das waere „geweckt" auch dann wahr, wenn die Bindung auf ein
    // fremdes Objekt zeigte.
    LETZTES_BADGE.store(b, core::sync::atomic::Ordering::Release);
    true
}

/// Das zuletzt in [`warte_auf_irq`] gesehene Badge. `static`, weil ein Funktionszeiger nichts
/// einfangen kann -- und ein Funktionszeiger ist der Preis dafuer, dass die Crate keine
/// Abhaengigkeit bekommt.
static LETZTES_BADGE: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
/// Wie oft ueberhaupt geweckt wurde, ueber alle Anfragen.
static WECKRUFE: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
/// Hat IRGENDEINE Anfrage den Poll-Weg benutzt? Die Groesse, an der `poll-runden == 0` haengt --
/// als „ueberhaupt" und nicht als Zahl: ein Treiber, der **einmal** pollt, hat die Zusage
/// gebrochen, und eine kleine Zahl lade dazu ein, sie fuer „fast nicht" zu halten.
static GEPOLLT: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
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
/// als [`caprock_part::PartError`]-Ordnungszahl -- „keine GPT" und „eine kaputte GPT" sind
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
/// `caprock-virtio` sie als Parameter entgegen, statt sie selbst zu wählen.
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
/// **Die vier B4-Zahlen in die Region schreiben**, wo der Kernel sie liest.
///
/// Sie stehen als Zahlen und nicht als Urteil da: der Treiber sagt, was er getan hat, und der
/// Kernel urteilt. Ein Treiber, der sein eigenes Ergebnis bestaetigt, bestaetigt nichts.
///
/// # Safety-Hinweis
/// wie [`bump_served`] -- eigene Region, feste Offsets ausserhalb von Queue, Kopf und Puffer.
fn melde(dma_cpu: u64, msix_ok: bool) {
    use core::sync::atomic::Ordering;
    // SAFETY: s. o.
    unsafe {
        core::ptr::write_volatile(
            (dma_cpu + OFF_POLLED) as *mut u64,
            u64::from(GEPOLLT.load(Ordering::Acquire)),
        );
        core::ptr::write_volatile(
            (dma_cpu + OFF_WAKEUPS) as *mut u64,
            WECKRUFE.load(Ordering::Acquire),
        );
        core::ptr::write_volatile(
            (dma_cpu + OFF_BADGE) as *mut u64,
            LETZTES_BADGE.load(Ordering::Acquire),
        );
        core::ptr::write_volatile((dma_cpu + OFF_MSIX_OK) as *mut u64, u64::from(msix_ok));
        core::ptr::write_volatile((dma_cpu + OFF_B4_AKTIV) as *mut u64, u64::from(B4_WARTEN));
        // **Zuletzt**, und das ist Absicht: die Marke sagt „die vier Zahlen darueber sind gueltig".
        // Zuerst geschrieben, koennte sie auf halb gefuellte Felder zeigen.
        core::ptr::write_volatile((dma_cpu + OFF_MAGIC) as *mut u64, B4_MAGIC);
    }
}

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
fn fehlercode(e: caprock_part::PartError) -> u64 {
    use caprock_part::PartError as E;
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
fn scan_partitions(
    blk: &VirtioBlk,
    dma_cpu: u64,
    dma_dev: u64,
    irq: Option<&caprock_virtio::IrqWarten>,
) -> Result<(u64, u64, u64), u64> {
    use caprock_part::{entry_at, parse_header, verify_entries, Crc32, PartError};
    let buf = dma_cpu + caprock_virtio::blk::OFF_DATA;
    // 1. Kopf von LBA 1.
    // SAFETY: MMIO im gemappten BAR; der Puffer gehoert dieser PD allein.
    let r = unsafe { blk.request(dma_cpu, dma_dev, Op::Read, 1, 1, MAX_POLL, irq) };
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
    let stueck = caprock_virtio::blk::MAX_SECTORS as u64 * SECTOR as u64;
    while gelesen < gesamt {
        let lba = hdr.entry_lba + gelesen / SECTOR as u64;
        let rest = gesamt - gelesen;
        let jetzt = if rest > stueck { stueck } else { rest };
        let sektoren = jetzt.div_ceil(SECTOR as u64) as u32;
        // SAFETY: wie oben; `sektoren` ist durch `MAX_SECTORS` begrenzt.
        let rr = unsafe { blk.request(dma_cpu, dma_dev, Op::Read, lba, sektoren, MAX_POLL, irq) };
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

libcaprock::entry!(run);

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
    // **Der Pool ist das Tor** (Z22 P3). `caprock_virtio::Region::from_raw` prueft nichts; hier
    // faellt die Identitaetsabbildung (`dev == cpu`), die fehlende Geraetesicht (`dev == 0`) und
    // der Ueberlauf **strukturell** aus, statt in einem `if` je Treiber wiederholt zu werden.
    let pool = match caprock_dma::DmaPool::new(dma.base(), dma.iova(), dma.len()) {
        Ok(p) => p,
        Err(e) => {
            // DIAGNOSE (Z22 P3): still zu sterben macht die Ursache unauffindbar.
            signal(NTFN, 0xD1A6_0000 | (e as u64));
            exit()
        }
    };
    let ganz = pool.whole();
    let (dma_cpu, dma_len, dma_dev) = (ganz.cpu(), ganz.len(), ganz.dev());
    // Der Datenbereich als **ein Stueck**. Ab hier ist seine Laenge eine Eigenschaft des Wertes
    // und nicht eine Konstante, die an drei Stellen von Hand richtig sein muss.
    let Some(daten) = pool.map(
        dma_cpu + caprock_virtio::blk::OFF_DATA,
        caprock_virtio::blk::REGION_BYTES - caprock_virtio::blk::OFF_DATA,
    ) else {
        signal(NTFN, 0xD1A6_00FF);
        exit()
    };
    let Some(shared_win) = map_window(SHARED) else { exit() };
    let (shared, shared_len) = (shared_win.base(), shared_win.len());
    // Die Gerätesicht MUSS eine eigene Achse sein. Wäre sie gleich der CPU-Sicht, liefe dieser
    // Treiber auf einer identity-Abbildung -- und genau die soll es nicht mehr geben.
    if dma_dev == 0 || dma_len < caprock_virtio::blk::REGION_BYTES {
        exit();
    }

    // 2. Das eigene Gerät auflösen — auf der eigenen Seite, ohne den Kernel.
    // SAFETY: `cfg` ist die gemappte Konfigurationsraum-Seite genau dieser Funktion, und das darin
    // genannte BAR ist über Slot 4 in dieser VSpace erreichbar.
    let Some(transport) = (unsafe { caprock_virtio::probe_ecam(cfg, device_fence) }) else {
        exit();
    };
    let blk = VirtioBlk::from_transport(transport);

    // 3. „Ich bin bereit." Erst **nach** dem Auflösen: ein Bereit-Signal vor dem Gerät wäre eine
    //    Zusage, die der Treiber nicht einhalten kann.
    //    Gemeldet wird ueber die **endowte** Cap: ihr Badge hat der Kernel gesetzt und **je
    //    Fassung verschieden** gewaehlt. Daran ist ablesbar, WELCHE Fassung bereit ist -- und das
    //    ist die Frage, auf die es beim Austausch ankommt.
    signal(NTFN, 0);

    // 3a. **T5: TLS aufsetzen und benutzen.**
    //
    //     Der Kernel haelt nur das Register; der Block wird HIER gebaut, und die Aufteilung ist je
    //     Architektur eine andere (Variante 1 gegen 2) -- `libcaprock` weiss das, der Kernel nicht.
    //
    //     Gemeldet wird in die DMA-Region, mit eigener Marke: ohne sie liest der Kernel fremde
    //     Bytes und haelt sie fuer Messwerte (der Fehler, den B4 einmal gemacht hat).
    // SAFETY: `TLS_PUFFER` gehoert diesem Thread allein und ist `static`, lebt also laenger als er.
    let tp = unsafe {
        libcaprock::tls_einrichten(
            core::ptr::addr_of_mut!(TLS_PUFFER) as *mut u8,
            core::mem::size_of_val(&*core::ptr::addr_of!(TLS_PUFFER)),
        )
    };
    if tp != 0 {
        // SAFETY: nach `tls_einrichten` zeigt das Thread-Pointer-Register auf einen gueltigen
        // Block; der Zugriff geht ueber die vom Uebersetzer erzeugte TLS-Adressierung.
        unsafe {
            // **T5b, OFFEN: der Zugriff auf `TLS_MARKE` ist hier ABGESCHALTET.**
            //
            // Gemessen, nicht vermutet: mit dem Zugriff antwortet dieser Treiber nicht mehr --
            // kein Fault, keine Meldung, die Suite laeuft in den Watchdog. Bisektiert in drei
            // Laeufen: T3+T4 allein sind gruen; `tls_einrichten` **im Treiber** ist gruen (diese
            // Zeile meldet ihre Marke); nur der vom Uebersetzer erzeugte `mov %fs:0x0` faellt aus.
            // Ein Versatzfehler in der Variante-2-Anordnung war es NICHT -- er war da, ist behoben,
            // und das Bild blieb.
            //
            // Bis das geklaert ist, misst diese Zeile, was sie messen kann: dass der Treiber den
            // Block baut und der Kernel den Zeiger annimmt. Ein abgeschalteter Zugriff, der als
            // solcher dasteht, ist ehrlicher als eine gruene Zeile ueber etwas anderes.
            // **Die Diagnose ZUERST, der Zugriff danach.** Stirbt der Zugriff, steht die Marke
            // schon da und der Kernel liest `wert=0` -- „gestorben" ist damit von „nie gelaufen"
            // unterscheidbar. Andersherum waeren beide dasselbe Bild, und genau daran hat die
            // erste Runde dieser Suche eine Bisektion gekostet.
            // **Je Architektur ein anderer Weg an denselben Wert** -- und ohne die Gatterung
            // uebersetzt aarch64 gar nicht (`mov x8, fs:[0]`). Vierte Instanz derselben Klasse in
            // diesem Baum, diesmal im Userland: *arch-neutraler Code greift auf etwas zu, das es
            // nur auf einer Architektur gibt.*
            let fs0: u64;
            #[cfg(target_arch = "x86_64")]
            // Ring 3 kann `FS_BASE` ohne `CR4.FSGSBASE` nicht lesen -- also ueber den
            // Selbstzeiger, den `tls_einrichten` bei `tp` abgelegt hat.
            core::arch::asm!("mov {}, fs:[0]", out(reg) fs0, options(nostack, readonly));
            #[cfg(target_arch = "aarch64")]
            // `TPIDR_EL0` ist aus EL0 direkt lesbar -- kein Umweg noetig.
            core::arch::asm!("mrs {}, tpidr_el0", out(reg) fs0, options(nostack, nomem));
            core::ptr::write_volatile((dma_cpu + OFF_TLS_TP) as *mut u64, tp);
            core::ptr::write_volatile((dma_cpu + OFF_TLS_FS0) as *mut u64, fs0);
            core::ptr::write_volatile((dma_cpu + OFF_TLS_WERT) as *mut u64, 0);
            core::ptr::write_volatile((dma_cpu + OFF_TLS_MAGIC) as *mut u64, TLS_MAGIC);

            core::ptr::write_volatile(core::ptr::addr_of_mut!(TLS_MARKE), TLS_WERT);
            let gelesen = core::ptr::read_volatile(core::ptr::addr_of!(TLS_MARKE));
            core::ptr::write_volatile((dma_cpu + OFF_TLS_WERT) as *mut u64, gelesen);
            core::ptr::write_volatile((dma_cpu + OFF_TLS_MAGIC) as *mut u64, TLS_MAGIC);
        }
    }

    // 3b. **Stufe B: den eigenen Interrupt binden** (B3/B4).
    //
    //     Der Treiber nennt **keinen Vektor** -- er nennt seine `Irq`-Cap. Welcher Interrupt das
    //     ist, weiss er gar nicht, und er soll es nicht wissen: der Vektor ist eine Zahl, keine
    //     Autoritaet.
    //
    //     **Vor der ersten Anfrage**, nicht danach. Eine Notification rastet zwar ein, aber nur
    //     wenn die Bindung schon steht: ein Interrupt, der vor dem `BIND_IRQ` eintrifft, findet
    //     keinen Eintrag und ist weg. Das ist der einzige Weg, hier einen Weckruf zu verlieren.
    //
    //     Scheitert die Bindung, bleibt `irq` `None` und der Treiber pollt weiter. Das ist die
    //     zulaessige Lage „dieses Geraet unterbricht nicht" -- und sie ist von aussen an
    //     `angeboten-dann-da` und `msix-ok` zu unterscheiden, nicht an einer stillen Naht hier.
    //     **Gebunden wird immer, gewartet nur mit [`B4_WARTEN`]** -- die Bindung ist B3 und
    //     traegt; der Warteweg ist B4 und haengt an einem offenen Befund.
    let gebunden = libcaprock::bind_irq(IRQ, IRQ_NTFN, IRQ_BADGE) == result::OK;
    let irq = if B4_WARTEN && gebunden {
        Some(caprock_virtio::IrqWarten {
            // **Zeile 0**, und sie hat der KERNEL geschrieben (E11). Der Treiber waehlt hier nur,
            // welche Queue sie bedient -- die Adresse und das Datenwort darin hat er nie gesehen.
            msix_zeile: 0,
            warte: warte_auf_irq,
        })
    } else {
        None
    };
    let irq = irq.as_ref();
    // **Die Zusage steht hier fest**, nicht je Anfrage: `irq` aendert sich waehrend eines Laufes
    // nicht, und die Crate hat keinen Rueckfallweg. Trotzdem wird `r.polled` unten je Ergebnis
    // nachgezogen -- eine Zusage, die nur an ihrer eigenen Voraussetzung haengt, prueft sich sonst
    // selbst.
    GEPOLLT.store(irq.is_none(), core::sync::atomic::Ordering::Release);
    melde(dma_cpu, false);

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
                    blk.request(dma_cpu, dma_dev, Op::Flush, 0, 0, MAX_POLL, irq)
                };
                capacity = r.capacity_sectors;
                reply(
                    EP,
                    [ST_OK, capacity, caprock_virtio::blk::MAX_SECTORS as u64, SECTOR as u64],
                );
            }
            OP_READ | OP_WRITE => {
                if capacity == 0 {
                    // SAFETY: wie oben. Ein Flush ohne Nutzdaten ist der billigste Weg, die
                    // Kapazitaet zu erfahren, ohne etwas zu veraendern.
                    capacity = unsafe { blk.request(dma_cpu, dma_dev, Op::Flush, 0, 0, MAX_POLL, irq) }
                        .capacity_sectors;
                }
                // **Bereichspruefung vor dem Geraet, nicht danach.** Ein Geraet, das ueber sein
                // Ende hinaus gefragt wird, darf antworten, wie es will; der Dienst hat vorher
                // nein zu sagen. Und `count` wird gegen die Puffergroesse geprueft, sonst
                // schriebe das Geraet hinter das Ende der Region.
                let zu_gross = count > caprock_virtio::blk::MAX_SECTORS as u64;
                let ueber_ende = capacity == 0
                    || sector >= capacity
                    || count > capacity - sector;
                if zu_gross || ueber_ende {
                    reply(EP, [ST_RANGE, 0, 0, capacity]);
                    continue;
                }
                let o = if op == OP_READ { Op::Read } else { Op::Write };
                if o == Op::Write {
                    // **Gegen BEIDE Laengen begrenzt.** Bis hierher stand hier nur
                    // `.min(shared_len)` -- die Laenge der QUELLE als Schranke fuer eine
                    // Kopie ins ZIEL. Das ging gut, solange `count` anderswo begrenzt war;
                    // die Schranke stand aber an der falschen Groesse, und das ist dieselbe
                    // Verwechslung wie `rx_used` gegen „Daten angekommen".
                    let n = (count * SECTOR as u64).min(shared_len).min(daten.len()) as usize;
                    // SAFETY: wie oben; `n` liegt jetzt nachweislich in beiden Bereichen.
                    unsafe {
                        core::ptr::copy_nonoverlapping(
                            shared as *const u8,
                            daten.cpu() as *mut u8,
                            n,
                        );
                    }
                }
                // SAFETY: wie oben; `count` ist geprueft und passt in die Region.
                let r = unsafe {
                    blk.request(dma_cpu, dma_dev, o, sector, count as u32, MAX_POLL, irq)
                };
                // Gelesene Sektoren in die geteilte Flaeche kopieren, damit der Client sie
                // SEHEN kann. Beim Schreiben umgekehrt -- dort hat er sie vorher hineingelegt.
                if r.completed() {
                    let n = (r.sectors as u64 * SECTOR as u64)
                        .min(shared_len)
                        .min(daten.len()) as usize;
                    // SAFETY: beide Bereiche gehoeren dieser PD; `n` ist durch die kleinere der
                    // beiden Laengen begrenzt.
                    unsafe {
                        let von = daten.cpu() as *const u8;
                        let nach = shared as *mut u8;
                        if op == OP_READ {
                            core::ptr::copy_nonoverlapping(von, nach, n);
                        }
                    }
                }
                if r.polled {
                    GEPOLLT.store(true, core::sync::atomic::Ordering::Release);
                }
                melde(dma_cpu, r.msix_ok);
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
                let r = unsafe { blk.request(dma_cpu, dma_dev, Op::Flush, 0, 0, MAX_POLL, irq) };
                reply(
                    EP,
                    [if r.completed() { ST_OK } else { ST_DEVICE }, 0, 0, bump_served(dma_cpu)],
                );
            }
            OP_SCAN => {
                match scan_partitions(&blk, dma_cpu, dma_dev, irq) {
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
