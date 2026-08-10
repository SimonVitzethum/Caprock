//! **virtio-blk**: eine Leseanfrage an ein Blockgeraet (A-5.2).
//!
//! ## Warum das mehr belegt als virtio-rng
//!
//! Der RNG hat **eine** Deskriptorzelle, und das Geraet schreibt hinein. Damit steht fest, dass
//! Bus-Master-DMA in unseren Speicher ankommt — und sonst nichts. Insbesondere steht **nicht**
//! fest, ob das Geraet unseren Speicher auch *lesen* kann: der RNG liest nie etwas von uns.
//!
//! `blk` schliesst die Luecke. Eine Leseanfrage ist eine **Deskriptorkette aus drei Gliedern**:
//!
//! | Glied | Richtung | Inhalt |
//! |---|---|---|
//! | 0 | Geraet **liest** | Anfragekopf: Typ, Sektornummer |
//! | 1 | Geraet schreibt | 512 Byte Sektordaten |
//! | 2 | Geraet schreibt | ein Statusbyte |
//!
//! Kommt Glied 0 nicht an, weiss das Geraet nicht einmal, welchen Sektor es liefern soll. Ein
//! Ergebnis, das die richtigen Bytes des richtigen Sektors traegt, belegt damit beide Richtungen
//! in **einer** Transaktion — und zusaetzlich, dass das Geraet Deskriptorketten ueber `next`
//! ueberhaupt verfolgt.
//!
//! ## Warum der Inhalt geprueft wird und nicht nur die Laenge
//!
//! Ein Puffer voller Nullen ist von einem nie beschriebenen Puffer nicht zu unterscheiden — und
//! ein frisch angelegtes Plattenabbild besteht genau daraus. Der Aufrufer legt deshalb eine
//! **Magie** in den Sektor und vergleicht sie; [`BlkResult::capacity_sectors`] gibt zusaetzlich
//! die vom Geraet gemeldete Groesse, die zum Abbild passen muss. Zwei unabhaengige Aussagen, von
//! denen keine durch Schweigen wahr wird.

use crate::{Transport, F_ACCESS_PLATFORM, F_VERSION_1, VIRTQ_DESC_F_NEXT, VIRTQ_DESC_F_WRITE};

/// `VIRTIO_BLK_T_IN` — lesen.
const T_IN: u32 = 0;
/// `VIRTIO_BLK_T_OUT` — schreiben.
const T_OUT: u32 = 1;
/// `VIRTIO_BLK_T_FLUSH` — alles Geschriebene dauerhaft machen.
///
/// Ohne Flush ist „geschrieben" eine Aussage ueber einen Puffer, nicht ueber die Platte. Genau
/// diese Verwechslung ist der Unterschied zwischen einem Dateisystem, das einen Stromausfall
/// ueberlebt, und einem, das es meistens tut.
const T_FLUSH: u32 = 4;
/// `VIRTIO_BLK_S_OK`.
pub const S_OK: u8 = 0;
/// Sektorgroesse. In virtio-blk **immer** 512, unabhaengig von der physischen Blockgroesse der
/// Platte: `capacity` zaehlt 512-Byte-Einheiten (Spec 5.2.4). Wer hier die Geraeteblockgroesse
/// einsetzt, rechnet die Kapazitaet um einen Faktor daneben.
pub const SECTOR: u32 = 512;

/// Kapazitaet in 512-Byte-Sektoren, Offset im geraetespezifischen Konfigurationsraum.
const CFG_CAPACITY: u64 = 0x00;

// Layout in der DMA-Region (Offsets ab Regionsbasis). Braucht 0x1200 Byte -> eine 8-KiB-Region.
/// Virtqueue 0 (Anfragewarteschlange).
pub const OFF_QUEUE: u64 = 0x0000;
/// Anfragekopf (16 Byte) — das Glied, das **das Geraet liest**.
pub const OFF_HDR: u64 = 0x0800;
/// Statusbyte.
pub const OFF_STATUS: u64 = 0x0810;
/// Datenpuffer. Beginnt auf einer Seitengrenze, damit er sich getrennt von der Virtqueue
/// abbilden laesst — was ihn davor bewahrt, den Ring mitzuverschenken, wenn ihn jemand anders
/// sehen soll.
pub const OFF_DATA: u64 = 0x1000;
/// Wie viele Sektoren eine Anfrage hoechstens umfasst.
///
/// Die Zahl folgt aus dem Layout, nicht aus einer Vorliebe: der Datenbereich beginnt bei
/// [`OFF_DATA`] und die Region ist [`REGION_BYTES`] gross. Wer sie erhoehen will, muss die Region
/// vergroessern — und nicht bloss diese Konstante, sonst schreibt das Geraet hinter das Ende.
pub const MAX_SECTORS: u32 = 8;
/// Mindestgroesse der DMA-Region fuer eine Anfrage (Virtqueue + Kopf + [`MAX_SECTORS`] Sektoren).
pub const REGION_BYTES: u64 = OFF_DATA + (MAX_SECTORS as u64) * (SECTOR as u64);

