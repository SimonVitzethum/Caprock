//! **Cargo kennt Linkerskripte nicht als Eingabe — hier wird es ihm gesagt.**
//!
//! Ohne diese Datei löst eine Änderung an `x86_64-link.ld` oder `linker.ld` **kein Neu-Linken**
//! aus. Der Bau läuft durch, meldet nichts, und man misst den **vorigen** Stand — und hält ihn
//! für das Ergebnis der Änderung.
//!
//! Das ist keine theoretische Sorge. Am 2026-08-10 hat dieses Loch zweimal zugeschlagen:
//!
//! * bei `programs/user*.ld` (die Segmentausrichtung von `wasmhost` schien wirkungslos, bis ein
//!   `touch` erzwungen wurde),
//! * und in einer Reihe von drei Behebungsversuchen am Boot-Abbild, deren Ergebnisse damit
//!   **rückwirkend unter Vorbehalt stehen**: jeder Versuch, bei dem das `touch` fehlte, hat den
//!   Vorgängerstand gemessen. Genau diese Ungewissheit macht eine Versuchstabelle wertlos, deren
//!   Spalte „bootet" nicht misst, was sie behauptet.
//!
//! Dieselbe Regel wie beim Binary-Fingerprint der Suiten, nur eine Ebene früher: **eine Messung
//! muss wissen, welches Artefakt sie gemessen hat.** Der Fingerprint schliesst „veralteter Build"
//! als Erklärung für einen Suitenlauf aus; diese Datei schliesst ihn für den Linkerschritt aus.

fn main() {
    // Beide Skripte, unabhängig von der Zielarchitektur des aktuellen Baus: welches gilt,
    // entscheidet `.cargo/config.toml`, und ein Skript, das heute nicht benutzt wird, ist morgen
    // das benutzte. Eine Liste, die von der Bau-Konfiguration abhängt, wäre der nächste stille
    // Sonderfall.
    println!("cargo:rerun-if-changed=x86_64-link.ld");
    println!("cargo:rerun-if-changed=linker.ld");
}
