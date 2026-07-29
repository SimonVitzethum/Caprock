//! Kernel-Glue des **generischen Binary-Loaders** (ext-26, [ADR 0011](../../docs/adr/0011-binary-loader.md)).
//!
//! Die **reine**, bounds-geprüfte Parse-Logik (Boot-Archiv, ab L1 Minimal-ELF64) liegt im Crate
//! `sel4lake-loader` (0 `unsafe`, host-getestet). Hier liegt der **privilegierte** Teil, der RAM
//! liest und (ab L1) Segmente in Regionen kopiert, W^X mappt, VSpace/PD anlegt, Caps endowt und
//! Threads spawnt — alles über die bestehenden `system::`-Primitive (keine neuen Sonderrechte).
//!
//! **L0:** das Boot-Archiv aus dem reservierten RAM-Fenster lesen + die Module melden.

use crate::manifest_keys::{MANIFEST_KEYS, MIN_MANIFEST_VERSION};
use crate::trusted_keys::{MIN_VERSION, TRUSTED_KEYS};
use core::sync::atomic::{AtomicU64, Ordering};
use sel4lake_cap::CapPtr;
use sel4lake_hal::{print, println};
use sel4lake_loader::archive::Archive;
use sel4lake_loader::cert::{TrustedCert, SIG_ALG_ED25519, SIG_ED25519_LEN};
use sel4lake_loader::elf::ElfImage;
use sel4lake_loader::manifest::{
    Entry as ManifestEntry, SystemManifest, Verified, SIG_ALG_ED25519 as MAN_SIG_ALG_ED25519,
};
use sel4lake_loader::{LoaderError, Program, DOMAIN_TRUSTED, DOMAIN_USERLAND};
use sel4lake_mem::Rights;
use sel4lake_microkit::Domain;
use sel4lake_sched::ThreadId;
use sel4lake_trust::{fingerprint, sha256, verify_sig};

/// Größe des reservierten RAM-Fensters für das Boot-Archiv (oben in RAM, vom `PhysAllocator`
/// ausgenommen — siehe `init_mem`-Aufruf in `main.rs`). QEMU legt das Archiv per
/// `-device loader,addr=MOD_BASE` hierher; der Wert MUSS zu `test-qemu.sh` passen.
pub const MOD_WINDOW: u64 = 0x0100_0000; // 16 MiB

/// Basis des Archiv-Fensters = `ram_end - MOD_WINDOW` für die QEMU-`virt`-Maschine mit 4 GiB RAM
/// (RAM `0x4000_0000` + 4 GiB = `0x1_4000_0000`). Liegt in der statisch identity-gemappten
/// Normal-WB-Region (GiB 2..9), also EL1-lesbar.
pub const MOD_BASE: u64 = 0x0001_4000_0000 - MOD_WINDOW; // 0x1_3F00_0000

/// **Wo das Boot-Archiv liegt** — zur Laufzeit gesetzt, nicht fest verdrahtet (A-1.1).
///
/// Auf ARM ist die Lage eine Verabredung mit dem Testaufbau (`-device loader,addr=MOD_BASE` in ein
/// reserviertes Fenster); auf x86 legt der Bootloader das Archiv als Multiboot-Modul dorthin, wo
/// er will, und sagt die Adresse erst beim Start. Also entscheidet der Hochlauf, nicht der
/// Übersetzer. Fehlt die Angabe, bleibt es beim ARM-Fenster (unverändertes Verhalten dort) bzw.
/// bei „kein Archiv".
static ARCHIVE_BASE: AtomicU64 = AtomicU64::new(0);
static ARCHIVE_LEN: AtomicU64 = AtomicU64::new(0);

/// Die Lage des Boot-Archivs melden. Muss **vor** der ersten Archiv-Benutzung laufen und
/// beschreibt einen Bereich, den der `PhysAllocator` nicht vergeben darf.
pub fn set_archive_span(base: u64, len: u64) {
    ARCHIVE_BASE.store(base, Ordering::Relaxed);
    ARCHIVE_LEN.store(len, Ordering::Relaxed);
}

/// Die aktuell geltende Lage des Archivs (`(0, 0)` = keine).
pub fn archive_span() -> (u64, u64) {
    let base = ARCHIVE_BASE.load(Ordering::Relaxed);
    if base != 0 {
        return (base, ARCHIVE_LEN.load(Ordering::Relaxed));
    }
    // Rückfall: das statisch reservierte ARM-Fenster. Auf x86 gibt es kein solches Fenster —
    // dort heißt „nicht gemeldet" genau das, und `read_archive` liefert None statt an einer
    // erratenen Adresse zu lesen.
    #[cfg(target_arch = "aarch64")]
    {
        (MOD_BASE, MOD_WINDOW)
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        (0, 0)
    }
}

