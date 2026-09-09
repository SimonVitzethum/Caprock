//! LXPD-Manifestprüfung — Feld-Fakten + Signatur, ohne Parser-Bibliothek.
//!
//! Das Manifest ist JSON (Felder u.a. `schema_version`/`driver`/`api_version`/`grants_bar`/
//! `dma_window`/`irq_vector`/`trampolines`/`class_b_objects`/`gpl_affected`/`coverage_pct`/
//! `signature`). `no_std` ohne `alloc` schliesst `serde_json` aus; stattdessen suchen wir
//! Felder auf Byte-Ebene und prüfen Werte mit einem minimalen, strikt validierenden
//! JSON-Leser (Objekt/Array/String/Literal, feste Schranken, keine Rekursion ins Blaue).
//!
//! ## Ehrlichkeit der Signaturprüfung
//!
//! `lx-bind` signiert so: `FNV-1a-64-Hex(Key-Bytes || Kanonik)`, wobei die Kanonik das
//! Manifest-Objekt **ohne** `signature`-Feld ist, Schlüssel der obersten Ebene **sortiert**,
//! kompakt serialisiert. Dieser Prüfer stellt dieselbe Kanonik wieder her — sortiert die
//! vorgefundenen Schlüssel (Blasensortierung über einem festen Puffer, kein `alloc`) und
//! füttert den Hasher im Strom: erst die Key-Bytes (Hex-Dekodierung wie `lx-bind`, sonst
//! roh), dann `{`, `"schlüssel":wert, …`, `}`.
//!
//! Entscheidend: Der Vergleich ist eine **echte Rechnung**. Wo die Kanonik nicht
//! wiederherstellbar wäre (kein Objekt, kaputte Syntax), gibt es [`LxpdError::BadManifest`],
//! bei fehlender/falscher Signatur [`LxpdError::BadSignature`] — aber **niemals** einen
//! angenommenen Pass. Ein falsch-negativer Ausschlag (fremde, aber äquivalente
//! String-Escapes) ist fail-closed und damit zulässig; ein falsch-positiver wäre es nicht.
//!
//! ## Schichten
//!
//! * [`manifest_facts`] — Fakten ohne Krypto (Version, Treiber, Coverage, Grants, IRQ,
//!   Stubnamen-Zahl, Signatur-Anwesenheit). Wer nur Fakten liest, sieht an
//!   [`ManifestFacts::signature_present`], dass die Signatur **nicht** geprüft wurde.
//! * [`verify_signature`] — nur die Kryptoprüfung.
//! * [`LxpdImage::verify_manifest`](crate::LxpdImage::verify_manifest) — Fakten plus
//!   Stubnamen-Abgleich gegen den Container plus Signatur.

use crate::LxpdError;

// --- JSON-Feldnamen als Byte-Konstanten (Feldsuche auf Bytes, kein Full-Parser) ---

/// Erwartete Manifest-Fassung.
pub const F_SCHEMA_VERSION: &[u8] = b"schema_version";
/// Treibername (muss nicht leer sein).
pub const F_DRIVER: &[u8] = b"driver";
/// Stubnamen der Form `src->dst` (Zahl muss zur Container-Zahl passen).
pub const F_TRAMPOLINES: &[u8] = b"trampolines";
/// Deckung in Prozent (strikt `100.0`, vgl. `bind`: darunter wird gar nicht gebunden).
pub const F_COVERAGE: &[u8] = b"coverage_pct";
/// BAR-Zusagen (mindestens ein Eintrag mit `size > 0`).
pub const F_GRANTS_BAR: &[u8] = b"grants_bar";
/// DMA-Fenster (Objekt mit `size > 0`).
pub const F_DMA_WINDOW: &[u8] = b"dma_window";
/// Grössenfeld in Grant-Objekten.
pub const F_SIZE: &[u8] = b"size";
/// IRQ-Vektor (muss `> 0` sein).
pub const F_IRQ_VECTOR: &[u8] = b"irq_vector";
/// Vom kanonischen Hash ausgenommenes Signaturfeld (FNV-1a-64-Hex, 16 Stellen).
pub const F_SIGNATURE: &[u8] = b"signature";

/// Manifest-Fassung, die dieser Prüfer versteht.
pub const MANIFEST_SCHEMA_VERSION: u32 = 1;
/// Länge einer FNV-1a-64-Hexsignatur.
pub const SIGNATURE_HEX_LEN: usize = 16;

