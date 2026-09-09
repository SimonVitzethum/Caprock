//! **System-Manifest** (A-1.2 – A-1.4) — das eine Dokument, das die Anfangsverteilung von
//! Autorität festlegt.
//!
//! Das Boot-Image enthält genau zwei Dinge: den Kernel und diese Datei. Sie sagt, was geladen
//! wird, mit welchem erwarteten Hash, in welche Domäne, mit welchen Anfangs-Caps und unter
//! welcher Politik. **Wer sie tauschen kann, besitzt die Maschine** — deshalb ist sie signiert,
//! und die Signatur ist an das Kernel-Image gebunden (A-1.3).
//!
//! ## Warum das nicht dasselbe ist wie ADR 0014
//!
//! Ein TrustedSAS-Zertifikat ([`crate::cert`]) bezeugt: *dieses Binary ist nach diesen Regeln
//! gebaut worden*. Das Manifest bezeugt: *diese Komponenten bekommen beim Start diese Autorität*.
//! Das erste bindet Code an eine Herkunft, das zweite Zuteilung an eine Maschine. Ein gültiges
//! Zertifikat für ein Binary sagt nichts darüber, ob es eine Loader-Cap bekommen soll.
//!
//! ## Angriffsfläche, und was daraus folgt
//!
//! Das Manifest wird **vor** jeder Signaturprüfung geparst — der Parser ist also die erste Stelle,
//! die fremde Bytes anfasst. Deshalb: feste Feldbreiten, Längenpräfixe, kein TOML, kein JSON, kein
//! `unsafe` (Crate-weit verboten), und jeder Fehlerpfad endet in einem **benannten** Fehler statt
//! in einem Zugriff daneben.
//!
//! **Benannt heisst: unterscheidbar.** Formfehler ergeben [`LoaderError::BadManifest`], ein
//! unbekanntes **Format** dagegen [`LoaderError::UnsupportedManifestFormat`] samt der beiden
//! Zahlen. Die beiden Lagen fuehren zu entgegengesetzten Handlungen — Kernel aktualisieren gegen
//! Herkunft untersuchen —, und bis 2026-08-10 waren sie hier nicht zu unterscheiden.
//!
//! ## Prüfreihenfolge: Signatur zuerst, Inhalt danach — **strukturell**
//!
//! „Nie umgekehrt" als Kommentar wäre eine Bitte. Hier trägt es der Typ: [`SystemManifest::parse`]
//! liefert Kopf, Nachricht und Signatur — aber **keinen Eintrag**. Einträge gibt es nur über
//! [`Verified`], und ein [`Verified`] entsteht ausschließlich aus
//! [`SystemManifest::verify_with`], das die Signatur über die gesamte Nachricht prüfen lässt. Wer
//! die Reihenfolge umdrehen wollte, müsste den Typ umschreiben, nicht bloß eine Zeile verschieben.
//!
//! ## Format (Little-Endian, feste Breiten) — `[0..msg_len)` wird signiert
//! ```text
//! Kopf (80 B):
//!   0  magic:u32              = 0x534C_4B4D ("SLKM")
//!   4  format_version:u16     = 2 (1 = Vorgaenger, s.u.)
//!   6  signature_algorithm_id:u16   (Ed25519 = 1)
//!   8  flags:u32
//!  12  manifest_version:u32   (monoton; Anti-Downgrade)
//!  16  entry_count:u32
//!  20  entry_len:u32          = 100 (v2) bzw. 96 (v1 — selbstbeschreibend, aber NIE zum
//!                                    Teil-Lesen: ein Kernel mit anderem Eintragsformat weist
//!                                    BENANNT ab, statt die bekannten Felder zu lesen und den Rest
//!                                    zu ueberspringen. Das waere hier besonders tueckisch, weil
//!                                    die Signatur ueber die GANZE Nachricht laeuft: das Ergebnis
//!                                    waere echt und missverstanden zugleich, mit verrutschtem
//!                                    `initial_caps`)
//!  24  kernel_hash:[u8;32]    SHA-256 des Kernel-Codes -> bindet das Manifest an DIESEN Kernel
//!  56  key_id:[u8;16]
//!  72  reserved:[u8;8]        = 0
//!
//! Eintrag v1 (96 B), entry_count mal — die Fassung, die `tools/sign_manifest.py` schreibt:
//!   0  name:[u8;16]           (NUL-gepolstert)
//!  16  program_id:u32         (stabile ID; ueberdauert Namensaenderungen)
//!  20  domain:u32             (0=TrustedSAS 1=HardwareLand 2=UserLand)
//!  24  iface_version:u32      (Schnittstellenversion — A-4.4 weist einen Austausch ab, der sie aendert)
//!  28  initial_caps:u32       (Bitmaske, s. CAP_*)
//!  32  sha256:[u8;32]         (erwarteter Hash des Moduls)
//!  64  policy_flags:u32       (s. POLICY_*)
//!  68  numa_node:u32
//!  72  core_affinity:u32      (0xFFFF_FFFF = beliebig)
//!  76  priority:u32
//!  80  budget_us:u32          (0 = kein Budget)
//!  84  dev_vendor:u16         (A-5.3 Geraete-Selektor; 0xFFFF = beliebig, 0 = Feld ungesetzt)
//!  86  dev_device:u16         (dito)
//!  88  dev_class:u32          (class<<16|subclass<<8|prog_if; 0xFFFF_FFFF = beliebig)
//!  92  service_id:u32         (A-5.4: WELCHEN Dienst dieser Client meint; 0 = nicht benannt)
//!
//! Eintrag v2 (100 B) = v1 plus:
//!  96  period_us:u32          (Z11c: die Periode zur `budget_us`-Reservierung; 0 = nichts gesagt)
//!
//! msg_len = 80 + entry_count * entry_len (96 oder 100, je Fassung)
//! msg_len  signature:[..]     (nicht leer; Laenge/Algorithmus prueft der Kernel-Verifier)
//! ```
//!
//! ## Warum v2 laenger ist statt breiter zu deuten (Z11c, 2026-09-09)
//!
//! Eine MCS-Reservierung braucht Budget **und** Periode; v1 hatte eine Zahl. Aus einer Zahl eine
//! Reservierung zu machen hiesse, die Periode zu erfinden — sie stuende dann in keinem Dokument.
//! Die reservierten Bytes sind weg (A-5.3 nahm 8, A-5.4 die letzten 4), also waechst der Eintrag
//! um 4 auf 100 und die Formatversion auf 2. Ein v1-Lader weist v2 **benannt** ab
//! ([`LoaderError::UnsupportedManifestFormat`] mit beiden Zahlen) statt 96 von 100 Byte zu
//! lesen — s. die Regel im Kopfkommentar von [`SystemManifest::parse`].
//!
//! ## Warum `period_us = 0` KEINE priority-0-Falle ist
//!
//! `priority = 0` ist die niedrigste gueltige Prioritaet und damit von „nichts gesagt" nicht zu
//! unterscheiden (Z11c, offen). Bei der Periode gibt es diese Falle nicht: eine Periode von
//! 0 µs ist keine gueltige Reservierung, sondern bedeutungslos — `0` heisst also eindeutig
//! „nichts gesagt", genau wie `budget_us = 0` „kein Budget" heisst. Entscheidend ist die
//! **Kombination**: erst `budget_us != 0 && period_us != 0` ist eine Reservierung
//! ([`Entry::hat_reservierung`]); jede der beiden Zahlen allein sagt nichts.
//!
//! ## Die Politikfelder gehören halb hierher (A-1.4)
//!
//! `policy_flags`, `numa_node`, `core_affinity`, `priority`, `budget_us` sind die Schnittstelle zu
//! Strang B: **das Format steht hier, die Bedeutung dort.** Was ein Farbstreifen ist und wie NUMA
//! vergeben wird, entscheidet nicht diese Datei.
//!
//! Was **nicht** hierher gehört: die konkrete Farbe. Sie ist maschinenlokal — ein Manifest, das
//! Farbe 7 verlangt, wäre auf der nächsten Maschine mit anderer Cache-Geometrie entweder falsch
//! oder still bedeutungslos. Das Manifest sagt „exklusiver Streifen ja/nein"; welcher, entscheidet
//! die Maschine.