/// Das Boot-Archiv aus dem reservierten Fenster lesen + **vollständig validieren**. `None`, wenn
/// kein gültiges Archiv vorliegt (fehlend/beschädigt) — der Kernel läuft dann ohne externe Module.
pub fn read_archive() -> Option<Archive<'static>> {
    let (base, len) = archive_span();
    if base == 0 || len == 0 {
        return None;
    }
    // SAFETY: `[base, base+len)` ist reservierter, identity-gemappter Normal-RAM — auf ARM das
    // statische Fenster (vom PhysAllocator ausgenommen, s. `system::init_mem`-Aufruf), auf x86 der
    // vom Bootloader gemeldete Modulbereich, der vor der ersten Allokation aus der Freiliste
    // ausgeschnitten wurde (`arch::x86_64::bringup`). Nur **lesender** Zugriff; der Parser
    // (`sel4lake-loader`) ist vollständig bounds-geprüft und panik-frei.
    let bytes = unsafe { core::slice::from_raw_parts(base as *const u8, len as usize) };
    Archive::parse(bytes).ok()
}

// ================================================================================================
// System-Manifest (A-1.2 – A-1.4): das eine Autoritätsdokument des Boot-Images
// ================================================================================================

extern "C" {
    /// Anfang des Kernel-Codes (Linker-Symbol, beide Architekturen).
    static __text_start: u8;
    /// Ende der Kernel-Konstanten (Linker-Symbol, beide Architekturen).
    static __rodata_end: u8;
}

/// **SHA-256 des Kernel-Codes** — `[__text_start, __rodata_end)` (A-1.3).
///
/// Genau diese Spanne, und nicht „das Image": sie ist zur Laufzeit **unveränderlich** (`.text` ist
/// R-X, `.rodata` R--, beides unter CR0.WP bzw. dem ARM-Pendant), sie ist auf beiden Architekturen
/// zusammenhängend, und sie steht im ELF byte-identisch so, wie sie später im Speicher liegt
/// (identity geladen, keine Relokation). Ein Hash über `.data`/`.bss` wäre schon nach dem ersten
/// Boot-Schritt ein anderer — der Kernel könnte ihn gar nicht reproduzieren.
///
/// Das Gegenstück auf der Werkzeugseite ist `tools/kernel_hash.py`; beide Definitionen müssen
/// zusammen geändert werden, sonst passt kein Manifest mehr auf keinen Kernel.
pub fn kernel_code_hash() -> [u8; 32] {
    // SAFETY: Linker-Symbole; es werden nur ihre Adressen gelesen und daraus ein **lesender**
    // Slice über den identity-gemappten, im Betrieb schreibgeschützten Kernel-Code gebildet.
    let (start, end) = unsafe {
        (
            &__text_start as *const u8 as usize,
            &__rodata_end as *const u8 as usize,
        )
    };
    if end <= start {
        return [0u8; 32]; // kann nur bei kaputtem Linker-Skript passieren; kein Panic im Kernel
    }
    // SAFETY: wie oben — die Spanne liegt vollständig im geladenen Kernel-Image.
    let bytes = unsafe { core::slice::from_raw_parts(start as *const u8, end - start) };
    sha256(bytes)
}

/// Das **System-Manifest** aus dem Archiv lesen und **vollständig prüfen** (A-1.3).
///
/// Reihenfolge, und sie ist nicht verhandelbar:
/// 1. strukturell parsen (feste Breiten, Längenpräfixe — noch keine Auswertung);
/// 2. Algorithmus + Signaturlänge;
/// 3. `key_id` in der read-only DB, nicht zurückgezogen, DB-selbstkonsistent;
/// 4. **Signatur über die gesamte Nachricht**;
/// 5. **Bindung an dieses Kernel-Image** (`kernel_hash`);
/// 6. Anti-Downgrade (`manifest_version >= MIN_MANIFEST_VERSION`).
///
/// Erst danach gibt es Einträge — das erzwingt der Typ [`Verified`], nicht die Disziplin des
/// Aufrufers: [`SystemManifest::parse`] liefert keinen einzigen Eintrag.
pub fn read_manifest() -> Option<Verified<'static>> {
    let archive = read_archive()?;
    let raw = archive.system_manifest();
    if raw.is_empty() {
        return None;
    }
    let m = SystemManifest::parse(raw).ok()?; // (1)
    if m.signature_algorithm_id != MAN_SIG_ALG_ED25519 {
        return None; // (2)
    }
    let sig: &[u8; SIG_ED25519_LEN] = m.signature().try_into().ok()?;
    let key = MANIFEST_KEYS.iter().find(|k| k.key_id == m.key_id)?; // (3)
    if key.revoked || fingerprint(&key.pubkey) != key.key_id {
        return None;
    }
    if m.kernel_hash != kernel_code_hash() {
        return None; // (5) — vorgezogen geprüft, aber die Signatur entscheidet in `verify_with`
    }
    if m.manifest_version < MIN_MANIFEST_VERSION {
        return None; // (6)
    }
    let pubkey = key.pubkey;
    m.verify_with(move |msg, _| verify_sig(&pubkey, msg, sig)).ok() // (4)
}

/// Puffergröße für die manipulierte Manifest-Kopie im [`manifest_audit`]-Live-Oracle.
const MANIFEST_AUDIT_MAX: usize = 2048;