/// Obergrenze der Schlüssel auf oberster Ebene (das `bind`-Manifest trägt 12).
pub(crate) const MAX_TOP_KEYS: usize = 16;
/// Obergrenze der Schlüssel in geschachtelten Objekten (BAR-Eintrag: 4, DMA: 3).
pub(crate) const MAX_NEST_KEYS: usize = 8;
/// Schachtelungstiefe beim Klammer-Abgleich.
const MAX_DEPTH: usize = 16;
/// Obergrenze der Array-Elemente in einem Durchlauf (Stubnamen, BARs).
const MAX_ITEMS: usize = 1024;

/// Geprüfte Manifest-Fakten — **ohne** Signatururteil (s. [`ManifestFacts::signature_present`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ManifestFacts {
    /// Manifest-Fassung (muss [`MANIFEST_SCHEMA_VERSION`] sein).
    pub schema_version: u32,
    /// Zahl der `src->dst`-Stubnamen.
    pub trampoline_names: usize,
    /// Geforderte Deckung (strikt `100.0`).
    pub coverage_pct: f64,
    /// Zahl der BAR-Zusagen (mindestens eine mit `size > 0`).
    pub bars: usize,
    /// Grösse des DMA-Fensters (`> 0`).
    pub dma_size: u64,
    /// IRQ-Vektor (`> 0`).
    pub irq: u32,
    /// Ein `signature`-Feld ist **anwesend** (nicht leerer String). Ob es **stimmt**,
    /// sagt nur [`verify_signature`].
    pub signature_present: bool,
}

// --- Minimaler JSON-Leser (strikt, schrankenbewehrt, ohne `alloc`) ---

fn is_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r')
}

fn skip_ws(d: &[u8], pos: &mut usize) {
    while d.get(*pos).is_some_and(|&b| is_ws(b)) {
        *pos += 1;
    }
}

/// String-Spanne samt Anführungszeichen ab `pos` (`d[pos]` muss `"` sein).
fn string_span(d: &[u8], pos: usize) -> Result<(usize, usize), LxpdError> {
    if d.get(pos) != Some(&b'"') {
        return Err(LxpdError::BadManifest);
    }
    // `pos < len`, also kein Überlauf: jede Schleifenrunde endet mit einem
    // erfolgreichen `.get`, d.h. der Index lag im Puffer.
    let mut i = pos + 1;
    loop {
        let b = *d.get(i).ok_or(LxpdError::BadManifest)?;
        if b == b'"' {
            return Ok((pos, i + 1));
        }
        if b == b'\\' {
            d.get(i + 1).ok_or(LxpdError::BadManifest)?;
            i += 2;
            continue;
        }
        if b < 0x20 {
            return Err(LxpdError::BadManifest);
        }
        i += 1;
    }
}

/// Wert-Spanne ab `pos`: String, geklammerter Block oder Literal bis zum Begrenzer.
fn value_span(d: &[u8], pos: usize) -> Result<(usize, usize), LxpdError> {
    let b = *d.get(pos).ok_or(LxpdError::BadManifest)?;
    if b == b'"' {
        return string_span(d, pos);
    }
    if b == b'{' || b == b'[' {
        return balanced_span(d, pos);
    }
    let mut i = pos;
    loop {
        let c = *d.get(i).ok_or(LxpdError::BadManifest)?;
        if c == b',' || c == b'}' || c == b']' {
            break;
        }
        if !(c.is_ascii_alphanumeric() || c == b'.' || c == b'+' || c == b'-' || is_ws(c)) {
            return Err(LxpdError::BadManifest);
        }
        i += 1;
    }
    let mut end = i;
    while end > pos && d.get(end - 1).is_some_and(|&b| is_ws(b)) {
        end -= 1;
    }
    if end == pos {
        return Err(LxpdError::BadManifest);
    }
    Ok((pos, end))
}

/// Geklammerter Block ab `pos` — mit Klammer**art**-Abgleich auf einem festen Stapel
/// (`{"a":]` ist kein Objekt, sondern kaputt).
fn balanced_span(d: &[u8], pos: usize) -> Result<(usize, usize), LxpdError> {
    let open = *d.get(pos).ok_or(LxpdError::BadManifest)?;
    if open != b'{' && open != b'[' {
        return Err(LxpdError::BadManifest);
    }
    let mut stack: [u8; MAX_DEPTH] = [0; MAX_DEPTH];
    let mut depth = 0usize;
    let mut i = pos;
    while let Some(&b) = d.get(i) {
        if b == b'"' {
            let (_, e) = string_span(d, i)?;
            i = e;
            continue;
        }
        if b == b'{' || b == b'[' {
            if depth >= MAX_DEPTH {
                return Err(LxpdError::BadManifest);
            }
            stack[depth] = b;
            depth += 1;
        } else if b == b'}' || b == b']' {
            if depth == 0 {
                return Err(LxpdError::BadManifest);
            }
            depth -= 1;
            let o = stack[depth];
            let passt = (o == b'{' && b == b'}') || (o == b'[' && b == b']');
            if !passt {
                return Err(LxpdError::BadManifest);
            }
            if depth == 0 {
                return Ok((pos, i + 1));
            }
        }
        i += 1;
    }
    Err(LxpdError::BadManifest)
}

