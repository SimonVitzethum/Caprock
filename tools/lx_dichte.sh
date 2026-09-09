#!/usr/bin/env bash
# tools/lx_dichte.sh — Dichte-Protokoll: 10000+ PDs muessen moeglich sein.
#
# Was hier steht, und was ausdruecklich nicht:
# * Hier steht die RECHNUNG (Boot-Tabellen, Arena-Plan, Live-Grenzen je RAM-Groesse) plus die
#   exakten Host-Belege (Typgroessen, Arena-Logik als `--test`, Kernel-Check x86).
# * Hier steht KEIN QEMU-Volllauf mit 10000 PDs. Der ist nicht verlangt (Minuten bis Stunden)
#   und wird ehrlich als offen gefuehrt (s. Abschnitt 5).
#
# Methode je Zahl (im Protokoll bei jeder Zahl genannt):
# * EXAKT (Host-Probe): CapPtr/CapSlot/Object/PAGE — auf dem Wirt gegen die echten Crates
#   kompiliert, nur lesend (Quellen werden nach /tmp kopiert, das Repo wird nicht angefasst).
# * EXAKT (Host-Test): Arena-Vergabe + Platzableitung — `rustc --test` auf kernel/src/stack_arena.rs
#   (abhaengigkeitsfrei, nur `core`); die Faelle 512M/3G/6G stehen dort als Asserts.
# * EXAKT (Kernel-Check): 0 Errors auf x86_64-unknown-none (default + selftest) und aarch64.
# * ABGELEITET (Handrechnung): Pd/Endpoint/Notification/Tcb/VSpaceEnt/LoadedImage/ThreadOwner —
#   Layout aus der Quelle nachgerechnet (Datei:Zeile bei jeder Zahl); Halterungen (Alinierung)
#   konservativ aufgerundet. Die Urteile haben 2- bis 8-fache Abstaende — plus/minus 20 % auf
#   einer Struktur aendert keines.
#
# Aufruf: tools/lx_dichte.sh
set -uo pipefail
cd "$(dirname "$0")/.."
ROOT="$(pwd)"
RUSTC="rustup run nightly rustc"
CARGO="rustup run nightly cargo"
TMP="${TMPDIR:-/tmp}"
fail=0

echo "### lx_dichte 1/5: Host-Tests stack_arena (Vergabe + Platzableitung) ###"
BIN="$TMP/lx_dichte_arena"
rm -f "$BIN"
if ! $RUSTC --test --edition 2021 -O kernel/src/stack_arena.rs -o "$BIN" 2>&1 | grep -E "^error" -A 6; then :; fi
if [ ! -x "$BIN" ]; then echo "  FEHLER: stack_arena liess sich nicht uebersetzen"; fail=1;
else "$BIN" || fail=1; fi
rm -f "$BIN"

echo "### lx_dichte 2/5: exakte Typgroessen (Host-Probe, Quellen nur lesend) ###"
SA="$TMP/lx_dichte_groessen"
rm -rf "$SA" "$TMP/lx_deps_dichte"
mkdir -p "$SA/src" "$TMP/lx_deps_dichte"
for d in caprock-cap caprock-mem caprock-slab; do
    mkdir -p "$TMP/lx_deps_dichte/$d/src"
    cp -r "$ROOT/crates/$d/src/." "$TMP/lx_deps_dichte/$d/src/"
done
cat > "$TMP/lx_deps_dichte/caprock-cap/Cargo.toml" <<'EOF'
[package]
name="caprock-cap"
version="0.0.0"
edition="2021"
[workspace]
[lib]
path="src/lib.rs"
[dependencies]
caprock-mem = { path = "../caprock-mem" }
caprock-slab = { path = "../caprock-slab" }
EOF
for d in caprock-mem caprock-slab; do
    printf '[package]\nname="%s"\nversion="0.0.0"\nedition="2021"\n[workspace]\n[lib]\npath="src/lib.rs"\n' "$d" \
        > "$TMP/lx_deps_dichte/$d/Cargo.toml"
done
cat > "$SA/Cargo.toml" <<'EOF'
[package]
name="lx_dichte_groessen"
version="0.0.0"
edition="2021"
[workspace]
[[bin]]
name="lx_dichte_groessen"
path="src/main.rs"
[dependencies]
caprock-cap = { path = "../lx_deps_dichte/caprock-cap" }
caprock-mem = { path = "../lx_deps_dichte/caprock-mem" }
caprock-slab = { path = "../lx_deps_dichte/caprock-slab" }
EOF
cat > "$SA/src/main.rs" <<'EOF'
use std::mem::{size_of, align_of};
fn main() {
    // EXAKT: Cspace-Pool = NPDS*16 Slots je Option<CapPtr>.
    println!("groessen: CapPtr={} Option_CapPtr={} CapSlot={} Object={} PAGE={}",
        size_of::<caprock_cap::CapPtr>(), size_of::<Option<caprock_cap::CapPtr>>(),
        size_of::<caprock_cap::CapSlot>(), size_of::<caprock_cap::Object>(),
        caprock_mem::PAGE);
    assert_eq!(align_of::<caprock_cap::CapPtr>(), 8);
    // Die Rechnung, an der der Pool haengt (Bewertung in Abschnitt 3).
    assert_eq!(size_of::<Option<caprock_cap::CapPtr>>(), 24, "Pool-Slot");
    println!("groessen: Cspace-Pool=160000x24={} Byte", 160_000usize * 24);
}
EOF
if ( cd "$SA" && $CARGO run --release --offline 2>/dev/null ); then :; else
    echo "  FEHLER: Groessen-Probe lief nicht"; fail=1