/// **Manifest-Audit** — strukturelle Selbstkonsistenz der Key-DB **plus** ein Live-Oracle des
/// aktiven Gates. `0` = sauber, sonst ein Anomalie-Code:
///
/// * 1 — Key-DB leer (kein Manifest könnte je angenommen werden).
/// * 2 — ein `key_id != fingerprint(pubkey)` (DB nicht selbst-zertifizierend).
/// * 3 — doppelte `key_id`.
/// * 4 — es liegt ein Manifest im Archiv, aber es wird **abgelehnt** (Gate/DB/Kernel-Hash passen
///   nicht zusammen). Das ist die häufigste echte Ursache: neu gebauter Kernel, altes Manifest.
/// * 5 — Live-Oracle: eine **manipulierte** Kopie wird **angenommen** (Gate setzt nicht durch).
/// * 6 — das angenommene Manifest ist inhaltlich widersprüchlich ([`Verified::audit`] ≠ 0);
///   der Inhaltscode steht dann in den oberen 16 Bit.
///
/// **Warum Code 5 hier stehen muss:** ein Schweigen darf nicht als Erfolg durchgehen. Ohne das
/// Oracle bewiese ein grünes Audit nur, dass nichts geprüft wurde — dieselbe Verwechslung, die
/// dieses Projekt bei der leeren SMMU-Event-Queue schon einmal bezahlt hat.
pub fn manifest_audit() -> u32 {
    if MANIFEST_KEYS.is_empty() {
        return 1;
    }
    for (i, k) in MANIFEST_KEYS.iter().enumerate() {
        if fingerprint(&k.pubkey) != k.key_id {
            return 2;
        }
        for k2 in &MANIFEST_KEYS[i + 1..] {
            if k2.key_id == k.key_id {
                return 3;
            }
        }
    }
    let Some(archive) = read_archive() else { return 0 };
    let raw = archive.system_manifest();
    if raw.is_empty() {
        return 0; // kein Manifest vorhanden -> nichts durchzusetzen (die Startmenge bleibt leer)
    }
    let Some(v) = read_manifest() else { return 4 };
    // Live-Oracle: ein signiertes Byte kippen -> die Signatur MUSS brechen.
    if raw.len() <= MANIFEST_AUDIT_MAX {
        let mut buf = [0u8; MANIFEST_AUDIT_MAX];
        buf[..raw.len()].copy_from_slice(raw);
        buf[24] ^= 0x01; // erstes Byte des kernel_hash -- signiert UND inhaltlich bindend
        let tampered = SystemManifest::parse(&buf[..raw.len()])
            .ok()
            .and_then(|m| {
                let sig: &[u8; SIG_ED25519_LEN] = m.signature().try_into().ok()?;
                let key = MANIFEST_KEYS.iter().find(|k| k.key_id == m.key_id)?;
                let pubkey = key.pubkey;
                m.verify_with(move |msg, _| verify_sig(&pubkey, msg, sig)).ok()
            });
        if tampered.is_some() {
            return 5;
        }
    }
    let content = v.audit();
    if content != 0 {
        return 6 | (content << 16);
    }
    0
}

/// Das Manifest melden: was steht drin, und hält das Gate? Reines Lesen; greift nicht in den
/// Boot-Ablauf ein. Gibt den Audit-Code zurück (0 = sauber), damit der Aufrufer ihn bewerten kann.
pub fn manifest_report() -> u32 {
    let audit = manifest_audit();
    match read_manifest() {
        Some(v) => {
            let h = v.header();
            println!(
                "manifest: v{} vom Schluessel {:02x}{:02x}{:02x}{:02x}.., an dieses Kernel-Image gebunden, {} Eintrag/Eintraege",
                h.manifest_version, h.key_id[0], h.key_id[1], h.key_id[2], h.key_id[3], v.count()
            );
            for e in v.iter() {
                print!(
                    "manifest:   [{}]{} dom={} iface=v{} caps={:#06x} politik={:#x} prio={} numa={} kern=",
                    e.program_id, e.name(), e.domain, e.iface_version, e.initial_caps,
                    e.policy_flags, e.priority, e.numa_node
                );
                if e.core_affinity == sel4lake_loader::manifest::ANY_CORE {
                    print!("beliebig");
                } else {
                    print!("{}", e.core_affinity);
                }
                println!(" budget={}us{}", e.budget_us, if e.is_root_task() { " ROOT" } else { "" });
            }
        }
        None => {
            // Bei Ablehnung ist die haeufigste echte Ursache "neuer Kernel, altes Manifest".
            // Deshalb steht hier, WORAN gebunden wurde -- sonst raet man.
            let k = kernel_code_hash();
            println!(
                "manifest: kein angenommenes System-Manifest (Audit-Code {audit}; dieser Kernel-Code-Hash beginnt {:02x}{:02x}{:02x}{:02x})",
                k[0], k[1], k[2], k[3]
            );
            if let Some(a) = read_archive() {
                let raw = a.system_manifest();
                if !raw.is_empty() {
                    match SystemManifest::parse(raw) {
                        Ok(m) => println!(
                            "manifest:   im Archiv: v{} alg={} key={:02x}{:02x}{:02x}{:02x}.. gebunden an {:02x}{:02x}{:02x}{:02x}.. ({} Eintraege, {} B Signatur)",
                            m.manifest_version, m.signature_algorithm_id,
                            m.key_id[0], m.key_id[1], m.key_id[2], m.key_id[3],
                            m.kernel_hash[0], m.kernel_hash[1], m.kernel_hash[2], m.kernel_hash[3],
                            m.entry_count, m.signature().len()
                        ),
                        Err(_) => println!("manifest:   im Archiv liegen {} B, die nicht parsen", raw.len()),
                    }
                }
            }
        }
    }
    println!(
        "manifest: {} (Signatur ueber die GESAMTE Nachricht, an DIESES Kernel-Image gebunden, Anti-Downgrade; manipulierte Kopie wird abgewiesen)",
        if audit == 0 { "ALL PASS" } else { "FAILURES" }
    );
    audit
}