/// Ein Schlüssel/Wert-Paar als Sicht auf die Eingabe (keine Kopie, kein `alloc`).
#[derive(Clone, Copy)]
pub(crate) struct Pair<'a> {
    pub(crate) key: &'a [u8],
    pub(crate) val: &'a [u8],
}

/// Objekt-Spanne (`{…}`) in `out` zerlegen. Doppelte Schlüssel: der letzte gewinnt
/// (wie die `BTreeMap`-Kanonik in `lx-bind`). Mehr Schlüssel als Puffer → [`LxpdError::BadManifest`].
pub(crate) fn parse_object_into<'a>(span: &'a [u8], out: &mut [Pair<'a>]) -> Result<usize, LxpdError> {
    if span.first() != Some(&b'{') || span.last() != Some(&b'}') {
        return Err(LxpdError::BadManifest);
    }
    let mut n = 0usize;
    let mut pos = 1usize;
    skip_ws(span, &mut pos);
    if span.get(pos) == Some(&b'}') {
        return Ok(0);
    }
    loop {
        if span.get(pos) != Some(&b'"') {
            return Err(LxpdError::BadManifest);
        }
        let (ks, ke) = string_span(span, pos)?;
        let key = span.get(ks + 1..ke - 1).ok_or(LxpdError::BadManifest)?;
        pos = ke;
        skip_ws(span, &mut pos);
        if span.get(pos) != Some(&b':') {
            return Err(LxpdError::BadManifest);
        }
        pos += 1;
        skip_ws(span, &mut pos);
        let (vs, ve) = value_span(span, pos)?;
        let val = span.get(vs..ve).ok_or(LxpdError::BadManifest)?;
        let mut ersetzt = false;
        for k in 0..n {
            if out.get(k).is_some_and(|p| p.key == key) {
                out[k].val = val;
                ersetzt = true;
                break;
            }
        }
        if !ersetzt {
            *out.get_mut(n).ok_or(LxpdError::BadManifest)? = Pair { key, val };
            n += 1;
        }
        pos = ve;
        skip_ws(span, &mut pos);
        match span.get(pos) {
            Some(b',') => {
                pos += 1;
                skip_ws(span, &mut pos);
            }
            Some(b'}') => return Ok(n),
            _ => return Err(LxpdError::BadManifest),
        }
    }
}

/// Die oberste Ebene muss **genau ein** Objekt sein (führende/folgende Leerzeichen ok,
/// Anhängsel dahinter nicht).
pub(crate) fn top_pairs<'a>(json: &'a [u8], buf: &mut [Pair<'a>]) -> Result<usize, LxpdError> {
    let mut pos = 0usize;
    skip_ws(json, &mut pos);
    let (s, e) = value_span(json, pos)?;
    let mut rest = e;
    skip_ws(json, &mut rest);
    if rest != json.len() {
        return Err(LxpdError::BadManifest);
    }
    let span = json.get(s..e).ok_or(LxpdError::BadManifest)?;
    if span.first() != Some(&b'{') {
        return Err(LxpdError::BadManifest);
    }
    parse_object_into(span, buf)
}

pub(crate) fn find<'a>(pairs: &[Pair<'a>], n: usize, key: &[u8]) -> Option<&'a [u8]> {
    let mut i = 0;
    while i < n {
        if pairs.get(i).is_some_and(|p| p.key == key) {
            return Some(pairs[i].val);
        }
        i += 1;
    }
    None
}

// --- Zahlen (per Hand, überlaufgeprüft) ---

pub(crate) fn parse_u32(s: &[u8]) -> Option<u32> {
    if s.is_empty() {
        return None;
    }
    let mut v: u32 = 0;
    for &b in s {
        if !b.is_ascii_digit() {
            return None;
        }
        v = v.checked_mul(10)?.checked_add(u32::from(b - b'0'))?;
    }
    Some(v)
}

