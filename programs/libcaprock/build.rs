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

fn main() {
    println!("cargo:rerun-if-changed=../user.ld");
    println!("cargo:rerun-if-changed=../user-x86.ld");
}