// ================================================================================================
// Root-Task (A-2.1): das erste Programm, und ab da ist jede Fähigkeit ein Userland-Programm
// ================================================================================================

/// **Boot-Argument** eines geladenen Programms: `(Anzahl der Programme << 32) | eigener Index`.
///
/// Das ist absichtlich das Minimum. Ein geladenes Programm muss wissen, wo es selbst in der
/// Startmenge steht und wie groß sie ist — sonst kann es „alle außer mir" nicht ausdrücken und
/// bräuchte stattdessen eine hart verdrahtete Indexverabredung mit dem Testskript. Alles
/// **darüber hinaus** gehört hinter eine Capability und nicht in ein Register: ein Boot-Info-Block,
/// den jedes Programm ungefragt lesen kann, wäre eine Autoritätsquelle neben dem Manifest.
pub const fn boot_arg(index: usize, count: usize) -> usize {
    (count << 32) | (index & 0xffff_ffff)
}

/// Warum ein Root-Task nicht startete. Bewusst **unterscheidbar**: „lädt nicht" ist als Diagnose
/// wertlos, und der Unterschied zwischen „kein Manifest" und „Hash passt nicht" ist der Unterschied
/// zwischen einem Aufbaufehler und einem Integritätsbefund.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RootTaskError {
    /// Kein Boot-Archiv (fehlt/beschädigt).
    NoArchive,
    /// Kein angenommenes System-Manifest (fehlt, Signatur/Bindung/Downgrade — s. [`manifest_audit`]).
    NoManifest,
    /// Das Manifest benennt keinen **eindeutigen** Root-Task (keiner oder mehrere).
    NoRootEntry,
    /// Der im Manifest benannte Eintrag liegt nicht im Archiv.
    NotInArchive,
    /// **Der Modul-Hash weicht ab.** Das Manifest sagt, wie das Modul auszusehen hat; es sieht
    /// anders aus. Kein Grenzfall, sondern der Fall, für den der Hash da ist.
    HashMismatch,
    /// Manifest und Archiv widersprechen sich in der Domäne des Eintrags.
    DomainMismatch,
    /// Das Manifest verlangt Autorität, die dieser Kernel nicht **erteilen** kann. Fail-closed:
    /// lieber gar nicht starten als mit stillschweigend weniger Rechten (ein halb bevollmächtigter
    /// Dienst scheitert später an einer Stelle, die niemand mit dem Manifest in Verbindung bringt).
    UnsupportedAuthority,
    /// Anfangs-Caps ließen sich nicht erzeugen (Kernel-Ressourcen erschöpft).
    NoResources,
    /// **Der Loader hat das Image abgelehnt** — mit dem genauen Grund.
    ///
    /// Dass dieser Grund mitgeführt wird, ist keine Bequemlichkeit. Beim ersten Lauf meldete
    /// dieser Pfad nur „LoadFailed", und die tatsächliche Ursache (der ELF-Parser kannte nur
    /// `EM_AARCH64`, das x86-Binary war für ihn schlicht kein ELF) war daran nicht zu erkennen.
    /// Eine Fehlerklasse, die alles einsammelt, ist als Diagnose wertlos.
    Rejected(LoaderError),
}

