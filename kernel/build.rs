//! **Cargo kennt Linkerskripte nicht als Eingabe — und es mischt Konfigurationen. Beides hier.**
//!
//! # 1. Das Linkerskript wird HIER gesetzt, nicht in `.cargo/config.toml`
//!
//! Am 2026-08-10 gemessen: Cargo liest `.cargo/config.toml` aus **jedem Vorfahrenverzeichnis**
//! und **hängt Array-Werte aneinander**, statt sie zu ersetzen. Ein Arbeitsbaum unterhalb des
//! Hauptbaums (`<repo>/.claude/worktrees/<id>`) erbt dessen Konfiguration deshalb ein zweites
//! Mal, und `-Tkernel/x86_64-link.ld` steht **zweimal** auf der Linkerzeile. `lld` wertet den
//! `SECTIONS`-Block dann zweimal aus; der zweite Durchlauf fängt wieder bei `. = 1M` an und sieht
//! lauter leere Sektionen. Ergebnis: **alle** Linkersymbole tragen den Wert des zweiten
//! Durchlaufs (`__text_start = 0x100000`), dazu sieben leere Doppelsektionen und ein nullgrosses
//! LOAD-Segment. Der Bau läuft durch. Das Abbild bootet nie.
//!
//! **Die Behebung ist nicht, die Flags nachträglich zu entdoppeln.** Ein Entdoppler kämpft gegen
//! einen Mechanismus, den er nicht kontrolliert (die Mischregeln stehen in Cargo, nicht hier),
//! und er müsste raten, welche Wiederholung Absicht ist — ein legitim doppelt gesetztes Flag
//! wegzuwerfen wäre eine **stille Semantikänderung**. Also wird das Skript dort gesetzt, wo es
//! **je Crate genau einmal** entsteht: hier. `cargo:rustc-link-arg` durchläuft keine
//! Vorfahren-Mischung.
//!
//! Nebenertrag, der vorher ein latenter Fehler war: der Pfad ist jetzt **absolut**. In
//! `.cargo/config.toml` stand er relativ (`-Tkernel/x86_64-link.ld`) und setzte damit voraus,
//! dass `cargo` aus dem Wurzelverzeichnis des Arbeitsbaums läuft — eine Bedingung, die nirgends
//! geprüft wurde.
//!
//! # 2. Der Wächter: doppeltes Linkerskript ist ein ABBRUCH
//!
//! Nach 1. sollte in `rustflags` überhaupt kein `-T` mehr stehen. Der Wächter prüft es trotzdem,
//! denn die Klasse ist grösser als dieser eine Fall: **wer immer** ein Linkerskript wieder in
//! eine Konfiguration schreibt, bekommt in einem verschachtelten Baum dasselbe Bild — und dieses
//! Bild ist von aussen nicht als Konfigurationsfehler zu erkennen. Ein zweites `-T` bricht den
//! Bau ab, mit dem Mechanismus in der Meldung.
//!
//! Andere doppelte Flags brechen **nicht** ab, sondern **melden sich**: `-C relocation-model`
//! zweimal ist folgenlos, aber es ist das Anzeichen dafür, dass die Mischung aktiv ist. Ein
//! Abbruch dort wäre ein Bauverbot für jeden verschachtelten Arbeitsbaum, ohne dass etwas kaputt
//! ist.
//!
//! # 3. Die Konfiguration wird an das Artefakt GEBUNDEN
//!
//! „`cargo build` läuft durch" war im Arbeitsbaum kein Beleg: es übersetzte sauber und lieferte
//! ein Abbild, das nie gebootet hätte. Der Binärfingerabdruck der Suiten bindet das **Artefakt**;
//! niemand band die **Konfiguration, die es erzeugt hat**. Deshalb steht der Fingerabdruck der
//! effektiven Flags ab jetzt als `CAPROCK_FLAGS_FP` im Kernel und wird im Bericht gedruckt.
//!
//! # 4. Linkerskripte sind Eingaben
//!
//! Ohne `rerun-if-changed` löst eine Änderung an einem `.ld` **kein Neu-Linken** aus; man misst
//! den vorigen Stand und hält ihn für das Ergebnis der Änderung. Am 2026-08-10 zweimal passiert.