/// Ergebnis einer Leseanfrage. Jedes Feld ist einzeln pruefbar — ein Sammel-`bool` haette den
/// Fall "Geraet hat geantwortet, aber mit Fehlerstatus" von "Geraet hat nicht geantwortet"
/// nicht getrennt, und das sind verschiedene Befunde.
#[derive(Clone, Copy, Default)]
pub struct BlkResult {
    /// Caps gefunden, `VIRTIO_F_ACCESS_PLATFORM` angeboten und Features angenommen.
    pub features_ok: bool,
    /// Kapazitaet laut geraetespezifischem Konfigurationsraum (512-Byte-Sektoren).
    pub capacity_sectors: u64,
    /// Hat der used-Ring fortgeschritten? (Das Geraet hat die Anfrage abgeschlossen.)
    pub used_advanced: bool,
    /// Statusbyte des Geraets (`S_OK` = 0).
    pub status: u8,
    /// Vom Geraet gemeldete Zahl geschriebener Bytes (Daten **+** Statusbyte).
    pub written: u32,
    /// Die ersten acht Bytes des Datenpuffers — bei einer Leseanfrage der Anfang des Sektors.
    pub first_word: u64,
    /// Wie viele Sektoren die Anfrage umfasste (`0` bei Flush).
    pub sectors: u32,
    /// Wie viele Bytes das Geraet bei dieser Anfrage schreiben MUSS, damit sie vollstaendig ist.
    ///
    /// Das haengt an der **Richtung**, nicht an der Sektorzahl -- und diese Verwechslung hat beim
    /// ersten Anlauf zugeschlagen: beim Lesen schreibt das Geraet die Sektoren **und** das
    /// Statusbyte, beim Schreiben und beim Flush nur das Statusbyte. Wer fuer alle Faelle
    /// `Sektoren * 512 + 1` erwartet, laesst jede Schreibanfrage durchfallen -- mit einer Zahl,
    /// die nach einem Geraetefehler aussieht, waehrend die Daten laengst auf der Platte stehen.
    /// Deshalb steht die Erwartung im Ergebnis, gebildet dort, wo die Richtung bekannt ist.
    pub expected_written: u32,
}

/// **virtio-blk**: Blockgeraet.
pub struct VirtioBlk {
    t: Transport,
}

impl VirtioBlk {
    /// Aus einem bereits aufgeloesten Transport bauen.
    pub const fn from_transport(t: Transport) -> Self {
        Self { t }
    }

    /// Handshake + **einen Sektor lesen** — die Kurzform von [`Self::request`] fuer den Fall,
    /// den es seit A-5.2 gibt.
    ///
    /// # Safety
    /// wie [`Self::request`].
    pub unsafe fn read_sector(
        &self,
        cpu_base: u64,
        dev_base: u64,
        sector: u64,
        max_poll: u64,
    ) -> BlkResult {
        self.request(cpu_base, dev_base, Op::Read, sector, 1, max_poll)
    }

