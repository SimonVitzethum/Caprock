//! **Loader-Dienst-PD fuer den BETRIEB: Treiber als LXPD-Images von Platte.**
//!
//! Der Boot laedt aus dem Boot-Archiv; danach kommen Treiber von Platte — ueber einen
//! **externen** Blockdriver (fremde PD, spricht das Minimal-Protokoll aus [`protokoll`]),
//! verifiziert, dann Instanziierung. Dieser Dienst ist die Betriebsseite davon: suchen, lesen,
//! pruefen, anstossen — und niemals laden ohne Pruefung.
//!
//! ## Ablauf (Zustandsmaschine, Phasen in [`Phase`])
//!
//! ```text
//! LEER --suchen(index)--> VERZEICHNIS --bild_lesen()--> BILD --pruefen(keys)--> GEPRUEFT
//!                                                                               |
//!                                                        anstossen() [Loader-Cap + ABI]
//!                                                                               v
//!                                                                            GELADEN
//! ```
//!
//! Jeder Schritt verlangt die Phase seines Vorgaengers; [`LadeFehler::Ungeprueft`] ist die
//! benannte Absage fuer jeden Sprung ausser der Reihe. `anstossen` ist zusaetzlich gegatet:
//! ohne Loader-Autoritaet ([`LadeFehler::KeineLoaderCap`]) geht nichts raus — fail-closed
//! client-seitig, BEVOR ein Syscall gebaut wird. Den Stand des vergebenen ABI-Patchs haelt
//! [`PATCH_TEXT`].
//!
//! ## Was der Dienst prueft, und woher die Regeln kommen (gegen `caprock-lxpd` gelesen)
//!
//! | Pruefung | Regelquelle | warum hier, nicht dort |
//! |---|---|---|
//! | GPT-Kopf + Eintrags-CRC (Stueck-fuer-Stueck) | `caprock-part` (dieselben Funktionen wie der Treiber) | Die Tabelle sind fremde Bytes; ungeprueft waere jede LBA eine Vermutung. |
//! | Typ-Auswahl per GUID ([`LXPD_PART_GUID`]) | lokale Konvention (s. dort) | Weder GPT noch Treiber kennen LXPD — die Auswahl ist Dienst-Politik. |
//! | Verzeichnis-Laengen (Eintrag/Bild/Manifest) | [`BildVerzeichnis`] (Plattenformat dieses Dienstes) | Groessen stehen fest, BEVOR gelesen wird — kein Lesen ins Ungewisse. |
//! | Eintrag: Fassung, Herkunft, Hash-Form, Key-ID-Form | `driver::parse_entry` (wiederverwendet) | Der Eintrag sagt WELCHES Bild (SHA-256) und WOHER (Quelle). |
//! | Herkunftsbindung an die gefundene Partition | [`LadeDienst::herkunft_bindet`] (Dienst-Regel) | Ein Eintrag fuer eine andere Partition (GUID/Bereich) meint andere Bytes. |
//! | Bildbindung SHA-256 | `driver::verify_image` (echte Rechnung) | FNV waere hier eine Attrappe (s. `driver.rs`-Doku: Platte darf der Angreifer beschreiben) — deshalb SHA-256, nicht FNV. |
//! | Container: Magic, Zaehler, Stubs, Trailer | `LxpdImage::parse` (wiederverwendet, nicht nachgebaut) | Dieselben Regeln wie der Boot-Pfad — ein Bild, das hier besteht und dort faellt, waere zwei Loader. |
//! | Manifest: Fakten (Version, Coverage 100, Grants, IRQ) + Stubzahl-Abgleich | `verify_manifest` | Bindet Manifest an Container (Zahl) und Treiber an Zusagen (Grants). |
//! | Manifest-Signatur (FNV-1a-64 ueber Key + Kanonik) | `manifest::verify_signature` (echte Rechnung) | Niemals angenommen ohne Rechnung. |
//! | ELF-Pfad: Loader-Regeln + Manifest-Fakten + Signatur | `elf_ready` + Fakten + Signatur | Pfad (b) ohne Manifest waere ein Bild ohne Zusagen. |
//! | Eintrags-Zeuge (`key_id` + FNV ueber Roh-Pubkey + Kanonik) | `driver::verify_witness` | Bindet den Eintrag an den Root-Schluessel der PD (Endowment). |
//! | Namensbindung Eintrag ↔ Manifest (`driver`) | [`treibername_gleich`] (Dienst-Regel) | Zwei gueltige, signierte Dokumente koennen trotzdem zusammengehoeren-nicht: Eintrag A + Manifest B waere ein Treiber mit fremden Zusagen. |
//!
//! ## Wer laden darf (gegen `abi`/`microkit`/`loader` gelesen)
//!
//! `SYS_LOAD` ist auf eine `Loader`-Cap mit WRITE gegatet; die Cap selbst kommt aus dem
//! System-Manifest (`initial_caps`-Bit [`CAP_LOADER_BIT`]). Der geladene Prozess erhaelt NUR
//! explizit delegierte Caps. Dieser Dienst braucht also im EIGENEN Manifest-Eintrag das
//! Loader-Bit — ohne es scheitert jeder Anstoss mit [`LadeFehler::KeineLoaderCap`], BEVOR ein
//! Syscall gebaut wird (Fail-closed client-seitig, nicht erst per `ERR_BADCAP`).
//!
//! Und der Anstoss ist ehrlich: `SYS_LOAD_IMAGE = 36` übergibt das geprüfte Bild aus einer
//! Memory-Cap des Aufrufers (`TAG`: Low-Byte = Bild-Slot, Bits 8..40 = exakte
//! Bildlänge, Bits 40..64 = `0`; `MSG0` = Programm-ID aus dem Boot-Manifest, `MSG1` =
//! Delegationsliste, `MSG2` = Anzahl, `MSG3` = Ressourcenwunsch wie `SYS_LOAD`). Vertrauen
//! kommt aus dem Boot-Manifest (Hash-Gleichheit mit dem Eintrag dieser Programm-ID) —
//! Manifest-Cap braucht es keine. [`KernelAnstoss`] ruft diesen Pfad über
//! `libcaprock::load_image`; der Host-Test fährt gegen [`FakeAnstoss`], der Gatter-Logik und
//! Slot-Kontext belegt, ohne je einen Syscall zu stellen (auf dem Host liefe er ins Leere).
//!
//! Welche Slots Bild und Loader-Cap halten, weiss erst die PD aus IHREM Manifest (Endowment
//! wie die Schlüssel — s. [`AnstossKontext`]): Der Dienst rät keine Slots, er reicht sie nur
//! durch. Der Kernel prüft NACH (`load_verified_image` im Parallelstrang): Die PD-Prüfung ist
//! die erste, nicht die einzige — ein Kern, der der PD glaubte, machte aus jeder
//! kompromittierten PD einen Loader.
//!
//! ## Was hier laeuft und was gestellt ist
//!
//! Auf dem Host laufen: GPT-Suche, Stueckelung, SHA-256-Bindung, Container-/Manifest-/Eintrags-
//! Pruefung, Gatter. Gestellt sind der Transport ([`BlockQuelle`] statt IPC — Bytes statt Worte,
//! s. `protokoll`), der Syscall ([`LadeAnstoss`] statt `SYS_LOAD`), die Schluessel (Endowment)
//! und der Speicher (feste Puffer statt PD-RAM). Was damit belegt ist: Kein defekter Sektor,
//! kein falscher Hash, kein abgebrochener Read, keine fremde Partition und kein gemischtes
//! Dokumentenpaar fuehrt je zu einem Anstoss — und ein Anstoss fuehrt nie ohne Pruefung.

#![no_std]
#![forbid(unsafe_code)]

// Die Crate ist `no_std` (sie laeuft in einer Dienst-PD ohne Betriebssystem). Der Testharness
// braucht `std` fuer Plattenabbilder und Schluessel-Helfer — test-only, der PD-Bau sieht es nie
// (dasselbe Muster wie `mem-server` / `lx-shim-demo`).
#[cfg(test)]
extern crate std;

pub mod protokoll;

use caprock_lxpd::{LxpdError, ManifestFacts};

// --- Platten-/Protokoll-Konstanten --------------------------------------------------

/// SektorgrOesse, in der GPT und Treiber rechnen (wie `caprock-part::SECTOR`).
pub const SEKTOR: usize = 512;
/// Hoechstzahl Sektoren je Leseanfrage (wie `caprock_virtio::blk::MAX_SECTORS`).
pub const MAX_SEKTOREN_JE_ANFRAGE: u32 = 8;
/// Staerkste Stueckelung einer Anfrage in Byte (8 * 512).
pub const STUECK_BYTES: usize = 4096;
/// Obergrenze eines Treiber-Bildes (128 Sektoren = 64 KiB). Begruendung: Ein LXPD-v1-Container
/// traegt hoechstens 8217 B (`20 + 256*32 + 5`); der ELF-Pfad braucht Luft, aber kein Dienst
/// braucht unbegrenztes Staging — darueber gibt es [`LadeFehler::BildZuGross`] (D11: benannte
/// Erschoepfung statt stiller Wiederverwendung oder stillen Abschneidens).
pub const MAX_BILD_BYTES: usize = 65536;
/// Obergrenze eines Manifests (16 Sektoren = 8 KiB). Ein Manifest sind Fakten, kein ROM.
pub const MAX_MANIFEST_BYTES: usize = 8192;
/// Obergrenze eines Treiber-Eintrags (8 Sektoren = 4 KiB). Ein Eintrag nennt Herkunft + Hash +
/// Zeugen — wer mehr braucht, schreibt kein Verzeichnis, sondern einen Katalog (dann aber mit
/// eigenem Format, nicht als „etwas laengerer Eintrag").
pub const MAX_EINTRAG_BYTES: usize = 4096;
/// Obergrenze eines Treibernamens fuer die Namensbindung (der Abgleich selbst laeuft auf den
/// JSON-Bytes; diese Schranke begrenzt nur, was der Dienst je vergleicht).
pub const MAX_TREIBER_NAME: usize = 64;
/// `initial_caps`-Bit fuer die Loader-Autoritaet (wie `caprock_loader::manifest::CAP_LOADER`).
pub const CAP_LOADER_BIT: u32 = 1 << 0;

/// Typ-GUID der LXPD-Treiberpartition (lokale Konvention, KEIN GPT-Standardwert):
/// die ASCII-Bytes `"LXPD-CAPROCK-DRV"` (16 Byte). Weder GPT noch `caprock-part` kennen LXPD —
/// die Auswahl nach genau diesen Bytes ist Dienst-Politik und steht genau einmal hier. Eine
/// Partition mit anderem Typ ist „fremd" und wird uebersprungen, nicht abgewiesen: Fremde
/// Partitionen (EFI-System, Daten) sind der Normalfall, kein Fehler — erst wenn KEINE LXPD-
/// Partition bleibt, gibt es [`LadeFehler::KeineLxpdPartition`].
///
/// Von der Typ-GUID zu unterscheiden: die EINDEUTIGE GUID jedes Eintrags (Bytes 16..32), an
/// die ein `DiskGuid`-Eintrag bindet — robust gegen Verschieben der Partition (s. `driver.rs`).
pub const LXPD_PART_GUID: [u8; 16] = *b"LXPD-CAPROCK-DRV";

/// Verzeichnis-Magic am Partitionsanfang (`"LXIMG2\0\0"` — Fassung 2: mit Eintrag; Fassung 1
/// ohne Eintrag ist hier kein Vorgaenger, sondern ein fremdes Format).
pub const VERZEICHNIS_MAGIC: [u8; 8] = *b"LXIMG2\0\0";
/// Kopflange des Verzeichnisses (Magic 8 + Eintraglen 4 + Bildlen 4 + Manifestlen 4 + Flags 4).
pub const VERZEICHNIS_LEN: usize = 24;

/// Die Block-Operationen, die dieser Lader je baut (fuer den Strukturtest in `protokoll`:
/// genau INFO + READ — was man nicht baut, schickt man nicht).
pub fn bekannte_block_ops() -> [u64; 2] {
    [protokoll::OP_INFO, protokoll::OP_READ]
}

// --- Fehler ------------------------------------------------------------------------