/// Die Anfangs-Caps eines Manifest-Eintrags erzeugen und als `(Slot, Cap)`-Liste ablegen.
///
/// **Slot-Konvention** (Teil der Loader-ABI, `docs/invariants.md`):
/// `0` = Loader-Cap, `1` = Notification, `2` = Endpoint. Ein nicht angeforderter Cap lässt seinen
/// Slot leer — die Nummern verschieben sich also nicht, je nachdem was angefragt wurde. (Genau das
/// wäre die Art Detail, an der ein Programm später still das falsche Objekt anspricht.)
fn endow_from_manifest(e: &ManifestEntry) -> Result<[Option<(usize, CapPtr)>; 3], RootTaskError> {
    use sel4lake_loader::manifest as man;
    let mut out: [Option<(usize, CapPtr)>; 3] = [None; 3];
    // Bis hierher erzeugte Caps wieder abräumen, wenn ein späterer Schritt scheitert — sie sind
    // dann nirgends installiert und würden sonst in der geteilten Tabelle belegt bleiben.
    fn undo(out: &[Option<(usize, CapPtr)>; 3]) {
        for &(_, cap) in out.iter().flatten() {
            let _ = crate::system::cap_delete(cap);
        }
    }
    // Was dieser Kernel heute NICHT erteilen kann, wird abgewiesen statt weggelassen.
    //
    // `CAP_PD_CONTROL` gehört dazu, und der Grund ist strukturell: eine PdControl-Cap bezeichnet
    // **eine bestimmte Ziel-PD**, die es beim Start des Root-Tasks noch gar nicht gibt — sie
    // entstünde erst mit dem, was er selbst lädt. Der ehrliche Weg wäre, dass `SYS_LOAD` die
    // PdControl-Cap der neu erzeugten PD zurückgibt; das ist eine ABI-Erweiterung und steht in
    // todo-A (A-3.2, wählbarer Empfangs-Slot). Bis dahin: nicht erteilbar, also nicht behaupten.
    const GRANTABLE: u32 = man::CAP_LOADER | man::CAP_NOTIFICATION | man::CAP_ENDPOINT;
    if e.initial_caps & !GRANTABLE != 0 {
        return Err(RootTaskError::UnsupportedAuthority);
    }
    if e.initial_caps & man::CAP_LOADER != 0 {
        // Quelle 0 = Boot-Archiv (die einzige heute).
        match crate::system::install_loader_cap(0, Rights::RWX) {
            Ok(cap) => out[0] = Some((0, cap)),
            Err(_) => return Err(RootTaskError::NoResources),
        }
    }
    if e.initial_caps & man::CAP_NOTIFICATION != 0 {
        let r = crate::system::create_notification().and_then(|ntfn| {
            // **Gebadgt**, nicht nackt: `SYS_SIGNAL` verodert das Badge der benutzten CAP in
            // `pending` -- das Nachrichtenwort spielt keine Rolle. Ohne Badge waere jedes Signal
            // ein ODER mit 0, und der Kernel saehe nicht, dass ueberhaupt jemand gerufen hat.
            let cap =
                crate::system::install_notification_cap_badged(ntfn as u32, Rights::RWX, ROOT_NTFN_BADGE)
                    .ok()?;
            ROOT_NTFN.store(ntfn as u64 + 1, Ordering::Relaxed); // +1, damit 0 = "keine" bleibt
            Some(cap)
        });
        match r {
            Some(cap) => out[1] = Some((1, cap)),
            None => {
                undo(&out);
                return Err(RootTaskError::NoResources);
            }
        }
    }
    if e.initial_caps & man::CAP_ENDPOINT != 0 {
        let r = crate::system::create_endpoint().and_then(|ep| {
            crate::system::install_endpoint_cap(ep as u32, Rights::RWX).ok()
        });
        match r {
            Some(cap) => out[2] = Some((2, cap)),
            None => {
                undo(&out);
                return Err(RootTaskError::NoResources);
            }
        }
    }
    Ok(out)
}

/// **Badge der Root-Notification.** Ein hohes Bit, damit es sich mit den Badges, die der Root-Task
/// selbst an seine Kinder vergibt (unteres Wort), nicht vermischt: ein akkumuliertes Badge soll
/// beide Aussagen getrennt lesbar lassen, statt sie ineinander laufen zu lassen.
pub const ROOT_NTFN_BADGE: u64 = 1 << 32;

/// Die Notification, die dem Root-Task endowt wurde (`0` = keine). Der Selbsttest liest daran ab,
/// ob er wirklich gelaufen ist — der Kernel hat sonst kein Fenster in einen isolierten Prozess.
static ROOT_NTFN: AtomicU64 = AtomicU64::new(0);

/// Die Notification-Id des Root-Tasks (`None` = keine endowt).
pub fn root_notification() -> Option<usize> {
    match ROOT_NTFN.load(Ordering::Relaxed) {
        0 => None,
        n => Some((n - 1) as usize),
    }
}

/// **Den Root-Task starten** (A-2.1) — der seL4-Weg: ein Startprogramm aus der Startmenge laden
/// und ihm die Wurzel-Caps übergeben.
///
/// Ab hier ist jede weitere Fähigkeit ein **Userland-Programm** statt eines Kernel-Patches; das
/// ist der eigentliche Hebel des Plans, und deshalb steht dieser Aufruf im Hochlauf und nicht
/// hinter einem Test-Feature.
///
/// Reihenfolge, und jede Stufe hat einen Grund:
/// 1. Manifest (signiert, an dieses Kernel-Image gebunden) — **wer** darf **was**;
/// 2. Eintrag mit [`POLICY_ROOT_TASK`](sel4lake_loader::manifest::POLICY_ROOT_TASK), eindeutig;
/// 3. das Modul im Archiv finden und seinen **Hash gegen das Manifest** prüfen. Ohne diesen
///    Schritt sagte das Manifest nur, *dass* etwas geladen wird, nicht *was*;
/// 4. Domäne von Manifest und Archiv müssen übereinstimmen (zwei Quellen, eine Aussage);
/// 5. Anfangs-Caps erzeugen — nicht erteilbare Autorität ist ein Fehler, keine Kürzung;
/// 6. laden (das Trust-Gate aus ADR 0014 gilt zusätzlich weiter: TrustedSAS braucht sein Zertifikat).
pub fn start_root_task() -> Result<(ThreadId, usize), RootTaskError> {
    let archive = read_archive().ok_or(RootTaskError::NoArchive)?;
    let man = read_manifest().ok_or(RootTaskError::NoManifest)?;
    let entry = man.root_task().ok_or(RootTaskError::NoRootEntry)?;
    let index = (0..archive.count())
        .find(|&i| archive.program(i).map(|p| p.program_id) == Some(entry.program_id))
        .ok_or(RootTaskError::NotInArchive)?;
    let prog = archive.program(index).ok_or(RootTaskError::NotInArchive)?;
    if sha256(prog.elf) != entry.sha256 {
        return Err(RootTaskError::HashMismatch);
    }
    if prog.domain != entry.domain {
        return Err(RootTaskError::DomainMismatch);
    }
    let caps = endow_from_manifest(&entry)?;
    let arg = boot_arg(index, archive.count());
    // Die belegten Einträge zu einem dichten Slice verdichten. `CapPtr` hat bewusst keinen
    // öffentlichen Konstruktor (ein fabrizierbarer Cap-Handle wäre eine Einladung), also dient
    // der erste erzeugte Cap als Füllwert — ohne Cap gibt es nichts zu verdichten.
    let Some(first) = caps.iter().flatten().next().copied() else {
        return load_image(&prog, &[], arg).map_err(RootTaskError::Rejected);
    };
    let mut endow = [first; 3];
    let mut n = 0usize;
    for &c in caps.iter().flatten() {
        endow[n] = c;
        n += 1;
    }
    match load_image(&prog, &endow[..n], arg) {
        Ok(r) => Ok(r),
        Err(e) => {
            // Die erzeugten Endowment-Caps sind noch nirgends installiert -> sonst lecken sie.
            for &(_, cap) in &endow[..n] {
                let _ = crate::system::cap_delete(cap);
            }
            Err(RootTaskError::Rejected(e))
        }
    }
}