pub(crate) fn parse_u64(s: &[u8]) -> Option<u64> {
    if s.is_empty() {
        return None;
    }
    let mut v: u64 = 0;
    for &b in s {
        if !b.is_ascii_digit() {
            return None;
        }
        v = v.checked_mul(10)?.checked_add(u64::from(b - b'0'))?;
    }
    Some(v)
}

/// Strikte JSON-Zahl: `-`? Ziffern (`.` Ziffern)? (`e`/`E` [`+`/`-`] Ziffern)? — und nichts sonst.
fn parse_f64(s: &[u8]) -> Option<f64> {
    let mut i = 0;
    let mut neg = false;
    if s.get(i) == Some(&b'-') {
        neg = true;
        i += 1;
    }
    let ganz_ab = i;
    while s.get(i).is_some_and(|b| b.is_ascii_digit()) {
        i += 1;
    }
    if i == ganz_ab {
        return None;
    }
    let mut v: f64 = 0.0;
    for &b in s.get(ganz_ab..i)? {
        v = v * 10.0 + f64::from(b - b'0');
    }
    if s.get(i) == Some(&b'.') {
        i += 1;
        let bruch_ab = i;
        while s.get(i).is_some_and(|b| b.is_ascii_digit()) {
            i += 1;
        }
        if i == bruch_ab {
            return None;
        }
        let mut stelle = 0.1;
        for &b in s.get(bruch_ab..i)? {
            v += f64::from(b - b'0') * stelle;
            stelle *= 0.1;
        }
    }
    if s.get(i) == Some(&b'e') || s.get(i) == Some(&b'E') {
        i += 1;
        let mut eneg = false;
        if s.get(i) == Some(&b'+') {
            i += 1;
        } else if s.get(i) == Some(&b'-') {
            eneg = true;
            i += 1;
        }
        let exp_ab = i;
        while s.get(i).is_some_and(|b| b.is_ascii_digit()) {
            i += 1;
        }
        if i == exp_ab {
            return None;
        }
        let mut exp: i32 = 0;
        for &b in s.get(exp_ab..i)? {
            exp = exp.checked_mul(10)?.checked_add(i32::from(b - b'0'))?;
        }
        // `powi` ist std-only — per Hand, gedeckelt (f64 trägt ±1e308; alles darüber ist
        // garantiert inf/0, egal welche Mantisse davor steht).
        if exp > 310 {
            v = if eneg { 0.0 } else { f64::INFINITY };
        } else {
            let mut k = exp;
            if eneg {
                while k > 0 {
                    v /= 10.0;
                    k -= 1;
                }
            } else {
                while k > 0 {
                    v *= 10.0;
                    k -= 1;
                }
            }
        }
    }
    if i != s.len() {
        return None;
    }
    Some(if neg { -v } else { v })
}

// --- Fakten ---------------------------------------------------------------

pub(crate) fn is_quoted(s: &[u8]) -> bool {
    s.len() >= 2 && s.first() == Some(&b'"') && s.last() == Some(&b'"')
}

fn contains_arrow(s: &[u8]) -> bool {
    let mut i = 0;
    while let Some(w) = s.get(i..i + 2) {
        if w == b"->" {
            return true;
        }
        i += 1;
    }
    false
}

/// Array-Elemente zählen, jedes muss ein `"src->dst"`-String sein.
fn count_arrow_strings(span: &[u8]) -> Result<usize, LxpdError> {
    if span.first() != Some(&b'[') {
        return Err(LxpdError::BadManifest);
    }
    let mut pos = 1usize;
    skip_ws(span, &mut pos);
    if span.get(pos) == Some(&b']') {
        return Ok(0);
    }
    let mut n = 0usize;
    loop {
        if n >= MAX_ITEMS {
            return Err(LxpdError::BadManifest);
        }
        let (vs, ve) = value_span(span, pos)?;
        let item = span.get(vs..ve).ok_or(LxpdError::BadManifest)?;
        if !is_quoted(item) {
            return Err(LxpdError::BadManifest);
        }
        let innen = item.get(1..item.len() - 1).ok_or(LxpdError::BadManifest)?;
        if !contains_arrow(innen) {
            return Err(LxpdError::BadManifest);
        }
        n += 1;
        pos = ve;
        skip_ws(span, &mut pos);
        match span.get(pos) {
            Some(b',') => {
                pos += 1;
                skip_ws(span, &mut pos);
            }
            Some(b']') => return Ok(n),
            _ => return Err(LxpdError::BadManifest),
        }
    }
}

