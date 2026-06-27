// SEL4Lake — Phase 2 (Loader), Schritt A/B: Zertifikats-Gate + Lade-Zustandsautomat.
//
// Formale Spezifikation der Loader-SICHERHEITSLOGIK (kernel/src/loader.rs `verify_image`/
// `verify_trusted_cert`, ADR 0014/0015): ein TrustedSAS-Image wird NUR mit gueltigem, bindendem
// Ed25519-Zertifikat geladen; UserLand/HardwareLand brauchen keines. Bewiesen: Soundness (kein
// unverifiziertes TrustedSAS), Programmintegritaets-/Identitaets-Bindung, Revocation, Domaenen-Gating,
// und die ATOMARITAET des Lade-Zustandsautomaten (genau ein Ausgang, kein inkonsistenter Ladezustand).
//
// Die Krypto (Ed25519/SHA-256) ist abstrahiert: `sig_valid`/`*_hash_ok` sind die durch die Signatur
// geschuetzten Fakten; der Parser, der sie liefert, ist mit Kani panik-/OOB-frei bewiesen
// (Verification/.. + docs/verification.md). Die Krypto-Primitive selbst sind etablierte Bibliotheken
// (Trusted Computing Base).
//
// Lauf:  tools/verus-verify.sh
use vstd::prelude::*;

