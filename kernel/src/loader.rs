//! Kernel-Glue des **generischen Binary-Loaders** (ext-26, [ADR 0011](../../docs/adr/0011-binary-loader.md)).
//!
//! Die **reine**, bounds-geprüfte Parse-Logik (Boot-Archiv, ab L1 Minimal-ELF64) liegt im Crate
//! `sel4lake-loader` (0 `unsafe`, host-getestet). Hier liegt der **privilegierte** Teil, der RAM
//! liest und (ab L1) Segmente in Regionen kopiert, W^X mappt, VSpace/PD anlegt, Caps endowt und
//! Threads spawnt — alles über die bestehenden `system::`-Primitive (keine neuen Sonderrechte).
//!
//! **L0:** das Boot-Archiv aus dem reservierten RAM-Fenster lesen + die Module melden.

use crate::trusted_keys::{MIN_VERSION, TRUSTED_KEYS};
use core::sync::atomic::{AtomicU64, Ordering};
use sel4lake_cap::CapPtr;
use sel4lake_hal::{print, println};
use sel4lake_loader::archive::Archive;
use sel4lake_loader::cert::{TrustedCert, SIG_ALG_ED25519, SIG_ED25519_LEN};
use sel4lake_loader::elf::ElfImage;
use sel4lake_loader::{LoaderError, Program, DOMAIN_TRUSTED, DOMAIN_USERLAND};
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
/// L1: nur **isolierte EL0-Domänen** (UserLand/HardwareLand). TrustedSAS (EL1) ist signatur-gegatet
/// (L3) und wird hier abgelehnt (`UnsupportedDomain`).
pub fn load_image(prog: &Program, endow: &[(usize, CapPtr)]) -> Result<(ThreadId, usize), LoaderError> {
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
    crate::system::load_elf(&img, domain, endow).ok_or(LoaderError::NoResources)
}

/// Ein Programm in eine **vor-erstellte** PD laden (ext-26, L3) — fuer HardwareLand-Backends
/// (Partner-Bindung + Kanal vom Aufrufer aufgesetzt) und kuenftige spezialisierte PDs. Die
/// Domaenen-Policy traegt die PD selbst (`install_cap_checked` + `domain_audit`). Trust-Gate
/// ([`verify_image`]) gilt auch hier.
pub fn load_program_into_pd(
    prog: &Program,
    pd: usize,
    endow: &[(usize, CapPtr)],
) -> Result<ThreadId, LoaderError> {
    if !verify_image(prog) {
        return Err(LoaderError::Unverified);
    }
    let img = ElfImage::parse(prog.elf)?;
    crate::system::load_into_pd(&img, pd, endow).ok_or(LoaderError::NoResources)
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
        load_image(&prog, endow).ok().map(|(_, pd)| pd)
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