use crate::LoaderError;

/// Magic ("SLKM").
pub const MANIFEST_MAGIC: u32 = 0x534C_4B4D;
/// Neueste unterstützte Formatversion (Z11c: traegt `period_us`).
pub const MANIFEST_FORMAT_VERSION: u16 = 2;
/// Vorgaenger-Fassung: wird weiterhin gelesen (`period_us` = 0 = „nichts gesagt").
pub const MANIFEST_FORMAT_VERSION_V1: u16 = 1;
/// Signaturalgorithmus Ed25519 (identisch nummeriert wie in [`crate::cert`]).
pub const SIG_ALG_ED25519: u16 = 1;

/// Länge des festen Kopfteils.
pub const HEADER_LEN: usize = 80;
/// Länge eines Eintrags der Fassung 1 (96 B — die Fassung, die `tools/sign_manifest.py` schreibt).
pub const ENTRY_LEN: usize = 96;
/// Länge eines Eintrags der Fassung 2 (96 B + `period_us`).
pub const ENTRY_LEN_V2: usize = 100;

/// Obergrenze der Eintragszahl. Bewusst klein: die Startmenge eines Knotens ist überschaubar, und
/// eine harte Schranke hier ist billiger als eine Schleife über eine fremde `u32`.
pub const MAX_ENTRIES: usize = 64;

// --- Anfangs-Caps (Bitmaske `initial_caps`) ---------------------------------------------------
//
// Die Maske sagt, welche **Autoritäts-Arten** die Komponente beim Start erhalten soll. Die
// konkreten Objekte (welcher Endpoint, welches MMIO-Fenster) sind maschinenlokal und werden vom
// Kernel-Glue zugeteilt — das Manifest legt die Art fest, nicht die Instanz.

/// Darf Programme aus der Startmenge laden (`SYS_LOAD`). Der Root-Task braucht das.
pub const CAP_LOADER: u32 = 1 << 0;
/// Darf fremde PDs steuern (`SYS_PDCTL`: start/stop/pause/resume).
pub const CAP_PD_CONTROL: u32 = 1 << 1;
/// Bekommt eine Notification (Signal-Empfang, IRQ-Zustellung).
pub const CAP_NOTIFICATION: u32 = 1 << 2;
/// Bekommt einen eigenen Endpoint (Dienst-Schnittstelle).
pub const CAP_ENDPOINT: u32 = 1 << 3;
/// Darf ein MMIO-Fenster halten (Treiber).
pub const CAP_MMIO: u32 = 1 << 4;
/// Darf einen IRQ binden (Treiber).
pub const CAP_IRQ: u32 = 1 << 5;
/// Darf eine DMA-Region halten (Treiber).
pub const CAP_DMA: u32 = 1 << 6;
/// **Bekommt die geteilte Uebertragungsflaeche eines Dienstes** (2026-08-25).
///
/// ## Warum das ein eigenes Bit ist und kein Anhaengsel von [`CAP_ENDPOINT`]
///
/// Bis heute kam die Flaeche mit dem Endpoint -- aber **nur, wenn der benannte Dienst zufaellig
/// ein Geraet hatte**: `driver_shared_cap` sucht in der Tabelle der Geraetezuteilungen und gibt
/// fuer einen Dienst ohne Geraet `None`. Damit hing die Slot-Zahl eines Programms an einer
/// Eigenschaft einer **anderen** PD und stand nicht in seinem eigenen Eintrag.
///
/// Eine feste Kopplung waere ablesbar gewesen; eine bedingte ist es nicht. Wer das Budget von
/// acht Slots plant, muss den Verbrauch aus dem Autoritaetsdokument lesen koennen -- und nicht
/// daraus, was ein fremder Dienst gerade ist.
pub const CAP_SHARED: u32 = 1 << 7;
/// Alle heute definierten Bits — was darüber hinaus gesetzt ist, versteht dieser Kernel nicht.
pub const CAP_KNOWN: u32 = CAP_LOADER
    | CAP_PD_CONTROL
    | CAP_NOTIFICATION
    | CAP_ENDPOINT
    | CAP_MMIO
    | CAP_IRQ
    | CAP_DMA
    | CAP_SHARED;

// --- Politikfelder (Bitmaske `policy_flags`) — Format hier, Bedeutung in Strang B -------------

/// Exklusiver Farbstreifen (Cache-Partition) für diese Komponente.
pub const POLICY_EXCLUSIVE_STRIPE: u32 = 1 << 0;
/// **Der Root-Task.** Genau ein Eintrag darf das tragen — er bekommt die Wurzel-Caps (A-2.1).
pub const POLICY_ROOT_TASK: u32 = 1 << 1;
/// Die Kern-Affinität ist bindend, nicht ein Wunsch.
pub const POLICY_PINNED: u32 = 1 << 2;
/// Diese Komponente ist **nicht** im Betrieb austauschbar (A-4.5, Negativliste).
pub const POLICY_NO_HOTRELOAD: u32 = 1 << 3;
/// **Ueber diese Komponente darf eine `Debuggable`-Cap gepraegt werden** (Z6b).
///
/// ## Warum die Entscheidung im MANIFEST steht und nicht in einem Laufzeitschalter
///
/// Die Zusage lautet: *eine PD, ueber die nie eine `Debuggable` gepraegt wurde, kann nicht debuggt
/// werden — auch nicht vom Betreiber.* Das Manifest ist Ed25519-**signiert** und ueber
/// `kernel_hash` an genau diesen Kernel gebunden, und `manifest_version` ist monoton
/// (Anti-Downgrade). Damit ist „wer debuggt werden darf" eine **attestierte** Entscheidung, die
/// niemand mit einer Shell umlegt — der Unterschied zwischen einer Politik und einer Zusicherung.
///
/// **Fehlt das Bit, fehlt die Cap.** Kein Nachreichen, kein „spaeter gewaehren": ein spaeterer Weg
/// ist genau der Weg, auf dem eine Vorgabe hereinkommt.
pub const POLICY_DEBUGGABLE: u32 = 1 << 4;
/// **Diese Komponente BIETET einen Dienst an** (2026-08-25).
///
/// ## Warum es das braucht, obwohl `CAP_ENDPOINT` schon existiert
///
/// Weil dieselbe Zeile bis heute **zwei** Dinge heisst. `CAP_ENDPOINT` bedeutet „gib mir den
/// Kanal des ueber `service_id` benannten Dienstes" *und* „gib mir einen eigenen Endpoint" --
/// unterschieden allein dadurch, ob gerade ein Dienst existiert. Das ist die im Kernel
/// versteckte Politik aus A-5.4, eine Ebene tiefer: der Ausgang haengt an der Ladereihenfolge.
///
/// Mit diesem Bit sagt der Eintrag es selbst. Eine Komponente, die es traegt, bekommt einen
/// **frischen** Kanal und wird unter ihrer eigenen `program_id` **registriert** -- damit kann ein
/// Client sie mit `service_id` benennen.
///
/// ## Und warum das nicht an einem Geraet haengen darf
///
/// Bis heute rief nur der HardwareLand-Zweig `set_driver_service`. Eine PD ohne Geraet wurde also
/// nie registriert; ein Client, der sie benannte, bekam `None` und danach einen **frischen,
/// unverbundenen** Endpoint. Beide Seiten haetten einen Kanal gehabt und keinen gemeinsamen --
/// mit gueltigen Caps und ohne eine einzige Fehlermeldung.
pub const POLICY_PROVIDES_SERVICE: u32 = 1 << 5;

/// Alle heute definierten Bits.
pub const POLICY_KNOWN: u32 = POLICY_EXCLUSIVE_STRIPE
    | POLICY_ROOT_TASK
    | POLICY_PINNED
    | POLICY_NO_HOTRELOAD
    | POLICY_DEBUGGABLE
    | POLICY_PROVIDES_SERVICE;

