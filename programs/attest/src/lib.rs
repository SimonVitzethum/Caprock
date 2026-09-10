//! **Attestierungs-PD-Skelett (Z7): was auf der Maschine laeuft, belegen.**
//!
//! Das Zertifikats-Gate (ADR 0014) sichert, welche Binaries starten DUERFEN. Diese PD belegt,
//! was TATSAECHLICH laeuft: Sie liest die SW-PCR-Messkette des Kernels
//! (`loader::messkette_*`), bindet sie an eine frische Nonce des Tenants und signiert den
//! Bericht mit IHREM EIGENEN Key. Der Tenant prueft offline: Signatur, Nonce (Replay),
//! Kette (Bruch) — s. `tools/lx_attest_check.py`.
//!
//! ## Was hier läuft und was gestellt ist
//!
//! Auf dem Host laufen: Extend-Formel, Kettenpruefung, Berichts-Codec, Signatur-Gatter,
//! Nonce-Bindung (Replay-Ablehnung). Gestellt sind der Transport (kein IPC — die PD liest die
//! Kette erst, sobald der Kernel einen lesenden Zugriff anbietet; der Stand steht als
//! PATCH-TEXT im Arbeitsergebnis, nicht im Kernel), der PD-Key (Endowment aus dem Manifest,
//! nicht Draht — wer ihn per Wort setzen koennte, waehlte seinen eigenen Pruefer) und der
//! Speicher (feste Arrays statt PD-RAM).
//!
//! ## Ehrliche Grenzen (gehoeren zum Skelett, nicht in eine Fussnote)
//!
//! * **Ohne TPM kein Hardware-Anker.** Die Kette ist SW-PCR: `neu = SHA-256(prev ||
//!   program_id || domain || image_hash)`, Anker = Kernel-Code-Hash. Ein physischer
//!   Angreifer mit RAM-Zugriff schreibt Liste UND Bericht um — der signierte Bericht ist
//!   Software-Evidenz, kein Remote-Trust gegen physische Angreifer. Die TPM-Messkette
//!   (Firmware → Bootloader → Kernel + Modul) kommt von aussen
//!   (`tools/lx_bootentscheidung.md` §2); diese PD beginnt erst beim Kernel.
//! * **Der PD-Key misst keine Hardware.** Er sagt „diese PD hat das unterschrieben", nicht
//!   „diese Maschine ist echt". Der Key kommt aus dem Manifest-Endowment der PD (wie die
//!   Schluessel in `lxpd-runtime`): Kompromittierung der PD = Kompromittierung der Aussage.
//! * **`TestSchluessel` ist KEIN Ed25519.** Die Host-Tests fahren einen symmetrischen
//!   Modell-Signierer (SHA-256-MAC, als `TEST-NUR` benannt), damit Kettenlogik, Bruch-
//!   Erkennung und Replay-Ablehnung ohne Kryptobibliothek belegbar sind. Was damit belegt
//!   ist: die PROTOKOLLOGIK (Nonce-Bindung, Pruefreihenfolge, Bruchindex). Was damit NICHT
//!   belegt ist: die Signaturstaerke. Die Produktions-PD signiert Ed25519 mit ihrem
//!   Manifest-Key; der Tenant prueft gegen den Manifest-Pubkey.
//!
//! ## Pruefreihenfolge, und sie ist nicht verhandelbar
//!
//! 1. **Signatur** — erst das Urteil ueber die Bytes, dann deren Deutung. Wer Nonce oder
//!    Kette vor der Signatur prueft, lehnt Faelschungen aus dem falschen Grund ab (immer
//!    noch Ablehnung, aber vermischte Diagnose).
//! 2. **Nonce (Replay)** — der Bericht muss die Nonce tragen, die der Tenant schickte.
//!    Ein alter, gueltig signierter Bericht mit alter Nonce ist der Replay-Fall.
//! 3. **Kette** — jedes Glied nachgerechnet, dann Kopf gegen den Bericht.
//!
//! ## Spiegel-Regel
//!
//! `pcr_verlaengern` ist die Formel des Kernels (`loader::messkette_verlaengern`:
//! 72-Byte-Verkettung `prev || program_id(le) || domain(le) || image_hash`, SHA-256).
//! Aenderung dort heisst Aenderung hier — und der Goldwert-Test faellt dann (er ist gegen
//! `hashlib` gerechnet, nicht gegen diese Datei, also faellt er aus dem RICHTIGEN Grund).