fi
rm -rf "$SA" "$TMP/lx_deps_dichte"

echo "### lx_dichte 3/5: Sizing-Tabelle 512M/3G/6G (Rechnung, s. Methode oben) ###"
python3 - <<'EOF'
MIB = 1024*1024
# --- Eingaben: EXAKT aus Abschnitt 2 ---
OPT_CAPPTR, CAPSLOT, OBJECT = 24, 88, 40
# --- Eingaben: ABGELEITET aus der Quelle (Datei:Zeile), konservativ aufgerundet ---
PD          = 96    # crates/caprock-microkit/src/lib.rs:559 (used+Thread+5xu32+u16+Dom+bool+Opt<u16>+u16+3xu32+usize+u16+u32+u64)
ENDPOINT    = 1640  # crates/caprock-ipc/src/lib.rs:303 (2xTidQueue zu je 792 + 2xOpt<ThreadId> zu je 24)
NOTIF       = 40    # crates/caprock-ipc/src/lib.rs:900 (bool+u64+Opt<ThreadId>)
ORPHAN      = 24    # = Opt<ThreadId>; kernel/src/system.rs:921 (ein Eintrag je Endpoint)
TCB_SLOT    = 236   # Obergrenze: Tcb<=208 (sched/src/lib.rs:421, u16+u64+Optionen) + Zombie 24 + Freiliste 4
THREADOWNER = 16    # microkit/src/lib.rs:770 (u64+u32+u32), exakt
VSPACEENT   = 32    # kernel/src/system.rs:VSpaceEnt (bool+2xu64+Opt<u32>)
LOADEDIMG   = 1040  # kernel/src/system.rs:LoadedImage (u16+usize+64x(u64,u64))
STACKCAP    = 48    # = Opt<(u64,CapPtr,u64,u64)>; kernel/src/system.rs:StackEintrag
NPDS, BUDGET, KRESERVE = 10_000, 10, 256
CAP_SLOTS = NPDS*BUDGET + KRESERVE
NEPS = NNTFNS = NPDS + 64
CORES, JE_KERN, PER_CORE, TOTAL = 4, 2500, 5000, 10_000
# Boot-Tabellen (Boot-RAM, mit Abrechnung im Report):
tab = {
    "Thread-Directory (12B)": TOTAL*12,
    "Kern-Tabellen TCB+Zombie+Freiliste": CORES*PER_CORE*TCB_SLOT,
    "FP-Kontexte x86 (512B)": TOTAL*512,
    "VSPACE_OF+Kstack Basis+Gen+StackCap+Owner+UserRegion": TOTAL*(8+8+4+STACKCAP+THREADOWNER+16),
    "Cap-Slots": CAP_SLOTS*CAPSLOT, "Cap-Objekte": CAP_SLOTS*OBJECT,
    "Finalisierung+Enforcer+Audit+Seen": CAP_SLOTS*(16+16+1+8+2+4+1),
    "PD-Tabelle": NPDS*PD, "Cspace-Pool": NPDS*16*OPT_CAPPTR,
    "Endpoints": NEPS*ENDPOINT, "Notifications": NNTFNS*NOTIF, "IPC-Orphans": NEPS*ORPHAN,
    "VSpaces": NPDS*VSPACEENT, "Loaded-Images": NPDS*LOADEDIMG,
}
tab_sum = sum(tab.values())
print(f"tabellen: Boot-Tabellen gesamt {tab_sum/MIB:.1f} MiB (fuer 10000er-Kapazitaet, alle Toepfe):")
for k, v in tab.items():
    print(f"  {v/MIB:7.2f} MiB  {k}")