/// BAR-Array prüfen: mindestens ein Eintrag, mindestens einer mit `size > 0`.
/// Gibt die Eintragszahl zurück. Fehler hier sind [`LxpdError::BadGrants`].
fn check_bars(span: &[u8]) -> Result<usize, LxpdError> {
    if span.first() != Some(&b'[') {
        return Err(LxpdError::BadGrants);
    }
    let mut pos = 1usize;
    skip_ws(span, &mut pos);
    if span.get(pos) == Some(&b']') {
        return Err(LxpdError::BadGrants);
    }
    let mut n = 0usize;
    let mut mit_groesse = 0usize;
    loop {
        if n >= MAX_ITEMS {
            return Err(LxpdError::BadManifest);
        }
        let (vs, ve) = value_span(span, pos).map_err(|_| LxpdError::BadGrants)?;
        let item = span.get(vs..ve).ok_or(LxpdError::BadGrants)?;
        let mut nest: [Pair; MAX_NEST_KEYS] =
            [Pair { key: b"", val: b"" }; MAX_NEST_KEYS];
        if let Ok(c) = parse_object_into(item, &mut nest) {
            if let Some(grob) = find(&nest, c, F_SIZE) {
                if parse_u64(grob).is_some_and(|g| g > 0) {
                    mit_groesse += 1;
                }
            }
        }
        n += 1;
        pos = ve;
        skip_ws(span, &mut pos);
        match span.get(pos) {
            Some(b',') => {
                pos += 1;
                skip_ws(span, &mut pos);
            }
            Some(b']') => break,
            _ => return Err(LxpdError::BadGrants),
        }
    }
    if mit_groesse == 0 {
        return Err(LxpdError::BadGrants);
    }
    Ok(n)
}

/// Manifest-Fakten prüfen (ohne Krypto): Fassung, Treiber, Coverage, Grants, IRQ,
/// Stubnamen-Form. Die Signatur wird hier nur auf **Anwesenheit** geprüft.
pub fn manifest_facts(json: &[u8]) -> Result<ManifestFacts, LxpdError> {
    let mut buf: [Pair; MAX_TOP_KEYS] = [Pair { key: b"", val: b"" }; MAX_TOP_KEYS];
    let n = top_pairs(json, &mut buf)?;

    let fassung = find(&buf, n, F_SCHEMA_VERSION).ok_or(LxpdError::BadManifest)?;
    if parse_u32(fassung).ok_or(LxpdError::BadManifest)? != MANIFEST_SCHEMA_VERSION {
        return Err(LxpdError::BadManifest);
    }
    let treiber = find(&buf, n, F_DRIVER).ok_or(LxpdError::BadManifest)?;
    if !is_quoted(treiber) || treiber.len() == 2 {
        return Err(LxpdError::BadManifest);
    }
    let deckung = find(&buf, n, F_COVERAGE).ok_or(LxpdError::BadManifest)?;
    let coverage_pct = parse_f64(deckung).ok_or(LxpdError::BadManifest)?;
    if coverage_pct != 100.0 {
        return Err(LxpdError::BadManifest);
    }
    let bars_span = find(&buf, n, F_GRANTS_BAR).ok_or(LxpdError::BadGrants)?;
    let bars = check_bars(bars_span)?;
    let dma_span = find(&buf, n, F_DMA_WINDOW).ok_or(LxpdError::BadGrants)?;
    let mut nest: [Pair; MAX_NEST_KEYS] = [Pair { key: b"", val: b"" }; MAX_NEST_KEYS];
    let dma_size = parse_object_into(dma_span, &mut nest)
        .ok()
        .and_then(|c| find(&nest, c, F_SIZE))
        .and_then(parse_u64)
        .ok_or(LxpdError::BadGrants)?;
    if dma_size == 0 {
        return Err(LxpdError::BadGrants);
    }
    let irq_span = find(&buf, n, F_IRQ_VECTOR).ok_or(LxpdError::BadGrants)?;
    let irq = parse_u32(irq_span).ok_or(LxpdError::BadGrants)?;
    if irq == 0 {
        return Err(LxpdError::BadGrants);
    }
    let tramp_span = find(&buf, n, F_TRAMPOLINES).ok_or(LxpdError::BadManifest)?;
    let trampoline_names = count_arrow_strings(tramp_span)?;
    let signature_present = find(&buf, n, F_SIGNATURE)
        .is_some_and(|s| is_quoted(s) && s.len() > 2);

    Ok(ManifestFacts {
        schema_version: MANIFEST_SCHEMA_VERSION,
        trampoline_names,
        coverage_pct,
        bars,
        dma_size,
        irq,
        signature_present,
    })
}

