//! Treiber-Eintrag: **woher** kommt ein LXPD-Image, **welches** Bild genau, **wem** wird
//! geglaubt.
//!
//! Ein Eintrag ist JSON (ein Objekt, Felder s. unten) und beschreibt genau EIN Treiber-Image.
//! Zwei Herkunftsarten:
//!
//! * **(a) Boot-Modul** — `"source":{"kind":"boot","module":N}`: das N-te Multiboot-Modul, das
//!   der Bootloader übergeben hat. Der Index ist eine Bootloader-Aussage (wie `mbi : N Modul(e)`
//!   in der Lade-Suite), die Bindung ans Byte ist [`verify_image`].
//! * **(b) Platte** — entweder per GPT-Partition (`"source":{"kind":"disk",
//!   "part_guid":"…"}`: die Unique-GUID des Partitionseintrags, 16 Bytes als 32 Hex-Zeichen) oder
//!   per Blockbereich (`"source":{"kind":"disk","start_lba":S,"sectors":L}`). Die GUID-Form ist
//!   gegen Verschieben der Partition robust, die Bereichs-Form gegen eine kaputte/fehlende
//!   Tabelle — beide nennen Bytes auf einem Datenträger, den ein beliebiger Mandant beschrieben
//!   haben kann (vgl. `caprock-part`: fremde Bytes, kein Kernprivileg).
//!
//! ## Warum `image_hash` SHA-256 ist — und die Signatur trotzdem FNV
//!
//! Zwei verschiedene Gegner, zwei verschiedene Antworten:
//!
//! * Das **Bild** liegt auf einer Platte, die der Angreifer beschreiben darf. Hier zählt
//!   Kollisions- und Preimage-Resistenz gegen einen aktiven Gegner — FNV-1a (nicht kryptografisch,
//!   trivial kollidierbar) wäre eine Attrappe. Deshalb **SHA-256** über die exakten Image-Bytes,
//!   abgelegt als 64 Kleinbuchstaben-Hex in `image_hash`. Die Implementierung unten ist reines
//!   sicheres Rust ohne Abhängigkeiten (`no_std`-fähig) und gegen die Standardvektoren geprüft.
//! * Die **Signatur** (`signature`, 16 Hex-Zeichen) ist derselbe Zeuge wie in [`crate::manifest`]:
//!   `FNV-1a-64-Hex(Roh-Pubkey || Kanonik)`, Kanonik = Objekt ohne `signature`, Schlüssel der
//!   obersten Ebene sortiert, kompakt. Sie erkennt versehentliche Verfälschung und bindet den
//!   Eintrag an einen Schlüssel — aber sie ersetzt keine Public-Key-Signatur: wer den Pubkey
//!   kennt (er steht öffentlich in `manifest_keys.rs`), kann sie nachrechnen UND fälschen. Echte
//!   Authentizität trägt die Ed25519-Signatur des einbettenden Dokuments (System-Manifest,
//!   Boot-Archiv); dieser Zeuge sagt nur: „gegen genau diesen Root-Schlüssel geprüft".
//!
//! ## Welcher Schlüssel? Derselbe wie das System-Manifest
//!
//! `key_id` ist `SHA-256(Pubkey)[..16]` als 32 Hex-Zeichen — exakt `key_id()` aus
//! `tools/gen_manifest_key.py` und `kid` aus `tools/sign_manifest.py`. Der Prüfer bekommt den
//! **Roh-Pubkey (32 Bytes)** vom Aufrufer (dessen Kopie der `MANIFEST_KEYS`-DB aus
//! `kernel/src/manifest_keys.rs`) und lehnt ab, wenn die eingetragene `key_id` nicht dazu passt.
//! Die Crate hängt damit an keinem Kernel-Modul und bleibt host-testbar; die Bindung „Eintrag →
//! Root-Schlüssel" steht trotzdem in jedem geprüften Eintrag.
//!
//! Abweichung zu `manifest::feed_key` bewusst festgehalten: dort kommt der Schlüssel als
//! **Hex-String** herein und wird sniffend dekodiert; hier ist er bereits Bytes (DB-Eintrag), also
//! wird er **roh** verfüttert. `feed_key` auf Roh-Bytes anzuwenden wäre nichtdeterministisch (ein
//! Pubkey, der zufällig nur aus Hex-Zeichen besteht, würde dekodiert, jeder andere nicht).
//!
//! ## Fehlerabbildung auf [`LxpdError`]
//!
//! * Strukturelles (kein Objekt, Felder fehlen/falsch, unbekannte `schema_version`, leere
//!   Namen, kaputte Herkunft, kaputte Bereichs-Arithmetik, Bild-Hash passt nicht,
//!   Eintragsmenge überlappt) → [`LxpdError::BadManifest`].
//! * `key_id` passt nicht zum übergebenen Pubkey, `signature` fehlt oder stimmt nicht →
//!   [`LxpdError::BadSignature`].
//!
//! ## Schichten (wie [`crate::manifest`])
//!
//! * [`parse_entry`] — Fakten ohne Krypto (Herkunft, Hash, Schlüsselbindung als Wert).
//! * [`verify_witness`] — nur die Schlüsselbindung (`key_id` + Zeuge).
//! * [`verify_image`] — nur die Bildbindung (SHA-256 über die Image-Bytes).
//! * [`overlap`] / [`check_set`] — keine zwei Einträge dürfen dieselbe Quelle beanspruchen.

#![forbid(unsafe_code)]