/// Warum ein Ladeschritt nicht ging. Jeder Ausgang hat einen eigenen Namen — „geht nicht"
/// allein sagte nicht, ob der Aufrufer es wieder versuchen, anders fragen oder den
/// Platteninhalt untersuchen soll.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LadeFehler {
    /// Die Kapazitaetsauskunft fehlt oder ist null — ohne sie ist jede LBA eine Vermutung.
    KeineKapazitaet,
    /// Keine GPT-Signatur (unformatierte Platte — anderer Fall als kaputte Tabelle).
    GptSignatur,
    /// Fremde GPT-Revision (anderes Layout, nicht dieselbe Tabelle mit Fehlern).
    GptRevision,
    /// `header_size` ausserhalb der Grenzen.
    GptKopfGroesse,
    /// Kopf-CRC falsch (stille Aenderung am Kopf).
    GptKopfCrc,
    /// `entry_size` zu klein oder kein Vielfaches von 8.
    GptEintragGroesse,
    /// `num_entries * entry_size` uebergelaufen oder absurd gross.
    GptEintragZahl,
    /// Eintragslisten-CRC falsch (stille Aenderung an der Liste).
    GptEintraegeCrc,
    /// Ein Eintrag ist groesser als ein Sektor — diesen Dienst liest das nicht (benannt, nicht
    /// halb; reale Werkzeuge schreiben 128 B).
    GptEintragZuGross,
    /// Belegte Partitionen ja, LXPD-Typ nein (Index ausserhalb oder falscher Typ ueberall).
    KeineLxpdPartition,
    /// Das Geraet antwortete nicht oder meldete einen Fehler.
    Geraet,
    /// Ausserhalb der Platte verlangt (haette der Dienst vorher wissen koennen).
    Bereich,
    /// Kuerzer gelesen als angefordert — abgebrochen, nicht teilweise gut.
    Abgebrochen,
    /// Verzeichnis-Magic falsch, Reservenull gesetzt oder Laengen widersprechen Partition.
    VerzeichnisKaputt,
    /// Bild laenger als [`MAX_BILD_BYTES`].
    BildZuGross,
    /// Manifest laenger als [`MAX_MANIFEST_BYTES`].
    ManifestZuGross,
    /// Eintrag laenger als [`MAX_EINTRAG_BYTES`].
    EintragZuGross,
    /// SHA-256 ueber die gelesenen Bildbytes stimmt nicht mit dem Eintrag ueberein — der
    /// Eintrag meint ein anderes Bild.
    BildHashWeichtAb,
    /// Weder LXPD- noch ELF-Magic — kein Bildformat, kein Parse-Versuch.
    KeinBildformat,
    /// Der Eintrag/das Manifest/der Container scheitert an den LXPD-Regeln (Diagnose im Wert).
    Container(LxpdError),
    /// Eintrag und Manifest gehoeren nicht zusammen (verschiedene `driver`-Namen) — zwei
    /// gueltige, signierte Dokumente, aber kein Paar.
    TreiberMismatch,
    /// Anstoss oder Pruefung ausser der Reihe — niemals laden ohne Pruefung.
    Ungeprueft,
    /// Diese PD haelt keine Loader-Cap (Manifest-Bit fehlt) — der Syscall wuerde mit
    /// `ERR_BADCAP` scheitern, also wird er gar nicht erst gebaut.
    KeineLoaderCap,
    /// VERGEBEN (seit `LOAD_IMAGE = 36`): Die ABI kann das geprüfte Bild übergeben — der
    /// Produktions-Anstoss antwortet nie mehr so. Die Variante bleibt (Kode 24 in
    /// [`protokoll::lade_kode`]), damit alte Logs lesbar bleiben; neuer Code trifft sie nicht.
    KeineBildUebergabe,
    /// Der Anstoss selbst wurde abgelehnt (Fake nein, Client-Gatter in `libcaprock` nein oder
    /// Kernel-Absage auf `LOAD_IMAGE`).
    AnstossAbgelehnt,
    /// Falsche Nachricht an den Dienst (unbekannte Art oder Reservewort gesetzt).
    FalscheNachricht,
}

// --- Transport-Abstraktion (gestellt: Bytes statt Worte) ------------------------------

/// Kapazitaetsauskunft des Blockdrivers (Antwort auf `OP_INFO`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct BlockInfo {
    /// Sektoren der Platte.
    pub kapazitaet: u64,
    /// HoChstzahl Sektoren je Anfrage (Geraetezusage).
    pub max_je_anfrage: u32,
}

/// Was der Blockdriver falsch machen kann (Antwort ungleich `ST_OK`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BlockFehler {
    /// Das Geraet antwortete nicht oder meldete einen Fehler.
    Geraet,
    /// Ausserhalb der Platte oder zu viel auf einmal.
    Bereich,
    /// Kuerzer als angefragt (der Dienst prueft die Zahl selbst — s. `lese_exakt`).
    Abgebrochen,
}

/// Die Blockquelle: Auskunft + sektorweises Lesen in einen Aufrufer-Puffer. In der PD spricht
/// dahinter das Wortprotokoll aus [`protokoll`] (Anfrage bauen, Worte schicken, Staging-Puffer
/// kopieren); im Host-Test steht dahinter ein Fake-Blockdriver.
pub trait BlockQuelle {
    /// Auskunft einholen.
    fn info(&mut self) -> Result<BlockInfo, BlockFehler>;
    /// `sektoren` Sektoren ab `lba` in `puffer` lesen. Gibt die WIRKLICH gelesene Sektorzahl
    /// zurueck — weniger als verlangt ist [`BlockFehler::Abgebrochen`], nicht „teilweise gut".
    fn lesen(&mut self, lba: u64, sektoren: u32, puffer: &mut [u8]) -> Result<u32, BlockFehler>;
}

/// Der Slot-Kontext des Lade-Anstosses: welche Slots die `LOAD_IMAGE`-Uebergabe trägt.
/// Kennt erst die PD aus IHREM Manifest (Endowment wie die Schlüssel — wer Slots per Wort
/// setzen könnte, wählte seine eigene Übergabe): Der Dienst rät keine Slots, `bedienen`/
/// `laden`/`anstossen` reichen sie nur durch bis zum Syscall. `deleg_liste` ist das gepackte
/// Wort wie bei `SYS_LOAD` (je Paar ein Byte, s. `libcaprock::load`), `extras` der
/// Ressourcenwunsch in `load_extras`-Belegung.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct AnstossKontext {
    /// Slot der `Loader`-Cap im eigenen Cspace (WRITE — gattert wie `SYS_LOAD`).
    pub loader_slot: u64,
    /// Slot der Memory-Cap mit dem geprüften Bild (kommt ins `TAG`-Low-Byte).
    pub bild_slot: u64,
    /// Programm-ID aus dem Boot-Manifest (`MSG0` — daran bindet das Kernel-Vertrauen).
    pub programm_id: u64,
    /// Gepackte Delegationsliste (`MSG1`).
    pub deleg_liste: u64,
    /// Zahl der gültigen Paare darin (`MSG2`).
    pub deleg_anzahl: u64,
    /// Ressourcenwunsch der neuen PD (`MSG3`, wie `SYS_LOAD`).
    pub extras: u64,
}

/// Der Lade-Anstoss: das, was in der PD `SYS_LOAD_IMAGE` mit Loader-Cap ist. Herausgezogen
/// als Trait, damit der Host-Test Gatter-Logik und Slot-Kontext belegen kann, ohne je einen
/// Syscall zu stellen: Die PD fährt [`KernelAnstoss`] (Produktionspfad über
/// `libcaprock::load_image`), der Host-Test einen Fake.
pub trait LadeAnstoss {
    /// Geprueftes Bild + Manifest + Eintrag instanziieren. Darf NUR mit geprueften Bytes
    /// gerufen werden — das Gatter steht im Dienst ([`LadeDienst::anstossen`]), nicht in der
    /// Disziplin des Aufrufers. `ctx` trägt die Slots aus dem Manifest der PD (s.
    /// [`AnstossKontext`); die Bildlänge kommt aus `bild` selbst — exakt die geprüften Bytes,
    /// keine zweite Zahl, die danebenliegen könnte.
    fn lade_bild(
        &mut self,
        ctx: &AnstossKontext,
        bild: &[u8],
        manifest: &[u8],
        eintrag: &[u8],
        art: BildArt,
    ) -> Result<u32, LadeFehler>;
}

/// Was geprueft wurde: Container-Form (a) oder reines ELF (b) samt Stubzahl-Hinweis.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BildArt {
    /// LXPD-v1-Container mit `n` Trampolin-Stubs.
    LxpdContainer { stuetz: u32 },
    /// Reines ET_EXEC-ELF (Loader-Regeln bestanden).
    Elf,
}

/// Der Produktions-Anstoss: `SYS_LOAD_IMAGE = 36` über `libcaprock::load_image`. Die Bytes
/// sind hier bereits geprüft (Hash + Container + Signatur — s. [`LadeDienst::pruefen`]); der
/// Kernel rechnet NACH (`load_verified_image` im Parallelstrang — niemals der PD glauben).
/// Die Bildlänge kommt aus den geprüften Bytes selbst; Slots, Programm-ID, Delegation und
/// Extras aus `ctx` (Manifest der PD).
///
/// Antwort: neue PD-Id bei `OK` (der Kernel legt sie in `x1` ab — s. `libcaprock::Ret::badge`),
/// sonst [`LadeFehler::AnstossAbgelehnt`]: Der Anstoss wurde abgelehnt — ob vom Client-Gatter
/// in `libcaprock` (Slot/Länge/Anzahl passten nicht) oder vom Kernel, steht im PD-Log, nicht
/// in vier Registern.
pub struct KernelAnstoss;

impl LadeAnstoss for KernelAnstoss {
    fn lade_bild(
        &mut self,
        ctx: &AnstossKontext,
        bild: &[u8],
        _manifest: &[u8],
        _eintrag: &[u8],
        _art: BildArt,
    ) -> Result<u32, LadeFehler> {
        let bild_len = u64::try_from(bild.len()).map_err(|_| LadeFehler::AnstossAbgelehnt)?;
        let r = libcaprock::load_image(
            ctx.loader_slot,
            ctx.bild_slot,
            bild_len,
            ctx.programm_id,
            ctx.deleg_liste,
            ctx.deleg_anzahl,
            ctx.extras,
        );
        if r.result != libcaprock::result::OK {
            return Err(LadeFehler::AnstossAbgelehnt);
        }
        u32::try_from(r.badge).map_err(|_| LadeFehler::AnstossAbgelehnt)
    }
}

// --- Vergebener Patch-Stand ---------------------------------------------------------------
///
/// Der ABI-/Kernel-Schritt (`LOAD_IMAGE = 36`) ist vergeben: ABI-Wahrheit in
/// `caprock_abi::sys::LOAD_IMAGE`, Dispatch- und Glue-Seite im Parallelstrang. Was hier als
/// Konstante steht ([`PATCH_TEXT`], Inhalt s. `patch.txt`), ist der neue Stand: Umsetzungs-
/// Vermerk plus Register-Tabelle — kein Vorschlag mehr, sondern die Wahrheit, gegen die der
/// Vertrags-Test läuft. Als Konstante im Code, damit der Test ihr Vorhandensein belegt (kein
/// Kommentar, der beim naechsten Edit verloren geht), und ausgeschrieben in BETRIEB.md.

/// Vergebener ABI-Patch-Stand: Umsetzungs-Vermerk plus Register-Tabelle der Bild-Uebergabe.
/// Als Konstante im Code, damit der Test Inhalt und Vorhandensein belegt (kein Kommentar, der
/// beim naechsten Edit verloren geht), und ausgeschrieben in BETRIEB.md.
pub const PATCH_TEXT: &str = include_str!("patch.txt");

// --- Verzeichnis ----------------------------------------------------------------------

/// Geparstes Bildverzeichnis: Laengen von Eintrag + Bild + Manifest. Keine Hashes, keine
/// Namen — die Bindung leistet der EINTRAG (SHA-256 + Zeuge), nicht das Verzeichnis. Das
/// Verzeichnis sagt nur, WO die drei Dokumente liegen und WIE GROSS sie sind, damit kein Byte
/// ins Ungewisse gelesen wird.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct BildVerzeichnis {
    /// Laenge des Treiber-Eintrags (JSON) in Byte (1..=[`MAX_EINTRAG_BYTES`]).
    pub eintrag_len: u32,
    /// Laenge des Treiber-Bildes in Byte (1..=[`MAX_BILD_BYTES`]).
    pub bild_len: u32,
    /// Laenge des JSON-Manifests in Byte (1..=[`MAX_MANIFEST_BYTES`]).
    pub manifest_len: u32,
}

fn rd_u32(d: &[u8], off: usize) -> Option<u32> {
    let b = d.get(off..off + 4)?;
    Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// Verzeichnis aus dem ersten Partitionssektor lesen. `part_sektoren` ist die Groesse der
/// gefundenen Partition — Laengen, die nicht hineinpassen, sind kein Bild, sondern ein
/// Widerspruch ([`LadeFehler::VerzeichnisKaputt`], nicht erst beim Lesen).
pub fn parse_verzeichnis(sektor: &[u8], part_sektoren: u64) -> Result<BildVerzeichnis, LadeFehler> {
    if sektor.len() < VERZEICHNIS_LEN {
        return Err(LadeFehler::VerzeichnisKaputt);
    }
    if sektor.get(..8) != Some(&VERZEICHNIS_MAGIC[..]) {
        return Err(LadeFehler::VerzeichnisKaputt);
    }
    let eintrag_len = rd_u32(sektor, 8).ok_or(LadeFehler::VerzeichnisKaputt)?;
    let bild_len = rd_u32(sektor, 12).ok_or(LadeFehler::VerzeichnisKaputt)?;
    let manifest_len = rd_u32(sektor, 16).ok_or(LadeFehler::VerzeichnisKaputt)?;
    let flags = rd_u32(sektor, 20).ok_or(LadeFehler::VerzeichnisKaputt)?;
    // Reservenull: Wer kuenftig etwas braucht, definiert einen Verzeichnis-Nachfolger — ein
    // gesetztes Bit heute ist ein fremdes Format, kein „egal".
    if flags != 0 {
        return Err(LadeFehler::VerzeichnisKaputt);
    }
    if eintrag_len == 0 || eintrag_len as usize > MAX_EINTRAG_BYTES {
        return Err(
            if eintrag_len == 0 { LadeFehler::VerzeichnisKaputt } else { LadeFehler::EintragZuGross },
        );
    }
    if bild_len == 0 || bild_len as usize > MAX_BILD_BYTES {
        return Err(if bild_len == 0 { LadeFehler::VerzeichnisKaputt } else { LadeFehler::BildZuGross });
    }
    if manifest_len == 0 || manifest_len as usize > MAX_MANIFEST_BYTES {
        return Err(
            if manifest_len == 0 { LadeFehler::VerzeichnisKaputt } else { LadeFehler::ManifestZuGross },
        );
    }
    let gesamt = (eintrag_len as u64)
        .checked_add(bild_len as u64)
        .and_then(|s| s.checked_add(manifest_len as u64))
        .ok_or(LadeFehler::VerzeichnisKaputt)?;
    let braucht = gesamt.div_ceil(SEKTOR as u64).checked_add(1).ok_or(LadeFehler::VerzeichnisKaputt)?;
    if braucht > part_sektoren {
        return Err(LadeFehler::VerzeichnisKaputt);
    }
    Ok(BildVerzeichnis { eintrag_len, bild_len, manifest_len })
}

// --- Gefundene Partition ---------------------------------------------------------------

/// Eine belegte LXPD-Partition: Lage, eindeutige GUID, Diagnose ueber die Fremden.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PartSichtung {
    /// Erste LBA der Partition.
    pub first_lba: u64,
    /// Groesse in Sektoren.
    pub sektoren: u64,
    /// Die EINDEUTIGE GUID dieses Eintrags (Bytes 16..32 — nicht die Typ-GUID): daran bindet
    /// ein `DiskGuid`-Eintrag (robust gegen Verschieben der Partition, s. `driver.rs`).
    pub unique_guid: [u8; 16],
    /// Wie viele belegte NICHT-LXPD-Partitionen uebersprungen wurden (Normalfall, kein Fehler).
    pub fremde: u32,
}