// --- Signatur (echte Rechnung, kein angenommener Pass) ---------------------

pub(crate) const FNV64_OFFSET: u64 = 0xcbf29ce4842225c5;
pub(crate) const FNV64_PRIME: u64 = 0x100000001b3;

pub(crate) fn fnv_feed(h: &mut u64, bytes: &[u8]) {
    for &b in bytes {
        *h ^= u64::from(b);
        *h = h.wrapping_mul(FNV64_PRIME);
    }
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn trim_ascii(s: &[u8]) -> &[u8] {
    let mut a = 0;
    let mut e = s.len();
    while s.get(a).is_some_and(|&b| is_ws(b)) {
        a += 1;
    }
    while e > a && s.get(e - 1).is_some_and(|&b| is_ws(b)) {
        e -= 1;
    }
    s.get(a..e).unwrap_or(b"")
}

/// Key-Bytes exakt wie `lx-bind::key_bytes` in den Hasher füttern: gerader Hex-String
/// → dekodiert, sonst die rohen Key-Bytes.
fn feed_key(h: &mut u64, key: &[u8]) {
    let t = trim_ascii(key);
    let mut is_hex = t.len() % 2 == 0;
    if is_hex {
        for &b in t {
            if hex_val(b).is_none() {
                is_hex = false;
                break;
            }
        }
    }
    if is_hex {
        let mut i = 0;
        while i < t.len() {
            // `t.len()` ist gerade und jeder Index wurde oben per `.get` erreicht.
            let hi = hex_val(t[i]).unwrap_or(0);
            let lo = hex_val(t[i + 1]).unwrap_or(0);
            fnv_feed(h, &[(hi << 4) | lo]);
            i += 2;
        }
    } else {
        fnv_feed(h, key);
    }
}

/// Einen Wert in kanonischer (kompakter) Form füttern: alles ausserhalb von Strings, was
/// Leerzeichen ist, fällt weg. Strings kommen byte-identisch wieder heraus — für von
/// `serde_json` erzeugte Manifeste ist das exakt die Kompaktform derselben Kodierung.
pub(crate) fn feed_canonical_value(h: &mut u64, span: &[u8]) {
    let mut i = 0;
    while i < span.len() {
        let b = span[i];
        if b == b'"' {
            match string_span(span, i) {
                Ok((s, e)) => {
                    fnv_feed(h, span.get(s..e).unwrap_or(b""));
                    i = e;
                    continue;
                }
                Err(_) => {
                    // Unerreichbar nach validiertem Parse; fail-closed weiterfüttern.
                    fnv_feed(h, span.get(i..).unwrap_or(b""));
                    break;
                }
            }
        }
        if !is_ws(b) {
            fnv_feed(h, &[b]);
        }
        i += 1;
    }
}

/// Die Signatur wirklich nachrechnen: `FNV-1a-64-Hex(Key || Kanonik)` gegen das
/// `signature`-Feld. Fehlend/falsch → [`LxpdError::BadSignature`].
pub fn verify_signature(json: &[u8], key: &[u8]) -> Result<(), LxpdError> {
    if key.is_empty() {
        return Err(LxpdError::BadSignature);
    }
    let mut buf: [Pair; MAX_TOP_KEYS] = [Pair { key: b"", val: b"" }; MAX_TOP_KEYS];
    let n = top_pairs(json, &mut buf)?;
    let sig = find(&buf, n, F_SIGNATURE).ok_or(LxpdError::BadSignature)?;
    if !is_quoted(sig) {
        // `null` (unsigniert aus `bind`) oder kein String: keine prüfbare Signatur.
        return Err(LxpdError::BadSignature);
    }
    let innen = sig.get(1..sig.len() - 1).ok_or(LxpdError::BadSignature)?;
    if innen.len() != SIGNATURE_HEX_LEN {
        return Err(LxpdError::BadSignature);
    }

    // Schlüssel der obersten Ebene sortieren (ohne `signature`), Blasensortierung über
    // Indexe — kein `alloc`, höchstens 16 Schlüssel.
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
    feed_key(&mut h, key);
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
        let byte = (h >> ((7 - k) * 8)) as u8; // `{:016x}`: höchstwertig zuerst
        erwartet[2 * k] = HEX[(byte >> 4) as usize];
        erwartet[2 * k + 1] = HEX[(byte & 0x0f) as usize];
    }
    if innen == erwartet {
        Ok(())
    } else {
        Err(LxpdError::BadSignature)
    }
}