use crate::manifest::{
    MAX_NEST_KEYS, MAX_TOP_KEYS, Pair, SIGNATURE_HEX_LEN, feed_canonical_value, find, fnv_feed,
    is_quoted, parse_object_into, parse_u32, parse_u64, top_pairs, FNV64_OFFSET,
};
use crate::LxpdError;

// --- Feldnamen -----------------------------------------------------------------

/// Manifest-Fassung des Eintrags.
pub const F_SCHEMA_VERSION: &[u8] = b"schema_version";
/// Treibername (nicht leer).
pub const F_DRIVER: &[u8] = b"driver";
/// Kompat-Feld: LXPD-API, die der Treiber spricht (nicht leer, z. B. `"X1"`).
pub const F_API_VERSION: &[u8] = b"api_version";
/// Herkunfts-Objekt (`kind` = `boot`/`disk`, s. [`Source`]).
pub const F_SOURCE: &[u8] = b"source";
/// SHA-256 des Images (64 Hex-Zeichen).
pub const F_IMAGE_HASH: &[u8] = b"image_hash";
/// `SHA-256(Pubkey)[..16]` (32 Hex-Zeichen, wie `gen_manifest_key.py`).
pub const F_KEY_ID: &[u8] = b"key_id";
/// FNV-1a-64-Zeuge (16 Hex-Zeichen, vom kanonischen Hash ausgenommen).
pub const F_SIGNATURE: &[u8] = b"signature";

/// Herkunfts-Art im `source`-Objekt.
pub const F_KIND: &[u8] = b"kind";
/// Boot-Modul-Index (`kind = boot`).
pub const F_MODULE: &[u8] = b"module";
/// GPT-Unique-GUID (`kind = disk`, 32 Hex-Zeichen).
pub const F_PART_GUID: &[u8] = b"part_guid";
/// Erster Block (`kind = disk`, Bereichs-Form).
pub const F_START_LBA: &[u8] = b"start_lba";
/// Blockzahl (`kind = disk`, Bereichs-Form, `> 0`).
pub const F_SECTORS: &[u8] = b"sectors";

/// Einzige Fassung, die dieser Prüfer versteht.
pub const DRIVER_SCHEMA_VERSION: u32 = 1;
/// Hex-Länge eines SHA-256 (`image_hash`).
pub const IMAGE_HASH_HEX_LEN: usize = 64;
/// Hex-Länge einer Key-ID (16 Bytes).
pub const KEY_ID_HEX_LEN: usize = 32;
/// Hex-Länge einer GPT-GUID (16 Bytes).
pub const GUID_HEX_LEN: usize = 32;
/// Roh-Länge eines Ed25519-Pubkeys aus der Key-DB.
pub const PUBKEY_LEN: usize = 32;

// --- Typen ---------------------------------------------------------------------

/// Woher das Treiber-Image kommt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// N-tes Multiboot-Modul des Bootloaders.
    Boot {
        /// Modul-Index (Bootloader-Aussage; die Byte-Bindung leistet [`verify_image`]).
        module: u32,
    },
    /// GPT-Partition mit dieser Unique-GUID (robust gegen Verschieben der Partition).
    DiskGuid {
        /// 16 GUID-Bytes (nie ganz null — das hiesse „unbenutzter Eintrag", s. `caprock-part`).
        guid: [u8; 16],
    },
    /// Blockbereich `[start_lba, start_lba + sectors)` (robust gegen kaputte Tabelle).
    DiskRange {
        /// Erster Block.
        start_lba: u64,
        /// Blockzahl (`> 0`, ohne Überlauf addierbar).
        sectors: u64,
    },
}

/// Geprüfter Treiber-Eintrag — Fakten **ohne** Kryptourteil (s. Schichten-Doku oben).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DriverEntry<'a> {
    /// Muss [`DRIVER_SCHEMA_VERSION`] sein.
    pub schema_version: u32,
    /// Treibername (JSON-String ohne Anführungszeichen, nicht leer).
    pub driver: &'a [u8],
    /// LXPD-API (JSON-String ohne Anführungszeichen, nicht leer).
    pub api: &'a [u8],
    /// Herkunft des Images.
    pub source: Source,
    /// Erwartetes SHA-256 des Images.
    pub image_hash: [u8; 32],
    /// Behauptete `SHA-256(Pubkey)[..16]` — ob sie **stimmt**, sagt nur [`verify_witness`].
    pub key_id: [u8; 16],
    /// Ein `signature`-Feld ist **anwesend** (nicht leerer String). Ob es **stimmt**,
    /// sagt nur [`verify_witness`].
    pub signature_present: bool,
}

// --- Hex -----------------------------------------------------------------------

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Hex-String (exakt `2 * out.len()` Zeichen) nach `out` dekodieren.
fn decode_hex(s: &[u8], out: &mut [u8]) -> Result<(), LxpdError> {
    if s.len() != out.len() * 2 {
        return Err(LxpdError::BadManifest);
    }
    let mut i = 0;
    while i < out.len() {
        let hi = hex_val(*s.get(2 * i).ok_or(LxpdError::BadManifest)?)
            .ok_or(LxpdError::BadManifest)?;
        let lo = hex_val(*s.get(2 * i + 1).ok_or(LxpdError::BadManifest)?)
            .ok_or(LxpdError::BadManifest)?;
        out[i] = (hi << 4) | lo;
        i += 1;
    }
    Ok(())
}

/// Zitierten, nicht-leeren String-Inhalt liefern (ohne Anführungszeichen).
fn quoted_inner<'a>(span: &'a [u8]) -> Result<&'a [u8], LxpdError> {
    if !is_quoted(span) {
        return Err(LxpdError::BadManifest);
    }
    let innen = span.get(1..span.len() - 1).ok_or(LxpdError::BadManifest)?;
    if innen.is_empty() {
        return Err(LxpdError::BadManifest);
    }
    Ok(innen)
}