// --- Phasen ------------------------------------------------------------------------------

/// Die Phase des Dienstes — jeder Schritt verlangt die seines Vorgaengers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    /// Nichts gesucht, nichts gelesen.
    Leer = 0,
    /// Verzeichnis einer LXPD-Partition gelesen, Dokumente noch nicht da.
    Verzeichnis = 1,
    /// Eintrag + Bild + Manifest exakt gelesen, noch ungeprueft.
    Bild = 2,
    /// Geprueft — NUR hier darf angestossen werden.
    Geprueft = 3,
    /// Angestossen (neue PD-Id bekannt).
    Geladen = 4,
}

/// Gepruefte Zusammenfassung: was der Anstoss uebergeben bekommt (Laengen + Art + Hash —
// keine Bytes im Gatter, die Bytes kommen aus den Puffern). Ohne `Eq`: Die Fakten tragen
// `coverage_pct` als `f64` — Gleichheit ja, Aequivalenz nein.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct PruefSumme {
    /// Gepruefte Bildlaenge.
    pub bild_len: usize,
    /// Gepruefte Manifestlaenge.
    pub manifest_len: usize,
    /// Gepruefte Eintragslaenge.
    pub eintrag_len: usize,
    /// Container-Form.
    pub art: BildArt,
    /// Nachgerechnetes SHA-256 des Bildes (stimmt mit dem Eintrag ueberein — sonst waere hier
    /// nichts). Kryptografisch, nicht FNV: Die Platte darf der Angreifer beschreiben.
    pub bild_hash: [u8; 32],
    /// Manifest-Fakten (Version, Grants, IRQ — die Zusagen des Treibers).
    pub fakten: ManifestFacts,
}

// --- Namensbindung ------------------------------------------------------------------------
///
/// Eintrag und Manifest sind zwei gueltige, signierte Dokumente — aber erst der gleiche
/// `driver`-Name macht sie zum PAAR. Ohne diese Regel legte ein luegender Treiber Eintrag A
/// neben Manifest B (fremde Zusagen, eigene Bytes), und jede Einzelpruefung bestuende.
///
/// Fail-closed an jeder Stelle: Escapes im Namen, fehlender Schluessel, unlesbarer Wert —
/// alles heisst „kein Paar" ([`LadeFehler::TreiberMismatch`]), nie „egal". Treibernamen sind
/// schlichte ASCII-Worte (`e1000e`); wer Escapes braucht, bekommt einen neuen Abgleich, keine
/// Deutung.

fn is_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r')
}

/// Traegt `manifest_json` den Treibernamen `name` als Wert seines obersten `"driver"`-Feldes?
pub fn treibername_gleich(manifest_json: &[u8], name: &[u8]) -> bool {
    if name.is_empty() || name.len() > MAX_TREIBER_NAME {
        return false;
    }
    // Zustand: Tiefe (geschweifte Klammern), im String, Escape. Der Schluessel zaehlt nur auf
    // Tiefe 1 — geschachtelte `"driver"` (z. B. in Grants) meinen etwas anderes.
    let mut tiefe = 0i32;
    let mut im_string = false;
    let mut escape = false;
    let mut i = 0usize;
    while i < manifest_json.len() {
        let b = manifest_json[i];
        if im_string {
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == b'"' {
                im_string = false;
            }
            i += 1;
            continue;
        }
        match b {
            b'"' => {
                im_string = true;
                // Schluessel-Kandidat: `"driver"` exakt, danach (ausserhalb des Strings) nur
                // Leerzeichen, `:`, Leerzeichen, `"`.
                if tiefe == 1 && manifest_json.get(i..i + 8) == Some(b"\"driver\"") {
                    let mut j = i + 8;
                    while manifest_json.get(j).is_some_and(|&c| is_ws(c)) {
                        j += 1;
                    }
                    if manifest_json.get(j) != Some(&b':') {
                        i += 1;
                        continue;
                    }
                    j += 1;
                    while manifest_json.get(j).is_some_and(|&c| is_ws(c)) {
                        j += 1;
                    }
                    if manifest_json.get(j) != Some(&b'"') {
                        return false;
                    }
                    j += 1;
                    let start = j;
                    // Wert lesen: keine Escapes erlaubt (fail-closed — s. Doku oben).
                    while let Some(&c) = manifest_json.get(j) {
                        if c == b'"' {
                            break;
                        }
                        if c == b'\\' || c < 0x20 {
                            return false;
                        }
                        j += 1;
                    }
                    if manifest_json.get(j) != Some(&b'"') {
                        return false;
                    }
                    let wert = manifest_json.get(start..j).unwrap_or(b"");
                    return wert == name;
                }
            }
            b'{' => tiefe += 1,
            b'}' => tiefe -= 1,
            _ => {}
        }
        i += 1;
    }
    false
}

// --- Der Dienst --------------------------------------------------------------------------

/// Der Loader-Dienst: feste Puffer, explizite Phase, kein Schritt ausser der Reihe.
pub struct LadeDienst {
    phase: Phase,
    sichtung: Option<PartSichtung>,
    verzeichnis: Option<BildVerzeichnis>,
    eintrag: [u8; MAX_EINTRAG_BYTES],
    bild: [u8; MAX_BILD_BYTES],
    manifest: [u8; MAX_MANIFEST_BYTES],
    summe: Option<PruefSumme>,
    pd_id: Option<u32>,
    /// Haelt diese PD eine Loader-Cap (Manifest-Bit)? Client-seitiges Gatter VOR jedem Syscall.
    hat_loader_cap: bool,
}

impl LadeDienst {
    /// Aufsetzen. `hat_loader_cap` spiegelt den EIGENEN Manifest-Eintrag (`initial_caps` &
    /// [`CAP_LOADER_BIT`): Wer das Bit nicht hat, scheitert am Anstoss — benannt, bevor ein
    /// Syscall gebaut wird.
    pub fn neu(hat_loader_cap: bool) -> Self {
        LadeDienst {
            phase: Phase::Leer,
            sichtung: None,
            verzeichnis: None,
            eintrag: [0u8; MAX_EINTRAG_BYTES],
            bild: [0u8; MAX_BILD_BYTES],
            manifest: [0u8; MAX_MANIFEST_BYTES],
            summe: None,
            pd_id: None,
            hat_loader_cap,
        }
    }

    /// Aktuelle Phase (fuer AUSKUNFT).
    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// Auskunft: (Phasen-Kode, gepruefte oder gelesene Bildlaenge).
    pub fn auskunft(&self) -> (u64, u64) {
        let len = self
            .summe
            .map(|s| s.bild_len as u64)
            .or_else(|| self.verzeichnis.map(|v| u64::from(v.bild_len)))
            .unwrap_or(0);
        (self.phase as u64, len)
    }

    /// Zurueck auf Anfang (nach Fehlschlag oder geladenem Treiber).
    pub fn zuruecksetzen(&mut self) {
        self.phase = Phase::Leer;
        self.sichtung = None;
        self.verzeichnis = None;
        self.summe = None;
        self.pd_id = None;
    }

    fn info_holen<B: BlockQuelle>(&self, quelle: &mut B) -> Result<BlockInfo, LadeFehler> {
        let i = quelle.info().map_err(|_| LadeFehler::Geraet)?;
        if i.kapazitaet == 0 {
            return Err(LadeFehler::KeineKapazitaet);
        }
        Ok(i)
    }

    /// Exakt `ziel.len()` Bytes ab `lba` lesen, gestueckelt in [`MAX_SEKTOREN_JE_ANFRAGE`].
    /// Bereich VOR dem Geraet (wie der Treiber), Kuerze als Abbruch (wie das Protokoll).
    fn lese_exakt<B: BlockQuelle>(
        quelle: &mut B,
        kapazitaet: u64,
        lba: u64,
        ziel: &mut [u8],
    ) -> Result<(), LadeFehler> {
        let mut rest = ziel.len();
        let mut sektor = lba;
        let mut off = 0usize;
        while rest > 0 {
            let n_sek = ((rest.div_ceil(SEKTOR)) as u64).min(u64::from(MAX_SEKTOREN_JE_ANFRAGE));
            let n = u32::try_from(n_sek).map_err(|_| LadeFehler::Bereich)?;
            let ende = (n as u64).checked_add(sektor).ok_or(LadeFehler::Bereich)?;
            if sektor >= kapazitaet || ende > kapazitaet {
                return Err(LadeFehler::Bereich);
            }
            let bytes = (n as usize) * SEKTOR;
            // Der letzte Happen endet oft mitten im Sektor: vollen Sektor lesen, Rest verwerfen.
            // Der Verwerf-Puffer steht auf dem Stapel (transient), nie im Zielpuffer — was nicht
            // zum Dokument gehoert, landet nicht darin.
            if bytes <= rest {
                let teil = ziel.get_mut(off..off + bytes).ok_or(LadeFehler::Bereich)?;
                let got = quelle.lesen(sektor, n, teil).map_err(|e| match e {
                    BlockFehler::Bereich => LadeFehler::Bereich,
                    BlockFehler::Abgebrochen => LadeFehler::Abgebrochen,
                    BlockFehler::Geraet => LadeFehler::Geraet,
                })?;
                if got != n {
                    return Err(LadeFehler::Abgebrochen);
                }
                off += teil.len();
                rest -= teil.len();
            } else {
                let teil_len = rest;
                let mut rest_sec: [u8; STUECK_BYTES] = [0u8; STUECK_BYTES];
                let voll = quelle
                    .lesen(sektor, n, &mut rest_sec[..bytes])
                    .map_err(|e| match e {
                        BlockFehler::Bereich => LadeFehler::Bereich,
                        BlockFehler::Abgebrochen => LadeFehler::Abgebrochen,
                        BlockFehler::Geraet => LadeFehler::Geraet,
                    })?;
                if voll != n {
                    return Err(LadeFehler::Abgebrochen);
                }
                let teil = ziel.get_mut(off..off + teil_len).ok_or(LadeFehler::Bereich)?;
                teil.copy_from_slice(&rest_sec[..teil_len]);
                off += teil_len;
                rest -= teil_len;
            }
            sektor += u64::from(n);
        }
        let _ = off;
        Ok(())
    }