#![no_std]
#![forbid(unsafe_code)]

// Die Crate ist `no_std` (sie laeuft in einer Dienst-PD ohne Betriebssystem). Der Testharness
// braucht `std` — test-only, der PD-Bau sieht es nie (dasselbe Muster wie `lxpd-runtime`).
#[cfg(test)]
extern crate std;

// --- SHA-256 (kompakt, abhaengigkeitsfrei) ------------------------------------------------
//
// Standard-Algorithmus (FIPS 180-4), keine Eigenentwicklung: Der Goldwert-Test faehrt den
// "abc"-Vektor, also waere eine Attrappe hier SOFORT sichtbar. `core`-only, kein Alloc.

/// SHA-256 ueber `daten` (allgemein, nicht nur 72-Byte-Verkettungen).
pub fn sha256(daten: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut i = 0usize;
    while i + 64 <= daten.len() {
        let mut block = [0u8; 64];
        block.copy_from_slice(&daten[i..i + 64]);
        runde(&mut h, &block, &K);
        i += 64;
    }
    // Rest + Padding: `0x80`, Nullen, 8-Byte-Bitlaenge. Ab 56 Restbytes braucht das
    // Padding einen zweiten Block — sonst schriebe die Laenge ueber die Daten.
    let rest = &daten[i..];
    let bitlen: u64 = (daten.len() as u64).wrapping_mul(8);
    let mut block = [0u8; 64];
    block[..rest.len()].copy_from_slice(rest);
    block[rest.len()] = 0x80;
    if rest.len() >= 56 {
        runde(&mut h, &block, &K);
        block = [0u8; 64];
    }
    block[56..64].copy_from_slice(&bitlen.to_be_bytes());
    runde(&mut h, &block, &K);
    let mut out = [0u8; 32];
    for (j, w) in h.iter().enumerate() {
        out[4 * j..4 * j + 4].copy_from_slice(&w.to_be_bytes());
    }
    out
}

fn runde(h: &mut [u32; 8], block: &[u8; 64], k: &[u32; 64]) {
    let mut w = [0u32; 64];
    for (j, wj) in w.iter_mut().enumerate().take(16) {
        let o = 4 * j;
        *wj = u32::from_be_bytes([block[o], block[o + 1], block[o + 2], block[o + 3]]);
    }
    for j in 16..64 {
        let s0 = w[j - 15].rotate_right(7) ^ w[j - 15].rotate_right(18) ^ (w[j - 15] >> 3);
        let s1 = w[j - 2].rotate_right(17) ^ w[j - 2].rotate_right(19) ^ (w[j - 2] >> 10);
        w[j] = w[j - 16].wrapping_add(s0).wrapping_add(w[j - 7]).wrapping_add(s1);
    }
    let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) =
        (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);
    for j in 0..64 {
        let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let ch = (e & f) ^ ((!e) & g);
        let t1 = hh
            .wrapping_add(s1)
            .wrapping_add(ch)
            .wrapping_add(k[j])
            .wrapping_add(w[j]);
        let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
        let maj = (a & b) ^ (a & c) ^ (b & c);
        let t2 = s0.wrapping_add(maj);
        hh = g;
        g = f;
        f = e;
        e = d.wrapping_add(t1);
        d = c;
        c = b;
        b = a;
        a = t1.wrapping_add(t2);
    }
    h[0] = h[0].wrapping_add(a);
    h[1] = h[1].wrapping_add(b);
    h[2] = h[2].wrapping_add(c);
    h[3] = h[3].wrapping_add(d);
    h[4] = h[4].wrapping_add(e);
    h[5] = h[5].wrapping_add(f);
    h[6] = h[6].wrapping_add(g);
    h[7] = h[7].wrapping_add(hh);
}

// --- Kette (Spiegel des Kernels) ------------------------------------------------------------

/// Hoechstzahl Glieder (Spiegel von `loader::MESSKETTE_MAX` — ein Glied je geladener PD).
pub const MESSKETTE_MAX: usize = 64;

/// Ein Kettenglied (Felder wie `loader::MessEintrag`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct MessEintrag {
    /// Stabile Programm-ID aus Archiv/Manifest.
    pub program_id: u32,
    /// Zieldomaene (`DOMAIN_*`, u32 wie `Program.domain`).
    pub domain: u32,
    /// `SHA-256(ELF-Bytes)` des Images.
    pub image_hash: [u8; 32],
    /// Kettenstand NACH diesem Eintrag.
    pub pcr: [u8; 32],
}