/// „Kern egal" in `core_affinity`.
pub const ANY_CORE: u32 = u32::MAX;

fn rd_u16(d: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([d[off], d[off + 1]])
}
fn rd_u32(d: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([d[off], d[off + 1], d[off + 2], d[off + 3]])
}

/// Ein Manifest-Eintrag: **eine** Komponente der Startmenge.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Entry<'a> {
    name: &'a [u8],
    /// Stabile numerische ID (überdauert Namensänderungen).
    pub program_id: u32,
    /// Zieldomäne (`crate::DOMAIN_*`).
    pub domain: u32,
    /// Schnittstellenversion — ein Hot-Reload, der sie ändert, wird abgewiesen (A-4.4).
    pub iface_version: u32,
    /// Anfangs-Caps (Bitmaske, s. `CAP_*`).
    pub initial_caps: u32,
    /// Erwarteter SHA-256 des Moduls. Der Kernel lädt nur, was diesen Hash hat.
    pub sha256: [u8; 32],
    /// Politik-Bitmaske (s. `POLICY_*`).
    pub policy_flags: u32,
    /// NUMA-Knoten (Bedeutung: Strang B).
    pub numa_node: u32,
    /// Kern-Affinität; [`ANY_CORE`] = beliebig.
    pub core_affinity: u32,
    /// Scheduling-Priorität.
    pub priority: u32,
    /// CPU-Budget in Mikrosekunden (0 = keines).
    pub budget_us: u32,
    /// CPU-Periode in Mikrosekunden (Z11c, nur Fassung 2; Fassung 1 liefert 0).
    ///
    /// `0` heisst „nichts gesagt" — und das ist hier **keine** priority-0-Falle: eine Periode
    /// von 0 µs waere keine gueltige Reservierung, waehrend Prioritaet 0 die niedrigste
    /// gueltige Prioritaet ist. Erst zusammen mit [`Entry::budget_us`] entsteht eine
    /// Reservierung, s. [`Entry::hat_reservierung`].
    pub period_us: u32,
    /// **Welches** Gerät diese Komponente bekommen soll (A-5.3). Siehe [`DeviceSelector`].
    pub device: DeviceSelector,
    /// **Welchen Dienst dieser Client meint** (A-5.4) — die `program_id` des Anbieters.
    ///
    /// `0` = nicht benannt. Das war bis A-5.4 der einzige Zustand, und es ging gut, solange es
    /// genau **einen** Dienst gab: „gib mir einen Endpoint" konnte dann nur dessen Kanal meinen.
    /// Bei zweien ist dieselbe Zeile eine **im Kernel versteckte Politik** — der Client bekäme
    /// irgendeinen, und welchen, entschiede die Ladereihenfolge.
    ///
    /// Deshalb: `0` bleibt zulässig und heißt „es gibt ohnehin nur einen"; gibt es mehrere, wird
    /// **abgewiesen** statt geraten.
    pub service_id: u32,
}

/// **Der Geräte-Selektor eines Manifest-Eintrags** (A-5.3).
///
/// Bis A-5.1 sagte das Manifest über Geräte-Autorität nur `mmio,dma` — „diese Komponente darf ein
/// Registerfenster und eine DMA-Region halten". *Welches* Gerät das ist, entschied der Kernel, und
/// zwar nach Fundreihenfolge. Bei genau einem zuteilbaren Gerät fiel das nicht auf; bei zweien wäre
/// es eine **im Kernel versteckte Politik** gewesen — die gefährlichste Sorte, weil sie nirgends
/// steht und sich mit der Enumerationsreihenfolge ändert.
///
/// ## Was hier NICHT steht, und warum
///
/// **Keine Instanz.** Kein Bus, kein Gerät, keine Funktion, keine physische Adresse. Ein Manifest,
/// das `00:04.0` nennt, ist an die Topologie *einer* Maschine gebunden und auf der nächsten
/// stillschweigend falsch — es zeigte dann auf ein anderes Gerät, nicht auf keines. Der Selektor
/// benennt eine **Art** von Gerät; welche Instanz das ist, sieht nur der Kernel, weil nur er
/// enumeriert.
///
/// ## Die Felder
///
/// `vendor`/`device` sind die PCI-IDs, `class` ist `class<<16 | subclass<<8 | prog_if`. Jedes Feld
/// einzeln auf „beliebig" stellbar ([`ANY16`]/[`ANY32`]); ein Eintrag ohne Geräte-Caps lässt alle
/// drei auf „beliebig" und ändert damit nichts.
///
/// **Fail-closed:** passt kein Gerät auf den Selektor, bekommt die Komponente **keines**. Der
/// bequeme Rückfall („nichts passt → nimm irgendeins") wäre genau die versteckte Politik, gegen die
/// dieses Feld antritt, nur eine Ebene tiefer.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct DeviceSelector {
    pub vendor: u16,
    pub device: u16,
    pub class: u32,
}

/// „Beliebig" für die 16-Bit-Felder eines [`DeviceSelector`].
pub const ANY16: u16 = 0xFFFF;
/// „Beliebig" für die 32-Bit-Felder eines [`DeviceSelector`].
pub const ANY32: u32 = 0xFFFF_FFFF;

impl DeviceSelector {
    /// Der Selektor, der auf **jedes** Gerät passt — der Zustand vor A-5.3.
    pub const ANY: Self = Self {
        vendor: ANY16,
        device: ANY16,
        class: ANY32,
    };

    /// Schränkt dieser Selektor überhaupt etwas ein?
    ///
    /// Wichtig für den Bericht: „keine Einschränkung" und „Einschränkung, auf die nichts passt"
    /// sind zwei völlig verschiedene Lagen, und nur die zweite ist ein Aufbaufehler.
    pub fn is_any(&self) -> bool {
        *self == Self::ANY
    }

    /// Passt ein gefundenes Gerät auf diesen Selektor?
    pub fn matches(&self, vendor: u16, device: u16, class: u32) -> bool {
        (self.vendor == ANY16 || self.vendor == vendor)
            && (self.device == ANY16 || self.device == device)
            && (self.class == ANY32 || self.class == class)
    }
}

impl<'a> Entry<'a> {
    /// Der Name als `&str` (bis zum ersten NUL), nicht-UTF8 → `"?"`.
    pub fn name(&self) -> &'a str {
        let end = self.name.iter().position(|&c| c == 0).unwrap_or(self.name.len());
        core::str::from_utf8(&self.name[..end]).unwrap_or("?")
    }
    /// Ist dieser Eintrag der Root-Task?
    pub fn is_root_task(&self) -> bool {
        self.policy_flags & POLICY_ROOT_TASK != 0
    }
    /// Traegt dieser Eintrag eine einhaltbare CPU-Reservierung? (Z11c)
    ///
    /// Erst Budget **und** Periode gemeinsam sind eine Reservierung. Jede der beiden Zahlen
    /// allein sagt nichts: `budget_us` ohne `period_us` liesse die Periode erfinden, `period_us`
    /// ohne `budget_us` den Anteil. Ein v1-Manifest (Periode immer 0) liefert hier also nie
    /// `true` — der Kernel weist `budget_us != 0` ohne Periode ab, statt zu raten.
    pub fn hat_reservierung(&self) -> bool {
        self.budget_us != 0 && self.period_us != 0
    }
    /// Verlangt dieser Eintrag Autorität, die dieser Kernel nicht kennt? Dann darf er **nicht**
    /// geladen werden: ein unbekanntes Bit bedeutet, dass das Manifest von einer Zuteilung
    /// ausgeht, die hier niemand vornimmt — stillschweigend weniger Autorität zu geben, wäre die
    /// gefährlichere Auslegung (der Dienst liefe halb und niemand sagte es).
    pub fn has_unknown_authority(&self) -> bool {
        self.initial_caps & !CAP_KNOWN != 0 || self.policy_flags & !POLICY_KNOWN != 0
    }
}