    /// Handshake + **eine Anfrage**: lesen, schreiben oder flushen.
    ///
    /// `cpu_base` ist die Basis der DMA-Region aus Sicht der CPU, `dev_base` dieselbe Region aus
    /// Sicht des Geraets — getrennt aus demselben Grund wie bei [`crate::VirtioRng::request`]:
    /// mit einem IOVA-Fenster ≠ 0 fallen die Achsen auseinander.
    ///
    /// `count` ist die Zahl der Sektoren (bei [`Op::Flush`] bedeutungslos, sonst 1..=[`MAX_SECTORS`]).
    /// Die Daten liegen ab [`OFF_DATA`]; bei [`Op::Write`] muss der Aufrufer sie **vorher**
    /// dorthin legen.
    ///
    /// `max_poll` begrenzt das Warten auf den used-Ring. Wer einen Fehlschlag **erwartet** (die
    /// IOMMU sperrt), setzt eine kurze Schranke — sonst kostet jeder Lauf hunderte Millisekunden
    /// Leerlauf.
    ///
    /// # Safety
    /// Die MMIO-Adressen des Transports muessen zum Geraet gehoeren; `[cpu_base, cpu_base +
    /// REGION_BYTES)` muss beschreibbarer Speicher sein, der dem Aufrufer allein gehoert.
    pub unsafe fn request(
        &self,
        cpu_base: u64,
        dev_base: u64,
        op: Op,
        sector: u64,
        count: u32,
        max_poll: u64,
    ) -> BlkResult {
        let mut r = BlkResult::default();
        // Die Schranke steht **vor** dem Handshake: eine Anfrage, die den Puffer ueberliefe, darf
        // das Geraet gar nicht erst zu sehen bekommen. Ein Geraet, das ueber das Ende hinaus
        // schreibt, ist von einem Speicherfehler nicht mehr zu unterscheiden.
        let n = match op {
            Op::Flush => 0,
            _ if count == 0 || count > MAX_SECTORS => return r,
            _ => count,
        };

        self.t.reset();
        // Ohne ACCESS_PLATFORM wuerde das Geraet an der IOMMU vorbei auf physische Adressen
        // greifen — und der Treiber haette keine Moeglichkeit, das zu bemerken. Abbrechen statt
        // still zurueckfallen (dieselbe Regel wie beim RNG).
        if self.t.offered() & F_ACCESS_PLATFORM == 0 {
            return r;
        }
        // Von den blk-eigenen Features wird **keins** verlangt: SEG_MAX, BLK_SIZE, FLUSH und der
        // Rest sind samt und sonders optional, und jedes zusaetzlich ausgehandelte Bit ist eine
        // Zusage, die der Treiber dann auch einhalten muesste.
        //
        // **Auch VIRTIO_BLK_F_FLUSH nicht** — und das ist kein Versehen: ohne ausgehandeltes
        // FLUSH ist `T_FLUSH` nach Spec unzulaessig, das Geraet darf es abweisen. Genau deshalb
        // wird der Statuscode einer Flush-Anfrage hier weitergereicht, statt still verworfen zu
        // werden: der Aufrufer soll erfahren, dass seine Dauerhaftigkeitszusage nicht gilt.
        if !self.t.negotiate(F_VERSION_1 | F_ACCESS_PLATFORM) {
            return r;
        }
        // Ohne aufgeloesten Konfigurationsraum LIEST `cfg64` nicht etwa daneben -- es gibt 0
        // zurueck. Das ist die gefaehrlichere Variante: eine Kapazitaet von 0 sieht aus wie eine
        // leere Platte und nicht wie eine nicht gefundene Capability. Also hier abbrechen, statt
        // eine Null weiterzureichen, die der Aufrufer nicht mehr einordnen kann.
        if !self.t.has_device_cfg() {
            return r;
        }
        r.features_ok = true;
        // Die Kapazitaet steht im geraetespezifischen Konfigurationsraum. Der RNG hat keinen —
        // deshalb ist `device_cfg` bis A-5.2 nirgends aufgeloest worden.
        r.capacity_sectors = self.t.cfg64(CFG_CAPACITY);

        let Some((q, notify_off)) =
            self.t.queue_setup(0, cpu_base + OFF_QUEUE, dev_base + OFF_QUEUE, 8)
        else {
            return r;
        };
        self.t.driver_ok();

        // **Die drei Puffer werden EINMAL herausgeschnitten** (todo E, Descriptor-Typestate).
        //
        // `carve` schneidet monoton — Kopf (0x800), Statusbyte (0x810), Daten (0x1000) —, also
        // koennen sie sich nicht ueberlappen. Danach ist jeder von ihnen ein `Owned<Driver>`, und
        // das Armieren VERBRAUCHT ihn: „Puffer steht in der Queue und wird nebenher noch
        // beschrieben" ist ab hier kein Fehler mehr, den man machen kann, sondern einer, den der
        // Uebersetzer nennt.
        let bytes = n * SECTOR;
        let mut region = crate::Region::from_raw(cpu_base, dev_base, REGION_BYTES);
        let (Some(mut hdr), Some(mut status)) =
            (region.carve(OFF_HDR, 16), region.carve(OFF_STATUS, 1))
        else {
            return r;
        };
        // Bei Flush gibt es kein Datenglied — der Puffer wird trotzdem geschnitten, weil
        // `first_word` auch dann berichtet wird. Er wird dann nur nie armiert und bleibt die ganze
        // Zeit unserer.
        let Some(mut data) = region.carve(OFF_DATA, if n > 0 { bytes } else { 8 }) else {
            return r;
        };

        // Anfragekopf: struct virtio_blk_req { le32 type; le32 reserved; le64 sector; }
        hdr.wr32(0, op.type_code());
        hdr.wr32(4, 0);
        hdr.wr64(8, sector);
        // Das Statusbyte vorbelegen: **nicht** mit Null. Ein Statusbyte, das schon 0 (= S_OK) ist,
        // bevor das Geraet es beschreibt, macht die Statuspruefung wertlos — sie waere auch dann
        // gruen, wenn das Geraet nie geantwortet hat.
        status.wr8(0, 0xff);
        if op == Op::Read {
            data.wr64(0, 0);
        }
        self.t.fence();

        // Die Kette. Glied 0 hat KEIN Write-Flag — das ist die Richtung, die der RNG-Test nicht
        // zeigen kann. Beim SCHREIBEN traegt auch das Datenglied keins: dort liest das Geraet
        // **beide** Glieder, und ein faelschlich gesetztes Write-Flag hiesse, dem Geraet
        // Schreibrecht auf unseren Puffer zu geben, das es fuer diese Anfrage nicht braucht.
        let last = if n == 0 { 1u16 } else { 2 };
        let armed_hdr = q.arm(0, hdr, VIRTQ_DESC_F_NEXT, 1);
        let mut ruhender_datenpuffer = Some(data);
        let armed_data = if n > 0 {
            let data_flags = match op {
                Op::Read => VIRTQ_DESC_F_NEXT | VIRTQ_DESC_F_WRITE,
                _ => VIRTQ_DESC_F_NEXT,
            };
            ruhender_datenpuffer.take().map(|d| q.arm(1, d, data_flags, 2))
        } else {
            None
        };
        let armed_status = q.arm(last, status, VIRTQ_DESC_F_WRITE, 0);

        let used0 = q.used_idx();
        q.publish(0, self.t.fence); // die hereingereichte Barriere, nicht irgendeine
        self.t.kick(notify_off, 0);

        let done = q.poll_used(used0, max_poll);
        if let Some(c) = done.as_ref() {
            r.used_advanced = true;
            r.written = c.len();
        }
        // Status und Daten werden IMMER gelesen, auch ohne used-Fortschritt: bliebe das
        // Statusbyte bei 0xff, ist damit belegt, dass das Geraet nichts geschrieben hat — und
        // nicht bloss, dass wir zu frueh aufgehoert haben zu warten.
        //
        // **Genau dafuer gibt es `reclaim_unproven`.** Der Weg ohne Abschlussbeleg ist hier kein
        // Versehen, sondern die Messung selbst — und er steht deshalb mit seinem Namen da, statt
        // als roher Zugriff an der Typisierung vorbei.
        self.t.fence();
        let (hdr, status, data) = match done.as_ref() {
            Some(c) => (
                q.reclaim(armed_hdr, c),
                q.reclaim(armed_status, c),
                armed_data.map(|d| q.reclaim(d, c)),
            ),
            None => (
                q.reclaim_unproven(armed_hdr),
                q.reclaim_unproven(armed_status),
                armed_data.map(|d| q.reclaim_unproven(d)),
            ),
        };
        let _ = hdr; // der Kopf wird nicht zurueckgelesen -- aber auch nicht stumm liegengelassen
        r.status = status.rd8(0);
        r.first_word = match data.or(ruhender_datenpuffer) {
            Some(d) => d.rd64(0),
            None => 0,
        };
        r.sectors = n;
        r.expected_written = if op == Op::Read { n * SECTOR + 1 } else { 1 };
        r
    }
}