/// Den Root-Task starten **und melden**. Gibt `true`, wenn er läuft.
pub fn start_root_task_reported() -> bool {
    match start_root_task() {
        Ok((tid, pd)) => {
            println!(
                "root    : ALL PASS (Startprogramm aus der Startmenge geladen: Thread {:?}, PD {pd}; Manifest-Hash geprueft, Wurzel-Caps endowt)",
                tid.to_raw()
            );
            true
        }
        Err(e) => {
            println!("root    : FAILURES ({e:?}) -- kein Root-Task, der Kernel hat nichts auszufuehren");
            false
        }
    }
}

/// **L0-Selbsttest/Telemetrie:** das Archiv lesen + die gefundenen Module melden. Greift NICHT in
/// den Boot-Ablauf ein (reines Lesen); das eigentliche Laden folgt ab L1.
pub fn probe() {
    match read_archive() {
        Some(a) => {
            print!("archive : {} Modul(e):", a.count());
            for p in a.iter() {
                // Stabile program_id (Verfeinerung 4) + Name + Version melden.
                print!(" [{}]{}@v{}", p.program_id, p.name(), p.version);
            }
            println!(" -> ALL PASS");
        }
        None => println!("archive : kein gueltiges Boot-Archiv (0 Module, FAILURES)"),
    }
}

/// **Die öffentliche Loader-API** (ADR 0011, Verfeinerung 1): ein quellen-agnostisches `Program`
/// laden + starten. Parst das ELF (Safe Rust, [`ElfImage::parse`]), bildet die Domäne ab und
/// delegiert den privilegierten Teil (Segmente kopieren/mappen, VSpace/PD, Spawn) an
/// [`crate::system::load_elf`]. `endow` = initiale Caps für die neue PD (`(Slot, Cap)`), in L2 aus
/// dem Manifest abgeleitet. Gibt `(ThreadId, pd)` der neuen, laufbereiten PD.
///
/// `boot_arg` landet im ersten Argument von `_start` — die **einzige** Information, die ein
/// geladenes Programm ohne Cap-Aufruf bekommt. Sie muss deshalb klein und selbsterklärend sein
/// (heute: Lage des Programms in der Startmenge, s. [`start_root_task`]); alles Weitere gehört
/// hinter eine Capability.
///
/// L1: nur **isolierte EL0-Domänen** (UserLand/HardwareLand). TrustedSAS (EL1) ist signatur-gegatet
/// (L3) und wird hier abgelehnt (`UnsupportedDomain`).
pub fn load_image(
    prog: &Program,
    endow: &[(usize, CapPtr)],
    boot_arg: usize,
) -> Result<(ThreadId, usize), LoaderError> {
    if !verify_image(prog) {
        return Err(LoaderError::Unverified);
    }
    let domain = match prog.domain {
        DOMAIN_USERLAND => Domain::UserLand,
        // TrustedSAS laeuft GELADEN als EL0-ISOLIERTE PD (nicht EL1): hardware-isoliert, behaelt
        // aber seine Trust-Stufe (darf PdControl/Loader-Caps halten). domain_audit erlaubt isolierte
        // TrustedSAS-PDs. So sind ALLE Domaenen sicher extern ladbar (auch die Trusted-Testdienste).
        DOMAIN_TRUSTED => Domain::TrustedSas,
        // HardwareLand braucht eine vor-erstellte Backend-PD (Partner-Bindung + Kanal) ->
        // ueber `load_program_into_pd`, NICHT hier (eine bare HardwareLand-PD bricht domain_audit).
        _ => return Err(LoaderError::UnsupportedDomain),
    };
    let img = ElfImage::parse(prog.elf)?; // Safe-Rust-Validierung; unsafe erst im Kopier-Glue
    crate::system::load_elf(&img, domain, endow, boot_arg).ok_or(LoaderError::NoResources)
}