/// Ein **strukturell** geparstes, noch **nicht verifiziertes** Manifest.
///
/// Absichtlich ohne Zugriff auf die Einträge: siehe Moduldoku, „Prüfreihenfolge".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SystemManifest<'a> {
    pub format_version: u16,
    pub signature_algorithm_id: u16,
    pub flags: u32,
    /// Monoton wachsende Manifest-Version (Anti-Downgrade; der Kernel setzt eine Untergrenze).
    pub manifest_version: u32,
    /// Zahl der Einträge (bereits gegen [`MAX_ENTRIES`] und die Datenlänge geprüft).
    pub entry_count: u32,
    /// Breite eines Eintrags in Byte (96 = v1, 100 = v2) — die geparste Fassung.
    pub entry_len: usize,
    /// SHA-256 des Kernel-Codes, an den dieses Manifest gebunden ist (A-1.3).
    pub kernel_hash: [u8; 32],
    /// Fingerprint des signierenden Schlüssels.
    pub key_id: [u8; 16],
    entries: &'a [u8],
    message: &'a [u8],
    signature: &'a [u8],
}

impl<'a> SystemManifest<'a> {
    /// Strukturell parsen: Magic, Formatversion, Eintragsbreite, Eintragszahl, und dass die
    /// Nachricht samt **nicht-leerer** Signatur in die Daten passt. **Keine** Krypto, **keine**
    /// Inhaltsauswertung. Fehlerhafte Eingabe → [`LoaderError::BadManifest`], nie ein Panic.
    pub fn parse(data: &'a [u8]) -> Result<SystemManifest<'a>, LoaderError> {
        if data.len() < HEADER_LEN {
            return Err(LoaderError::BadManifest);
        }
        if rd_u32(data, 0) != MANIFEST_MAGIC {
            return Err(LoaderError::BadManifest);
        }
        // **Formatversion und Eintragsbreite werden BENANNT abgewiesen, nicht als Formfehler.**
        //
        // „Neuer als dieser Kernel" und „kaputte Bytes" führen zu entgegengesetzten Handlungen;
        // bis 2026-08-10 waren sie hier nicht zu unterscheiden. Und **niemals** wird gelesen, was
        // man kennt, und der Rest übersprungen: signiert ist die GANZE Nachricht, ein
        // teilgelesenes v2-Manifest wäre authentisch und unverstanden zugleich — mit verrutschtem
        // `initial_caps`/`policy_flags` und gültiger Signatur darüber. S. `LoaderError`.
        //
        // Z11c (2026-09-09): der Parser kennt ZWEI Fassungen — (1, 96) und (2, 100). Alles andere
        // ist entweder neuer als dieser Kernel oder kaputt, und beides bekommt die benannte
        // Absage mit beiden Zahlen. Insbesondere liest kein v2-Eintrag je als zwei v1-Einträge:
        // die Breite steht im Kopf, die Schrittweite folgt ihr.
        let format_version = rd_u16(data, 4);
        let entry_len = rd_u32(data, 20);
        let entry_len_ok = (format_version == MANIFEST_FORMAT_VERSION_V1
            && entry_len as usize == ENTRY_LEN)
            || (format_version == MANIFEST_FORMAT_VERSION && entry_len as usize == ENTRY_LEN_V2);
        if !entry_len_ok {
            return Err(LoaderError::UnsupportedManifestFormat {
                format_version,
                entry_len,
            });
        }
        let entry_len = entry_len as usize;
        let entry_count = rd_u32(data, 16);
        if entry_count as usize > MAX_ENTRIES {
            return Err(LoaderError::BadManifest);
        }
        // Overflow-sicher: entry_count <= MAX_ENTRIES, also passt das Produkt in usize.
        let msg_len = HEADER_LEN + (entry_count as usize) * entry_len;
        if data.len() <= msg_len {
            return Err(LoaderError::BadManifest); // Signatur fehlt bzw. ist leer
        }
        let mut kernel_hash = [0u8; 32];
        kernel_hash.copy_from_slice(&data[24..56]);
        let mut key_id = [0u8; 16];
        key_id.copy_from_slice(&data[56..72]);
        Ok(SystemManifest {
            format_version,
            signature_algorithm_id: rd_u16(data, 6),
            flags: rd_u32(data, 8),
            manifest_version: rd_u32(data, 12),
            entry_count,
            entry_len,
            kernel_hash,
            key_id,
            entries: &data[HEADER_LEN..msg_len],
            message: &data[..msg_len],
            signature: &data[msg_len..],
        })
    }

    /// Die **gesamte** signierte Nachricht `[0..msg_len)`.
    pub fn message(&self) -> &'a [u8] {
        self.message
    }
    /// Die Signatur (variabel lang; Länge/Algorithmus prüft der Kernel-Verifier).
    pub fn signature(&self) -> &'a [u8] {
        self.signature
    }

    /// **Das Tor zu den Einträgen.** `check(message, signature)` muss die Signatur prüfen; nur bei
    /// `true` entsteht ein [`Verified`]. Es gibt keinen anderen Weg, einen Eintrag zu lesen —
    /// das ist die Durchsetzung von „Signatur zuerst, Inhalt danach".
    pub fn verify_with(
        self,
        check: impl FnOnce(&'a [u8], &'a [u8]) -> bool,
    ) -> Result<Verified<'a>, LoaderError> {
        if check(self.message, self.signature) {
            Ok(Verified(self))
        } else {
            Err(LoaderError::Unverified)
        }
    }
}

/// Ein Manifest, dessen Signatur geprüft **wurde**. Erst hier gibt es Einträge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Verified<'a>(SystemManifest<'a>);