// --- SHA-256 (reines sicheres Rust, keine Abhängigkeit) -------------------------
//
// Standardrunde (FIPS 180-4). Wer sie gegen die Implementierung prüft, mit der sie
// verglichen wird, prüft nichts — deshalb stehen die Vektoren unten (`"", "abc"`).

const SHA_K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

fn sha_compress(h: &mut [u32; 8], block: &[u8; 64]) {
    let mut w = [0u32; 64];
    let mut i = 0;
    while i < 16 {
        w[i] = u32::from_be_bytes([
            block[4 * i],
            block[4 * i + 1],
            block[4 * i + 2],
            block[4 * i + 3],
        ]);
        i += 1;
    }
    while i < 64 {
        let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
        let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
        w[i] = w[i - 16]
            .wrapping_add(s0)
            .wrapping_add(w[i - 7])
            .wrapping_add(s1);
        i += 1;
    }
    let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) =
        (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);
    i = 0;
    while i < 64 {
        let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let ch = (e & f) ^ ((!e) & g);
        let t1 = hh
            .wrapping_add(s1)
            .wrapping_add(ch)
            .wrapping_add(SHA_K[i])
            .wrapping_add(w[i]);
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
        i += 1;
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

/// SHA-256 über `data`.
pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut off = 0usize;
    while off + 64 <= data.len() {
        let mut block = [0u8; 64];
        block.copy_from_slice(data.get(off..off + 64).unwrap_or(&[0u8; 64]));
        sha_compress(&mut h, &block);
        off += 64;
    }
    // Rest + Padding: genau ein `0x80`-Byte, Nullen, 8-Byte-Bit-Länge (big-endian).
    // Passt der Rest nicht mehr mit in den Block (≥ 56 Bytes), werden es zwei Blöcke.
    let rest = data.len() - off;
    let bitlen = (data.len() as u64).wrapping_mul(8);
    let mut tail = [0u8; 128];
    tail[..rest].copy_from_slice(data.get(off..).unwrap_or(b""));
    tail[rest] = 0x80;
    let padlen = if rest < 56 { 64 } else { 128 };
    tail[padlen - 8..padlen].copy_from_slice(&bitlen.to_be_bytes());
    let mut b1 = [0u8; 64];
    b1.copy_from_slice(&tail[..64]);
    sha_compress(&mut h, &b1);
    if padlen == 128 {
        let mut b2 = [0u8; 64];
        b2.copy_from_slice(&tail[64..128]);
        sha_compress(&mut h, &b2);
    }
    let mut out = [0u8; 32];
    let mut i = 0;
    while i < 8 {
        out[4 * i..4 * i + 4].copy_from_slice(&h[i].to_be_bytes());
        i += 1;
    }
    out
}

// --- Herkunft parsen ------------------------------------------------------------

/// Das `source`-Objekt in einen [`Source`] lesen.
fn parse_source(span: &[u8]) -> Result<Source, LxpdError> {
    let mut nest: [Pair; MAX_NEST_KEYS] = [Pair { key: b"", val: b"" }; MAX_NEST_KEYS];
    let n = parse_object_into(span, &mut nest).map_err(|_| LxpdError::BadManifest)?;
    let art = quoted_inner(find(&nest, n, F_KIND).ok_or(LxpdError::BadManifest)?)?;
    if art == b"boot" {
        let modul = find(&nest, n, F_MODULE).ok_or(LxpdError::BadManifest)?;
        let index = parse_u32(modul).ok_or(LxpdError::BadManifest)?;
        return Ok(Source::Boot { module: index });
    }
    if art == b"disk" {
        let guid = find(&nest, n, F_PART_GUID);
        let start = find(&nest, n, F_START_LBA);
        let len = find(&nest, n, F_SECTORS);
        match (guid, start, len) {
            (Some(g), None, None) => {
                let mut bytes = [0u8; 16];
                decode_hex(quoted_inner(g)?, &mut bytes)?;
                if bytes == [0u8; 16] {
                    // Null-GUID = unbenutzter GPT-Eintrag (`caprock-part`): keine Herkunft.
                    return Err(LxpdError::BadManifest);
                }
                return Ok(Source::DiskGuid { guid: bytes });
            }
            (None, Some(s), Some(l)) => {
                let start_lba = parse_u64(s).ok_or(LxpdError::BadManifest)?;
                let sectors = parse_u64(l).ok_or(LxpdError::BadManifest)?;
                if sectors == 0 {
                    return Err(LxpdError::BadManifest);
                }
                // Ein überlaufender Bereich würde jede spätere Überlappungsprüfung zur
                // Attrappe machen (vgl. `caprock-part`: geprüfte Multiplikation).
                start_lba.checked_add(sectors).ok_or(LxpdError::BadManifest)?;
                return Ok(Source::DiskRange { start_lba, sectors });
            }
            // Beides oder keines: der Eintrag weiss nicht, welche Bytes er meint.
            _ => return Err(LxpdError::BadManifest),
        }
    }
    Err(LxpdError::BadManifest)
}

// --- Fakten ---------------------------------------------------------------------