/// Ein Programm in eine **vor-erstellte** PD laden (ext-26, L3) — fuer HardwareLand-Backends
/// (Partner-Bindung + Kanal vom Aufrufer aufgesetzt) und kuenftige spezialisierte PDs. Die
/// Domaenen-Policy traegt die PD selbst (`install_cap_checked` + `domain_audit`). Trust-Gate
/// ([`verify_image`]) gilt auch hier.
pub fn load_program_into_pd(
    prog: &Program,
    pd: usize,
    endow: &[(usize, CapPtr)],
    boot_arg: usize,
) -> Result<ThreadId, LoaderError> {
    if !verify_image(prog) {
        return Err(LoaderError::Unverified);
    }
    let img = ElfImage::parse(prog.elf)?;
    crate::system::load_into_pd(&img, pd, endow, boot_arg).ok_or(LoaderError::NoResources)
}

/// **Integritaets-/Trust-Gate** (ADR 0011 §7 + ADR 0014, ext-28): darf dieses Image geladen werden?
///
/// **Alle** geladenen Prozesse laufen **EL0-isoliert** (`load_into_pd` spawnt stets EL0; selbst eine
/// TrustedSAS-PD ist geladen EL0-isoliert) — ein fehlerhaftes/boesartiges Image faultet daher nur
/// sich selbst, ohne Privileg-Eskalation. Daher braucht **UserLand/HardwareLand kein Zertifikat**
/// (unveraendert; ihre Isolation traegt die Hardware).
///
/// **TrustedSAS** dagegen darf seine Trust-Stufe behalten (PdControl-/Loader-Caps halten duerfen) —
/// deshalb wird es **nur mit einem gueltigen, auf genau dieses Binary gebundenen Ed25519-Zertifikat**
/// geladen ([`verify_trusted_cert`]). Ohne gueltiges Zertifikat: [`LoaderError::Unverified`].
fn verify_image(prog: &Program) -> bool {
    if prog.domain != DOMAIN_TRUSTED {
        return true; // UserLand/HardwareLand: hardware-isoliert, kein Zertifikat noetig.
    }
    verify_trusted_cert(prog)
}

/// **TrustedSAS-Zertifikatspruefung** (ext-28, ADR 0014). Akzeptiert das Image **nur**, wenn das
/// mitgelieferte Zertifikat in **jeder** Hinsicht gueltig ist. Rein verifizierend (kein Heap, kein
/// RNG); die read-only Key-DB ([`TRUSTED_KEYS`]) ist in den Kernel kompiliert.
///
/// Geprueft (alle Felder sind durch die Signatur ueber die **gesamte** Nachricht geschuetzt):
/// 1. Zertifikat parst (Magic/Formatversion/Laengen — [`TrustedCert::parse`]).
/// 2. Signaturalgorithmus = Ed25519 **und** Signaturlaenge = 64.
/// 3. `key_id` in der Key-DB vorhanden, **nicht** zurueckgezogen, und DB-selbstkonsistent
///    (`key_id == fingerprint(pubkey)`).
/// 4. Ed25519-Signatur ueber die **gesamte** Nachricht gueltig (`verify_strict`).
/// 5. **Binary-Bindung:** `binary_hash == SHA-256(ELF)` und `manifest_hash == SHA-256(Manifest)`.
/// 6. **Identitaet:** `program_id`/`version` stimmen mit dem Archiv-Eintrag ueberein.
/// 7. **Anti-Downgrade:** `version >= MIN_VERSION[program_id]` (falls gepflegt).
/// 8. **Unsafe-Audit:** `unsafe_status == ALL_PASS` (Programm forbid-rein, Projekt sauber,
///    Allowlist erfuellt).
fn verify_trusted_cert(prog: &Program) -> bool {
    let Ok(cert) = TrustedCert::parse(prog.cert) else {
        return false; // (1) fehlend/kaputt
    };
    // (2) Algorithmus + Signaturlaenge passend zum Verfahren.
    if cert.signature_algorithm_id != SIG_ALG_ED25519 {
        return false;
    }
    let Ok(sig): Result<&[u8; SIG_ED25519_LEN], _> = cert.signature().try_into() else {
        return false;
    };
    // (3) Key-ID-Lookup in der read-only Key-DB; Revocation + DB-Selbstkonsistenz.
    let Some(key) = TRUSTED_KEYS.iter().find(|k| k.key_id == cert.key_id) else {
        return false;
    };
    if key.revoked || fingerprint(&key.pubkey) != key.key_id {
        return false;
    }
    // (4) Signatur ueber die GESAMTE Nachricht.
    if !verify_sig(&key.pubkey, cert.message(), sig) {
        return false;
    }
    // (5) Binary-/Manifest-Bindung.
    if cert.binary_hash != sha256(prog.elf) || cert.manifest_hash != sha256(prog.manifest) {
        return false;
    }
    // (6) Identitaets-Konsistenz mit dem Archiv-Eintrag.
    if cert.program_id != prog.program_id || cert.version != prog.version {
        return false;
    }
    // (7) Anti-Downgrade (firmware-gepflegte Untergrenze je program_id).
    if let Some(&(_, min_ver)) = MIN_VERSION.iter().find(|&&(id, _)| id == prog.program_id) {
        if cert.version < min_ver {
            return false;
        }
    }
    // (8) Unsafe-Audit-Status: alle TrustedSAS-Regeln bestanden.
    cert.unsafe_all_pass()
}