impl<'a> Verified<'a> {
    /// Der geprüfte Kopf.
    pub fn header(&self) -> &SystemManifest<'a> {
        &self.0
    }

    /// Zahl der Einträge.
    pub fn count(&self) -> usize {
        self.0.entry_count as usize
    }

    /// Eintrag `i`. `None` nur bei `i >= count()`.
    pub fn entry(&self, i: usize) -> Option<Entry<'a>> {
        if i >= self.count() {
            return None;
        }
        // Bereits bei `parse` sichergestellt: `entries` ist genau `count * entry_len` lang, und
        // `entry_len` ist 96 (v1) oder 100 (v2) — die Schrittweite folgt der geparsten Fassung.
        let w = self.0.entry_len;
        let e = self.0.entries.get(i * w..(i + 1) * w)?;
        let mut sha256 = [0u8; 32];
        sha256.copy_from_slice(&e[32..64]);
        Some(Entry {
            name: &e[0..16],
            program_id: rd_u32(e, 16),
            domain: rd_u32(e, 20),
            iface_version: rd_u32(e, 24),
            initial_caps: rd_u32(e, 28),
            sha256,
            policy_flags: rd_u32(e, 64),
            numa_node: rd_u32(e, 68),
            core_affinity: rd_u32(e, 72),
            priority: rd_u32(e, 76),
            budget_us: rd_u32(e, 80),
            // Z11c: die Periode steht nur in Fassung 2 (Offset 96). Fassung 1 liefert 0 =
            // „nichts gesagt" — alte Manifeste bleiben lesbar, und ein altes Manifest bekommt
            // dadurch nie eine Reservierung angedichtet (`hat_reservierung` bleibt falsch).
            period_us: if w >= ENTRY_LEN_V2 { rd_u32(e, 96) } else { 0 },
            // A-5.3 nimmt 8 der 12 reservierten Bytes. Das Format bleibt eingefroren: `ENTRY_LEN`
            // aendert sich nicht, und ein aelteres Manifest (Nullen im reservierten Bereich) ergibt
            // `vendor=0 device=0 class=0` -- was auf **kein** Geraet passt. Deshalb wird `0` hier
            // ausdruecklich als "nicht gesetzt" gelesen und auf `ANY` abgebildet: sonst haette die
            // Formaterweiterung stillschweigend jedem alten Manifest die Geraete-Zuteilung
            // weggenommen, und zwar mit derselben Fehlermeldung wie ein echter Fehlgriff.
            device: {
                let v = rd_u16(e, 84);
                let d = rd_u16(e, 86);
                let c = rd_u32(e, 88);
                if v == 0 && d == 0 && c == 0 {
                    DeviceSelector::ANY
                } else {
                    DeviceSelector { vendor: v, device: d, class: c }
                }
            },
            service_id: rd_u32(e, 92),
        })
    }

    /// Über alle Einträge iterieren.
    pub fn iter(&self) -> impl Iterator<Item = Entry<'a>> + '_ {
        (0..self.count()).filter_map(move |i| self.entry(i))
    }

    /// Den Eintrag mit `program_id` suchen.
    pub fn find(&self, program_id: u32) -> Option<Entry<'a>> {
        self.iter().find(|e| e.program_id == program_id)
    }

    /// Der Root-Task-Eintrag — **nur**, wenn es genau einen gibt.
    ///
    /// Zwei Root-Tasks sind kein Sonderfall, den man auflösen könnte, sondern eine Aussage, die
    /// das Manifest nicht trifft: welcher von beiden bekommt die Wurzel-Caps? Also keiner.
    pub fn root_task(&self) -> Option<Entry<'a>> {
        let mut found = None;
        for e in self.iter() {
            if e.is_root_task() {
                if found.is_some() {
                    return None;
                }
                found = Some(e);
            }
        }
        found
    }

    /// Selbstkonsistenz des **Inhalts** (erst nach der Signaturprüfung sinnvoll). `0` = sauber,
    /// sonst ein Anomalie-Code:
    /// * 1 — kein Eintrag (ein Manifest ohne Startmenge legt nichts fest).
    /// * 2 — doppelte `program_id` (die ID soll gerade eindeutig sein).
    /// * 3 — unbekannte Domäne.
    /// * 4 — ein Eintrag verlangt Autorität, die dieser Kernel nicht kennt.
    /// * 5 — mehr als ein Root-Task bzw. keiner.
    pub fn audit(&self) -> u32 {
        let n = self.count();
        if n == 0 {
            return 1;
        }
        for i in 0..n {
            let Some(a) = self.entry(i) else { return 1 };
            for j in (i + 1)..n {
                if self.entry(j).map(|b| b.program_id) == Some(a.program_id) {
                    return 2;
                }
            }
            if a.domain > crate::DOMAIN_USERLAND {
                return 3;
            }
            if a.has_unknown_authority() {
                return 4;
            }
        }
        if self.root_task().is_none() {
            return 5;
        }
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DOMAIN_TRUSTED, DOMAIN_USERLAND};

    struct E {
        name: &'static str,
        program_id: u32,
        domain: u32,
        initial_caps: u32,
        policy_flags: u32,
        /// A-5.3: `None` = die reservierten Bytes bleiben **null**, also genau das, was ein vor
        /// A-5.3 erzeugtes Manifest liefert.
        device: Option<DeviceSelector>,
        /// A-5.4: die `program_id` des gemeinten Dienstes (`0` = nicht benannt).
        service_id: u32,
        /// Z11c: die Periode zur Budget-Reservierung (`0` = nichts gesagt). Nur der v2-Bauer
        /// schreibt sie; der v1-Bauer legt Nullen — genau wie ein vor Z11c erzeugtes Manifest.
        period_us: u32,
    }

    fn e(name: &'static str, program_id: u32) -> E {
        E {
            name,
            program_id,
            domain: DOMAIN_USERLAND,
            initial_caps: 0,
            policy_flags: 0,
            device: None,
            service_id: 0,
            period_us: 0,
        }
    }

    /// Ein strukturell gültiges Manifest der NEUESTEN Fassung bauen (Signatur ist Dummy — der
    /// Parser prüft keine Krypto).
    fn build(entries: &[E], siglen: usize) -> Vec<u8> {
        build_mit(MANIFEST_FORMAT_VERSION, ENTRY_LEN_V2, entries, siglen)
    }

    /// Ein strukturell gültiges Manifest der Fassung 1 bauen — also genau das, was
    /// `tools/sign_manifest.py` vor Z11c schrieb (96-B-Einträge, keine Periode).
    fn build_v1(entries: &[E], siglen: usize) -> Vec<u8> {
        build_mit(MANIFEST_FORMAT_VERSION_V1, ENTRY_LEN, entries, siglen)
    }

    fn build_mit(version: u16, breite: usize, entries: &[E], siglen: usize) -> Vec<u8> {
        let msg_len = HEADER_LEN + entries.len() * breite;
        let mut v = vec![0u8; msg_len + siglen];
        v[0..4].copy_from_slice(&MANIFEST_MAGIC.to_le_bytes());
        v[4..6].copy_from_slice(&version.to_le_bytes());
        v[6..8].copy_from_slice(&SIG_ALG_ED25519.to_le_bytes());
        v[12..16].copy_from_slice(&7u32.to_le_bytes()); // manifest_version
        v[16..20].copy_from_slice(&(entries.len() as u32).to_le_bytes());
        v[20..24].copy_from_slice(&(breite as u32).to_le_bytes());
        for i in 24..56 {
            v[i] = i as u8; // kernel_hash
        }
        for i in 56..72 {
            v[i] = (i + 1) as u8; // key_id
        }
        for (i, en) in entries.iter().enumerate() {
            let b = HEADER_LEN + i * breite;
            let nb = en.name.as_bytes();
            v[b..b + nb.len().min(16)].copy_from_slice(&nb[..nb.len().min(16)]);
            v[b + 16..b + 20].copy_from_slice(&en.program_id.to_le_bytes());
            v[b + 20..b + 24].copy_from_slice(&en.domain.to_le_bytes());
            v[b + 24..b + 28].copy_from_slice(&3u32.to_le_bytes()); // iface_version
            v[b + 28..b + 32].copy_from_slice(&en.initial_caps.to_le_bytes());
            for k in 0..32 {
                v[b + 32 + k] = (en.program_id as u8).wrapping_add(k as u8);
            }
            v[b + 64..b + 68].copy_from_slice(&en.policy_flags.to_le_bytes());
            v[b + 68..b + 72].copy_from_slice(&1u32.to_le_bytes()); // numa_node
            v[b + 72..b + 76].copy_from_slice(&ANY_CORE.to_le_bytes());
            v[b + 76..b + 80].copy_from_slice(&5u32.to_le_bytes()); // priority
            v[b + 80..b + 84].copy_from_slice(&1000u32.to_le_bytes()); // budget_us
            if let Some(d) = en.device {
                v[b + 84..b + 86].copy_from_slice(&d.vendor.to_le_bytes());
                v[b + 86..b + 88].copy_from_slice(&d.device.to_le_bytes());
                v[b + 88..b + 92].copy_from_slice(&d.class.to_le_bytes());
            }
            v[b + 92..b + 96].copy_from_slice(&en.service_id.to_le_bytes());
            if breite >= ENTRY_LEN_V2 {
                v[b + 96..b + 100].copy_from_slice(&en.period_us.to_le_bytes());
            }
        }
        for i in 0..siglen {
            v[msg_len + i] = (i + 9) as u8;
        }
        v
    }

    fn root(name: &'static str, id: u32) -> E {
        E {
            name,
            program_id: id,
            domain: DOMAIN_TRUSTED,
            initial_caps: CAP_LOADER | CAP_PD_CONTROL,
            policy_flags: POLICY_ROOT_TASK,
            device: None,
            service_id: 0,
            period_us: 0,
        }
    }

    // --- A-5.3: der Geraete-Selektor -------------------------------------------------------------

    /// **Die Rueckwaertsfalle.** Ein vor A-5.3 erzeugtes Manifest hat dort Nullen stehen. Wuerden
    /// die als `vendor=0 device=0 class=0` gelesen, passte der Selektor auf **kein** Geraet -- die
    /// Formaterweiterung haette jedem alten Manifest still die Geraete-Zuteilung weggenommen, und
    /// zwar mit derselben Meldung wie ein echter Fehlgriff.
    #[test]
    fn nullbytes_heissen_beliebig_nicht_nichts() {
        let raw = build(&[e("alt", 1)], 64);
        let v = accept(SystemManifest::parse(&raw).unwrap());
        let en = v.entry(0).unwrap();
        assert_eq!(en.device, DeviceSelector::ANY);
        assert!(en.device.is_any());
        assert!(en.device.matches(0x1af4, 0x1042, 0x01_0000));
    }

    /// Ein gesetzter Selektor kommt unveraendert heraus und passt nur auf das benannte Geraet.
    #[test]
    fn selektor_trennt_zwei_geraete() {
        let blk = DeviceSelector { vendor: 0x1af4, device: 0x1042, class: ANY32 };
        let mut en = e("blk", 3);
        en.device = Some(blk);
        let raw = build(&[en], 64);
        let v = accept(SystemManifest::parse(&raw).unwrap());
        let got = v.entry(0).unwrap().device;
        assert_eq!(got, blk);
        assert!(!got.is_any(), "ein gesetzter Selektor ist nicht 'beliebig'");
        assert!(got.matches(0x1af4, 0x1042, 0x01_0000), "das benannte Geraet passt");
        // virtio-net: gleicher Hersteller, andere Geraete-ID -> passt NICHT. Genau diese
        // Unterscheidung ist der Zweck des Feldes; ohne sie entschiede die Fundreihenfolge.
        assert!(!got.matches(0x1af4, 0x1041, 0x02_0000), "virtio-net darf nicht passen");
        assert!(!got.matches(0x8086, 0x1042, 0x01_0000), "anderer Hersteller darf nicht passen");
    }

    /// Ein Selektor nur ueber die **Klasse** -- die maschinenunabhaengige Aussage („ein
    /// Massenspeicher"), ohne sich auf Hersteller-IDs festzulegen.
    #[test]
    fn selektor_nur_ueber_die_klasse() {
        let sel = DeviceSelector { vendor: ANY16, device: ANY16, class: 0x01_0000 };
        let mut en = e("irgendein-blk", 4);
        en.device = Some(sel);
        let raw = build(&[en], 64);
        let v = accept(SystemManifest::parse(&raw).unwrap());
        let got = v.entry(0).unwrap().device;
        assert!(got.matches(0x1af4, 0x1042, 0x01_0000));
        assert!(got.matches(0x8086, 0x0953, 0x01_0000), "anderer Hersteller, gleiche Klasse");
        assert!(!got.matches(0x1af4, 0x1041, 0x02_0000), "Netzwerkklasse passt nicht");
    }

    /// **Der Client benennt seinen Dienst** (A-5.4). Ohne dieses Feld hiess "gib mir einen
    /// Endpoint" bei zwei Diensten: "gib mir irgendeinen" -- eine Politik, die niemand
    /// aufgeschrieben hat und die sich mit der Ladereihenfolge aendert.
    #[test]
    fn dienst_benennung_kommt_durch() {
        let mut client = e("fs", 4);
        client.service_id = 3;
        let raw = build(&[client], 64);
        let v = accept(SystemManifest::parse(&raw).unwrap());
        assert_eq!(v.entry(0).unwrap().service_id, 3);
        // Und ein Eintrag ohne Benennung bleibt 0 -- der Zustand vor A-5.4, der zulaessig bleibt,
        // solange es ohnehin nur einen Dienst gibt.
        let raw2 = build(&[e("alt", 9)], 64);
        let v2 = accept(SystemManifest::parse(&raw2).unwrap());
        assert_eq!(v2.entry(0).unwrap().service_id, 0);
    }

    /// Der Selektor liegt in den **reservierten** Bytes und aendert die Eintragslaenge nicht --
    /// sonst waere jedes bestehende signierte Manifest ungueltig geworden. (Fassung 1; Fassung 2
    /// waechst dafuer um genau die 4 Byte der Periode, s. `eintragsbreite_v2_ist_100`.)
    #[test]
    fn selektor_aendert_die_eintragslaenge_nicht() {
        assert_eq!(ENTRY_LEN, 96);
        let mut en = e("x", 1);
        en.device = Some(DeviceSelector { vendor: 1, device: 2, class: 3 });
        let raw = build_v1(&[en], 64);
        assert_eq!(raw.len(), HEADER_LEN + ENTRY_LEN + 64);
        // Die letzten vier Bytes tragen seit A-5.4 die Dienst-Benennung; ungesetzt sind sie null.
        let b = HEADER_LEN;
        assert_eq!(&raw[b + 92..b + 96], &[0u8; 4]);
    }

    fn accept<'a>(m: SystemManifest<'a>) -> Verified<'a> {
        m.verify_with(|_, _| true).unwrap()
    }

    // --- Z11c: `period_us` (Format v2) -----------------------------------------------------------

    /// **Alte Manifeste bleiben lesbar.** Ein vor Z11c erzeugtes Manifest (Fassung 1, 96 B je
    /// Eintrag — genau das, was `tools/sign_manifest.py` schreibt) parst unveraendert, und die
    /// fehlende Periode heisst 0 = „nichts gesagt": kein Budget wird angedichtet, keine
    /// Reservierung behauptet. Das ist die Gegenprobe zur priority-0-Falle — dort waere 0 ein
    /// gueltiger Wert, hier ist 0 µs bedeutungslos.
    #[test]
    fn v1_bleibt_lesbar_periode_ist_nichts_gesagt() {
        let raw = build_v1(&[root("init", 1), e("hello", 2)], 64);
        assert_eq!(raw.len(), HEADER_LEN + 2 * ENTRY_LEN + 64);
        let m = SystemManifest::parse(&raw).unwrap();
        assert_eq!(m.format_version, MANIFEST_FORMAT_VERSION_V1);
        assert_eq!(m.entry_len, ENTRY_LEN);
        assert_eq!(m.message().len(), HEADER_LEN + 2 * ENTRY_LEN);
        let v = accept(m);
        let e0 = v.entry(0).unwrap();
        assert_eq!(e0.budget_us, 1000); // das Budget steht auch in v1 schon da
        assert_eq!(e0.period_us, 0); // die Periode stand in keinem Dokument: nichts gesagt
        assert!(!e0.hat_reservierung()); // Budget ohne Periode ist KEINE Reservierung
        assert_eq!(v.entry(1).unwrap().period_us, 0);
    }

    /// **Die neue Zahl kommt durch.** Fassung 2 traegt die Periode bei Offset 96, und sie wird
    /// als Reservierung lesbar: Budget UND Periode gesetzt.
    #[test]
    fn v2_traegt_die_periode() {
        let mut rt = root("init", 1);
        rt.period_us = 20_000;
        let raw = build(&[rt], 64);
        assert_eq!(raw.len(), HEADER_LEN + ENTRY_LEN_V2 + 64);
        let m = SystemManifest::parse(&raw).unwrap();
        assert_eq!(m.format_version, MANIFEST_FORMAT_VERSION);
        assert_eq!(m.entry_len, ENTRY_LEN_V2);
        let v = accept(m);
        let e0 = v.entry(0).unwrap();
        assert_eq!(e0.budget_us, 1000);
        assert_eq!(e0.period_us, 20_000);
        assert!(e0.hat_reservierung());
    }

    /// **Die Reservierung braucht beide Zahlen.** Jede allein sagt nichts — genau die Aussage,
    /// an der v1 scheiterte („eine Zahl, aus der eine Reservierung zu machen hiesse, die Periode
    /// zu erfinden"). Alle vier Kombinationen, ausgeschrieben statt abgezaehlt.
    #[test]
    fn reservierung_braucht_budget_und_periode() {
        let fall = |budget: u32, periode: u32| {
            let mut en = e("x", 1);
            en.period_us = periode;
            let mut raw = build(&[en], 64);
            // `build` schreibt immer budget 1000 — den Budget-Fall formen wir per Hand.
            raw[HEADER_LEN + 80..HEADER_LEN + 84].copy_from_slice(&budget.to_le_bytes());
            let v = accept(SystemManifest::parse(&raw).unwrap());
            v.entry(0).unwrap().hat_reservierung()
        };
        assert!(!fall(0, 0), "nichts gesagt ist keine Reservierung");
        assert!(!fall(1000, 0), "Budget ohne Periode ist keine Reservierung");
        assert!(!fall(0, 20_000), "Periode ohne Budget ist keine Reservierung");
        assert!(fall(1000, 20_000), "Budget mit Periode ist eine Reservierung");
    }

    /// **Fassung 2 ist genau 4 Byte laenger.** Kein Umbau, kein Deuten: `period_us` haengt an,
    /// alles andere bleibt an seinem Offset — ein v2-Eintrag ist ein v1-Eintrag mit Anhang.
    #[test]
    fn eintragsbreite_v2_ist_100() {
        assert_eq!(ENTRY_LEN, 96);
        assert_eq!(ENTRY_LEN_V2, 100);
        let mut en = e("x", 1);
        en.period_us = 20_000;
        let raw = build(&[en], 64);
        assert_eq!(raw.len(), HEADER_LEN + ENTRY_LEN_V2 + 64);
        // Die ersten 96 Byte liegen wie in v1: Name, IDs, Caps, Selektor, Dienst.
        let raw_v1 = build_v1(&[e("x", 1)], 64);
        assert_eq!(&raw[HEADER_LEN..HEADER_LEN + 96], &raw_v1[HEADER_LEN..HEADER_LEN + 96]);
    }

    /// **Jede fremde Kombination wird benannt abgewiesen, nicht gelesen.** (1, 100) ist kein v2,
    /// (2, 96) kein v1, und (3, 96) ist neuer als dieser Kernel — alle drei enden in
    /// `UnsupportedManifestFormat` mit den beiden Zahlen, nie in `BadManifest` und nie in einem
    /// Eintrag.
    #[test]
    fn fremde_kombinationen_werden_benannt_abgewiesen() {
        let kombination = |version: u16, breite: u32| {
            let mut raw = build(&[root("init", 1)], 64);
            raw[4..6].copy_from_slice(&version.to_le_bytes());
            raw[20..24].copy_from_slice(&breite.to_le_bytes());
            SystemManifest::parse(&raw).unwrap_err()
        };
        assert_eq!(
            kombination(1, 100),
            LoaderError::UnsupportedManifestFormat { format_version: 1, entry_len: 100 }
        );
        assert_eq!(
            kombination(2, 96),
            LoaderError::UnsupportedManifestFormat { format_version: 2, entry_len: 96 }
        );
        assert_eq!(
            kombination(3, 96),
            LoaderError::UnsupportedManifestFormat { format_version: 3, entry_len: 96 }
        );
        assert_eq!(
            kombination(2, 104),
            LoaderError::UnsupportedManifestFormat { format_version: 2, entry_len: 104 }
        );
    }

    #[test]
    fn parse_roundtrip() {
        let raw = build(&[root("init", 1), e("hello", 2)], 64);
        let m = SystemManifest::parse(&raw).unwrap();
        assert_eq!(m.format_version, MANIFEST_FORMAT_VERSION);
        assert_eq!(m.entry_len, ENTRY_LEN_V2);
        assert_eq!(m.signature_algorithm_id, SIG_ALG_ED25519);
        assert_eq!(m.manifest_version, 7);
        assert_eq!(m.entry_count, 2);
        assert_eq!(m.kernel_hash[0], 24);
        assert_eq!(m.key_id[0], 57);
        assert_eq!(m.message().len(), HEADER_LEN + 2 * ENTRY_LEN_V2);
        assert_eq!(m.signature().len(), 64);
        // message + signature partitionieren die Eingabe exakt.
        assert_eq!(m.message().len() + m.signature().len(), raw.len());

        let v = accept(m);
        assert_eq!(v.count(), 2);
        let e0 = v.entry(0).unwrap();
        assert_eq!(e0.name(), "init");
        assert_eq!(e0.program_id, 1);
        assert_eq!(e0.domain, DOMAIN_TRUSTED);
        assert_eq!(e0.iface_version, 3);
        assert_eq!(e0.initial_caps, CAP_LOADER | CAP_PD_CONTROL);
        assert!(e0.is_root_task());
        assert_eq!(e0.numa_node, 1);
        assert_eq!(e0.core_affinity, ANY_CORE);
        assert_eq!(e0.priority, 5);
        assert_eq!(e0.budget_us, 1000);
        assert_eq!(e0.period_us, 0); // der Bauer nannte keine Periode: nichts gesagt, s. Z11c
        assert!(!e0.hat_reservierung()); // Budget ohne Periode ist keine Reservierung
        assert_eq!(e0.sha256[0], 1);
        let e1 = v.entry(1).unwrap();
        assert_eq!(e1.name(), "hello");
        assert!(!e1.is_root_task());
        assert!(v.entry(2).is_none());
        assert_eq!(v.iter().count(), 2);
        assert_eq!(v.find(2).unwrap().name(), "hello");
        assert!(v.find(99).is_none());
        assert_eq!(v.root_task().unwrap().program_id, 1);
        assert_eq!(v.audit(), 0);
    }

    #[test]
    fn failed_signature_yields_no_entries() {
        let raw = build(&[root("init", 1)], 64);
        let m = SystemManifest::parse(&raw).unwrap();
        assert_eq!(m.verify_with(|_, _| false).unwrap_err(), LoaderError::Unverified);
    }

    #[test]
    fn verify_sees_exactly_message_and_signature() {
        let raw = build(&[root("init", 1)], 64);
        let m = SystemManifest::parse(&raw).unwrap();
        let msg_len = HEADER_LEN + ENTRY_LEN_V2;
        m.verify_with(|msg, sig| {
            assert_eq!(msg, &raw[..msg_len]);
            assert_eq!(sig, &raw[msg_len..]);
            true
        })
        .unwrap();
    }

    #[test]
    fn bad_magic_rejected() {
        let mut raw = build(&[root("init", 1)], 64);
        raw[0] ^= 0xFF;
        assert_eq!(SystemManifest::parse(&raw).unwrap_err(), LoaderError::BadManifest);
    }

    /// **Eine unbekannte Formatversion ist von kaputten Bytes UNTERSCHEIDBAR.**
    ///
    /// Die beiden führen zu entgegengesetzten Handlungen — Kernel aktualisieren gegen Herkunft
    /// untersuchen. Bis 2026-08-10 endeten sie in derselben Variante, und dieser Test hat die
    /// Ununterscheidbarkeit **festgeschrieben** statt sie zu beanstanden.
    #[test]
    fn bad_format_version_rejected() {
        let mut raw = build(&[root("init", 1)], 64);
        raw[4] = 0xEE;
        assert_eq!(
            SystemManifest::parse(&raw).unwrap_err(),
            LoaderError::UnsupportedManifestFormat {
                format_version: 0xEE,
                entry_len: ENTRY_LEN_V2 as u32,
            }
        );
    }

    /// Eine fremde Eintragsbreite wird **erkannt**, nicht um acht Bytes verrutscht gelesen.
    ///
    /// Der Fall, den die Regel verbietet: signiert ist die GANZE Nachricht. Läse ein v1-Lader
    /// 96 von 104 Byte je Eintrag, wäre das Ergebnis authentisch und unverstanden zugleich —
    /// `initial_caps` und `policy_flags` lägen dann auf fremden Bytes, und die Signatur stimmte
    /// darüber trotzdem.
    #[test]
    fn foreign_entry_width_is_detected_not_misread() {
        let mut raw = build(&[root("init", 1)], 64);
        raw[20..24].copy_from_slice(&104u32.to_le_bytes());
        assert_eq!(
            SystemManifest::parse(&raw).unwrap_err(),
            LoaderError::UnsupportedManifestFormat {
                format_version: MANIFEST_FORMAT_VERSION,
                entry_len: 104,
            }
        );
    }

    /// **Sprechprobe der Unterscheidung selbst.** Ohne sie belegte keiner der beiden Tests oben,
    /// dass die Absagen überhaupt verschieden sind: zwei Tests, die je eine Variante erwarten,
    /// wären auch dann grün, wenn beide Varianten denselben Wert hätten.
    #[test]
    fn version_refusal_differs_from_form_refusal() {
        let mut v = build(&[root("init", 1)], 64);
        v[4] = 0xEE;
        let mut f = build(&[root("init", 1)], 64);
        f[0] ^= 0xFF; // kaputte Magic
        let a = SystemManifest::parse(&v).unwrap_err();
        let b = SystemManifest::parse(&f).unwrap_err();
        assert_ne!(a, b, "Versionsabsage und Formabsage muessen unterscheidbar sein");
        assert_eq!(b, LoaderError::BadManifest);
    }

    /// Der Kopf wird **vollständig** abgewiesen — es gibt keinen Weg an einen Eintrag heran.
    ///
    /// Die Absage fällt vor der Signaturprüfung; deshalb ist sie **unauthentifiziert** und durch
    /// ein gekipptes Byte provozierbar. Was der signierte Kopf trägt, ist die andere Richtung:
    /// ein *angenommenes* Manifest kann keine untergeschobene Formatversion haben.
    #[test]
    fn unknown_format_yields_no_entries_at_all() {
        let mut raw = build(&[root("init", 1)], 64);
        raw[20..24].copy_from_slice(&104u32.to_le_bytes());
        assert!(SystemManifest::parse(&raw).is_err());
    }

    #[test]
    fn entry_count_beyond_data_rejected() {
        let mut raw = build(&[root("init", 1)], 64);
        raw[16..20].copy_from_slice(&5u32.to_le_bytes());
        assert_eq!(SystemManifest::parse(&raw).unwrap_err(), LoaderError::BadManifest);
    }

    #[test]
    fn entry_count_beyond_limit_rejected() {
        let mut raw = build(&[root("init", 1)], 64);
        raw[16..20].copy_from_slice(&(MAX_ENTRIES as u32 + 1).to_le_bytes());
        assert_eq!(SystemManifest::parse(&raw).unwrap_err(), LoaderError::BadManifest);
    }

    #[test]
    fn empty_signature_rejected() {
        let raw = build(&[root("init", 1)], 0);
        assert_eq!(SystemManifest::parse(&raw).unwrap_err(), LoaderError::BadManifest);
    }

    #[test]
    fn too_small_rejected() {
        assert_eq!(SystemManifest::parse(&[]).unwrap_err(), LoaderError::BadManifest);
        assert_eq!(
            SystemManifest::parse(&[0u8; HEADER_LEN - 1]).unwrap_err(),
            LoaderError::BadManifest
        );
    }

    #[test]
    fn zero_entries_parses_but_audit_complains() {
        let raw = build(&[], 64);
        let v = accept(SystemManifest::parse(&raw).unwrap());
        assert_eq!(v.count(), 0);
        assert_eq!(v.audit(), 1);
    }

    #[test]
    fn duplicate_program_id_caught() {
        let raw = build(&[root("init", 1), e("dup", 1)], 64);
        assert_eq!(accept(SystemManifest::parse(&raw).unwrap()).audit(), 2);
    }

    #[test]
    fn unknown_domain_caught() {
        let mut bad = e("weird", 2);
        bad.domain = 99;
        let raw = build(&[root("init", 1), bad], 64);
        assert_eq!(accept(SystemManifest::parse(&raw).unwrap()).audit(), 3);
    }

    #[test]
    fn unknown_authority_bit_caught() {
        let mut bad = e("future", 2);
        bad.initial_caps = 1 << 31;
        let raw = build(&[root("init", 1), bad], 64);
        assert_eq!(accept(SystemManifest::parse(&raw).unwrap()).audit(), 4);
        let mut bad2 = e("future", 2);
        bad2.policy_flags = 1 << 30;
        let raw2 = build(&[root("init", 1), bad2], 64);
        assert_eq!(accept(SystemManifest::parse(&raw2).unwrap()).audit(), 4);
    }

    #[test]
    fn two_root_tasks_yield_none_not_the_first() {
        let raw = build(&[root("a", 1), root("b", 2)], 64);
        let v = accept(SystemManifest::parse(&raw).unwrap());
        assert!(v.root_task().is_none());
        assert_eq!(v.audit(), 5);
    }

    #[test]
    fn no_root_task_caught() {
        let raw = build(&[e("hello", 1)], 64);
        assert_eq!(accept(SystemManifest::parse(&raw).unwrap()).audit(), 5);
    }

    /// Kein Eingabemuster darf den Parser zum Absturz bringen (das Gegenstück zum Kani-Beweis:
    /// derselbe Anspruch, hier als billiger Dauerlauf über strukturierte Mutationen).
    #[test]
    fn arbitrary_mutations_never_panic() {
        let base = build(&[root("init", 1), e("hello", 2)], 64);
        for i in 0..base.len() {
            for bit in 0..8 {
                let mut m = base.clone();
                m[i] ^= 1 << bit;
                if let Ok(p) = SystemManifest::parse(&m) {
                    if let Ok(v) = p.verify_with(|_, _| true) {
                        for k in 0..v.count() + 2 {
                            let _ = v.entry(k);
                        }
                        let _ = v.audit();
                        let _ = v.root_task();
                    }
                }
            }
        }
        // Und über abgeschnittene Präfixe.
        for n in 0..base.len() {
            let _ = SystemManifest::parse(&base[..n]);
        }
    }
}