    /// Schritt 1: die `index`-te LXPD-Partition suchen (GPT selbst lesen — `OP_SCAN` kennt
    /// keine Typen, s. `protokoll`). Zwei Durchgaenge: erst CRC-Strom ueber die ganze
    /// Eintragsliste (wer je Stueck prueft, prueft ein Stueck), dann gezielte Eintragslesung.
    /// Die Liste ist typisch 16 KiB — der Lader liest sie beim Laden, nicht im heissen Pfad.
    pub fn suchen<B: BlockQuelle>(
        &mut self,
        quelle: &mut B,
        index: usize,
    ) -> Result<PartSichtung, LadeFehler> {
        use caprock_part::{entry_at, parse_header, verify_entries, Crc32, PartError};
        let info = self.info_holen(quelle)?;
        let kapazitaet = info.kapazitaet;
        // 1. Kopf von LBA 1.
        let mut kopf: [u8; SEKTOR] = [0u8; SEKTOR];
        Self::lese_exakt(quelle, kapazitaet, 1, &mut kopf)?;
        let h = parse_header(&kopf).map_err(|e| match e {
            PartError::BadSignature => LadeFehler::GptSignatur,
            PartError::BadRevision => LadeFehler::GptRevision,
            PartError::BadHeaderSize => LadeFehler::GptKopfGroesse,
            PartError::HeaderCrc => LadeFehler::GptKopfCrc,
            PartError::BadEntrySize => LadeFehler::GptEintragGroesse,
            PartError::BadEntryCount => LadeFehler::GptEintragZahl,
            PartError::EntriesCrc => LadeFehler::GptEintraegeCrc,
            PartError::TooShort => LadeFehler::GptKopfGroesse,
        })?;
        if h.entry_size as usize > SEKTOR {
            return Err(LadeFehler::GptEintragZuGross);
        }
        // 2. CRC-Durchgang ueber die GANZE Liste (Stueckelung wie der Treiber: 8 Sektoren).
        let gesamt = h.entries_bytes();
        let mut crc = Crc32::new();
        let mut gelesen = 0u64;
        let mut stueck: [u8; STUECK_BYTES] = [0u8; STUECK_BYTES];
        while gelesen < gesamt {
            let rest = gesamt - gelesen;
            let jetzt = rest.min(STUECK_BYTES as u64) as usize;
            let sektoren = (jetzt.div_ceil(SEKTOR) as u32).min(MAX_SEKTOREN_JE_ANFRAGE);
            let lba = h.entry_lba + gelesen / SEKTOR as u64;
            let got = quelle
                .lesen(lba, sektoren, &mut stueck[..sektoren as usize * SEKTOR])
                .map_err(|e| match e {
                    BlockFehler::Bereich => LadeFehler::Bereich,
                    BlockFehler::Abgebrochen => LadeFehler::Abgebrochen,
                    BlockFehler::Geraet => LadeFehler::Geraet,
                })?;
            if got != sektoren {
                return Err(LadeFehler::Abgebrochen);
            }
            // NUR `entries_bytes` in die Pruefsumme — nicht die Sektor-Auffuellung (derselbe
            // haeufigste Grund fuer eine „kaputte" Tabelle wie im Treiber).
            let nimm = jetzt.min(sektoren as usize * SEKTOR);
            crc.update(&stueck[..nimm]);
            gelesen += nimm as u64;
        }
        verify_entries(&h, &crc).map_err(|_| LadeFehler::GptEintraegeCrc)?;
        // 3. Such-Durchgang: Eintrag fuer Eintrag (jeder passt in einen Sektor — s. Schranke
        // oben; kein Eintrag wird je halb gelesen). Die EINDEUTIGE GUID (Bytes 16..32) liest
        // der Dienst roh aus dem Sektor — `caprock-part` fuehrt sie nicht (bewusst: sie ist
        // Auswahl, kein Parser-Bestandteil).
        let mut fremde = 0u32;
        let mut gefunden = 0usize;
        let mut i = 0u32;
        let mut sec: [u8; SEKTOR] = [0u8; SEKTOR];
        while i < h.num_entries {
            let byte_off = i as u64 * h.entry_size as u64;
            let lba = h.entry_lba + byte_off / SEKTOR as u64;
            let inner = (byte_off % SEKTOR as u64) as usize;
            Self::lese_exakt(quelle, kapazitaet, lba, &mut sec)?;
            let esz = h.entry_size as usize;
            let stueck_slice = sec.get(inner..inner + esz).ok_or(LadeFehler::Bereich)?;
            if let Some(p) = entry_at(&h, stueck_slice, i, i) {
                if p.is_used() {
                    if p.type_guid == LXPD_PART_GUID && p.within(&h) {
                        if gefunden == index {
                            let guid_bytes =
                                stueck_slice.get(16..32).ok_or(LadeFehler::Bereich)?;
                            let mut unique_guid = [0u8; 16];
                            unique_guid.copy_from_slice(guid_bytes);
                            let s = PartSichtung {
                                first_lba: p.first_lba,
                                sektoren: p.sectors(),
                                unique_guid,
                                fremde,
                            };
                            self.sichtung = Some(s);
                            self.phase = Phase::Verzeichnis;
                            return Ok(s);
                        }
                        gefunden += 1;
                    } else {
                        fremde += 1;
                    }
                }
            }
            i += 1;
        }
        Err(LadeFehler::KeineLxpdPartition)
    }

    /// Schritt 2: Verzeichnis + Eintrag + Bild + Manifest exakt einlesen (Stueckelung, exakte
    /// Laengen — niemals ungeprueft laden, und „kurz" ist kein „klein"). Reihenfolge auf
    /// Platte: Verzeichnis (Sektor 0), Eintrag, Bild, Manifest.
    pub fn bild_lesen<B: BlockQuelle>(&mut self, quelle: &mut B) -> Result<(), LadeFehler> {
        let s = self.sichtung.ok_or(LadeFehler::Ungeprueft)?;
        if self.phase != Phase::Verzeichnis {
            return Err(LadeFehler::Ungeprueft);
        }
        let info = self.info_holen(quelle)?;
        let mut sec: [u8; SEKTOR] = [0u8; SEKTOR];
        Self::lese_exakt(quelle, info.kapazitaet, s.first_lba, &mut sec)?;
        let v = parse_verzeichnis(&sec, s.sektoren)?;
        let elen = v.eintrag_len as usize;
        let blen = v.bild_len as usize;
        let mlen = v.manifest_len as usize;
        let mut lba = s.first_lba + 1;
        let eintrag = self.eintrag.get_mut(..elen).ok_or(LadeFehler::EintragZuGross)?;
        Self::lese_exakt(quelle, info.kapazitaet, lba, eintrag)?;
        lba += (elen.div_ceil(SEKTOR)) as u64;
        let bild = self.bild.get_mut(..blen).ok_or(LadeFehler::BildZuGross)?;
        Self::lese_exakt(quelle, info.kapazitaet, lba, bild)?;
        lba += (blen.div_ceil(SEKTOR)) as u64;
        let man = self.manifest.get_mut(..mlen).ok_or(LadeFehler::ManifestZuGross)?;
        Self::lese_exakt(quelle, info.kapazitaet, lba, man)?;
        self.verzeichnis = Some(v);
        self.phase = Phase::Bild;
        Ok(())
    }

    /// Die Herkunftsbindung: Der Eintrag muss DIESE Partition meinen — `DiskGuid` mit der
    /// eindeutigen GUID der Sichtung, `DiskRange` innerhalb der Partition, `Boot` nirgends
    /// (ein Boot-Modul liegt nicht auf Platte — wer es dort sucht, folgt einem falschen
    /// Dokument).
    fn herkunft_bindet(
        sichtung: &PartSichtung,
        quelle: &caprock_lxpd::driver::Source,
    ) -> Result<(), LadeFehler> {
        use caprock_lxpd::driver::Source;
        match *quelle {
            Source::DiskGuid { guid } => {
                if guid == sichtung.unique_guid {
                    Ok(())
                } else {
                    Err(LadeFehler::Container(LxpdError::BadManifest))
                }
            }
            Source::DiskRange { start_lba, sectors } => {
                let platten_ende = sichtung
                    .first_lba
                    .checked_add(sichtung.sektoren)
                    .ok_or(LadeFehler::Container(LxpdError::BadManifest))?;
                let bereich_ende = start_lba
                    .checked_add(sectors)
                    .ok_or(LadeFehler::Container(LxpdError::BadManifest))?;
                if start_lba >= sichtung.first_lba && bereich_ende <= platten_ende {
                    Ok(())
                } else {
                    Err(LadeFehler::Container(LxpdError::BadManifest))
                }
            }
            Source::Boot { .. } => Err(LadeFehler::Container(LxpdError::BadManifest)),
        }
    }

    /// Schritt 3: Eintrag (Fakten, Herkunft, Bild-Hash, Zeuge), Container/ELF, Manifest
    /// (Fakten, Signatur, Stubzahl, Name) — alles VOR dem Anstoss, nichts danach. Billiges vor
    /// Teurem, Form vor Krypto.
    pub fn pruefen(
        &mut self,
        manifest_schluessel: &[u8],
        pubkey: &[u8; 32],
    ) -> Result<PruefSumme, LadeFehler> {
        use caprock_lxpd::driver;
        if self.phase != Phase::Bild {
            return Err(LadeFehler::Ungeprueft);
        }
        let v = self.verzeichnis.ok_or(LadeFehler::Ungeprueft)?;
        let s = self.sichtung.ok_or(LadeFehler::Ungeprueft)?;
        let elen = v.eintrag_len as usize;
        let blen = v.bild_len as usize;
        let mlen = v.manifest_len as usize;
        let eintrag = self.eintrag.get(..elen).ok_or(LadeFehler::Ungeprueft)?;
        let bild = self.bild.get(..blen).ok_or(LadeFehler::Ungeprueft)?;
        let manifest = self.manifest.get(..mlen).ok_or(LadeFehler::Ungeprueft)?;
        // 1. Eintrag: Fakten ohne Krypto (Fassung, Namen, Herkunft, Hash-Form, Key-ID-Form).
        let e = driver::parse_entry(eintrag).map_err(LadeFehler::Container)?;
        // 2. Herkunft: Meint der Eintrag DIESE Partition?
        Self::herkunft_bindet(&s, &e.source)?;
        // 3. Manifest: Fakten + echte Signatur (niemals angenommen ohne Rechnung).
        let fakten = caprock_lxpd::manifest::manifest_facts(manifest)
            .map_err(LadeFehler::Container)?;
        caprock_lxpd::manifest::verify_signature(manifest, manifest_schluessel)
            .map_err(LadeFehler::Container)?;
        // 4. Container oder ELF — dieselben Funktionen wie der Boot-Pfad (kein zweiter Loader).
        // Die Stubzahl-Bindung (Manifest ↔ Container) steckt in `verify_manifest`.
        let art = if caprock_lxpd::is_lxpd(bild) {
            let img = caprock_lxpd::LxpdImage::parse(bild).map_err(LadeFehler::Container)?;
            let n = img.tramp_count() as u32;
            let f = img
                .verify_manifest(manifest, manifest_schluessel)
                .map_err(LadeFehler::Container)?;
            debug_assert!(f.trampoline_names == fakten.trampoline_names);
            BildArt::LxpdContainer { stuetz: n }
        } else if caprock_lxpd::is_elf(bild) {
            caprock_lxpd::elf_ready(bild).map_err(LadeFehler::Container)?;
            BildArt::Elf
        } else {
            return Err(LadeFehler::KeinBildformat);
        };
        // 5. Bildbindung SHA-256: Der Eintrag meint GENAU diese Bytes (ein gekipptes Bit ist
        // ein anderes Bild — s. `driver.rs`: FNV waere hier eine Attrappe).
        driver::verify_image(&e, bild)
            .map_err(|_| LadeFehler::BildHashWeichtAb)?;
        let hash = driver::sha256(bild);
        // 6. Eintrags-Zeuge: `key_id` + FNV ueber Roh-Pubkey + Kanonik gegen den Root-Schluessel.
        driver::verify_witness(eintrag, pubkey).map_err(LadeFehler::Container)?;
        // 7. Namensbindung: Eintrag und Manifest muessen dasselbe `driver` nennen — zwei
        // gueltige Dokumente sind noch kein Paar.
        if !treibername_gleich(manifest, e.driver) {
            return Err(LadeFehler::TreiberMismatch);
        }
        let summe =
            PruefSumme { bild_len: blen, manifest_len: mlen, eintrag_len: elen, art, bild_hash: hash, fakten };
        self.summe = Some(summe);
        self.phase = Phase::Geprueft;
        Ok(summe)
    }

    /// Geprüfte Bildbytes für die `LOAD_IMAGE`-Übergabe.
    ///
    /// Erst nach [`LadeDienst::pruefen`] vorhanden (`Some` genau in [`Phase::Geprueft`]),
    /// sonst `None` — die PD kopiert sie in ihre Memory-Cap (das Shared-Fenster des
    /// Blockdienstes) und stößt dann an. Der Kernel liest die Bytes aus der Cap, nicht
    /// aus der PD: Was hier zurückkommt, ist die Vorlage für die Übergabe, nicht die
    /// Übergabe selbst. Nach [`LadeDienst::zuruecksetzen`] ist wieder zu.
    pub fn geprueftes_bild(&self) -> Option<&[u8]> {
        let s = self.summe?;
        if self.phase != Phase::Geprueft {
            return None;
        }
        self.bild.get(..s.bild_len)
    }

    /// Schritt 4: der Lade-Anstoss — gegatet (Loader-Cap) und nur aus [`Phase::Geprueft`].
    /// `ctx` sind die Slots aus dem Manifest der PD (s. [`AnstossKontext`): Der Dienst erfindet
    /// keine Übergabe, er reicht sie nur durch. Gibt die neue PD-Id zurueck.
    pub fn anstossen<A: LadeAnstoss>(
        &mut self,
        anstoss: &mut A,
        ctx: &AnstossKontext,
    ) -> Result<u32, LadeFehler> {
        if self.phase != Phase::Geprueft {
            return Err(LadeFehler::Ungeprueft);
        }
        if !self.hat_loader_cap {
            return Err(LadeFehler::KeineLoaderCap);
        }
        let s = self.summe.ok_or(LadeFehler::Ungeprueft)?;
        let bild = self.bild.get(..s.bild_len).ok_or(LadeFehler::Ungeprueft)?;
        let manifest = self.manifest.get(..s.manifest_len).ok_or(LadeFehler::Ungeprueft)?;
        let eintrag = self.eintrag.get(..s.eintrag_len).ok_or(LadeFehler::Ungeprueft)?;
        let pd = anstoss.lade_bild(ctx, bild, manifest, eintrag, s.art)?;
        self.pd_id = Some(pd);
        self.phase = Phase::Geladen;
        Ok(pd)
    }

    /// Alles in einem: suchen → lesen → pruefen → anstossen (der Weg, den `bedienen` faehrt).
    pub fn laden<B, A>(
        &mut self,
        quelle: &mut B,
        anstoss: &mut A,
        ctx: &AnstossKontext,
        manifest_schluessel: &[u8],
        pubkey: &[u8; 32],
        index: usize,
    ) -> Result<u32, LadeFehler>
    where
        B: BlockQuelle,
        A: LadeAnstoss,
    {
        self.suchen(quelle, index)?;
        self.bild_lesen(quelle)?;
        self.pruefen(manifest_schluessel, pubkey)?;
        self.anstossen(anstoss, ctx)
    }
}