// --- Host-Tests ------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn fnv64(data: &[u8]) -> u64 {
        let mut h: u64 = 0xcbf29ce4842225c5;
        for b in data {
            h ^= *b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        h
    }

    fn hex_val_t(b: u8) -> u8 {
        match b {
            b'0'..=b'9' => b - b'0',
            b'a'..=b'f' => b - b'a' + 10,
            b'A'..=b'F' => b - b'A' + 10,
            _ => 0,
        }
    }

    /// Spiegel von `lx-bind::key_bytes` (Hex-Dekodierung, sonst roh).
    fn key_bytes_mirror(key: &str) -> Vec<u8> {
        let s = key.trim();
        let is_hex = s.len() % 2 == 0 && s.bytes().all(|c| c.is_ascii_hexdigit());
        if is_hex {
            let b = s.as_bytes();
            let mut out = Vec::with_capacity(b.len() / 2);
            let mut i = 0;
            while i < b.len() {
                out.push((hex_val_t(b[i]) << 4) | hex_val_t(b[i + 1]));
                i += 2;
            }
            out
        } else {
            key.as_bytes().to_vec()
        }
    }

    fn sign(canonical: &[u8], key: &str) -> String {
        let mut input = key_bytes_mirror(key);
        input.extend_from_slice(canonical);
        format!("{:016x}", fnv64(&input))
    }

    /// Kanonik per Hand (kompakt, Schlüssel sortiert, ohne `signature`).
    const CANON: &[u8] = b"{\"api_version\":\"X1\",\"arch\":\"x86-64\",\"class_b_objects\":[],\"coverage_pct\":100.0,\"dma_window\":{\"base\":0,\"size\":65536,\"bits\":64},\"driver\":\"e1000e\",\"gpl_affected\":false,\"grants_bar\":[{\"index\":0,\"base\":4096,\"size\":8192,\"flags\":\"RW\"}],\"irq_vector\":7,\"schema_version\":1,\"trampolines\":[\"a->b\",\"c->d\"]}";

    fn pretty(sig: &str) -> Vec<u8> {
        format!(
            "{{\n  \"signature\" : \"{sig}\",\n  \"driver\":\"e1000e\",\n  \"trampolines\" : [ \"a->b\" , \"c->d\" ],\n  \"schema_version\" : 1,\n  \"api_version\" : \"X1\",\n  \"arch\" : \"x86-64\",\n  \"grants_bar\" : [ {{\"index\":0,\"base\":4096,\"size\":8192,\"flags\":\"RW\"}} ],\n  \"dma_window\" : {{\"base\" : 0 , \"size\" : 65536 , \"bits\" : 64}},\n  \"irq_vector\" : 7,\n  \"class_b_objects\" : [],\n  \"gpl_affected\" : false,\n  \"coverage_pct\" : 100.0\n}}"
        )
        .into_bytes()
    }

    #[test]
    fn facts_and_signature_roundtrip() {
        let js = pretty(&sign(CANON, "deadbeef"));
        let f = manifest_facts(&js).unwrap();
        assert_eq!(f.schema_version, 1);
        assert_eq!(f.trampoline_names, 2);
        assert_eq!(f.coverage_pct, 100.0);
        assert_eq!(f.bars, 1);
        assert_eq!(f.dma_size, 65536);
        assert_eq!(f.irq, 7);
        assert!(f.signature_present);
        assert_eq!(verify_signature(&js, b"deadbeef"), Ok(()));
    }

    #[test]
    fn wrong_key_rejected() {
        let js = pretty(&sign(CANON, "deadbeef"));
        assert_eq!(verify_signature(&js, b"cafebabe").unwrap_err(), LxpdError::BadSignature);
        assert_eq!(verify_signature(&js, b"").unwrap_err(), LxpdError::BadSignature);
    }

    #[test]
    fn tampered_signature_rejected() {
        let mut sig = sign(CANON, "deadbeef").into_bytes();
        sig[0] = if sig[0] == b'0' { b'1' } else { b'0' };
        let js = pretty(core::str::from_utf8(&sig).unwrap());
        assert_eq!(verify_signature(&js, b"deadbeef").unwrap_err(), LxpdError::BadSignature);
    }

    #[test]
    fn missing_signature_rejected() {
        // Kein `signature`-Feld, aber sonst gültig.
        let bare: Vec<u8> = b"{\"schema_version\":1,\"driver\":\"e1000e\",\"coverage_pct\":100.0,\"grants_bar\":[{\"size\":8}],\"dma_window\":{\"size\":1},\"irq_vector\":7,\"trampolines\":[\"a->b\"]}".to_vec();
        let f = manifest_facts(&bare).unwrap();
        assert!(!f.signature_present);
        assert_eq!(verify_signature(&bare, b"deadbeef").unwrap_err(), LxpdError::BadSignature);
        // `null` wie aus unsigniertem `bind`.
        let nullsig: Vec<u8> = b"{\"schema_version\":1,\"driver\":\"e1000e\",\"coverage_pct\":100.0,\"grants_bar\":[{\"size\":8}],\"dma_window\":{\"size\":1},\"irq_vector\":7,\"trampolines\":[\"a->b\"],\"signature\":null}".to_vec();
        assert_eq!(
            verify_signature(&nullsig, b"deadbeef").unwrap_err(),
            LxpdError::BadSignature
        );
    }

    #[test]
    fn bad_coverage_rejected() {
        let js: Vec<u8> = b"{\"schema_version\":1,\"driver\":\"e1000e\",\"coverage_pct\":99.5,\"grants_bar\":[{\"size\":8}],\"dma_window\":{\"size\":1},\"irq_vector\":7,\"trampolines\":[\"a->b\"]}".to_vec();
        assert_eq!(manifest_facts(&js).unwrap_err(), LxpdError::BadManifest);
    }

    #[test]
    fn bad_grants_rejected() {
        let base = |bar: &str, dma: &str, irq: &str| {
            format!(
                "{{\"schema_version\":1,\"driver\":\"e1000e\",\"coverage_pct\":100.0,\"grants_bar\":{bar},\"dma_window\":{dma},\"irq_vector\":{irq},\"trampolines\":[\"a->b\"]}}"
            )
            .into_bytes()
        };
        // BAR-Grösse null.
        assert_eq!(
            manifest_facts(&base("[{\"size\":0}]", "{\"size\":1}", "7")).unwrap_err(),
            LxpdError::BadGrants
        );
        // Leeres BAR-Array.
        assert_eq!(
            manifest_facts(&base("[]", "{\"size\":1}", "7")).unwrap_err(),
            LxpdError::BadGrants
        );
        // DMA-Fenster null.
        assert_eq!(
            manifest_facts(&base("[{\"size\":8}]", "{\"size\":0}", "7")).unwrap_err(),
            LxpdError::BadGrants
        );
        // IRQ null.
        assert_eq!(
            manifest_facts(&base("[{\"size\":8}]", "{\"size\":1}", "0")).unwrap_err(),
            LxpdError::BadGrants
        );
    }

    #[test]
    fn structural_damage_rejected() {
        // Kein Objekt.
        assert_eq!(manifest_facts(b"[1,2]").unwrap_err(), LxpdError::BadManifest);
        // Anhängsel hinter dem Objekt.
        assert_eq!(
            manifest_facts(b"{\"schema_version\":1} trailing").unwrap_err(),
            LxpdError::BadManifest
        );
        // Falsche Fassung.
        assert_eq!(
            manifest_facts(b"{\"schema_version\":2,\"driver\":\"x\",\"coverage_pct\":100.0,\"grants_bar\":[{\"size\":1}],\"dma_window\":{\"size\":1},\"irq_vector\":1,\"trampolines\":[]}").unwrap_err(),
            LxpdError::BadManifest
        );
        // Leerer Treiber.
        assert_eq!(
            manifest_facts(b"{\"schema_version\":1,\"driver\":\"\",\"coverage_pct\":100.0,\"grants_bar\":[{\"size\":1}],\"dma_window\":{\"size\":1},\"irq_vector\":1,\"trampolines\":[]}").unwrap_err(),
            LxpdError::BadManifest
        );
        // Stubname ohne Pfeil.
        assert_eq!(
            manifest_facts(b"{\"schema_version\":1,\"driver\":\"x\",\"coverage_pct\":100.0,\"grants_bar\":[{\"size\":1}],\"dma_window\":{\"size\":1},\"irq_vector\":1,\"trampolines\":[\"ohne-pfeil\"]}").unwrap_err(),
            LxpdError::BadManifest
        );
        // Abgeschnitten.
        assert_eq!(
            manifest_facts(b"{\"schema_version\":1,\"driver\":\"x\"").unwrap_err(),
            LxpdError::BadManifest
        );
    }
}