use std::env;

fn main() {
    // ---- 4. Eingaben ---------------------------------------------------------------------
    println!("cargo:rerun-if-changed=x86_64-link.ld");
    println!("cargo:rerun-if-changed=linker.ld");
    println!("cargo:rerun-if-env-changed=CARGO_ENCODED_RUSTFLAGS");

    let manifest = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let target = env::var("TARGET").unwrap_or_default();

    // ---- 1. Das Skript, genau einmal, mit absolutem Pfad ----------------------------------
    let skript = if target.starts_with("x86_64") {
        "x86_64-link.ld"
    } else {
        "linker.ld"
    };
    println!("cargo:rustc-link-arg=-T{manifest}/{skript}");

    // ---- 2. Der Waechter ------------------------------------------------------------------
    // `CARGO_ENCODED_RUSTFLAGS` enthaelt die EFFEKTIVEN Flags (0x1f-getrennt), also das
    // Ergebnis der Mischung ueber alle Vorfahren-Konfigurationen. Genau die Groesse, um die es
    // geht -- nicht das, was in EINER Datei steht.
    let flags: Vec<String> = env::var("CARGO_ENCODED_RUSTFLAGS")
        .unwrap_or_default()
        .split('\u{1f}')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();

    let skripte: Vec<&String> = flags.iter().filter(|f| f.contains("-T")).collect();
    if skripte.len() > 1 {
        panic!(
            "BAUUMGEBUNG KAPUTT: {} Linkerskript-Flags in RUSTFLAGS ({:?}).\n\
             Cargo mischt .cargo/config.toml aus JEDEM Vorfahrenverzeichnis und HAENGT Arrays\n\
             aneinander. Liegt dieser Arbeitsbaum innerhalb eines anderen Checkouts, wird das\n\
             Skript zweimal uebergeben, lld wertet SECTIONS zweimal aus, und ALLE Linkersymbole\n\
             tragen danach den Wert des zweiten Durchlaufs -- der Bau laeuft durch, das Abbild\n\
             bootet nie. Abhilfe: das Skript gehoert in build.rs (rustc-link-arg), nicht in eine\n\
             Konfiguration; oder der Arbeitsbaum gehoert AUSSERHALB des Hauptbaums.",
            skripte.len(),
            skripte
        );
    }

    // Doppelte Flags im Uebrigen: melden, nicht abbrechen. Sie sind folgenlos -- aber sie sind
    // das Anzeichen, dass die Mischung aktiv ist, und beim naechsten Flag mit Zaehnen waere sie
    // es nicht mehr.
    let mut gesehen: Vec<&String> = Vec::new();
    let mut doppelt: Vec<&String> = Vec::new();
    for f in &flags {
        if gesehen.contains(&f) {
            if !doppelt.contains(&f) {
                doppelt.push(f);
            }
        } else {
            gesehen.push(f);
        }
    }
    if !doppelt.is_empty() {
        println!(
            "cargo:warning=RUSTFLAGS enthalten doppelte Eintraege ({doppelt:?}) -- \
             die Vorfahren-Mischung von .cargo/config.toml ist aktiv. Folgenlos fuer diese \
             Flags; ein Linkerskript an dieser Stelle waere es nicht (s. kernel/build.rs)."
        );
    }

    // ---- 3. Konfiguration an das Artefakt binden ------------------------------------------
    // FNV-1a ueber die effektiven Flags UND das, was wir selbst dazugelegt haben. Kein
    // kryptografischer Anspruch: die Frage ist „dieselbe Konfiguration wie im gemeldeten Lauf?",
    // nicht „hat jemand sie boesartig nachgebaut?".
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in flags
        .join("\u{1f}")
        .bytes()
        .chain(format!("|-T{manifest}/{skript}").bytes())
    {
        h ^= byte as u64;
        h = h.wrapping_mul(0x1000_0000_01b3);
    }
    println!("cargo:rustc-env=CAPROCK_FLAGS_FP={h:016x}");
    println!("cargo:rustc-env=CAPROCK_FLAGS_N={}", flags.len());
}