/// Ein Extend-Schritt (Spiegel von `loader::messkette_verlaengern`): `SHA-256(prev ||
///
/// program_id(le) || domain(le) || image_hash)` — 72 Byte, Little-Endian wie der Kernel
/// (`to_le_bytes` dort wie hier).
pub fn pcr_verlaengern(
    prev: &[u8; 32],
    program_id: u32,
    domain: u32,
    image_hash: &[u8; 32],
) -> [u8; 32] {
    let mut buf = [0u8; 72];
    buf[..32].copy_from_slice(prev);
    buf[32..36].copy_from_slice(&program_id.to_le_bytes());
    buf[36..40].copy_from_slice(&domain.to_le_bytes());
    buf[40..72].copy_from_slice(image_hash);
    sha256(&buf)
}

/// Woran die Nachrechnung scheiterte — mit Index, nicht nur „kaputt".
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum KettenBruch {
    /// Glied `usize` laesst sich aus seinem Vorgaenger nicht herleiten (falscher Hash,
    /// vertauschte Reihenfolge, falsche Domaene — alles heisst Bruch an DIESER Stelle).
    BruchBei(usize),
}

/// Die Kette gegen `genesis` nachrechnen (leer ist gueltig: Anker ohne Ladung).
/// Gibt `Ok(Kopf-PCR)` oder den ERSTEN Bruch — ein zweiter dahinter ist dann keine
/// unabhaengige Aussage mehr.
pub fn kette_pruefen(
    genesis: &[u8; 32],
    glieder: &[MessEintrag],
) -> Result<[u8; 32], KettenBruch> {
    let mut prev = *genesis;
    for (i, g) in glieder.iter().enumerate() {
        let soll = pcr_verlaengern(&prev, g.program_id, g.domain, &g.image_hash);
        if soll != g.pcr {
            return Err(KettenBruch::BruchBei(i));
        }
        prev = g.pcr;
    }
    Ok(prev)
}

// --- Bericht + Nonce + Signatur ---------------------------------------------------------------

/// Der signierte Bericht: Kettenkopf, Gliedzahl, Tenant-Nonce.
///
/// Codec (48 Byte, fest): `pcr(32) || anzahl(u64 le) || nonce(u64 le)`. Fest, damit der
/// Offline-Pruefer (`tools/lx_attest_check.py`) dieselben Bytes sieht wie die PD — ein
/// Berichtsformat mit variabler Laenge waere eine zweite Codec-Entscheidung.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Bericht {
    /// Kettenkopf zum Berichtszeitpunkt (`messkette_aktuell` bzw. `kette_pruefen`-Kopf).
    pub pcr: [u8; 32],
    /// Gliedzahl zum Berichtszeitpunkt.
    pub anzahl: u64,
    /// Frische Nonce des Tenants (genau DIESE Nonce muss zurueckkommen — sonst Replay).
    pub nonce: u64,
}

/// Berichts-Codec: 48 Byte, fest.
pub fn bericht_bytes(b: &Bericht) -> [u8; 48] {
    let mut out = [0u8; 48];
    out[..32].copy_from_slice(&b.pcr);
    out[32..40].copy_from_slice(&b.anzahl.to_le_bytes());
    out[40..48].copy_from_slice(&b.nonce.to_le_bytes());
    out
}

/// Wer einen Bericht signieren UND fremde Signaturen pruefen kann. Getrennt vom Transport:
/// Der Host-Test faehrt [`TestSchluessel`], die Produktions-PD Ed25519 mit ihrem
/// Manifest-Key (privat in der PD, oeffentlich beim Tenant).
pub trait Signierer {
    /// Bericht signieren (nur der Halter des privaten Schluessels).
    fn signiere(&self, nachricht: &[u8]) -> [u8; 64];
    /// Signatur pruefen (Tenant-Seite; kommt ohne privaten Schluessel aus).
    fn pruefe(&self, nachricht: &[u8], signatur: &[u8; 64]) -> bool;
}

