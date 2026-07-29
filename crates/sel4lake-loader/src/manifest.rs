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
//! `unsafe` (Crate-weit verboten), und jeder Fehlerpfad endet in [`LoaderError::BadManifest`]
//! statt in einem Zugriff daneben.
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
//!   4  format_version:u16     = 1
//!   6  signature_algorithm_id:u16   (Ed25519 = 1)
//!   8  flags:u32
//!  12  manifest_version:u32   (monoton; Anti-Downgrade)
//!  16  entry_count:u32
//!  20  entry_len:u32          = 96  (selbstbeschreibend: ein Kernel mit anderem Eintragsformat
//!                                    ERKENNT das, statt die Felder zu verrutschen)
//!  24  kernel_hash:[u8;32]    SHA-256 des Kernel-Codes -> bindet das Manifest an DIESEN Kernel
//!  56  key_id:[u8;16]
//!  72  reserved:[u8;8]        = 0
//!
//! Eintrag (96 B), entry_count mal:
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
//!  84  reserved:[u8;12]       = 0
//!
//! msg_len = 80 + entry_count * 96
//! msg_len  signature:[..]     (nicht leer; Laenge/Algorithmus prueft der Kernel-Verifier)
//! ```
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
/// Unterstützte Formatversion.
pub const MANIFEST_FORMAT_VERSION: u16 = 1;
/// Signaturalgorithmus Ed25519 (identisch nummeriert wie in [`crate::cert`]).
pub const SIG_ALG_ED25519: u16 = 1;

/// Länge des festen Kopfteils.
pub const HEADER_LEN: usize = 80;
/// Länge eines Eintrags (Formatversion 1).
pub const ENTRY_LEN: usize = 96;

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
/// Alle heute definierten Bits — was darüber hinaus gesetzt ist, versteht dieser Kernel nicht.
pub const CAP_KNOWN: u32 =
    CAP_LOADER | CAP_PD_CONTROL | CAP_NOTIFICATION | CAP_ENDPOINT | CAP_MMIO | CAP_IRQ | CAP_DMA;

// --- Politikfelder (Bitmaske `policy_flags`) — Format hier, Bedeutung in Strang B -------------

/// Exklusiver Farbstreifen (Cache-Partition) für diese Komponente.
pub const POLICY_EXCLUSIVE_STRIPE: u32 = 1 << 0;
/// **Der Root-Task.** Genau ein Eintrag darf das tragen — er bekommt die Wurzel-Caps (A-2.1).
pub const POLICY_ROOT_TASK: u32 = 1 << 1;
/// Die Kern-Affinität ist bindend, nicht ein Wunsch.
pub const POLICY_PINNED: u32 = 1 << 2;
/// Diese Komponente ist **nicht** im Betrieb austauschbar (A-4.5, Negativliste).
pub const POLICY_NO_HOTRELOAD: u32 = 1 << 3;
/// Alle heute definierten Bits.
pub const POLICY_KNOWN: u32 =
    POLICY_EXCLUSIVE_STRIPE | POLICY_ROOT_TASK | POLICY_PINNED | POLICY_NO_HOTRELOAD;

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
        let format_version = rd_u16(data, 4);
        if format_version != MANIFEST_FORMAT_VERSION {
            return Err(LoaderError::BadManifest);
        }
        // Selbstbeschreibende Eintragsbreite: passt sie nicht, ist es ein anderes Format. Das
        // hier ist der Unterschied zwischen "erkannt" und "um vier Bytes verrutscht gelesen".
        if rd_u32(data, 20) as usize != ENTRY_LEN {
            return Err(LoaderError::BadManifest);
        }
        let entry_count = rd_u32(data, 16);
        if entry_count as usize > MAX_ENTRIES {
            return Err(LoaderError::BadManifest);
        }
        // Overflow-sicher: entry_count <= MAX_ENTRIES, also passt das Produkt in usize.
        let msg_len = HEADER_LEN + (entry_count as usize) * ENTRY_LEN;
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
        // Bereits bei `parse` sichergestellt: `entries` ist genau `count * ENTRY_LEN` lang.
        let e = self.0.entries.get(i * ENTRY_LEN..(i + 1) * ENTRY_LEN)?;
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
    }

    fn e(name: &'static str, program_id: u32) -> E {
        E { name, program_id, domain: DOMAIN_USERLAND, initial_caps: 0, policy_flags: 0 }
    }

    /// Ein strukturell gültiges Manifest bauen (Signatur ist Dummy — der Parser prüft keine Krypto).
    fn build(entries: &[E], siglen: usize) -> Vec<u8> {
        let msg_len = HEADER_LEN + entries.len() * ENTRY_LEN;
        let mut v = vec![0u8; msg_len + siglen];
        v[0..4].copy_from_slice(&MANIFEST_MAGIC.to_le_bytes());
        v[4..6].copy_from_slice(&MANIFEST_FORMAT_VERSION.to_le_bytes());
        v[6..8].copy_from_slice(&SIG_ALG_ED25519.to_le_bytes());
        v[12..16].copy_from_slice(&7u32.to_le_bytes()); // manifest_version
        v[16..20].copy_from_slice(&(entries.len() as u32).to_le_bytes());
        v[20..24].copy_from_slice(&(ENTRY_LEN as u32).to_le_bytes());
        for i in 24..56 {
            v[i] = i as u8; // kernel_hash
        }
        for i in 56..72 {
            v[i] = (i + 1) as u8; // key_id
        }
        for (i, en) in entries.iter().enumerate() {
            let b = HEADER_LEN + i * ENTRY_LEN;
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
        }
    }

    fn accept<'a>(m: SystemManifest<'a>) -> Verified<'a> {
        m.verify_with(|_, _| true).unwrap()
    }

    #[test]
    fn parse_roundtrip() {
        let raw = build(&[root("init", 1), e("hello", 2)], 64);
        let m = SystemManifest::parse(&raw).unwrap();
        assert_eq!(m.format_version, MANIFEST_FORMAT_VERSION);
        assert_eq!(m.signature_algorithm_id, SIG_ALG_ED25519);
        assert_eq!(m.manifest_version, 7);
        assert_eq!(m.entry_count, 2);
        assert_eq!(m.kernel_hash[0], 24);
        assert_eq!(m.key_id[0], 57);
        assert_eq!(m.message().len(), HEADER_LEN + 2 * ENTRY_LEN);
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
        let msg_len = HEADER_LEN + ENTRY_LEN;
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

    #[test]
    fn bad_format_version_rejected() {
        let mut raw = build(&[root("init", 1)], 64);
        raw[4] = 0xEE;
        assert_eq!(SystemManifest::parse(&raw).unwrap_err(), LoaderError::BadManifest);
    }

    #[test]
    fn foreign_entry_width_is_detected_not_misread() {
        let mut raw = build(&[root("init", 1)], 64);
        raw[20..24].copy_from_slice(&104u32.to_le_bytes());
        assert_eq!(SystemManifest::parse(&raw).unwrap_err(), LoaderError::BadManifest);
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
            assert!(m.message().len() == HEADER_LEN + (m.entry_count as usize) * ENTRY_LEN);
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