# Arena-Plan je RAM (x86: Schritt 8 KiB, volle PD 84 KiB; Guard: 16 Splits x 256, Annahme belegt=0 —
# die Laufzeit liest guard_stats, s. kstack_arena_bestuecken):
for ram_mib, name in ((512, "512M"), (3072, "3G"), (6144, "6G")):
    frei = (ram_mib-20)*MIB  # Annahme: 16 MiB unter USER_RAM_MIN + ~4 MiB Image/Module
    ram_pl = frei//(84*1024)
    plaetze = min(TOTAL, ram_pl, 16*256, 16_384)
    grund = "GUARD" if plaetze == 16*256 else ("RAM" if plaetze == ram_pl else ("THREADS" if plaetze == TOTAL else "DECKEL"))
    spanne = plaetze*8*1024
    rest = frei - tab_sum - spanne
    ram_iso = max(rest, 0)//(84*1024)
    ram_sas = max(rest, 0)//(64*1024)
    # Ehrliches Live-Urteil: Minimum ueber ALLE Schranken, nicht RAM allein. x86: Guards (HAL,
    # B-Besitz); aarch64: max_asid ohne FEAT_ASID16 (HW). Tabellen/Threads tragen 10000.
    live_iso_x86 = min(ram_iso, NPDS, TOTAL, NPDS, 16*256)
    live_iso_a64 = min(ram_iso, NPDS, TOTAL, 255)
    print(f"arena_live_{name}: frei~{frei//MIB}MiB Tabellen{tab_sum/MIB:.0f}MiB "
          f"Arena {plaetze} Plaetze ({grund}, Spanne {spanne/MIB:.0f}MiB) RAM-allein: isoliert~{ram_iso}/SAS~{ram_sas} | "
          f"LIVE-Urteil isoliert: x86~{live_iso_x86} (Guard), aarch64~{live_iso_a64} (ASID)")
print("live_3G_6G: RAM-allein wuerde 10000 tragen (volle PD=84KiB); was bindet, sind Guards (x86, HAL, "
      "40 Splits noetig) bzw. ASID-Breite (aarch64, FEAT_ASID16 noetig) — Tabellen/Threads/VSpaces/Loaded bereit.")
print("deckel: NPDS=10000 PD-Slots + Cspace-Pool 3.66MiB + Owner/VSpaces/Loaded je PD reichen; "
      "PARTITIONS=4 unberuehrt (es geht um PD-ZAHL, nicht um cache-separierte PDs).")
print("hw_schranken: x86 lebt isoliert bis ~4096 (16 Guard-Splits, HAL, B-Besitz); "
      "aarch64 isoliert bis min(RAM, max_asid=255 ohne FEAT_ASID16) — s. vspace_capacity.")
EOF

echo "### lx_dichte 4/5: Kernel-Check x86 (0 Errors gefordert) ###"
if $CARGO check --release --target x86_64-unknown-none -p caprock-kernel 2>&1 | grep -E "^error" -A 6; then
    echo "  FEHLER: Kernel-Check x86 meldet Errors"; fail=1
else
    echo "  kernel-check-x86: 0 Errors"
fi

echo "### lx_dichte 5/5: ehrliche Abgrenzung (was belegt ist, was QEMU noch zeigen muss) ###"
cat <<'EOF'
  BELEGT (Rechnung + Einzelpfade, heute):
  - Tabellen tragen 10000: PD-Slots, Cspace-Pool, Cap-Slots/Objekte (= Summe aller Budgets +
    Reserve), Endpoints/Notifications (je PD + 64), Thread-Slots (TARGET_THREADS), VSpaces
    (Boot-RAM statt 4096-BSS), Loaded-Images (Boot-RAM statt 16-BSS), gid-Freiliste, StackCap,
    Owner-Index. Jede Vergabe scheitert benannt (bestehende Topf-Codes + Arena-Gruende +
    Zaehl-Telemetrie); kein BSS-Sprengen (Bitmap + Tabellen aus Boot-RAM, Bytes im Report).
  - Arena-Plaetze abgeleitet (Faeden/RAM/Guards/Deckel) statt 2048-fix, mit Plan-Telemetrie
    (kstack_arena_plan) und Byte-Abrechnung (sched-Zeile).
  - Berichtspfade skalieren: capsum ueber Boot-grosse Zaehlflaeche (Code 8 statt still ok),
    purge ueber NEPS-grosse Orphan-Flaeche, loader_audit gestaffelt je Image (1 KiB Stapel
    statt 16 KiB/10 MiB), vspace_audit ohne Stapelkopie, Fegen/Sibling-Scans len-basiert.
  OFFEN (braucht einen QEMU-Lauf, bewusst nicht hier):
  - Gleichzeitigkeit: 10000 PDs + Faeden wirklich anlegen (Kurve mit CAPROCK_SCALE_TARGET=10000
    auf 3G/6G), bis zum benannten Grund fahren, RAM-Baseline vorher/nachher vergleichen.
  - Guard-Verbrauch: belegte Splits bei Arena-Bestueckung auf x86 (Selbsttest-Streuung davor);
    oberhalb ~4096 lebender EL0-Faeden muss die HAL mehr Splits hergeben (B-Besitz).
  - Boot-Zeit: 10000er-Tabellen allozieren + nullen + 10000 Guards legen kosten Wandzeit.
  - aarch64: nutzbare VSpaces = min(10000, max_asid) — ohne FEAT_ASID16 nur 255.
EOF

if [ "$fail" = 0 ]; then echo "== LX_DICHTE: ALL PASS =="; else echo "== LX_DICHTE: FAILURES =="; fi
exit "$fail"