/// Modell-Signierer NUR fuer Host-Tests: symmetrische MAC `SHA-256(key || msg) ||
///
/// SHA-256(msg || key)` als 64 Byte. Heisst TEST, weil sie es ist: Wer diese Bytes fuer
/// Ed25519-Sicherheit haelt, haelt ein Modell fuer die Maschine. Was sie traegt: die
/// Protokolllogik (Bindung Bericht+Nonce an einen Key, Schluesselwechsel bricht).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TestSchluessel {
    /// Der Modell-Key (32 Byte — steht fuer „PD-privat" auf der einen, „Tenant-bekannt"
    /// auf der anderen Seite; symmetrisch NUR im Modell).
    pub schluessel: [u8; 32],
}

fn mac_haelfte(erst: &[u8], zweit: &[u8]) -> [u8; 32] {
    // Gestaffelt haengen statt zu kopieren: `erst.len() + zweit.len()` passt immer auf
    // den Stapel der Tests (Bericht = 48 Byte, Key = 32 Byte).
    let mut buf = [0u8; 80];
    let n = erst.len() + zweit.len();
    buf[..erst.len()].copy_from_slice(erst);
    buf[erst.len()..n].copy_from_slice(zweit);
    sha256(&buf[..n])
}

impl Signierer for TestSchluessel {
    fn signiere(&self, nachricht: &[u8]) -> [u8; 64] {
        let a = mac_haelfte(&self.schluessel, nachricht);
        let b = mac_haelfte(nachricht, &self.schluessel);
        let mut sig = [0u8; 64];
        sig[..32].copy_from_slice(&a);
        sig[32..].copy_from_slice(&b);
        sig
    }
    fn pruefe(&self, nachricht: &[u8], signatur: &[u8; 64]) -> bool {
        &self.signiere(nachricht) == signatur
    }
}

/// Ein unterschriebener Bericht (Transportform PD → Tenant).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SignierterBericht {
    /// Der Bericht (Kopf, Anzahl, Nonce).
    pub bericht: Bericht,
    /// Die Signatur ueber [`bericht_bytes`] mit dem PD-Key.
    pub signatur: [u8; 64],
}

/// Attestieren (PD-Seite): Bericht bauen + mit PD-Key unterschreiben.
pub fn attestieren<S: Signierer>(
    signierer: &S,
    pcr: &[u8; 32],
    anzahl: u64,
    nonce: u64,
) -> SignierterBericht {
    let bericht = Bericht { pcr: *pcr, anzahl, nonce };
    let signatur = signierer.signiere(&bericht_bytes(&bericht));
    SignierterBericht { bericht, signatur }
}

/// Warum ein Bericht nicht gilt — in Pruefreihenfolge (Signatur, Nonce, Kette).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PruefFehler {
    /// Die Signatur stammt nicht von diesem Key (Faelschung oder falscher Key).
    SignaturUngueltig,
    /// Gueltig signiert, aber mit alter Nonce — der Replay-Fall (`erwartet` vs. `erhalten`).
    Replay {
        /// Die Nonce, die der Tenant schickte.
        erwartet: u64,
        /// Die Nonce, die der Bericht traegt.
        erhalten: u64,
    },
    /// Gueltig signiert, frische Nonce — aber die Kette traegt diesen Kopf nicht: Bruch an
    /// `KettenBruch::BruchBei(i)`, oder Kopfweiche (`BruchBei(anzahl)` = alle Glieder echt,
    /// der Kopf gehoert trotzdem nicht dazu).
    KetteGebrochen(KettenBruch),
}

/// Pruefen (Tenant-Seite): Signatur → Nonce → Kette (s. Modul-Doku).
///
/// `anzahl` im Bericht muss zur Gliedzahl passen — sonst belegte ein Kopf ueber drei
/// Gliedern einen Bericht ueber vier (Anzahl ist Teil der signierten Bytes, also schuetzt
/// die Signatur sie; hier wird sie ZUSAETZLICH gegen die Liste gehalten).
pub fn bericht_pruefen<S: Signierer>(
    signierer: &S,
    signiert: &SignierterBericht,
    nonce_erwartet: u64,
    genesis: &[u8; 32],
    glieder: &[MessEintrag],
) -> Result<(), PruefFehler> {
    // 1. Signatur — vor jeder Deutung der Bytes.
    if !signierer.pruefe(&bericht_bytes(&signiert.bericht), &signiert.signatur) {
        return Err(PruefFehler::SignaturUngueltig);
    }
    // 2. Nonce — der Replay-Fall ist signiert-gueltig und trotzdem abzulehnen.
    if signiert.bericht.nonce != nonce_erwartet {
        return Err(PruefFehler::Replay {
            erwartet: nonce_erwartet,
            erhalten: signiert.bericht.nonce,
        });
    }
    // 3. Kette — erst die Glieder, dann Kopf und Anzahl gegen den Bericht.
    match kette_pruefen(genesis, glieder) {
        Err(b) => Err(PruefFehler::KetteGebrochen(b)),
        Ok(kopf) => {
            let n = glieder.len() as u64;
            if kopf != signiert.bericht.pcr || n != signiert.bericht.anzahl {
                return Err(PruefFehler::KetteGebrochen(KettenBruch::BruchBei(
                    glieder.len(),
                )));
            }
            Ok(())
        }
    }
}