/// Treiber-Eintrag prüfen (ohne Krypto): Fassung, Namen, Herkunft, Hash-Form, Key-ID-Form.
/// Die Signatur wird hier nur auf **Anwesenheit** geprüft.
pub fn parse_entry(json: &[u8]) -> Result<DriverEntry<'_>, LxpdError> {
    let mut buf: [Pair; MAX_TOP_KEYS] = [Pair { key: b"", val: b"" }; MAX_TOP_KEYS];
    let n = top_pairs(json, &mut buf)?;

    let fassung = find(&buf, n, F_SCHEMA_VERSION).ok_or(LxpdError::BadManifest)?;
    if parse_u32(fassung).ok_or(LxpdError::BadManifest)? != DRIVER_SCHEMA_VERSION {
        return Err(LxpdError::BadManifest);
    }
    let treiber = quoted_inner(find(&buf, n, F_DRIVER).ok_or(LxpdError::BadManifest)?)?;
    let api = quoted_inner(find(&buf, n, F_API_VERSION).ok_or(LxpdError::BadManifest)?)?;
    let quelle = find(&buf, n, F_SOURCE).ok_or(LxpdError::BadManifest)?;
    if quelle.first() != Some(&b'{') {
        return Err(LxpdError::BadManifest);
    }
    let source = parse_source(quelle)?;
    let mut image_hash = [0u8; 32];
    decode_hex(
        quoted_inner(find(&buf, n, F_IMAGE_HASH).ok_or(LxpdError::BadManifest)?)?,
        &mut image_hash,
    )?;
    let mut key_id = [0u8; 16];
    decode_hex(
        quoted_inner(find(&buf, n, F_KEY_ID).ok_or(LxpdError::BadManifest)?)?,
        &mut key_id,
    )?;
    let signature_present = find(&buf, n, F_SIGNATURE)
        .is_some_and(|s| is_quoted(s) && s.len() > 2);

    Ok(DriverEntry {
        schema_version: DRIVER_SCHEMA_VERSION,
        driver: treiber,
        api,
        source,
        image_hash,
        key_id,
        signature_present,
    })
}

// --- Schlüsselbindung ------------------------------------------------------------

/// Die Schlüsselbindung wirklich nachrechnen: erst muss die eingetragene `key_id` zum
/// übergebenen Pubkey passen (`SHA-256(Pubkey)[..16]`, wie `gen_manifest_key.py`), dann der
/// FNV-1a-64-Zeuge über `Roh-Pubkey || Kanonik` (Kanonik wie in [`crate::manifest`], aber mit
/// **roh** verfüttertem Schlüssel — s. Modul-Doku). Sonst [`LxpdError::BadSignature`].
pub fn verify_witness(json: &[u8], pubkey: &[u8; PUBKEY_LEN]) -> Result<(), LxpdError> {
    let mut buf: [Pair; MAX_TOP_KEYS] = [Pair { key: b"", val: b"" }; MAX_TOP_KEYS];
    let n = top_pairs(json, &mut buf)?;
    let kid_span = quoted_inner(find(&buf, n, F_KEY_ID).ok_or(LxpdError::BadSignature)?)
        .map_err(|_| LxpdError::BadSignature)?;
    let mut kid = [0u8; 16];
    decode_hex(kid_span, &mut kid).map_err(|_| LxpdError::BadSignature)?;
    let digest = sha256(pubkey);
    if kid != digest[..16] {
        // Der Eintrag beruft sich auf einen anderen Root-Schlüssel als den vorgelegten.
        return Err(LxpdError::BadSignature);
    }
    let sig = find(&buf, n, F_SIGNATURE).ok_or(LxpdError::BadSignature)?;
    if !is_quoted(sig) {
        return Err(LxpdError::BadSignature);
    }
    let innen = sig.get(1..sig.len() - 1).ok_or(LxpdError::BadSignature)?;
    if innen.len() != SIGNATURE_HEX_LEN {
        return Err(LxpdError::BadSignature);
    }

    // Schlüssel der obersten Ebene sortieren (ohne `signature`) — dieselbe Kanonik wie
    // `manifest::verify_signature`.
    let mut idx: [usize; MAX_TOP_KEYS] = [0; MAX_TOP_KEYS];
    let mut m = 0usize;
    for k in 0..n {
        if buf[k].key != F_SIGNATURE {
            idx[m] = k;
            m += 1;
        }
    }
    let mut a = 0;
    while a < m {
        let mut b = a + 1;
        while b < m {
            if buf[idx[b]].key < buf[idx[a]].key {
                let t = idx[a];
                idx[a] = idx[b];
                idx[b] = t;
            }
            b += 1;
        }
        a += 1;
    }

    let mut h = FNV64_OFFSET;
    fnv_feed(&mut h, pubkey);
    fnv_feed(&mut h, b"{");
    for j in 0..m {
        if j > 0 {
            fnv_feed(&mut h, b",");
        }
        fnv_feed(&mut h, b"\"");
        fnv_feed(&mut h, buf[idx[j]].key);
        fnv_feed(&mut h, b"\":");
        feed_canonical_value(&mut h, buf[idx[j]].val);
    }
    fnv_feed(&mut h, b"}");

    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut erwartet = [0u8; SIGNATURE_HEX_LEN];
    for k in 0..8 {
        let byte = (h >> ((7 - k) * 8)) as u8;
        erwartet[2 * k] = HEX[(byte >> 4) as usize];
        erwartet[2 * k + 1] = HEX[(byte & 0x0f) as usize];
    }
    if innen == erwartet {
        Ok(())
    } else {
        Err(LxpdError::BadSignature)
    }
}

// --- Bildbindung ------------------------------------------------------------------

