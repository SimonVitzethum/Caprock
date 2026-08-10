//! **Cargo kennt Linkerskripte nicht als Eingabe — hier wird es ihm für die PROGRAMME gesagt.**
//!
//! `user.ld` und `user-x86.ld` gelten für jedes geladene Programm. Sie stehen hier und nicht in
//! jedem Programm-Crate, weil **jedes** Programm `libcaprock` linkt: ändert sich ein Skript, wird
//! diese Crate neu gebaut, und damit linken alle Programme neu. Eine Datei statt sieben, und
//! keine, die beim achten Programm vergessen werden kann.
//!
//! Warum überhaupt: ohne den Hinweis löst eine Änderung an einem `.ld` **kein Neu-Linken** aus.
//! Am 2026-08-10 hat das die Behebung der Segmentausrichtung von `wasmhost` wirkungslos aussehen
//! lassen — die Änderung war richtig, gemessen wurde der Stand davor.
//!
//! Der Pfad ist relativ zu diesem Crate-Verzeichnis; `..` ist `programs/`.

//! # Der Waechter gegen doppelte Linkerskripte -- auch hier, obwohl es hier (heute) nicht greift
//!
//! Am 2026-08-10 gemessen: Cargo mischt `.cargo/config.toml` aus **jedem** Vorfahrenverzeichnis
//! und **haengt Array-Werte aneinander**. Fuer den Kernel war das verheerend (Linkerskript
//! doppelt, `SECTIONS` zweimal ausgewertet, alle Symbole falsch, Bau laeuft durch) -- s.
//! `kernel/build.rs`.
//!
//! **Fuer die Programme trifft es heute nicht zu, und das ist gemessen und nicht vermutet:** in
//! einem Arbeitsbaum unterhalb des Hauptbaums steht `target.x86_64-caprock-user.rustflags` genau
//! einmal da, weil dieser Zielschluessel nur in `programs/.cargo/config.toml` vorkommt und die
//! geerbte Wurzel-Konfiguration andere Schluessel benutzt. Die Lade-Suite war also nie betroffen.
//!
//! Der Waechter steht trotzdem hier: „heute nicht betroffen" haengt an einer Zufaelligkeit der
//! Schluesselnamen, nicht an einer Struktur. Wer morgen einen gemeinsamen Zielschluessel
//! einfuehrt, bekaeme genau das Bild des Kernels -- und das ist von aussen nicht als
//! Konfigurationsfehler zu erkennen.

fn main() {
    println!("cargo:rerun-if-changed=../user.ld");
    println!("cargo:rerun-if-changed=../user-x86.ld");
    println!("cargo:rerun-if-env-changed=CARGO_ENCODED_RUSTFLAGS");

    let flags: Vec<String> = std::env::var("CARGO_ENCODED_RUSTFLAGS")
        .unwrap_or_default()
        .split('\u{1f}')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    let skripte: Vec<&String> = flags.iter().filter(|f| f.contains("-T")).collect();
    if skripte.len() > 1 {
        panic!(
            "BAUUMGEBUNG KAPUTT: {} Linkerskript-Flags in RUSTFLAGS ({:?}).\n\
             Cargo mischt .cargo/config.toml aus JEDEM Vorfahrenverzeichnis und HAENGT Arrays\n\
             aneinander -- liegt dieser Arbeitsbaum in einem anderen Checkout, wird das Skript\n\
             zweimal uebergeben und das Ergebnis ist unbrauchbar, ohne dass der Bau scheitert.\n\
             Begruendung und Messung: kernel/build.rs.",
            skripte.len(),
            skripte
        );
    }
}
