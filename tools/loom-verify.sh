#!/usr/bin/env bash
# Caprock — Concurrency-Verifikation: Loom (exhaustive Interleaving-Exploration).
#
# Verifiziert die Synchronisationsprimitive aus caprock-sync (writer-bevorzugender RwSpinLock +
# Ticket-SpinLock) ueber ALLE Thread-Interleavings: gegenseitiger Ausschluss, kein Lost-Update,
# kein torn read, korrekter fetch_and(!WRITER)-Release, Writer-Vorrang.
#
# WAS SICH MIT B-7.2 GEAENDERT HAT — und das ist der ganze Punkt:
#
#   Frueher lag unter Verification/concurrency/loom/src/{lib,ticket}.rs eine von Hand gepflegte
#   "GETREUE Kopie" der Lock-Logik mit loom-Atomics. Ein Beweis ueber eine Kopie beweist etwas
#   ueber die Kopie: nichts hielt die beiden Fassungen zusammen, und eine geaenderte
#   Speicherordnung im echten Lock haette den Beweis nicht einmal gestreift.
#
#   Jetzt ist die gepruefte Datei crates/caprock-sync/src/lib.rs SELBST. Der einzige Unterschied
#   zum Kernel-Build ist `--cfg loom`, das in der Datei die Atomics von core::sync::atomic auf
#   loom::sync::atomic umstellt und die Warteschleife an den Loom-Scheduler abgeben laesst. Die
#   Beweise stehen als `#[cfg(all(loom, test))] mod loom_proofs` IN dieser Datei -- wie die
#   Kani-Beweise auch. Faellt hier etwas um, ist der Code umgefallen, nicht ein Modell davon.
#
# WAS LOOM WEITERHIN NICHT ABDECKT (steht auch im Modulkopf von caprock-sync):
#   * Die IRQ-Maskierung. Loom modelliert Threads, keine Unterbrechungen -- ein Interrupt mitten
#     im kritischen Abschnitt DESSELBEN Kerns ist kein Thread-Interleaving und prinzipiell nicht
#     darstellbar. Unter Loom laeuft das Host-Ziel, dort greifen die No-Op-Stubs
#     (IRQ_MASKING_IMPLEMENTED == false). Der reentrante Ticket-Deadlock wird von der
#     Uebersetzungszeit-Zusicherung (`target_os = "none"` => Maskierung Pflicht) gehalten,
#     NICHT von Loom.
#   * Die Datenzelle bleibt core::cell::UnsafeCell (loom::cell::UnsafeCell gibt Zeiger nur ueber
#     einen Scope-Guard heraus und ist mit Deref/DerefMut nicht vereinbar). Geprueft wird das
#     Synchronisationsprotokoll; dass der Ausschluss traegt, zeigen die Beweise ueber
#     beobachtbare Werte (Lost-Update, torn read).
#   * Looms Speichermodell ist C11, nicht aarch64/x86.
#
# WARUM ein eigenes Bauverzeichnis in $TMPDIR: der Workspace .cargo/config erzwingt Custom-Target +
# build-std (bare-metal Kernel). Loom braucht Host-std + crates.io (loom-Crate) -> wir bauen das
# Artefakt in $TMPDIR (ohne .cargo/config) und lassen loom dort laufen.
#
# Voraussetzung: cargo + Netzzugang fuer den einmaligen loom-Fetch.
# Aufruf:  tools/loom-verify.sh              (alle Modelle)
#          tools/loom-verify.sh loom_proofs  (nur die Lock-Beweise am echten Code)
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
export PATH="$HOME/.cargo/bin:$PATH"
TMP="${TMPDIR:-/tmp}"
SA="$TMP/caprock_loom"; rm -rf "$SA"; mkdir -p "$SA/src"

cp "$ROOT/Verification/concurrency/loom/Cargo.toml" "$SA/Cargo.toml"
# `loom` ist ein externes cfg -> als erwartet deklarieren, sonst rauscht `unexpected_cfgs` ueber
# jede Zeile. Dieselbe Deklaration steht in crates/caprock-sync/Cargo.toml fuer den Workspace.
printf "\n[lints.rust]\nunexpected_cfgs = { level = \"warn\", check-cfg = ['cfg(kani)', 'cfg(loom)'] }\n" >> "$SA/Cargo.toml"
# C9: das Feature der Sperrhaltedauer-Marke MUSS hier deklariert sein, auch wenn es AUS bleibt --
# sonst meldet `unexpected_cfgs` 44 Warnungen (eine je `#[cfg(feature = "sperrwacht")]`) und
# ertraenkt die Ausgabe, in der man die Beweise lesen will. Deklariert, nicht gesetzt: die Marke
# benutzt `core`-Atomics, die Loom NICHT verfolgt -- sie gehoert nicht in dieses Modell, und
# eingeschaltet waere sie ein Stueck unbeobachteter Zustand mitten im geprueften Lock.
printf "\n[features]\nsperrwacht = []\n" >> "$SA/Cargo.toml"

# ---- Der ECHTE Quelltext, unveraendert uebernommen --------------------------------------------
SRC="$ROOT/crates/caprock-sync/src/lib.rs"
cp "$SRC" "$SA/src/lib.rs"
echo "== Quelle: crates/caprock-sync/src/lib.rs ($(wc -l < "$SRC") Zeilen, unveraendert) =="

# ---- Die uebrigen Modelle ---------------------------------------------------------------------
# hierarchy/crosscore modellieren ANDERE Gegenstaende (globale Sperrordnung, Cross-Core-IPC) und
# sind keine Nachbildungen von caprock-sync -- sie bleiben eigenstaendig.
# ticket.rs entfaellt: der Ticket-Lock wird jetzt am echten Code bewiesen (loom_proofs).
for m in hierarchy crosscore; do
  cp "$ROOT/Verification/concurrency/loom/src/$m.rs" "$SA/src/"
  printf '\n#[cfg(test)]\n#[allow(dead_code)]\nmod %s;\n' "$m" >> "$SA/src/lib.rs"
done

echo "== Loom: caprock-sync RwSpinLock + Ticket-SpinLock am ECHTEN Code (alle Interleavings) =="
( cd "$SA" && RUSTFLAGS="--cfg loom" cargo test --release "$@" )