/// Das Image an den Eintrag binden: `SHA-256(image) == entry.image_hash`, sonst
/// [`LxpdError::BadManifest`] — der Eintrag beschreibt dann ein anderes Bild.
pub fn verify_image(entry: &DriverEntry, image: &[u8]) -> Result<(), LxpdError> {
    if sha256(image) == entry.image_hash {
        Ok(())
    } else {
        Err(LxpdError::BadManifest)
    }
}

// --- Mengenprüfung ------------------------------------------------------------------

/// Beanspruchen zwei Einträge dieselbe Quelle? Gleicher Modul-Index, gleiche GUID oder
/// sich schneidende Blockbereiche — artfremde Herkünfte (Boot gegen Platte) können nie
/// dieselben Bytes meinen und überlappen nicht.
pub fn overlap(a: &DriverEntry, b: &DriverEntry) -> bool {
    match (a.source, b.source) {
        (Source::Boot { module: x }, Source::Boot { module: y }) => x == y,
        (Source::DiskGuid { guid: x }, Source::DiskGuid { guid: y }) => x == y,
        (
            Source::DiskRange { start_lba: s1, sectors: l1 },
            Source::DiskRange { start_lba: s2, sectors: l2 },
        ) => {
            // `parse_entry` hat Überlauffreiheit bereits geprüft; hier defensiv mit
            // Sättigung statt Panik (Einträge können auch per Hand gebaut sein).
            let e1 = s1.saturating_add(l1);
            let e2 = s2.saturating_add(l2);
            s1 < e2 && s2 < e1
        }
        _ => false,
    }
}

/// Eine Eintragsmenge freigeben: kein Paar darf überlappen. Leere und einelementige Mengen
/// sind frei. Erster Überlapp → [`LxpdError::BadManifest`].
pub fn check_set(entries: &[DriverEntry]) -> Result<(), LxpdError> {
    let mut i = 0;
    while i < entries.len() {
        let mut j = i + 1;
        while j < entries.len() {
            if overlap(&entries[i], &entries[j]) {
                return Err(LxpdError::BadManifest);
            }
            j += 1;
        }
        i += 1;
    }
    Ok(())
}