// Formale Verifikation (Tier 1, Kani). Das Manifest ist die **erste** Struktur, die der Kernel von
// aussen anfasst — noch vor jeder Signaturpruefung. Crash-Freiheit auf beliebiger Eingabe ist
// deshalb keine Kür.
#[cfg(kani)]
mod kani_proofs {
    use super::*;

    // 200 reicht fuer jeden Pfad: `parse` kommt ueber die Laengenpruefung nur, wenn
    // `80 + count*96 < len <= MAXLEN`, also count <= 1; groessere `count` loesen immer den frueheren
    // Ruecksprung aus.
    const MAXLEN: usize = 200;

    /// **BEWEIS:** `SystemManifest::parse` paniert/OOBt **nie** — fuer beliebige Bytes + Laenge.
    #[kani::proof]
    #[kani::unwind(3)]
    fn parse_never_panics() {
        let data: [u8; MAXLEN] = kani::any();
        let len: usize = kani::any();
        kani::assume(len <= MAXLEN);
        let _ = SystemManifest::parse(&data[..len]);
    }

    /// **BEWEIS:** Bei Erfolg partitionieren `message()` + `signature()` die Eingabe **exakt**, die
    /// Signatur ist **nicht leer**, und jeder Index < `count()` liefert einen Eintrag ohne Panic.
    #[kani::proof]
    #[kani::unwind(3)]
    fn parse_partitions_and_entries_are_safe() {
        let data: [u8; MAXLEN] = kani::any();
        let len: usize = kani::any();
        kani::assume(len <= MAXLEN);
        if let Ok(m) = SystemManifest::parse(&data[..len]) {
            assert!(m.message().len() + m.signature().len() == len);
            assert!(!m.signature().is_empty());
            // Z11c: zwei Fassungen — die Nachrichtenlaenge folgt der geparsten Breite.
            assert!(
                m.message().len() == HEADER_LEN + (m.entry_count as usize) * m.entry_len
                    && (m.entry_len == ENTRY_LEN || m.entry_len == ENTRY_LEN_V2)
            );
            if let Ok(v) = m.verify_with(|_, _| true) {
                let n = v.count();
                kani::assume(n <= 1);
                let mut i = 0;
                while i < n {
                    assert!(v.entry(i).is_some());
                    i += 1;
                }
            }
        }
    }
}