// --- Host-Tests ---------------------------------------------------------------------------------
//
// Fake-PD (Modell-Key) plus Fake-Tenant (Noncen-Vergabe): echte Formel (Goldwerte aus
// `hashlib`), echte Brueche (Manipulation, Tausch, falsche Genesis), echte Replays
// (alte Nonce), echte Schluesselwechsel — jede Lage mit dem NAMEN des Fehlers, der
// fallen muss.

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec::Vec;

    const GENESIS: [u8; 32] = [
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
        0x0e, 0x0f, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b,
        0x1c, 0x1d, 0x1e, 0x1f,
    ];
    const BILD_A: [u8; 32] = [0x11; 32];
    const BILD_B: [u8; 32] = [0x22; 32];
    const BILD_C: [u8; 32] = [0x33; 32];
    // Goldwerte aus `hashlib` (72-Byte-Verkettung `prev || pid(le) || dom(le) || bild`):
    // P1 = extend(GENESIS, 7, 2, BILD_A), P2 = extend(P1, 8, 1, BILD_B).
    const P1_GOLD: [u8; 32] = [
        0xbd, 0x33, 0xb6, 0xa2, 0x20, 0xa7, 0x22, 0x06, 0x8e, 0xd2, 0x7f, 0xf9, 0x68, 0x11,
        0x1c, 0x60, 0x77, 0x25, 0xf7, 0x7b, 0xb5, 0xe8, 0xe4, 0xd6, 0xeb, 0xcb, 0x03, 0xbc,
        0xf2, 0xdf, 0x2f, 0x68,
    ];
    const P2_GOLD: [u8; 32] = [
        0xb7, 0x6c, 0x12, 0x22, 0x9b, 0x0b, 0xa4, 0xc1, 0x57, 0xc4, 0x00, 0x86, 0x72, 0x86,
        0xf3, 0x88, 0xdb, 0x63, 0xf9, 0x5f, 0x9a, 0xc3, 0x9f, 0x08, 0x91, 0x14, 0x44, 0x17,
        0xfb, 0x97, 0xab, 0x65,
    ];
    const KEY_A: TestSchluessel = TestSchluessel { schluessel: [0x42; 32] };
    const KEY_B: TestSchluessel = TestSchluessel { schluessel: [0x99; 32] };

    fn glied(pid: u32, dom: u32, bild: [u8; 32], prev: &[u8; 32]) -> MessEintrag {
        MessEintrag { program_id: pid, domain: dom, image_hash: bild, pcr: pcr_verlaengern(prev, pid, dom, &bild) }
    }

    fn kette3() -> Vec<MessEintrag> {
        let e0 = glied(7, 2, BILD_A, &GENESIS);
        let e1 = glied(8, 1, BILD_B, &e0.pcr);
        let e2 = glied(9, 3, BILD_C, &e1.pcr);
        std::vec![e0, e1, e2]
    }

    #[test]
    fn sha256_ist_echt() {
        // Der "abc"-Vektor (FIPS 180-4): faellt, sobald `sha256` eine Attrappe ist.
        assert_eq!(
            sha256(b"abc"),
            [
                0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde,
                0x5d, 0xae, 0x22, 0x23, 0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c,
                0xb4, 0x10, 0xff, 0x61, 0xf2, 0x00, 0x15, 0xad
            ]
        );
    }

    #[test]
    fn goldener_extend() {
        // Gegen `hashlib` gerechnet, nicht gegen diese Datei — ein Formel-Drift im Kernel
        // faellt hier, nicht erst im Audit.
        assert_eq!(pcr_verlaengern(&GENESIS, 7, 2, &BILD_A), P1_GOLD);
    }

    #[test]
    fn goldene_zweite_stufe() {
        // Verkettung, nicht Einmal-Hash: P2 haengt an P1, nicht an der Genesis.
        assert_eq!(pcr_verlaengern(&P1_GOLD, 8, 1, &BILD_B), P2_GOLD);
        assert!(pcr_verlaengern(&GENESIS, 8, 1, &BILD_B) != P2_GOLD);
    }

    #[test]
    fn kette_ok_drei() {
        let k = kette3();
        let kopf = kette_pruefen(&GENESIS, &k).unwrap();
        assert_eq!(kopf, k[2].pcr);
    }

    #[test]
    fn manipulation_bricht() {
        // Ein gekipptes Bit im Bild ist ein anderes Bild — Bruch GENAU dort.
        let mut k = kette3();
        k[1].image_hash[0] ^= 0x01;
        assert_eq!(kette_pruefen(&GENESIS, &k), Err(KettenBruch::BruchBei(1)));
    }

    #[test]
    fn reihenfolge_bricht() {
        // Vertauschte Glieder: schon das erste passt nicht mehr zur Genesis.
        let k = kette3();
        let getauscht = std::vec![k[1], k[0], k[2]];
        assert_eq!(kette_pruefen(&GENESIS, &getauscht), Err(KettenBruch::BruchBei(0)));
    }

    #[test]
    fn falsche_genesis_bricht() {
        // Falscher Anker (falscher Kernel): Bruch bei 0, nicht „irgendwo".
        let k = kette3();
        let mut andere = GENESIS;
        andere[0] ^= 0x01;
        assert_eq!(kette_pruefen(&andere, &k), Err(KettenBruch::BruchBei(0)));
    }

    #[test]
    fn runder_weg_ok() {
        // PD attestiert, Tenant prueft: Signatur + Nonce + Kette in einem.
        let k = kette3();
        let kopf = kette_pruefen(&GENESIS, &k).unwrap();
        let s = attestieren(&KEY_A, &kopf, k.len() as u64, 41);
        assert!(bericht_pruefen(&KEY_A, &s, 41, &GENESIS, &k).is_ok());
    }

    #[test]
    fn replay_abgelehnt() {
        // Gueltig signiert, aber alte Nonce: der Replay-Fall mit beiden Noncen im Fehler.
        let k = kette3();
        let kopf = kette_pruefen(&GENESIS, &k).unwrap();
        let s = attestieren(&KEY_A, &kopf, k.len() as u64, 41);
        assert_eq!(
            bericht_pruefen(&KEY_A, &s, 42, &GENESIS, &k),
            Err(PruefFehler::Replay { erwartet: 42, erhalten: 41 })
        );
    }

    #[test]
    fn falscher_schluessel_abgelehnt() {
        // Mit KEY_A signiert, mit KEY_B geprueft: keine Aussage, kein Durchwinken.
        let k = kette3();
        let kopf = kette_pruefen(&GENESIS, &k).unwrap();
        let s = attestieren(&KEY_A, &kopf, k.len() as u64, 41);
        assert_eq!(
            bericht_pruefen(&KEY_B, &s, 41, &GENESIS, &k),
            Err(PruefFehler::SignaturUngueltig)
        );
    }

    #[test]
    fn kopfweiche_abgelehnt() {
        // Alle Glieder echt, aber der Bericht traegt einen fremden Kopf: kein Durchwinken
        // ueber „die Kette ist ja gueltig" — der Kopf gehoert nicht dazu.
        let k = kette3();
        let s = attestieren(&KEY_A, &[0x77; 32], k.len() as u64, 41);
        assert!(matches!(
            bericht_pruefen(&KEY_A, &s, 41, &GENESIS, &k),
            Err(PruefFehler::KetteGebrochen(_))
        ));
    }

    #[test]
    fn leerer_lauf_ok() {
        // Kein Modul geladen: Anker ohne Glieder ist gueltig (der Kernel meldet die Genesis
        // auch im leeren Lauf — Schweigen waere hier kein Beleg).
        let leer: Vec<MessEintrag> = Vec::new();
        let kopf = kette_pruefen(&GENESIS, &leer).unwrap();
        assert_eq!(kopf, GENESIS);
        let s = attestieren(&KEY_A, &kopf, 0, 7);
        assert!(bericht_pruefen(&KEY_A, &s, 7, &GENESIS, &leer).is_ok());
    }
}