// --- Host-Tests ------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const KEY_A: [u8; 32] = [0x42; 32];
    const KEY_B: [u8; 32] = [0x99; 32];

    fn hex(bytes: &[u8]) -> String {
        const H: &[u8; 16] = b"0123456789abcdef";
        let mut s = String::with_capacity(bytes.len() * 2);
        for &b in bytes {
            s.push(H[(b >> 4) as usize] as char);
            s.push(H[(b & 0x0f) as usize] as char);
        }
        s
    }

    fn fnv64(data: &[u8]) -> u64 {
        let mut h: u64 = 0xcbf29ce4842225c5;
        for b in data {
            h ^= *b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        h
    }

    fn key_id_of(pubkey: &[u8; 32]) -> String {
        hex(&sha256(pubkey)[..16])
    }

    /// Zeuge über Roh-Pubkey + Kanonik (Spiegel von [`verify_witness`], s. Modul-Doku).
    fn sign(canonical: &[u8], pubkey: &[u8; 32]) -> String {
        let mut input = pubkey.to_vec();
        input.extend_from_slice(canonical);
        format!("{:016x}", fnv64(&input))
    }

    /// Kompakte Kanonik (Schlüssel sortiert, ohne `signature`) — das, was das Werkzeug
    /// schreibt und der Prüfer wiederherstellen muss.
    fn canon(source: &str, img_hash: &str, kid: &str) -> Vec<u8> {
        format!(
            "{{\"api_version\":\"X1\",\"driver\":\"e1000e\",\"image_hash\":\"{img_hash}\",\"key_id\":\"{kid}\",\"schema_version\":1,\"source\":{source}}}"
        )
        .into_bytes()
    }

    /// Dasselbe Objekt, absichtlich hässlich: unsortiert, pretty — der Prüfer muss die
    /// Kanonik selbst wiederherstellen.
    fn pretty(source: &str, img_hash: &str, kid: &str, sig: &str) -> Vec<u8> {
        format!(
            "{{\n  \"signature\" : \"{sig}\",\n  \"source\" : {source},\n  \"driver\" : \"e1000e\",\n  \"image_hash\" : \"{img_hash}\",\n  \"key_id\" : \"{kid}\",\n  \"schema_version\" : 1,\n  \"api_version\" : \"X1\"\n}}"
        )
        .into_bytes()
    }

    const IMG: &[u8] = b"LXPD-test-image-plaetzehalter-bytes";
    const GUID_HEX: &str = "00112233445566778899aabbccddeeff";

    fn img_hash() -> String {
        hex(&sha256(IMG))
    }

    fn boot_src() -> String {
        "{\"kind\":\"boot\",\"module\":3}".to_string()
    }

    fn guid_src() -> String {
        format!("{{\"kind\":\"disk\",\"part_guid\":\"{GUID_HEX}\"}}")
    }

    fn range_src() -> String {
        "{\"kind\":\"disk\",\"sectors\":50,\"start_lba\":100}".to_string()
    }

    fn entry_for(source: &str, key: &[u8; 32]) -> Vec<u8> {
        let kid = key_id_of(key);
        let ih = img_hash();
        let c = canon(source, &ih, &kid);
        pretty(source, &ih, &kid, &sign(&c, key))
    }

    #[test]
    fn sha256_kennt_die_standardvektoren() {
        // Ohne diese Anker prüfte der Rest nur Selbstübereinstimmung — auch ein falsches
        // Polynom wäre mit sich selbst einig.
        assert_eq!(
            hex(&sha256(b"")),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            hex(&sha256(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        // Blockgrenze (56/64 Bytes) ist die klassische Padding-Falle: je ein Vektor diesseits,
        // drauf und jenseits.
        assert_eq!(
            hex(&sha256(&[0x61; 55])),
            "9f4390f8d30c2dd92ec9f095b65e2b9ae9b0a925a5258e241c9f1e910f734318"
        );
        assert_eq!(
            hex(&sha256(&[0x61; 56])),
            "b35439a4ac6f0948b6d6f9e3c6af0f5f590ce20f1bde7090ef7970686ec6738a"
        );
        assert_eq!(
            hex(&sha256(&[0x61; 64])),
            "ffe054fe7ae0cb6dc65c3af9b61d5209f439851db43d0ba5997337df154668eb"
        );
    }

    #[test]
    fn boot_eintrag_rundlauf() {
        let js = entry_for(&boot_src(), &KEY_A);
        let e = parse_entry(&js).unwrap();
        assert_eq!(e.schema_version, 1);
        assert_eq!(e.driver, b"e1000e");
        assert_eq!(e.api, b"X1");
        assert_eq!(e.source, Source::Boot { module: 3 });
        assert!(e.signature_present);
        assert_eq!(verify_witness(&js, &KEY_A), Ok(()));
        assert_eq!(verify_image(&e, IMG), Ok(()));
    }

    #[test]
    fn disk_guid_eintrag_rundlauf() {
        let js = entry_for(&guid_src(), &KEY_A);
        let e = parse_entry(&js).unwrap();
        let mut g = [0u8; 16];
        let raw = GUID_HEX.as_bytes();
        let mut i = 0;
        while i < 16 {
            let hi = hex_val(raw[2 * i]).unwrap();
            let lo = hex_val(raw[2 * i + 1]).unwrap();
            g[i] = (hi << 4) | lo;
            i += 1;
        }
        assert_eq!(e.source, Source::DiskGuid { guid: g });
        assert_eq!(verify_witness(&js, &KEY_A), Ok(()));
        assert_eq!(verify_image(&e, IMG), Ok(()));
    }

    #[test]
    fn disk_range_eintrag_rundlauf() {
        let js = entry_for(&range_src(), &KEY_A);
        let e = parse_entry(&js).unwrap();
        assert_eq!(
            e.source,
            Source::DiskRange { start_lba: 100, sectors: 50 }
        );
        assert_eq!(verify_witness(&js, &KEY_A), Ok(()));
        assert_eq!(verify_image(&e, IMG), Ok(()));
    }

    #[test]
    fn falsche_herkunft_abgewiesen() {
        let kid = key_id_of(&KEY_A);
        let ih = img_hash();
        let with_src = |src: &str| {
            let c = canon(src, &ih, &kid);
            pretty(src, &ih, &kid, &sign(&c, &KEY_A))
        };
        // Unbekannte Art.
        assert_eq!(
            parse_entry(&with_src("{\"kind\":\"tape\",\"module\":0}")).unwrap_err(),
            LxpdError::BadManifest
        );
        // Art fehlt.
        assert_eq!(
            parse_entry(&with_src("{\"module\":0}")).unwrap_err(),
            LxpdError::BadManifest
        );
        // Boot ohne Modul / mit unlesbarem Modul.
        assert_eq!(
            parse_entry(&with_src("{\"kind\":\"boot\"}")).unwrap_err(),
            LxpdError::BadManifest
        );
        assert_eq!(
            parse_entry(&with_src("{\"kind\":\"boot\",\"module\":\"drei\"}")).unwrap_err(),
            LxpdError::BadManifest
        );
        assert_eq!(
            parse_entry(&with_src("{\"kind\":\"boot\",\"module\":-1}")).unwrap_err(),
            LxpdError::BadManifest
        );
        // Kein Objekt als Herkunft.
        assert_eq!(
            parse_entry(&with_src("\"boot\"")).unwrap_err(),
            LxpdError::BadManifest
        );
    }

    #[test]
    fn platte_ohne_und_mit_beidem_abgewiesen() {
        let kid = key_id_of(&KEY_A);
        let ih = img_hash();
        let with_src = |src: &str| {
            let c = canon(src, &ih, &kid);
            pretty(src, &ih, &kid, &sign(&c, &KEY_A))
        };
        // Weder GUID noch Bereich: unklar, welche Bytes gemeint sind.
        assert_eq!(
            parse_entry(&with_src("{\"kind\":\"disk\"}")).unwrap_err(),
            LxpdError::BadManifest
        );
        // Beides: zwei Antworten auf dieselbe Frage.
        assert_eq!(
            parse_entry(&with_src(&format!(
                "{{\"kind\":\"disk\",\"part_guid\":\"{GUID_HEX}\",\"sectors\":50,\"start_lba\":100}}"
            )))
            .unwrap_err(),
            LxpdError::BadManifest
        );
        // Halber Bereich (nur Start).
        assert_eq!(
            parse_entry(&with_src("{\"kind\":\"disk\",\"start_lba\":100}")).unwrap_err(),
            LxpdError::BadManifest
        );
    }

    #[test]
    fn null_guid_und_kaputtes_hex_abgewiesen() {
        let kid = key_id_of(&KEY_A);
        let ih = img_hash();
        let with_src = |src: &str| {
            let c = canon(src, &ih, &kid);
            pretty(src, &ih, &kid, &sign(&c, &KEY_A))
        };
        // Null-GUID = unbenutzter GPT-Eintrag, keine Herkunft.
        assert_eq!(
            parse_entry(&with_src("{\"kind\":\"disk\",\"part_guid\":\"00000000000000000000000000000000\"}"))
                .unwrap_err(),
            LxpdError::BadManifest
        );
        // Kein Hex / falsche Länge.
        assert_eq!(
            parse_entry(&with_src("{\"kind\":\"disk\",\"part_guid\":\"xyz\"}")).unwrap_err(),
            LxpdError::BadManifest
        );
        assert_eq!(
            parse_entry(&with_src("{\"kind\":\"disk\",\"part_guid\":\"0011\"}")).unwrap_err(),
            LxpdError::BadManifest
        );
        // Kaputter Bild-Hash / kaputte Key-ID (Formfehler sind Faktenfehler, kein Krypto).
        let bad_hash = {
            let c = canon(&boot_src(), "zz", &kid);
            pretty(&boot_src(), "zz", &kid, &sign(&c, &KEY_A))
        };
        assert_eq!(parse_entry(&bad_hash).unwrap_err(), LxpdError::BadManifest);
        let short_hash = {
            let c = canon(&boot_src(), "abcd", &kid);
            pretty(&boot_src(), "abcd", &kid, &sign(&c, &KEY_A))
        };
        assert_eq!(parse_entry(&short_hash).unwrap_err(), LxpdError::BadManifest);
        let bad_kid = {
            let c = canon(&boot_src(), &ih, "0011");
            pretty(&boot_src(), &ih, "0011", &sign(&c, &KEY_A))
        };
        assert_eq!(parse_entry(&bad_kid).unwrap_err(), LxpdError::BadManifest);
    }

    #[test]
    fn bereich_ohne_laenge_und_ueberlauf_abgewiesen() {
        let kid = key_id_of(&KEY_A);
        let ih = img_hash();
        let with_src = |src: &str| {
            let c = canon(src, &ih, &kid);
            pretty(src, &ih, &kid, &sign(&c, &KEY_A))
        };
        // Leerer Bereich.
        assert_eq!(
            parse_entry(&with_src("{\"kind\":\"disk\",\"sectors\":0,\"start_lba\":100}"))
                .unwrap_err(),
            LxpdError::BadManifest
        );
        // Überlaufender Bereich (Ende jenseits u64::MAX).
        assert_eq!(
            parse_entry(&with_src(
                "{\"kind\":\"disk\",\"sectors\":2,\"start_lba\":18446744073709551615}"
            ))
            .unwrap_err(),
            LxpdError::BadManifest
        );
        // Unlesbare Zahl.
        assert_eq!(
            parse_entry(&with_src("{\"kind\":\"disk\",\"sectors\":\"viel\",\"start_lba\":1}"))
                .unwrap_err(),
            LxpdError::BadManifest
        );
    }

    #[test]
    fn falsches_bild_abgewiesen() {
        let js = entry_for(&boot_src(), &KEY_A);
        let e = parse_entry(&js).unwrap();
        // Ein gekipptes Bit im Image — der Eintrag meint ein anderes Bild.
        let mut bild = IMG.to_vec();
        bild[0] ^= 0x01;
        assert_eq!(verify_image(&e, &bild).unwrap_err(), LxpdError::BadManifest);
        assert_eq!(verify_image(&e, b"").unwrap_err(), LxpdError::BadManifest);
        // Das richtige Bild besteht weiterhin (kein einseitiger Prüfer).
        assert_eq!(verify_image(&e, IMG), Ok(()));
    }

    #[test]
    fn falsche_signatur_abgewiesen() {
        let js = entry_for(&boot_src(), &KEY_A);
        // Ein Zeichen gekippt: das erste Hex-Zeichen des Zeugen umschreiben.
        let mut gekippt = js.clone();
        let sigpos = js
            .windows(b"\"signature\"".len())
            .position(|w| w == b"\"signature\"")
            .expect("Zeuge vorhanden");
        let hexstart = sigpos + b"\"signature\"".len() + 4; // `"signature" : "` — pretty-Form
        let mut i = hexstart;
        while gekippt[i] == b'"' || gekippt[i] == b' ' || gekippt[i] == b':' {
            i += 1;
        }
        gekippt[i] = if gekippt[i] == b'0' { b'1' } else { b'0' };
        assert_eq!(verify_witness(&gekippt, &KEY_A).unwrap_err(), LxpdError::BadSignature);
        // Fehlender Zeuge: Fakten ja, Krypto nein.
        let kid = key_id_of(&KEY_A);
        let ih = img_hash();
        let ohne: Vec<u8> = format!(
            "{{\"api_version\":\"X1\",\"driver\":\"e1000e\",\"image_hash\":\"{ih}\",\"key_id\":\"{kid}\",\"schema_version\":1,\"source\":{}}}",
            boot_src()
        )
        .into_bytes();
        let f = parse_entry(&ohne).unwrap();
        assert!(!f.signature_present);
        assert_eq!(verify_witness(&ohne, &KEY_A).unwrap_err(), LxpdError::BadSignature);
    }

    #[test]
    fn falscher_schluessel_abgewiesen() {
        let js = entry_for(&boot_src(), &KEY_A);
        // Falscher Pubkey: schon die key_id passt nicht (andere Wurzel).
        assert_eq!(verify_witness(&js, &KEY_B).unwrap_err(), LxpdError::BadSignature);
        // Leerer Schlüssel erst recht nicht.
        assert_eq!(verify_witness(&js, &[0u8; 32]).unwrap_err(), LxpdError::BadSignature);
        // Umgekehrt: Eintrag für B besteht gegen B, nicht gegen A.
        let js_b = entry_for(&boot_src(), &KEY_B);
        assert_eq!(verify_witness(&js_b, &KEY_B), Ok(()));
        assert_eq!(verify_witness(&js_b, &KEY_A).unwrap_err(), LxpdError::BadSignature);
    }

    #[test]
    fn unbekannte_fassung_abgewiesen() {
        let kid = key_id_of(&KEY_A);
        let ih = img_hash();
        for fassung in ["0", "2", "99", "\"1\""] {
            let js: Vec<u8> = format!(
                "{{\"api_version\":\"X1\",\"driver\":\"e1000e\",\"image_hash\":\"{ih}\",\"key_id\":\"{kid}\",\"schema_version\":{fassung},\"source\":{}}}",
                boot_src()
            )
            .into_bytes();
            assert_eq!(parse_entry(&js).unwrap_err(), LxpdError::BadManifest, "{fassung}");
        }
        // Fehlende Fassung ebenso.
        let js: Vec<u8> = format!(
            "{{\"api_version\":\"X1\",\"driver\":\"e1000e\",\"image_hash\":\"{ih}\",\"key_id\":\"{kid}\",\"source\":{}}}",
            boot_src()
        )
        .into_bytes();
        assert_eq!(parse_entry(&js).unwrap_err(), LxpdError::BadManifest);
    }

    #[test]
    fn leere_namen_abgewiesen() {
        let kid = key_id_of(&KEY_A);
        let ih = img_hash();
        let js: Vec<u8> = format!(
            "{{\"api_version\":\"\",\"driver\":\"\",\"image_hash\":\"{ih}\",\"key_id\":\"{kid}\",\"schema_version\":1,\"source\":{}}}",
            boot_src()
        )
        .into_bytes();
        assert_eq!(parse_entry(&js).unwrap_err(), LxpdError::BadManifest);
    }

    #[test]
    fn ueberlappung_boot() {
        let js_a = entry_for(&boot_src(), &KEY_A);
        let js_b = entry_for(&boot_src(), &KEY_A);
        let a = parse_entry(&js_a).unwrap();
        let b = parse_entry(&js_b).unwrap();
        assert!(overlap(&a, &b));
        assert_eq!(check_set(&[a, b]).unwrap_err(), LxpdError::BadManifest);
        // Drei Einträge: der dritte doppelt den ersten.
        assert_eq!(check_set(&[a, b, a]).unwrap_err(), LxpdError::BadManifest);
        // Anderer Modul-Index: frei.
        let kid = key_id_of(&KEY_A);
        let ih = img_hash();
        let src1 = "{\"kind\":\"boot\",\"module\":4}";
        let c1 = canon(src1, &ih, &kid);
        let js1 = pretty(src1, &ih, &kid, &sign(&c1, &KEY_A));
        let c = parse_entry(&js1).unwrap();
        assert!(!overlap(&a, &c));
        assert_eq!(check_set(&[a, c]), Ok(()));
        // Leere und einelementige Mengen sind frei.
        assert_eq!(check_set(&[]), Ok(()));
        assert_eq!(check_set(&[a]), Ok(()));
    }

    #[test]
    fn ueberlappung_platte() {
        let js_g = entry_for(&guid_src(), &KEY_A);
        let js_g2 = entry_for(&guid_src(), &KEY_A);
        let g = parse_entry(&js_g).unwrap();
        let g2 = parse_entry(&js_g2).unwrap();
        assert!(overlap(&g, &g2));
        assert_eq!(check_set(&[g, g2]).unwrap_err(), LxpdError::BadManifest);

        // Bereiche: [100,150) gegen [140,190) schneiden sich; [150,200) berührt nur.
        let kid = key_id_of(&KEY_A);
        let ih = img_hash();
        let bereich_json = |s: u64, l: u64| {
            let src = format!("{{\"kind\":\"disk\",\"sectors\":{l},\"start_lba\":{s}}}");
            let c = canon(&src, &ih, &kid);
            pretty(&src, &ih, &kid, &sign(&c, &KEY_A))
        };
        let js_r1 = bereich_json(100, 50);
        let js_r2 = bereich_json(140, 50);
        let js_r3 = bereich_json(150, 50);
        let r1 = parse_entry(&js_r1).unwrap();
        let r2 = parse_entry(&js_r2).unwrap();
        let r3 = parse_entry(&js_r3).unwrap();
        assert!(overlap(&r1, &r2));
        assert_eq!(check_set(&[r1, r2]).unwrap_err(), LxpdError::BadManifest);
        // Berührung an der Kante ist kein Schnitt (halboffen).
        assert!(!overlap(&r1, &r3));
        assert_eq!(check_set(&[r1, r3]), Ok(()));
        // Artfremdes überlappt nie: Boot-Modul gegen Platte.
        let js_b = entry_for(&boot_src(), &KEY_A);
        let b = parse_entry(&js_b).unwrap();
        assert!(!overlap(&b, &g));
        assert!(!overlap(&b, &r1));
        assert!(!overlap(&g, &r1));
        assert_eq!(check_set(&[b, g, r1]), Ok(()));
    }

    #[test]
    fn kaputte_huelle_abgewiesen() {
        assert_eq!(parse_entry(b"[1,2]").unwrap_err(), LxpdError::BadManifest);
        assert_eq!(
            parse_entry(b"{\"schema_version\":1} Anhaengsel").unwrap_err(),
            LxpdError::BadManifest
        );
        assert_eq!(
            parse_entry(b"{\"schema_version\":1,\"driver\":\"x\"").unwrap_err(),
            LxpdError::BadManifest
        );
        assert_eq!(parse_entry(b"").unwrap_err(), LxpdError::BadManifest);
    }
}