/// Was eine Anfrage tut.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Op {
    /// Sektoren lesen (das Geraet **schreibt** in den Puffer).
    Read,
    /// Sektoren schreiben (das Geraet **liest** den Puffer).
    Write,
    /// Alles bisher Geschriebene dauerhaft machen. Ohne Nutzdaten.
    Flush,
}

impl Op {
    fn type_code(self) -> u32 {
        match self {
            Op::Read => T_IN,
            Op::Write => T_OUT,
            Op::Flush => T_FLUSH,
        }
    }
}

impl BlkResult {
    /// Ist der Sektor vollstaendig und mit der erwarteten Magie angekommen?
    ///
    /// `written` zaehlt die vom Geraet beschriebenen Bytes und schliesst das Statusbyte ein
    /// (Spec: alle geraeteschreibbaren Glieder der Kette) — erwartet werden also `SECTOR + 1`.
    pub fn ok(&self, magic: u64) -> bool {
        self.completed() && self.first_word == magic
    }

    /// Ist die Anfrage **durchgelaufen** — unabhaengig vom Inhalt?
    ///
    /// `written` zaehlt die vom Geraet beschriebenen Bytes und schliesst das Statusbyte ein
    /// (Spec: alle geraeteschreibbaren Glieder der Kette). Wie viele es sein muessen, steht in
    /// [`BlkResult::expected_written`] -- s. dort, warum das an der Richtung haengt und nicht an
    /// der Sektorzahl.
    pub fn completed(&self) -> bool {
        self.features_ok
            && self.used_advanced
            && self.status == S_OK
            && self.written >= self.expected_written
    }
}
