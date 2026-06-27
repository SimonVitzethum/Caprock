// SEL4Lake — Verus-Pilot (Tier 2), Teil 9: TrustedSAS-Key-DB-Konsistenz (`trust_audit`, ext-28).
//
// Formale Spezifikation der `trust_audit`-Invariante (kernel/src/loader.rs, ADR 0014, Codes 2+3):
// die read-only Key-DB ist **selbst-zertifizierend** -- fuer JEDEN Eintrag gilt
// `key_id == fingerprint(pubkey)`, und die `key_id`s sind **eindeutig**. Damit kann der Kernel beim
// Cert-Lookup einem `key_id` genau einen PubKey zuordnen, ohne Verwechslung. Bewiesen: `add_key`
// (einen Schluessel mit `key_id := fingerprint(pubkey)` aufnehmen -- nur, wenn die key_id noch nicht
// existiert) **erhaelt** die Invariante.
//
// `fingerprint` ist hier eine **uninterpretierte** Spec-Funktion (= SHA-256(pubkey)[..16]); die
// Beweise haengen nur von ihrer Existenz als Funktion ab, nicht von ihrer Implementierung.
//
// Lauf:  tools/verus-verify.sh
use vstd::prelude::*;

verus! {

/// 128-bit-Fingerprint eines 32-Byte-PubKeys (= SHA-256(pubkey)[..16]) — uninterpretiert.
pub uninterp spec fn fingerprint(pubkey: Seq<u8>) -> Seq<u8>;

/// Ein Key-DB-Eintrag (Modell von `TrustedKey`): key_id + zugehoeriger PubKey.
pub struct Key {
    pub key_id: Seq<u8>,
    pub pubkey: Seq<u8>,
}

pub struct KeyDb {
    pub keys: Seq<Key>,
}

/// **Key-DB-Invariante (`trust_audit` Codes 2+3):** jeder Eintrag ist selbst-zertifizierend
/// (`key_id == fingerprint(pubkey)`) UND die `key_id`s sind paarweise verschieden.
pub open spec fn keydb_inv(db: KeyDb) -> bool {
    &&& (forall|i: int|
        #![trigger db.keys[i]]
        0 <= i < db.keys.len() ==> db.keys[i].key_id == fingerprint(db.keys[i].pubkey))
    &&& (forall|i: int, j: int|
        #![trigger db.keys[i].key_id, db.keys[j].key_id]
        0 <= i < db.keys.len() && 0 <= j < db.keys.len() && i != j
            ==> db.keys[i].key_id != db.keys[j].key_id)
}

/// **BEWEIS:** `add_key` (einen PubKey aufnehmen, `key_id := fingerprint(pubkey)` — **nur**, wenn
/// kein bestehender Eintrag dieselbe key_id traegt) **erhaelt** die Key-DB-Invariante.
pub proof fn add_key(db: KeyDb, pubkey: Seq<u8>) -> (db2: KeyDb)
    requires
        keydb_inv(db),
        forall|i: int| 0 <= i < db.keys.len() ==> #[trigger] db.keys[i].key_id != fingerprint(pubkey),
    ensures
        keydb_inv(db2),
{
    let entry = Key { key_id: fingerprint(pubkey), pubkey };
    let db2 = KeyDb { keys: db.keys.push(entry) };

    // (Selbst-Zertifizierung) jeder Eintrag: alte unveraendert, der neue per Konstruktion.
    assert forall|i: int| 0 <= i < db2.keys.len() implies #[trigger] db2.keys[i].key_id
        == fingerprint(db2.keys[i].pubkey) by {
        if i < db.keys.len() {
            assert(db2.keys[i] == db.keys[i]);
        }
    }
    // (Eindeutigkeit) jedes Paar: alte Paare unveraendert; ein Paar mit dem neuen Eintrag
    // unterscheidet sich, da dessen key_id == fingerprint(pubkey) und keine alte key_id das ist.
    assert forall|i: int, j: int|
        0 <= i < db2.keys.len() && 0 <= j < db2.keys.len() && i != j
        implies #[trigger] db2.keys[i].key_id != #[trigger] db2.keys[j].key_id by {
        let last = db.keys.len();
        if i < last && j < last {
            assert(db2.keys[i] == db.keys[i] && db2.keys[j] == db.keys[j]);
        } else if i == last {
            assert(db2.keys[j] == db.keys[j] && db2.keys[i].key_id == fingerprint(pubkey));
        } else {
            assert(db2.keys[i] == db.keys[i] && db2.keys[j].key_id == fingerprint(pubkey));
        }
    }
    db2
}

fn main() {}

} // verus!