// --- Host-Tests ---------------------------------------------------------------------------
///
/// Fake-Blockdriver (fremde PD, Minimal-Protokoll) plus Fake-Anstoss (Syscall-Ersatz): defekte
/// Sektoren, falscher Hash, abgebrochene Reads, fremde Partitionen, gemischte Dokumente — jede
/// Lage mit dem NAMEN des Fehlers, der fallen muss.

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::vec::Vec;

    pub(crate) const PUBKEY: [u8; 32] = [0x42; 32];
    const ANDERER_KEY: [u8; 32] = [0x99; 32];

    // --- Hash-/Hex-Helfer (Spiegel von caprock-lxpd, ohne serde) ----------------------------

    fn fnv32(s: &str) -> u32 {
        let mut h: u32 = 0x811c_9dc5;
        for b in s.bytes() {
            h ^= u32::from(b);
            h = h.wrapping_mul(0x0100_0193);
        }
        h
    }

    fn fnv64(bytes: &[u8]) -> u64 {
        let mut h: u64 = 0xcbf29ce484222325;
        for &b in bytes {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x100000001b3);
        }
        h
    }

    fn hex_val(b: u8) -> u8 {
        match b {
            b'0'..=b'9' => b - b'0',
            b'a'..=b'f' => b - b'a' + 10,
            b'A'..=b'F' => b - b'A' + 10,
            _ => 0,
        }
    }

    fn hex(bytes: &[u8]) -> std::string::String {
        const H: &[u8; 16] = b"0123456789abcdef";
        let mut s = std::string::String::with_capacity(bytes.len() * 2);
        for &b in bytes {
            s.push(H[(b >> 4) as usize] as char);
            s.push(H[(b & 0x0f) as usize] as char);
        }
        s
    }

    /// Spiegel von `lx-bind::key_bytes` (Manifest-Schluessel: Hex-String → dekodiert, sonst roh).
    fn key_bytes_mirror(key: &str) -> Vec<u8> {
        let s = key.trim();
        let is_hex = s.len() % 2 == 0 && s.bytes().all(|c| c.is_ascii_hexdigit());
        if is_hex {
            let b = s.as_bytes();
            let mut out = Vec::with_capacity(b.len() / 2);
            let mut i = 0;
            while i < b.len() {
                out.push((hex_val(b[i]) << 4) | hex_val(b[i + 1]));
                i += 2;
            }
            out
        } else {
            key.as_bytes().to_vec()
        }
    }

    /// Manifest-Signatur ueber Key-Bytes + Kanonik (Spiegel von `manifest::verify_signature`).
    fn sign_manifest(canonical: &[u8], key: &str) -> std::string::String {
        let mut input = key_bytes_mirror(key);
        input.extend_from_slice(canonical);
        std::format!("{:016x}", fnv64(&input))
    }

    /// Eintrags-Zeuge ueber ROH-Pubkey + Kanonik (Spiegel von `driver::verify_witness` —
    /// bewusst roh, nicht sniffend, s. dort).
    fn sign_entry(canonical: &[u8], pubkey: &[u8; 32]) -> std::string::String {
        let mut input = pubkey.to_vec();
        input.extend_from_slice(canonical);
        std::format!("{:016x}", fnv64(&input))
    }

    fn key_id_of(pubkey: &[u8; 32]) -> std::string::String {
        use caprock_lxpd::driver::sha256;
        hex(&sha256(pubkey)[..16])
    }

    // --- Bild-Bau --------------------------------------------------------------------------

    fn stub(id: u32, family: &str, source: &str) -> [u8; 32] {
        let mut s = [0x90u8; 32];
        s[0..4].copy_from_slice(b"LXTR");
        s[4..8].copy_from_slice(&id.to_le_bytes());
        s[8..12].copy_from_slice(&fnv32(family).to_le_bytes());
        s[12..16].copy_from_slice(&fnv32(source).to_le_bytes());
        s
    }

    fn container(stubs: &[[u8; 32]]) -> Vec<u8> {
        let n = stubs.len() as u32;
        let mut v = Vec::new();
        v.extend_from_slice(b"LXPD");
        v.extend_from_slice(&n.to_le_bytes());
        v.extend_from_slice(&0u32.to_le_bytes());
        v.extend_from_slice(&(stubs.len() as u64 * 32).to_le_bytes());
        for s in stubs {
            v.extend_from_slice(&s[..]);
        }
        v.extend_from_slice(b"LXEND");
        v
    }

    const CANON: &[u8] = b"{\"api_version\":\"X1\",\"arch\":\"x86-64\",\"class_b_objects\":[],\"coverage_pct\":100.0,\"dma_window\":{\"base\":0,\"size\":65536,\"bits\":64},\"driver\":\"e1000e\",\"gpl_affected\":false,\"grants_bar\":[{\"index\":0,\"base\":4096,\"size\":8192,\"flags\":\"RW\"}],\"irq_vector\":7,\"schema_version\":1,\"trampolines\":[\"dma_map_single->caprock_dma_map\",\"spin_lock->caprock_spin_lock\"]}";

    fn manifest_json(sig: &str) -> Vec<u8> {
        std::format!(
            "{{\n  \"signature\": \"{sig}\",\n  \"driver\": \"e1000e\",\n  \"trampolines\": [\n    \"dma_map_single->caprock_dma_map\",\n    \"spin_lock->caprock_spin_lock\"\n  ],\n  \"schema_version\": 1,\n  \"api_version\": \"X1\",\n  \"arch\": \"x86-64\",\n  \"grants_bar\": [ {{ \"index\": 0, \"base\": 4096, \"size\": 8192, \"flags\": \"RW\" }} ],\n  \"dma_window\": {{ \"base\": 0, \"size\": 65536, \"bits\": 64 }},\n  \"irq_vector\": 7,\n  \"class_b_objects\": [],\n  \"gpl_affected\": false,\n  \"coverage_pct\": 100.0\n}}"
        )
        .into_bytes()
    }

    pub(crate) fn gutes_bild() -> Vec<u8> {
        container(&[stub(0, "dma", "dma_map_single"), stub(1, "spin", "spin_lock")])
    }

    pub(crate) fn gutes_manifest() -> Vec<u8> {
        manifest_json(&sign_manifest(CANON, "deadbeef"))
    }

    // --- Eintrag-Bau (Spiegel von driver.rs-Tests: Kanonik sortiert, pretty unsortiert) -----

    fn canon_entry(source: &str, img_hash: &str, kid: &str) -> Vec<u8> {
        std::format!(
            "{{\"api_version\":\"X1\",\"driver\":\"e1000e\",\"image_hash\":\"{img_hash}\",\"key_id\":\"{kid}\",\"schema_version\":1,\"source\":{source}}}"
        )
        .into_bytes()
    }

    fn pretty_entry(source: &str, img_hash: &str, kid: &str, sig: &str) -> Vec<u8> {
        std::format!(
            "{{\n  \"signature\" : \"{sig}\",\n  \"source\" : {source},\n  \"driver\" : \"e1000e\",\n  \"image_hash\" : \"{img_hash}\",\n  \"key_id\" : \"{kid}\",\n  \"schema_version\" : 1,\n  \"api_version\" : \"X1\"\n}}"
        )
        .into_bytes()
    }

    pub(crate) fn guid_hex(g: &[u8; 16]) -> std::string::String {
        hex(g)
    }

    pub(crate) fn guter_eintrag(bild: &[u8], guid: &[u8; 16], key: &[u8; 32]) -> Vec<u8> {
        use caprock_lxpd::driver::sha256;
        let src = std::format!("{{\"kind\":\"disk\",\"part_guid\":\"{}\"}}", guid_hex(guid));
        let ih = hex(&sha256(bild));
        let kid = key_id_of(key);
        let c = canon_entry(&src, &ih, &kid);
        pretty_entry(&src, &ih, &kid, &sign_entry(&c, key))
    }

    // --- GPT-Bau (gueltige Tabelle als Bezugspunkt — ohne Positivfall waere jede Abweisung
    // wertlos: ein Parser, der alles ablehnt, besteht jeden Negativtest) ---------------------

    fn crc32(bytes: &[u8]) -> u32 {
        let mut c = caprock_part::Crc32::new();
        c.update(bytes);
        c.finish()
    }

    fn reseal(h: &mut [u8]) {
        h[16..20].copy_from_slice(&0u32.to_le_bytes());
        let crc = crc32(&h[..92]);
        h[16..20].copy_from_slice(&crc.to_le_bytes());
    }

    /// Eintraege: (first, last, Typ-GUID, Unique-GUID).
    fn gpt_bauen(
        platte_sek: u64,
        parts: &[(u64, u64, [u8; 16], [u8; 16])],
    ) -> (Vec<u8>, Vec<u8>) {
        let n: u32 = 128;
        let esz: u32 = 128;
        let mut entries = std::vec![0u8; (n * esz) as usize];
        for (i, &(first, last, typ, uniq)) in parts.iter().enumerate() {
            let o = i * esz as usize;
            entries[o..o + 16].copy_from_slice(&typ);
            entries[o + 16..o + 32].copy_from_slice(&uniq);
            entries[o + 32..o + 40].copy_from_slice(&first.to_le_bytes());
            entries[o + 40..o + 48].copy_from_slice(&last.to_le_bytes());
        }
        let ecrc = crc32(&entries);
        let mut h = std::vec![0u8; SEKTOR];
        h[..8].copy_from_slice(&caprock_part::SIGNATURE);
        h[8..12].copy_from_slice(&caprock_part::REVISION.to_le_bytes());
        h[12..16].copy_from_slice(&92u32.to_le_bytes());
        h[24..32].copy_from_slice(&1u64.to_le_bytes());
        h[32..40].copy_from_slice(&(platte_sek - 1).to_le_bytes());
        h[40..48].copy_from_slice(&34u64.to_le_bytes());
        h[48..56].copy_from_slice(&(platte_sek - 34).to_le_bytes());
        h[72..80].copy_from_slice(&2u64.to_le_bytes());
        h[80..84].copy_from_slice(&n.to_le_bytes());
        h[84..88].copy_from_slice(&esz.to_le_bytes());
        h[88..92].copy_from_slice(&ecrc.to_le_bytes());
        reseal(&mut h);
        (h, entries)
    }

    fn verzeichnis_bauen(eintrag: &[u8], bild: &[u8], manifest: &[u8]) -> Vec<u8> {
        let mut v = std::vec![0u8; SEKTOR];
        v[..8].copy_from_slice(&VERZEICHNIS_MAGIC);
        v[8..12].copy_from_slice(&(eintrag.len() as u32).to_le_bytes());
        v[12..16].copy_from_slice(&(bild.len() as u32).to_le_bytes());
        v[16..20].copy_from_slice(&(manifest.len() as u32).to_le_bytes());
        v[20..24].copy_from_slice(&0u32.to_le_bytes());
        v
    }

    // --- Fake-Blockdriver --------------------------------------------------------------------

    /// Stand-in fuer die fremde Blockdriver-PD: Plattenabbild plus Fehler-Einspeisung.
    pub(crate) struct FakeBlock {
        platte: Vec<u8>,
        /// Ab dieser LBA schlaegt jeder Read fehl (defekte Sektoren).
        pub(crate) fehler_ab: Option<u64>,
        /// Gibt einen Sektor weniger zurueck als verlangt (abgebrochene Reads).
        pub(crate) kurz: bool,
        /// Ein Byte im Plattenabbild kippen (falscher Hash / kaputte Struktur).
        pub(crate) kipp: Option<(u64, usize)>,
        /// Gelesene Sektoren insgesamt (Beleg fuer Stueckelung).
        pub(crate) gelesen: u64,
    }

    impl FakeBlock {
        pub(crate) fn neu(platte_sek: u64) -> Self {
            FakeBlock {
                platte: std::vec![0u8; platte_sek as usize * SEKTOR],
                fehler_ab: None,
                kurz: false,
                kipp: None,
                gelesen: 0,
            }
        }

        pub(crate) fn schreiben(&mut self, lba: u64, bytes: &[u8]) {
            let o = lba as usize * SEKTOR;
            self.platte[o..o + bytes.len()].copy_from_slice(bytes);
        }

        pub(crate) fn byte_an(&self, lba: u64, inner: usize) -> u8 {
            let mut b = self.platte[lba as usize * SEKTOR + inner];
            if let Some((kl, ki)) = self.kipp {
                if kl == lba && ki == inner {
                    b ^= 0x01;
                }
            }
            b
        }
    }

    impl BlockQuelle for FakeBlock {
        fn info(&mut self) -> Result<BlockInfo, BlockFehler> {
            Ok(BlockInfo {
                kapazitaet: (self.platte.len() / SEKTOR) as u64,
                max_je_anfrage: MAX_SEKTOREN_JE_ANFRAGE,
            })
        }

        fn lesen(&mut self, lba: u64, sektoren: u32, puffer: &mut [u8]) -> Result<u32, BlockFehler> {
            let kap = (self.platte.len() / SEKTOR) as u64;
            if lba >= kap || lba + u64::from(sektoren) > kap {
                return Err(BlockFehler::Bereich);
            }
            if puffer.len() < sektoren as usize * SEKTOR {
                return Err(BlockFehler::Bereich);
            }
            if self.fehler_ab.is_some_and(|ab| lba + u64::from(sektoren) > ab) {
                return Err(BlockFehler::Geraet);
            }
            let n = if self.kurz && sektoren > 1 { sektoren - 1 } else { sektoren };
            for s in 0..n {
                for i in 0..SEKTOR {
                    puffer[s as usize * SEKTOR + i] = self.byte_an(lba + u64::from(s), i);
                }
            }
            self.gelesen += u64::from(n);
            Ok(n)
        }
    }

    /// Stand-in fuer den Syscall: nimmt NUR gepruefte Bytes (der Dienst gattert, der Fake
    /// zaehlt — was hier ankommt, kam durch `pruefen`) und protokolliert den Slot-Kontext, den
    /// die PD aus ihrem Manifest mitgab (s. [`AnstossKontext`]).
    pub(crate) struct FakeAnstoss {
        pub(crate) aufrufe: u32,
        pub(crate) letzte_art: Option<BildArt>,
        pub(crate) letzte_bildlen: usize,
        pub(crate) letzte_hash: [u8; 32],
        pub(crate) letzter_kontext: Option<AnstossKontext>,
    }

    impl FakeAnstoss {
        pub(crate) fn neu() -> Self {
            FakeAnstoss {
                aufrufe: 0,
                letzte_art: None,
                letzte_bildlen: 0,
                letzte_hash: [0u8; 32],
                letzter_kontext: None,
            }
        }
    }

    impl LadeAnstoss for FakeAnstoss {
        fn lade_bild(
            &mut self,
            ctx: &AnstossKontext,
            bild: &[u8],
            manifest: &[u8],
            eintrag: &[u8],
            art: BildArt,
        ) -> Result<u32, LadeFehler> {
            use caprock_lxpd::driver::sha256;
            if bild.is_empty() || manifest.is_empty() || eintrag.is_empty() {
                return Err(LadeFehler::AnstossAbgelehnt);
            }
            self.aufrufe += 1;
            self.letzte_art = Some(art);
            self.letzte_bildlen = bild.len();
            self.letzte_hash = sha256(bild);
            self.letzter_kontext = Some(*ctx);
            Ok(42)
        }
    }

    pub(crate) const LXPD_UNIQUE: [u8; 16] =
        [0x01, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF];

    /// Volle Platte: GPT (Eintrag 0..n = fremd/leer, dann LXPD bei `lxpd_lba` mit Unique-GUID)
    /// + Verzeichnis + Eintrag + Bild + Manifest. Exakte Lagen: GPT-Kopf LBA 1, Eintraege ab
    /// LBA 2 (32 Sektoren), Verzeichnis auf Sektor 0 der Partition, dann Eintrag/Bild/Manifest.
    pub(crate) fn platte_bauen(
        eintrag: &[u8],
        bild: &[u8],
        manifest: &[u8],
        fremde_vorher: &[(u64, u64)],
    ) -> (FakeBlock, u64) {
        let e_sek = (eintrag.len().div_ceil(SEKTOR)) as u64;
        let b_sek = (bild.len().div_ceil(SEKTOR)) as u64;
        let m_sek = (manifest.len().div_ceil(SEKTOR)) as u64;
        let lxpd_lba = 100u64;
        let lxpd_sek = 1 + e_sek + b_sek + m_sek + 4; // Luft am Ende
        let platte_sek = lxpd_lba + lxpd_sek + 40; // + Sicherungskopf-Luft
        let mut f = FakeBlock::neu(platte_sek);
        let mut parts: Vec<(u64, u64, [u8; 16], [u8; 16])> = Vec::new();
        for (k, &(a, b)) in fremde_vorher.iter().enumerate() {
            let mut u = [0u8; 16];
            u[0] = 0xF0 + k as u8;
            parts.push((a, b, [0x0Fu8; 16], u));
        }
        parts.push((lxpd_lba, lxpd_lba + lxpd_sek - 1, LXPD_PART_GUID, LXPD_UNIQUE));
        let (kopf, eintraege) = gpt_bauen(platte_sek, &parts);
        f.schreiben(1, &kopf);
        let mut off = 2u64;
        for st in eintraege.chunks(STUECK_BYTES) {
            f.schreiben(off, st);
            off += (st.len() / SEKTOR) as u64;
        }
        f.schreiben(lxpd_lba, &verzeichnis_bauen(eintrag, bild, manifest));
        let mut strom: Vec<u8> = Vec::new();
        for dok in [eintrag, bild, manifest] {
            strom.extend_from_slice(dok);
            while strom.len() % SEKTOR != 0 {
                strom.push(0);
            }
        }
        let mut s = lxpd_lba + 1;
        for st in strom.chunks(SEKTOR) {
            f.schreiben(s, st);
            s += 1;
        }
        (f, lxpd_lba)
    }

    pub(crate) fn gutes_paar() -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let bild = gutes_bild();
        let manifest = gutes_manifest();
        let eintrag = guter_eintrag(&bild, &LXPD_UNIQUE, &PUBKEY);
        (eintrag, bild, manifest)
    }

    fn dienst_mit_cap() -> (LadeDienst, FakeAnstoss) {
        (LadeDienst::neu(true), FakeAnstoss::neu())
    }

    /// Slot-Kontext, wie ihn eine PD aus IHREM Manifest mitgäbe: Loader-Cap in Slot 0,
    /// geprüftes Bild in Slot 1, Programm-ID 7, keine Delegation, Vorgabe-Extras. Absichtlich
    /// krumme Werte (3/5/9), wo der Durchlauf geprüft wird — 0/1/7 bewiese nichts, was 0/0/0
    /// nicht auch täte.
    fn test_kontext() -> AnstossKontext {
        AnstossKontext {
            loader_slot: 0,
            bild_slot: 1,
            programm_id: 7,
            deleg_liste: 0,
            deleg_anzahl: 0,
            extras: 0,
        }
    }

    // --- Tests -----------------------------------------------------------------------------------

    #[test]
    fn rundweg_suchen_lesen_pruefen_anstossen() {
        // Positivpfad ueber alle vier Schritte: GPT-Suche findet die LXPD-Partition (Index 0,
        // Unique-GUID gebunden), Verzeichnis + Eintrag + Bild + Manifest werden exakt gelesen,
        // Eintrag + SHA-256 + Container + Signatur + Zeuge + Name bestehen, der Anstoss traegt
        // Art, Laenge und Hash. Exakte Ausgaben: PD-Id 42, ein Anstoss, 2 Stuetzstellen.
        use caprock_lxpd::driver::sha256;
        let (eintrag, bild, manifest) = gutes_paar();
        let (mut q, _) = platte_bauen(&eintrag, &bild, &manifest, &[(34, 60)]);
        let (mut d, mut a) = dienst_mit_cap();
        let s = d.suchen(&mut q, 0).expect("LXPD-Partition");
        assert_eq!(s.first_lba, 100);
        assert_eq!(s.unique_guid, LXPD_UNIQUE);
        assert_eq!(s.fremde, 1, "eine fremde Partition uebersprungen");
        assert_eq!(d.phase(), Phase::Verzeichnis);
        d.bild_lesen(&mut q).expect("Dokumente lesen");
        assert_eq!(d.phase(), Phase::Bild);
        let summe = d.pruefen(b"deadbeef", &PUBKEY).expect("pruefen");
        assert_eq!(summe.art, BildArt::LxpdContainer { stuetz: 2 });
        assert_eq!(summe.bild_len, bild.len());
        assert_eq!(summe.bild_hash, sha256(&bild));
        assert_eq!(summe.fakten.trampoline_names, 2);
        assert_eq!(d.phase(), Phase::Geprueft);
        let pd = d.anstossen(&mut a, &test_kontext()).expect("anstossen");
        assert_eq!(pd, 42);
        assert_eq!(a.aufrufe, 1);
        assert_eq!(a.letzte_art, Some(BildArt::LxpdContainer { stuetz: 2 }));
        assert_eq!(a.letzte_bildlen, bild.len());
        assert_eq!(a.letzte_hash, sha256(&bild));
        assert_eq!(a.letzter_kontext, Some(test_kontext()), "Kontext durchgereicht");
        assert_eq!(d.phase(), Phase::Geladen);
        // Gestueckelt: Kopf (1) + CRC-Durchgang (32) + Suche (2: ein Fremder, dann Treffer) +
        // Verzeichnis/Eintrag/Bild/Manifest (je 1–2) — kein einzelner Read, nirgends.
        assert!(q.gelesen >= 38, "gestueckelt gelesen, nicht einmal: {}", q.gelesen);
    }

    #[test]
    fn bereichs_eintrag_als_herkunft() {
        // Die Bereichs-Form (`start_lba`/`sectors` innerhalb der Partition) ist gleichwertig —
        // robust gegen eine kaputte/fehlende Tabelle, s. driver.rs. Ausserhalb: BadManifest.
        let bild = gutes_bild();
        let manifest = gutes_manifest();
        // Eintrag auf Bereichs-Form umschreiben: gleichen Hash, gleiche key_id, neu signieren.
        // `start_lba` = 100: die LXPD-Partition beginnt dort (s. `platte_bauen`).
        use caprock_lxpd::driver::sha256;
        let src = "{\"kind\":\"disk\",\"sectors\":4,\"start_lba\":100}";
        let ih = hex(&sha256(&bild));
        let kid = key_id_of(&PUBKEY);
        let c = canon_entry(&src, &ih, &kid);
        let bereich = pretty_entry(&src, &ih, &kid, &sign_entry(&c, &PUBKEY));
        let (mut q, _) = platte_bauen(&bereich, &bild, &manifest, &[]);
        let (mut d, mut a) = dienst_mit_cap();
        d.suchen(&mut q, 0).expect("Suche ok");
        d.bild_lesen(&mut q).expect("Lesen ok");
        d.pruefen(b"deadbeef", &PUBKEY).expect("Bereichs-Herkunft ok");
        d.anstossen(&mut a, &test_kontext()).expect("Anstoss ok");
        assert_eq!(a.aufrufe, 1);
        // Bereich ausserhalb der Partition: benannt abgewiesen, kein Anstoss.
        let src2 = "{\"kind\":\"disk\",\"sectors\":10,\"start_lba\":9000}";
        let c2 = canon_entry(src2, &ih, &kid);
        let ausserhalb = pretty_entry(src2, &ih, &kid, &sign_entry(&c2, &PUBKEY));
        let (mut q2, _) = platte_bauen(&ausserhalb, &bild, &manifest, &[]);
        let (mut d2, mut a2) = dienst_mit_cap();
        d2.suchen(&mut q2, 0).expect("Suche ok");
        d2.bild_lesen(&mut q2).expect("Lesen ok");
        assert_eq!(
            d2.pruefen(b"deadbeef", &PUBKEY).err(),
            Some(LadeFehler::Container(LxpdError::BadManifest))
        );
        assert_eq!(d2.anstossen(&mut a2, &test_kontext()).err(), Some(LadeFehler::Ungeprueft));
    }

    #[test]
    fn defekter_sektor_haelt_jeden_anstoss_auf() {
        // Der Blockdriver meldet einen Geraetefehler mitten im Bild: Der Schritt scheitert mit
        // GERAET, die Phase bleibt VERZEICHNIS, und `anstossen` scheitert mit UNGEPRUEFT — kein
        // Wort, kein Anstoss, kein halb gelesenes Bild.
        let (eintrag, bild, manifest) = gutes_paar();
        let (mut q, lxpd) = platte_bauen(&eintrag, &bild, &manifest, &[]);
        // Layout: Verzeichnis(lxpd) Eintrag(lxpd+1) Bild(lxpd+2) Manifest — Bildsektor defekt.
        q.fehler_ab = Some(lxpd + 2);
        let (mut d, mut a) = dienst_mit_cap();
        d.suchen(&mut q, 0).expect("Suche ok");
        assert_eq!(d.bild_lesen(&mut q).err(), Some(LadeFehler::Geraet));
        assert_eq!(d.phase(), Phase::Verzeichnis);
        assert_eq!(d.pruefen(b"deadbeef", &PUBKEY).err(), Some(LadeFehler::Ungeprueft));
        assert_eq!(d.anstossen(&mut a, &test_kontext()).err(), Some(LadeFehler::Ungeprueft));
        assert_eq!(a.aufrufe, 0, "kein Anstoss ohne Bild");
    }

    #[test]
    fn falscher_hash_weist_ab_als_anderes_bild() {
        // Ein gekipptes Bit im Bild (Family-Hash von Stub 0 — daran bindet weder Parse noch
        // Manifest, also ueberlebt der Container): vollstaendig gelesen, aber SHA-256 weicht
        // ab — der Eintrag meint ein ANDERES Bild (BadManifest-Familie als `BildHashWeichtAb`,
        // nicht „kaputt"). Gegenprobe nebenbei: Ein Bit in der Stub-ID faellt FRUEHER (Parse),
        // nicht hier — jede Schicht faengt ihren eigenen Fehler.
        let (eintrag, bild, manifest) = gutes_paar();
        let (mut q, lxpd) = platte_bauen(&eintrag, &bild, &manifest, &[]);
        q.kipp = Some((lxpd + 2, 30)); // Bild-Byte 30: Family-Hash, vom Parse ungeaehlt
        let (mut d, mut a) = dienst_mit_cap();
        d.suchen(&mut q, 0).expect("Suche ok");
        d.bild_lesen(&mut q).expect("Lesen ok — der Fehler liegt tiefer");
        assert_eq!(d.pruefen(b"deadbeef", &PUBKEY).err(), Some(LadeFehler::BildHashWeichtAb));
        assert_eq!(d.phase(), Phase::Bild, "zurueck auf BILD, nicht vorwaerts");
        assert_eq!(d.anstossen(&mut a, &test_kontext()).err(), Some(LadeFehler::Ungeprueft));
        assert_eq!(a.aufrufe, 0);
    }

    #[test]
    fn abgebrochene_reads_sind_kein_kleines_bild() {
        // Der Treiber liefert kuerzer als angefragt: ABGEBROCHEN statt „kleines Bild" — ein
        // abgebrochener Read duerfte sonst als kuerzeres (gueltiges) Dokument durchgehen.
        let (eintrag, bild, manifest) = gutes_paar();
        let (mut q, _) = platte_bauen(&eintrag, &bild, &manifest, &[]);
        q.kurz = true;
        let (mut d, mut a) = dienst_mit_cap();
        // Die GPT-Liste (32 Sektoren) bricht schon beim CRC-Durchgang ab.
        assert_eq!(d.suchen(&mut q, 0).err(), Some(LadeFehler::Abgebrochen));
        assert_eq!(d.phase(), Phase::Leer);
        assert_eq!(d.anstossen(&mut a, &test_kontext()).err(), Some(LadeFehler::Ungeprueft));
        assert_eq!(a.aufrufe, 0);
    }

    #[test]
    fn fremde_partition_ist_kein_lxpd() {
        // Nur fremde Typen auf der Platte: KEINE_LXPD_PARTITION — und Index 5 auf einer Platte
        // mit EINER LXPD-Partition derselbe Fehler (kein Wrap, kein Raten).
        let (eintrag, bild, manifest) = gutes_paar();
        let platte_sek = 300u64;
        let mut q = FakeBlock::neu(platte_sek);
        let (kopf, eintraege) =
            gpt_bauen(platte_sek, &[(34, 100, [0x0Fu8; 16], [0xF0u8; 16])]);
        q.schreiben(1, &kopf);
        let mut off = 2u64;
        for st in eintraege.chunks(STUECK_BYTES) {
            q.schreiben(off, st);
            off += (st.len() / SEKTOR) as u64;
        }
        let (mut d, _) = dienst_mit_cap();
        assert_eq!(d.suchen(&mut q, 0).err(), Some(LadeFehler::KeineLxpdPartition));
        let (mut q2, _) = platte_bauen(&eintrag, &bild, &manifest, &[]);
        let (mut d2, _) = dienst_mit_cap();
        assert_eq!(d2.suchen(&mut q2, 5).err(), Some(LadeFehler::KeineLxpdPartition));
        // Index 1 waehlt die ZWEITE LXPD-Partition (kein Raten, kein Wrap auf 0).
        let bild_sek = (bild.len().div_ceil(SEKTOR)) as u64;
        let man_sek = (manifest.len().div_ceil(SEKTOR)) as u64;
        let ent_sek = (eintrag.len().div_ceil(SEKTOR)) as u64;
        let lxpd2 = 400u64;
        let platte_sek2 = lxpd2 + 1 + ent_sek + bild_sek + man_sek + 40;
        let mut q3 = FakeBlock::neu(platte_sek2);
        let guid2: [u8; 16] = [0x02; 16];
        let parts: Vec<(u64, u64, [u8; 16], [u8; 16])> = std::vec![
            (100, 100 + 1 + ent_sek + bild_sek + man_sek + 4, LXPD_PART_GUID, LXPD_UNIQUE),
            (lxpd2, lxpd2 + 1 + ent_sek + bild_sek + man_sek + 4, LXPD_PART_GUID, guid2),
        ];
        let (k3, e3) = gpt_bauen(platte_sek2, &parts);
        q3.schreiben(1, &k3);
        let mut o3 = 2u64;
        for st in e3.chunks(STUECK_BYTES) {
            q3.schreiben(o3, st);
            o3 += (st.len() / SEKTOR) as u64;
        }
        for &(base, guid) in &[(100u64, LXPD_UNIQUE), (lxpd2, guid2)] {
            let ent = guter_eintrag(&bild, &guid, &PUBKEY);
            q3.schreiben(base, &verzeichnis_bauen(&ent, &bild, &manifest));
            let mut strom: Vec<u8> = Vec::new();
            for dok in [&ent, &bild, &manifest] {
                strom.extend_from_slice(dok);
                while strom.len() % SEKTOR != 0 {
                    strom.push(0);
                }
            }
            let mut s = base + 1;
            for st in strom.chunks(SEKTOR) {
                q3.schreiben(s, st);
                s += 1;
            }
        }
        let (mut d3, _) = dienst_mit_cap();
        let s = d3.suchen(&mut q3, 1).expect("zweite LXPD-Partition");
        assert_eq!(s.first_lba, lxpd2);
        assert_eq!(s.unique_guid, guid2);
    }

    #[test]
    fn kaputter_gpt_kopf_faellt_vor_der_suche() {
        // Gekippte Kopf-CRC: GptKopfCrc — unterschieden von „keine GPT" (GptSignatur): Die erste
        // ist Datenverlust, die zweite eine unformatierte Platte, und der Aufrufer handelt
        // entgegengesetzt.
        let (eintrag, bild, manifest) = gutes_paar();
        let (mut q, _) = platte_bauen(&eintrag, &bild, &manifest, &[]);
        q.kipp = Some((1, 40)); // ein Bit im GPT-Kopf (LBA 1)
        let (mut d, mut a) = dienst_mit_cap();
        assert_eq!(d.suchen(&mut q, 0).err(), Some(LadeFehler::GptKopfCrc));
        assert_eq!(d.anstossen(&mut a, &test_kontext()).err(), Some(LadeFehler::Ungeprueft));
        let (mut q2, _) = platte_bauen(&eintrag, &bild, &manifest, &[]);
        q2.kipp = Some((1, 0)); // ein Bit in „EFI PART"
        let (mut d2, _) = dienst_mit_cap();
        assert!(d2.suchen(&mut q2, 0).is_err(), "kein LXPD-Erfolg mit zerstoerter Signatur");
    }

    #[test]
    fn falsche_signatur_weist_ab_mit_rechnung() {
        // Echte Rechnung, kein angenommener Pass: falscher Manifest-Schluessel UND falscher
        // Root-Pubkey werden je benannt abgewiesen — obwohl Hash und Container-Form stimmen.
        let (eintrag, bild, manifest) = gutes_paar();
        let (mut q, _) = platte_bauen(&eintrag, &bild, &manifest, &[]);
        let (mut d, mut a) = dienst_mit_cap();
        d.suchen(&mut q, 0).expect("Suche ok");
        d.bild_lesen(&mut q).expect("Lesen ok");
        assert_eq!(
            d.pruefen(b"cafebabe", &PUBKEY).err(),
            Some(LadeFehler::Container(LxpdError::BadSignature)),
            "falscher Manifest-Schluessel"
        );
        let (mut q2, _) = platte_bauen(&eintrag, &bild, &manifest, &[]);
        let (mut d2, mut a2) = dienst_mit_cap();
        d2.suchen(&mut q2, 0).expect("Suche ok");
        d2.bild_lesen(&mut q2).expect("Lesen ok");
        assert_eq!(
            d2.pruefen(b"deadbeef", &ANDERER_KEY).err(),
            Some(LadeFehler::Container(LxpdError::BadSignature)),
            "falscher Root-Pubkey (schon die key_id passt nicht)"
        );
        assert_eq!(d.anstossen(&mut a, &test_kontext()).err(), Some(LadeFehler::Ungeprueft));
        assert_eq!(d2.anstossen(&mut a2, &test_kontext()).err(), Some(LadeFehler::Ungeprueft));
        assert_eq!(a.aufrufe + a2.aufrufe, 0);
    }

    #[test]
    fn stubzahl_gegen_manifest_gebunden() {
        // Zwei Stubs im Container, aber ein Manifest mit NUR einem Stubnamen (echt signiert
        // fuer SEINE Kanonik): BadManifest — das Manifest gehoert zu einem anderen Bild.
        let bild = gutes_bild(); // 2 Stubs
        let manifest = gutes_manifest();
        let canon_one: &[u8] = b"{\"coverage_pct\":100.0,\"dma_window\":{\"size\":1},\"driver\":\"e1000e\",\"grants_bar\":[{\"size\":8}],\"irq_vector\":7,\"schema_version\":1,\"trampolines\":[\"a->b\"]}";
        let klein: Vec<u8> = std::format!(
            "{{\"schema_version\":1,\"driver\":\"e1000e\",\"coverage_pct\":100.0,\"grants_bar\":[{{\"size\":8}}],\"dma_window\":{{\"size\":1}},\"irq_vector\":7,\"trampolines\":[\"a->b\"],\"signature\":\"{}\"}}",
            sign_manifest(canon_one, "deadbeef")
        )
        .into_bytes();
        let eintrag = guter_eintrag(&bild, &LXPD_UNIQUE, &PUBKEY);
        let (mut q, _) = platte_bauen(&eintrag, &bild, &klein, &[]);
        let (mut d, mut a) = dienst_mit_cap();
        d.suchen(&mut q, 0).expect("Suche ok");
        d.bild_lesen(&mut q).expect("Lesen ok");
        assert_eq!(
            d.pruefen(b"deadbeef", &PUBKEY).err(),
            Some(LadeFehler::Container(LxpdError::BadManifest))
        );
        assert_eq!(d.anstossen(&mut a, &test_kontext()).err(), Some(LadeFehler::Ungeprueft));
        assert_eq!(a.aufrufe, 0);
        let _ = manifest;
    }

    #[test]
    fn gemischtes_paar_ist_kein_paar() {
        // Eintrag A (e1000e, gueltig, signiert) neben Manifest B (anderer Treiber, gueltig,
        // signiert, GLEICHE Stubzahl): Jede Einzelpruefung bestuende — die Namensbindung
        // faengt das Paar: TREIBER_MISMATCH. Ein Treiber mit fremden Zusagen laeuft nicht.
        let bild = gutes_bild();
        let canon_b: &[u8] = b"{\"api_version\":\"X1\",\"arch\":\"x86-64\",\"class_b_objects\":[],\"coverage_pct\":100.0,\"dma_window\":{\"base\":0,\"size\":65536,\"bits\":64},\"driver\":\"anderer\",\"gpl_affected\":false,\"grants_bar\":[{\"index\":0,\"base\":4096,\"size\":8192,\"flags\":\"RW\"}],\"irq_vector\":7,\"schema_version\":1,\"trampolines\":[\"dma_map_single->caprock_dma_map\",\"spin_lock->caprock_spin_lock\"]}";
        let manifest_b: Vec<u8> = std::format!(
            "{{\n  \"signature\": \"{}\",\n  \"driver\": \"anderer\",\n  \"trampolines\": [\n    \"dma_map_single->caprock_dma_map\",\n    \"spin_lock->caprock_spin_lock\"\n  ],\n  \"schema_version\": 1,\n  \"api_version\": \"X1\",\n  \"arch\": \"x86-64\",\n  \"grants_bar\": [ {{ \"index\": 0, \"base\": 4096, \"size\": 8192, \"flags\": \"RW\" }} ],\n  \"dma_window\": {{ \"base\": 0, \"size\": 65536, \"bits\": 64 }},\n  \"irq_vector\": 7,\n  \"class_b_objects\": [],\n  \"gpl_affected\": false,\n  \"coverage_pct\": 100.0\n}}",
            sign_manifest(canon_b, "deadbeef")
        )
        .into_bytes();
        // Gegenprobe: Manifest B ist FUER SICH gueltig + signiert (sonst belegte der Test
        // etwas anderes, als er behauptet).
        caprock_lxpd::manifest::manifest_facts(&manifest_b).expect("B gueltig");
        caprock_lxpd::manifest::verify_signature(&manifest_b, b"deadbeef").expect("B signiert");
        let eintrag = guter_eintrag(&bild, &LXPD_UNIQUE, &PUBKEY);
        let (mut q, _) = platte_bauen(&eintrag, &bild, &manifest_b, &[]);
        let (mut d, mut a) = dienst_mit_cap();
        d.suchen(&mut q, 0).expect("Suche ok");
        d.bild_lesen(&mut q).expect("Lesen ok");
        assert_eq!(d.pruefen(b"deadbeef", &PUBKEY).err(), Some(LadeFehler::TreiberMismatch));
        assert_eq!(d.anstossen(&mut a, &test_kontext()).err(), Some(LadeFehler::Ungeprueft));
        assert_eq!(a.aufrufe, 0);
        // Namensregel selbst, direkt: Tiefe, Escapes, Fehlen.
        assert!(treibername_gleich(b"{\"driver\":\"e1000e\"}", b"e1000e"));
        assert!(!treibername_gleich(b"{\"driver\":\"anderer\"}", b"e1000e"));
        assert!(!treibername_gleich(b"{\"grants\":[{\"driver\":\"e1000e\"}]}", b"e1000e"));
        assert!(!treibername_gleich(b"{\"driver\":\"e1000\\\"e\"}", b"e1000e"));
        assert!(!treibername_gleich(b"{\"treiber\":\"e1000e\"}", b"e1000e"));
        assert!(!treibername_gleich(b"", b"e1000e"));
        assert!(!treibername_gleich(b"{\"driver\":\"e1000e\"}", b""));
    }

    #[test]
    fn weder_lxpd_noch_elf_ist_kein_parse_versuch() {
        // Zufallsbytes mit gueltigem Eintrag (Hash stimmt!) + Manifest: KeinBildformat — kein
        // Parse-Versuch auf etwas, das kein Format traegt.
        let bild = std::vec![0xA5u8; 96];
        let manifest = gutes_manifest();
        let eintrag = guter_eintrag(&bild, &LXPD_UNIQUE, &PUBKEY);
        let (mut q, _) = platte_bauen(&eintrag, &bild, &manifest, &[]);
        let (mut d, mut a) = dienst_mit_cap();
        d.suchen(&mut q, 0).expect("Suche ok");
        d.bild_lesen(&mut q).expect("Lesen ok");
        assert_eq!(d.pruefen(b"deadbeef", &PUBKEY).err(), Some(LadeFehler::KeinBildformat));
        assert_eq!(d.anstossen(&mut a, &test_kontext()).err(), Some(LadeFehler::Ungeprueft));
        assert_eq!(a.aufrufe, 0);
    }

    #[test]
    fn bild_zu_gross_wird_benannt_nicht_abgeschnitten() {
        // Das Verzeichnis kuendigt mehr als MAX_BILD_BYTES an: BILD_ZU_GROSS — gelesen wird
        // nichts ausser dem Verzeichnis (kein Abschneiden, kein Raten).
        let (eintrag, bild, manifest) = gutes_paar();
        let (mut q, lxpd) = platte_bauen(&eintrag, &bild, &manifest, &[]);
        let mut sec = std::vec![0u8; SEKTOR];
        for i in 0..SEKTOR {
            sec[i] = q.byte_an(lxpd, i);
        }
        sec[12..16].copy_from_slice(&(MAX_BILD_BYTES as u32 + 512).to_le_bytes());
        q.schreiben(lxpd, &sec);
        let (mut d, mut a) = dienst_mit_cap();
        d.suchen(&mut q, 0).expect("Suche ok");
        assert_eq!(d.bild_lesen(&mut q).err(), Some(LadeFehler::BildZuGross));
        assert_eq!(d.phase(), Phase::Verzeichnis);
        assert_eq!(d.anstossen(&mut a, &test_kontext()).err(), Some(LadeFehler::Ungeprueft));
        assert_eq!(a.aufrufe, 0);
    }

    #[test]
    fn fremde_guid_ist_keine_herkunft() {
        // Eintrag nennt eine ANDERE Unique-GUID als die gefundene Partition: BadManifest — der
        // Eintrag meint andere Bytes (z. B. nach Verschieben der Partition, s. driver.rs).
        let bild = gutes_bild();
        let manifest = gutes_manifest();
        let fremd: [u8; 16] = [0x77; 16];
        let eintrag = guter_eintrag(&bild, &fremd, &PUBKEY);
        let (mut q, _) = platte_bauen(&eintrag, &bild, &manifest, &[]);
        let (mut d, mut a) = dienst_mit_cap();
        let s = d.suchen(&mut q, 0).expect("Suche ok");
        assert_eq!(s.unique_guid, LXPD_UNIQUE, "gefunden ist die echte Partition");
        d.bild_lesen(&mut q).expect("Lesen ok");
        assert_eq!(
            d.pruefen(b"deadbeef", &PUBKEY).err(),
            Some(LadeFehler::Container(LxpdError::BadManifest))
        );
        assert_eq!(d.anstossen(&mut a, &test_kontext()).err(), Some(LadeFehler::Ungeprueft));
        assert_eq!(a.aufrufe, 0);
    }

    #[test]
    fn ohne_loader_cap_kein_syscall_kein_anstoss() {
        // Geprueft, aber ohne Autoritaet: KEINE_LOADER_CAP — der Syscall wird gar nicht erst
        // gebaut (Fail-closed client-seitig, nicht erst per ERR_BADCAP im Kernel).
        let (eintrag, bild, manifest) = gutes_paar();
        let (mut q, _) = platte_bauen(&eintrag, &bild, &manifest, &[]);
        let (mut d, mut a) = (LadeDienst::neu(false), FakeAnstoss::neu());
        d.suchen(&mut q, 0).expect("Suche ok");
        d.bild_lesen(&mut q).expect("Lesen ok");
        d.pruefen(b"deadbeef", &PUBKEY).expect("pruefen ok");
        assert_eq!(d.phase(), Phase::Geprueft);
        assert_eq!(d.anstossen(&mut a, &test_kontext()).err(), Some(LadeFehler::KeineLoaderCap));
        assert_eq!(a.aufrufe, 0, "kein Wort, kein Anstoss ohne Cap");
    }

    #[test]
    fn vertrag_load_image_36_und_tag_belegung() {
        // Vertrag gegen die ABI-Wahrheit (`caprock_abi::sys::LOAD_IMAGE`): Nummer 36, TAG
        // Low-Byte = Bild-Slot, Bits 8..40 = exakte Länge, Bits 40..64 = 0. Der Spiegel steht
        // in `libcaprock` (Produktions-Dependenz, nicht nachgebaut); was hier pinnt, ist, dass
        // er SIE ist — läuft er auseinander, stellte jeder Anstoss einen fremden Syscall.
        assert_eq!(libcaprock::sys::LOAD_IMAGE, 36, "ABI-Nummer aus patch.txt");
        let tag = libcaprock::pack_tag(5, 96).expect("passt");
        assert_eq!(tag, 5 | (96 << 8));
        assert_eq!(tag & 0xff, 5, "TAG Low-Byte = Bild-Slot");
        assert_eq!((tag >> 8) & 0xffff_ffff, 96, "TAG Bits 8..40 = exakte Bildlänge");
        assert_eq!(tag >> 40, 0, "TAG Bits 40..64 = 0");
        assert_eq!(libcaprock::pack_tag(5, 0), None, "leeres Bild: Absage");
        assert_eq!(libcaprock::pack_tag(5, 1 << 32), None, "High-Bits: Absage, kein Abschnitt");
        assert_eq!(libcaprock::pack_tag(0x100, 96), None, "Slot über ein Byte: Absage");
        // Der vergebene Stand als Konstante im Code (kein Kommentar, der verloren geht).
        assert!(PATCH_TEXT.contains("UMGESETZT"), "Patch nennt den Stand");
        assert!(PATCH_TEXT.contains("LOAD_IMAGE"), "Patch nennt den Syscall");
        assert!(PATCH_TEXT.contains("36"), "Patch nennt die Nummer");
        assert!(!PATCH_TEXT.contains("todo!"), "kein Vorschlag mehr, sondern Stand");
    }

    #[test]
    fn slot_kontext_laueft_bis_zum_anstoss_durch() {
        // Was die PD aus IHREM Manifest mitgab (krumme Werte: 3/5/9 — 0/0/0 bewiese nichts),
        // kommt beim Anstoss unverändert an: Der Dienst erfindet keine Übergabe, er reicht sie
        // nur durch. Auf dem Host steht dahinter der Fake (der Syscall liefe hier ins Leere);
        // in der PD derselbe Weg mit `KernelAnstoss` bis `libcaprock::load_image`.
        let ctx = AnstossKontext {
            loader_slot: 3,
            bild_slot: 5,
            programm_id: 9,
            deleg_liste: 0x0403,
            deleg_anzahl: 2,
            extras: 0x10,
        };
        let (eintrag, bild, manifest) = gutes_paar();
        let (mut q, _) = platte_bauen(&eintrag, &bild, &manifest, &[]);
        let (mut d, mut a) = dienst_mit_cap();
        d.suchen(&mut q, 0).expect("Suche ok");
        d.bild_lesen(&mut q).expect("Lesen ok");
        d.pruefen(b"deadbeef", &PUBKEY).expect("pruefen ok");
        let pd = d.anstossen(&mut a, &ctx).expect("anstossen");
        assert_eq!(pd, 42);
        assert_eq!(a.aufrufe, 1);
        assert_eq!(a.letzter_kontext, Some(ctx), "Kontext unverändert durchgereicht");
    }

    #[test]
    fn laden_ueber_worte_end_to_end() {
        // Transport-Eigenschaft: Der GESAMTE Weg ueber Worte (`bedienen`) — suchen, lesen,
        // pruefen, anstossen — mit PD-Id 42 als Antwort. Was ueber den Draht nicht geht, geht
        // in der PD nicht.
        use super::protokoll::{Nachricht, bedienen, lade_kode};
        let (eintrag, bild, manifest) = gutes_paar();
        let (mut q, _) = platte_bauen(&eintrag, &bild, &manifest, &[]);
        let (mut d, mut a) = dienst_mit_cap();
        let aus = bedienen(
            &mut d,
            &mut q,
            &mut a,
            &test_kontext(),
            b"deadbeef",
            &PUBKEY,
            Nachricht::auskunft().als_worte(),
        );
        assert_eq!(aus, [0, 0, 0, 0], "Phase LEER, kein Bild");
        let ant = bedienen(
            &mut d,
            &mut q,
            &mut a,
            &test_kontext(),
            b"deadbeef",
            &PUBKEY,
            Nachricht::laden(0).als_worte(),
        );
        assert_eq!(ant, [0, 42, 0, 0], "LADEN ok, neue PD 42");
        assert_eq!(a.aufrufe, 1);
        assert_eq!(a.letzter_kontext, Some(test_kontext()), "Kontext über Worte durchgereicht");
        let aus2 = bedienen(
            &mut d,
            &mut q,
            &mut a,
            &test_kontext(),
            b"deadbeef",
            &PUBKEY,
            Nachricht::auskunft().als_worte(),
        );
        assert_eq!(aus2[0], 0);
        assert_eq!(aus2[1], 4, "Phase GELADEN");
        assert_eq!(aus2[2], bild.len() as u64);
        // Falscher Schluessel ueber Worte: benannte Absage als Kode, kein Anstoss.
        let (mut d2, mut a2) = dienst_mit_cap();
        let (mut q2, _) = platte_bauen(&eintrag, &bild, &manifest, &[]);
        let ant2 = bedienen(
            &mut d2,
            &mut q2,
            &mut a2,
            &test_kontext(),
            b"falsch",
            &PUBKEY,
            Nachricht::laden(0).als_worte(),
        );
        assert_eq!(ant2[0], lade_kode(LadeFehler::Container(LxpdError::BadSignature)));
        assert_eq!(a2.aufrufe, 0);
    }

    #[test]
    fn geprueftes_bild_erst_nach_pruefen() {
        // Die Vorlage fuer die LOAD_IMAGE-Uebergabe: vor dem Pruefen gibt es nichts
        // (None — keine Bytes, die der Kernel lesen koennte), danach exakt die geprueften
        // Bytes (Laenge + SHA-256 stimmen mit der Pruefsumme ueberein), nach dem
        // Zuruecksetzen wieder nichts. Wer ohne Pruefung anstieesse, lade Ungeprueftes.
        use caprock_lxpd::driver::sha256;
        let (eintrag, bild, manifest) = gutes_paar();
        let (mut q, _) = platte_bauen(&eintrag, &bild, &manifest, &[]);
        let (mut d, mut a) = dienst_mit_cap();
        assert_eq!(d.geprueftes_bild(), None, "leer: keine Vorlage");
        d.suchen(&mut q, 0).expect("Suche ok");
        assert_eq!(d.geprueftes_bild(), None, "Verzeichnis: noch ungeprueft");
        d.bild_lesen(&mut q).expect("Lesen ok");
        assert_eq!(d.geprueftes_bild(), None, "Bild: noch ungeprueft");
        let summe = d.pruefen(b"deadbeef", &PUBKEY).expect("pruefen ok");
        let vorlage_len = {
            let vorlage = d.geprueftes_bild().expect("geprueft: Vorlage da");
            assert_eq!(vorlage.len(), summe.bild_len, "exakt die gepruefte Laenge");
            assert_eq!(vorlage, bild.as_slice(), "exakt die geprueften Bytes");
            assert_eq!(sha256(vorlage), summe.bild_hash, "Hash traegt die Vorlage");
            vorlage.len()
        };
        // Der Anstoss nimmt sie dahinter auch wirklich (Fake zaehlt die Laenge).
        d.anstossen(&mut a, &test_kontext()).expect("anstossen");
        assert_eq!(a.letzte_bildlen, vorlage_len);
        d.zuruecksetzen();
        assert_eq!(d.geprueftes_bild(), None, "zurueckgesetzt: wieder zu");
        assert_eq!(d.anstossen(&mut a, &test_kontext()).err(), Some(LadeFehler::Ungeprueft));
    }

    #[test]
    fn schritte_ausser_der_reihe_sind_ungeprueft() {
        // Jeder Sprung ausser der Reihe scheitert mit UNGEPRUEFT — die Phase traegt die
        // Reihenfolge, nicht die Disziplin des Aufrufers.
        let (eintrag, bild, manifest) = gutes_paar();
        let (mut d, mut a) = dienst_mit_cap();
        let (mut q, _) = platte_bauen(&eintrag, &bild, &manifest, &[]);
        assert_eq!(d.bild_lesen(&mut q).err(), Some(LadeFehler::Ungeprueft));
        assert_eq!(d.pruefen(b"deadbeef", &PUBKEY).err(), Some(LadeFehler::Ungeprueft));
        assert_eq!(d.anstossen(&mut a, &test_kontext()).err(), Some(LadeFehler::Ungeprueft));
        d.suchen(&mut q, 0).expect("Suche ok");
        assert_eq!(d.pruefen(b"deadbeef", &PUBKEY).err(), Some(LadeFehler::Ungeprueft));
        assert_eq!(d.anstossen(&mut a, &test_kontext()).err(), Some(LadeFehler::Ungeprueft));
        d.bild_lesen(&mut q).expect("Lesen ok");
        assert_eq!(d.anstossen(&mut a, &test_kontext()).err(), Some(LadeFehler::Ungeprueft));
        assert_eq!(a.aufrufe, 0);
        // Und nach dem Zuruecksetzen ist wieder alles zu.
        d.zuruecksetzen();
        assert_eq!(d.phase(), Phase::Leer);
        assert_eq!(d.anstossen(&mut a, &test_kontext()).err(), Some(LadeFehler::Ungeprueft));
    }
}
