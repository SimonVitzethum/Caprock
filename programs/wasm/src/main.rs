//! Minimaler Host-Demo des WASM-2a-Geruestes (kein PD-Bau, nur Anschauung):
//! parst ein eingebettetes Modul (ein Speicher, min 1, kein max), validiert es und
//! zeigt Fuel- plus Negativlisten-Absagen. Fehler sind benannt, nie Panic-Pfade.

#![forbid(unsafe_code)]

use caprock_wasm::{Fuel, WASI_MODUL, grant_laenge, parse_modul, pruefe_import, validiere, wachstum, zugriff_pruefen};

/// Eingebettetes 2a-Modul: `\0asm` v1 + Memory-Sektion (ein Eintrag, min 1, kein max).
const MODUL: [u8; 13] = [
    0x00, 0x61, 0x73, 0x6D, 0x01, 0x00, 0x00, 0x00, // Magie + Version
    0x05, 0x03, 0x01, 0x00, 0x01, // Memory: 1 Eintrag, Merkmale 0, min 1
];

fn main() {
    let info = match parse_modul(&MODUL) {
        Ok(x) => x,
        Err(f) => {
            println!("demo: Parser-Absage: {}", f.name());
            return;
        }
    };
    let seiten = match validiere(&info) {
        Ok(s) => s,
        Err(f) => {
            println!("demo: Validator-Absage: {}", f.name());
            return;
        }
    };
    println!("demo: {seiten} Seite(n), Grant {} B", grant_laenge(seiten).unwrap_or(0));
    println!("demo: Sektionen: {}", info.sektionen);

    let mut fuel = Fuel::neu(caprock_wasm::FUEL_FIX).expect("Fix-Budget startet");
    fuel.verbrauch(7).expect("7 Schritte");
    println!("demo: Fuel-Rest: {}", fuel.rest());

    println!("demo: fd_write: {:?}", pruefe_import(WASI_MODUL, "fd_write").map_err(|f| f.name()));
    println!("demo: path_open: {:?}", pruefe_import(WASI_MODUL, "path_open").map_err(|f| f.name()));
    println!("demo: grow(0): {:?}", wachstum(0, seiten).map_err(|f| f.name()));
    println!("demo: grow(1): {:?}", wachstum(1, seiten).map_err(|f| f.name()));
    println!(
        "demo: oob(65536,1): {:?}",
        zugriff_pruefen(65536, 1, 65536).map_err(|f| f.name())
    );
}