verus! {

/// Sicherheitsdomaene des zu ladenden Images.
pub enum Domain { TrustedSas, HardwareLand, UserLand }

/// Ein Schluessel der read-only, in den Kernel kompilierten Key-DB (Modell von `TrustedKey`).
pub struct Key {
    pub key_id: nat,
    pub revoked: bool,
    /// Selbstkonsistenz `key_id == fingerprint(pubkey)` (s. trust_audit / Verus-Pilot).
    pub fingerprint_ok: bool,
}

/// Die durch die Signatur geschuetzten + an das Binary gebundenen Fakten eines Zertifikats
/// (abstrahiert von der konkreten Krypto). Jedes Feld = ein Check aus `verify_trusted_cert`.
pub struct Cert {
    pub alg_ed25519: bool,    // signature_algorithm_id == Ed25519
    pub sig_len_64: bool,     // |signature| == 64
    pub key_id: nat,          // referenzierte Key-ID
    pub sig_valid: bool,      // verify_strict(pubkey, message, sig) == true
    pub binary_hash_ok: bool, // binary_hash == SHA-256(ELF)        -> Programmintegritaet
    pub manifest_hash_ok: bool,
    pub program_id_ok: bool,  // cert.program_id == Archiv-Eintrag  -> Identitaet
    pub version_ok: bool,     // cert.version == Archiv-Eintrag
    pub version_ge_min: bool, // Anti-Downgrade
    pub unsafe_all_pass: bool,// unsafe_status == ALL_PASS
}

/// Ein zu ladendes Programm: Domaene + (TrustedSAS-)Zertifikat.
pub struct Program { pub domain: Domain, pub cert: Cert }

/// Existiert ein **gueltiger** (nicht-revozierter, selbstkonsistenter) Schluessel fuer `kid`?
pub open spec fn valid_key(db: Seq<Key>, kid: nat) -> bool {
    exists|i: int| 0 <= i < db.len() && db[i].key_id == kid && !db[i].revoked && db[i].fingerprint_ok
}

/// **Zertifikat akzeptiert** — die Konjunktion ALLER dokumentierten Checks (`verify_trusted_cert`).
pub open spec fn cert_accepted(db: Seq<Key>, c: Cert) -> bool {
    &&& c.alg_ed25519 && c.sig_len_64
    &&& valid_key(db, c.key_id)
    &&& c.sig_valid
    &&& c.binary_hash_ok && c.manifest_hash_ok
    &&& c.program_id_ok && c.version_ok && c.version_ge_min
    &&& c.unsafe_all_pass
}

/// **Das Gate** `verify_image`: UserLand/HardwareLand -> true (hardware-isoliert, kein Cert noetig);
/// TrustedSAS -> nur mit akzeptiertem Zertifikat.
pub open spec fn verify_image(db: Seq<Key>, p: Program) -> bool {
    match p.domain {
        Domain::TrustedSas => cert_accepted(db, p.cert),
        _ => true,
    }
}

/// Ausgang des Lade-Zustandsautomaten: geladen (mit Domaene) ODER abgewiesen — nichts dazwischen.
pub enum LoadResult { Loaded { domain: Domain }, Rejected }

/// **`load`**: Gate auswerten, dann GENAU EIN Ausgang. Modelliert die Atomaritaet — `load_image` ruft
/// `verify_image` VOR jeder Ressourcenvergabe; ein abgewiesenes Image erzeugt weder Thread noch PD.
pub open spec fn load(db: Seq<Key>, p: Program) -> LoadResult {
    if verify_image(db, p) { LoadResult::Loaded { domain: p.domain } } else { LoadResult::Rejected }
}

// ===================== Bewiesene Eigenschaften =====================

/// **BEWEIS (Soundness — kein unverifiziertes TrustedSAS):** wird ein TrustedSAS-Image akzeptiert, so
/// hat ein **gueltiger, nicht-revozierter** Schluessel ein Zertifikat signiert, das **an genau dieses
/// Binary gebunden** ist (binary/manifest-Hash), die **Identitaet** trifft (program_id/version), die
/// Version nicht downgegradet ist und der **Unsafe-Audit ALL_PASS** ist.
pub proof fn soundness_trusted(db: Seq<Key>, p: Program)
    requires
        verify_image(db, p),
        p.domain == Domain::TrustedSas,
    ensures
        valid_key(db, p.cert.key_id),
        p.cert.sig_valid,
        p.cert.binary_hash_ok && p.cert.manifest_hash_ok,
        p.cert.program_id_ok && p.cert.version_ok && p.cert.version_ge_min,
        p.cert.unsafe_all_pass,
{
}

/// **BEWEIS (Revocation):** wird ein TrustedSAS-Image akzeptiert, so ist der zugeordnete Schluessel
/// **nicht** zurueckgezogen (ein revozierter Schluessel laesst kein Zertifikat mehr passieren).
pub proof fn accepted_key_not_revoked(db: Seq<Key>, p: Program)
    requires
        verify_image(db, p),
        p.domain == Domain::TrustedSas,
    ensures
        exists|i: int| 0 <= i < db.len() && db[i].key_id == p.cert.key_id && !db[i].revoked,
{
}

/// **BEWEIS (Revocation-Vollstaendigkeit):** sind ALLE Schluessel der Key-ID des Zertifikats
/// zurueckgezogen, wird das TrustedSAS-Image **abgewiesen**.
pub proof fn all_revoked_rejects(db: Seq<Key>, p: Program)
    requires
        p.domain == Domain::TrustedSas,
        forall|i: int| 0 <= i < db.len() && db[i].key_id == p.cert.key_id ==> db[i].revoked,
    ensures
        !verify_image(db, p),
{
    assert(!valid_key(db, p.cert.key_id));
}

/// **BEWEIS (Domaenen-Gating):** UserLand/HardwareLand-Images laden **immer** (kein Zertifikat noetig);
/// nur TrustedSAS ist zertifikats-gegatet.
pub proof fn untrusted_loads_unconditionally(db: Seq<Key>, p: Program)
    requires
        p.domain != Domain::TrustedSas,
    ensures
        load(db, p) is Loaded,
        verify_image(db, p),
{
}

/// **BEWEIS (Atomaritaet / keine inkonsistenten Ladezustaende):** `load` liefert **genau einen**
/// Ausgang: bei bestandenem Gate „Loaded" mit der **deklarierten Domaene**, sonst „Rejected" — nie
/// ein Zwischen-/Teilzustand.
pub proof fn load_atomic(db: Seq<Key>, p: Program)
    ensures
        load(db, p) is Loaded <==> verify_image(db, p),
        load(db, p) is Loaded ==> load(db, p)->Loaded_domain == p.domain,
        load(db, p) is Rejected <==> !verify_image(db, p),
{
}

/// **BEWEIS (Determinismus):** dieselbe (read-only) Key-DB + dasselbe Programm liefern **dieselbe**
/// Entscheidung — die in den Kernel kompilierte Key-DB ist unveraenderlich (kein Syscall), also ist
/// das Gate reproduzierbar. (`load` ist eine reine Funktion ihrer Eingaben.)
pub proof fn deterministic(db: Seq<Key>, p1: Program, p2: Program)
    requires
        p1 == p2,
    ensures
        load(db, p1) == load(db, p2),
{
}

fn main() {}

} // verus!