/// Größe des Puffers für die manipulierte Zertifikatskopie im [`trust_audit`]-Live-Oracle.
const TRUST_AUDIT_CERT_MAX: usize = 512;

/// **Nur das Trust-Gate auswerten, NICHT laden** (ext-28). Fuehrt [`verify_image`] aus, ohne
/// Ressourcen zu allozieren — ausschliesslich fuer den `certfuzz`-Fuzzer (ADR 0013, daher nur mit
/// Feature `kernel-fuzz` einkompiliert; der Release-Kernel braucht diesen verify-only-Pfad nicht).
#[cfg(feature = "kernel-fuzz")]
pub fn verify_only(prog: &Program) -> bool {
    verify_image(prog)
}

/// **TrustedSAS-Trust-Audit** (ext-28, ADR 0014) — bleibt **immer** im Kernel (kein Fuzzer-Feature).
/// Strukturelle Selbstkonsistenz der read-only Key-DB + Live-Oracle des aktiven Gates. `0` = sauber,
/// sonst ein Anomalie-Code:
/// * 1 — Key-DB leer (kein TrustedSAS koennte je geladen werden).
/// * 2 — ein `key_id != fingerprint(pubkey)` (DB nicht selbst-zertifizierend).
/// * 3 — doppelte `key_id` in der DB.
/// * 4 — Live-Oracle: ein bekannt **gueltiges** Zertifikat wird abgelehnt (Gate/DB defekt).
/// * 5 — Live-Oracle: eine **manipulierte** Kopie wird akzeptiert (Gate setzt NICHT durch).
///
/// Die Invariante „jede geladene TrustedSAS-PD stammt aus einem verifizierten Zertifikat" gilt
/// **strukturell**: [`load_image`]/[`load_program_into_pd`] rufen [`verify_image`] **vor** jeder
/// Ressourcenvergabe — ein abgelehntes Image erzeugt weder Thread noch PD. Code 4/5 belegen, dass
/// dieses Gate zur Audit-Zeit aktiv durchsetzt (akzeptiert gueltige, weist manipulierte ab).
pub fn trust_audit() -> u32 {
    if TRUSTED_KEYS.is_empty() {
        return 1;
    }
    for (i, k) in TRUSTED_KEYS.iter().enumerate() {
        if fingerprint(&k.pubkey) != k.key_id {
            return 2;
        }
        for k2 in &TRUSTED_KEYS[i + 1..] {
            if k2.key_id == k.key_id {
                return 3;
            }
        }
    }
    // Live-Oracle gegen das erste echte TrustedSAS-Zertifikat im Archiv (falls vorhanden).
    if let Some(archive) = read_archive() {
        if let Some(tx) = archive
            .iter()
            .find(|p| p.domain == DOMAIN_TRUSTED && !p.cert.is_empty())
        {
            if !verify_image(&tx) {
                return 4; // gueltig -> MUSS akzeptiert werden
            }
            let c = tx.cert;
            if c.len() <= TRUST_AUDIT_CERT_MAX {
                let mut buf = [0u8; TRUST_AUDIT_CERT_MAX];
                buf[..c.len()].copy_from_slice(c);
                buf[40] ^= 0x01; // ein signiertes Byte (binary_hash) kippen -> Signatur bricht
                let tampered = Program::new(
                    tx.program_id,
                    b"trust-audit",
                    tx.version,
                    DOMAIN_TRUSTED,
                    [0u8; 32],
                    tx.elf,
                    tx.manifest,
                    &buf[..c.len()],
                );
                if verify_image(&tampered) {
                    return 5; // manipuliert -> MUSS abgelehnt werden
                }
            }
        }
    }
    0
}

/// `SYS_LOAD`-Callback (ext-26, L2): das Programm mit Index `index` aus dem Boot-Archiv laden +
/// die `endow`-Caps (vom Dispatch aus dem Aufrufer-Cspace delegiert) in die neue PD endowen. Gibt
/// die neue PD-Id. Der Dispatch hat die `Loader`-Cap-Autoritaet bereits geprueft.
pub fn load_by_index(index: u32, endow: &[(usize, CapPtr)]) -> Option<usize> {
    let pd = (|| {
        let archive = read_archive()?;
        let prog = archive.program(index as usize)?;
        load_image(&prog, endow, boot_arg(index as usize, archive.count()))
            .ok()
            .map(|(_, pd)| pd)
    })();
    if pd.is_none() {
        // Laden fehlgeschlagen (Archiv fehlt / Index ungueltig / verify/parse/Ressourcen) -> die vom
        // Syscall-Dispatch erzeugten Endowment-Cap-KOPIEN wurden NICHT installiert (load_into_pd endowt
        // erst nach vollem Erfolg). Sie sind frische CDT-Blaetter + NICHT die letzte Referenz (das
        // Original im Aufrufer-Cspace lebt) -> delete_leaf senkt nur den Refcount. Ohne dieses Cleanup
        // lecken sie als verwaiste CDT-Kinder und blockieren sogar `delete` des Eltern-Caps (HasChildren).
        for &(_, cap) in endow {
            let _ = crate::system::cap_delete(cap);
        }
    }
    pd
}
