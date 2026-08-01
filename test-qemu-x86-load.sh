#!/usr/bin/env bash
# **Strang A** — Startmenge, Manifest, Root-Task auf x86_64 (todo-A-ausfuehren.md, A-1/A-2).
#
# Getrennt von `test-qemu-x86.sh`, und das ist Absicht: jene Datei gehoert Strang B
# (Dateibesitz-Regel in todo-A-ausfuehren.md), und beide Straenge arbeiten parallel. Sobald die
# Straenge zusammenlaufen, gehoeren die Pruefungen hier in die Hauptsuite -- bis dahin waere ein
# gemeinsamer Schreibzugriff auf dieselbe Datei die teuerste Art, Zeit zu verlieren.
#
# Was hier geprueft wird und in `test-qemu-x86.sh` NICHT vorkommt:
#   * Multiboot-Module: der Bootloader liefert die Startmenge, der Allokator laesst sie in Ruhe.
#   * System-Manifest: signiert, an DIESES Kernel-Image gebunden, Anti-Downgrade, Live-Oracle.
#   * Root-Task: aus dem Manifest ausgewaehlt, Hash geprueft, Wurzel-Caps endowt -- und er laedt
#     seinerseits ueber SEINE Loader-Cap ein weiteres Programm.
#   * Negativfaelle: kein Manifest, falscher Hash, fremder Kernel. Ein Gate, das nur bei gueltiger
#     Eingabe geprueft wird, ist kein geprueftes Gate.
set -uo pipefail
cd "$(dirname "$0")"

SECONDS_RUN="${1:-120}"
RAM="${2:-512M}"

if [ -r /dev/kvm ] && [ -w /dev/kvm ]; then
    ACCEL=(-enable-kvm -cpu host,+invtsc)
    echo "== Beschleunigung: KVM (-cpu host) =="
else
    ACCEL=(-cpu Skylake-Client)
    echo "== Beschleunigung: TCG (kein /dev/kvm) =="
fi

KELF=build/target/x86_64-unknown-none/release/sel4lake-kernel
PROG=programs/build/target/x86_64-sel4lake-user/release
MANKEY=keys/manifest-test.manifest.ed25519

python3 -c "import cryptography" 2>/dev/null || {
    echo "== FEHLT: das Python-Paket 'cryptography' (signieren geht ohne nicht) =="
    echo "   pip3 install --user cryptography   # oder --break-system-packages auf Debian"
    exit 2
}

# **Schluessel aus einem frischen Clone.** `keys/` ist gitignored -- ohne diesen Schritt waere die
# Suite aus einem Clone nicht lauffaehig, und genau diese Fehlerform hat das Projekt schon
# mehrfach bezahlt (docs/plan-betriebsbereit.md, Stufe 0, Punkt 2). Vorhandene Schluessel bleiben
# unangetastet; fehlt das Paar, entsteht es und die in den Kernel kompilierte Key-DB wird neu
# geschrieben (danach MUSS neu gebaut werden -- deshalb steht das hier vor dem Build).
echo "== Manifest-Schluessel =="
python3 tools/gen_manifest_key.py --ensure || exit 2

echo "== build (Kernel, --features selftest) =="
# `--features selftest` ausdruecklich (nicht ueber `default`): seit A-2.2 ist `selftest` NICHT mehr
# in `default`. Diese Suite prueft den Root-Task-Pfad aus Ring 3 (zweites Badge, SYS_CDELETE) und
# das saubere `system_off` -- alles Aussagen, die der Testharness (`system::testsupport`) meldet.
# Ohne das Feature laedt der Kernel den Root-Task und geht dann in `idle()`: kein Bericht, kein
# `system_off`, Timeout. Das gebootete Image einer Test-Suite braucht den Harness -- dieselbe Zeile,
# die B fuer test-qemu-x86.sh eingezogen hat.
./build-x86.sh --features selftest >/dev/null 2>&1 || { echo "BUILD FAILED (Kernel)"; exit 1; }

echo "== build (Programme, x86_64-sel4lake-user) =="
( cd programs && rustup run nightly cargo build --release --target x86_64-sel4lake-user.json ) \
    >/dev/null 2>&1 || { echo "BUILD FAILED (Programme)"; exit 1; }

echo "== zertifizieren (TrustedSAS: init) =="
mkdir -p certs build
# Der Root-Task ist TrustedSAS (er darf eine Loader-Cap halten) -> ohne gueltiges, auf genau
# dieses Binary gebundenes Zertifikat laedt der Kernel ihn nicht (ADR 0014). Das Zertifikat kommt
# aus DEMSELBEN Werkzeug wie auf ARM; ohne den privaten TrustedSAS-Schluessel entsteht keines.
TRUSTKEY=keys/trusted-test.ed25519
if [ ! -f "$TRUSTKEY" ]; then
    python3 tools/gen_trusted_key.py --name trusted-test >/dev/null 2>&1 || {
        echo "  FEHLER: TrustedSAS-Schluessel liess sich nicht erzeugen"; exit 2; }
    echo "  (Schluessel neu erzeugt -> kernel/src/trusted_keys.rs regeneriert, Kernel wird neu gebaut)"
    ./build-x86.sh --features selftest >/dev/null 2>&1 || { echo "BUILD FAILED (nach Key-Regen)"; exit 1; }
fi
python3 tools/sign_trusted.py --crate programs/trusted/init --elf "$PROG/init.elf" \
    --program-id 1 --version 1 --policy internal-test --key "$TRUSTKEY" \
    --out certs/init-x86.cert >/dev/null 2>&1 || { echo "  FEHLER: init liess sich nicht zertifizieren"; exit 2; }

# --- Das Boot-Image: Kernel + EINE Datei ---------------------------------------------------------
#
# Eintrag 1 = init (TrustedSAS, Root-Task, Loader- + Notification-Cap)
# Eintrag 2 = hello (UserLand) -- das, was init von sich aus nachlaedt.
build_archive() {   # $1 = Ausgabedatei, $2 = Kernel-ELF fuer die Bindung, $3 = manifest-version
    python3 tools/sign_manifest.py --kernel "$2" --key "$MANKEY" --manifest-version "$3" \
        --out build/system.manifest \
        --entry "1:init:0:1:$PROG/init.elf:loader,ntfn:root:3::any:0" \
        --entry "2:hello:2:1:$PROG/hello.elf:ntfn::1::any:0" >/dev/null 2>&1 || return 1
    python3 tools/mkarchive.py "$1" --system-manifest build/system.manifest \
        "1:init:0:1:$PROG/init.elf::certs/init-x86.cert" \
        "2:hello:2:1:$PROG/hello.elf" >/dev/null 2>&1
}

echo "== Boot-Archiv bauen =="
build_archive build/boot-archive-x86.bin "$KELF" 1 || { echo "  FEHLER: Archiv/Manifest"; exit 2; }

# $1 = Archiv (leer = keines), $2 = Logdatei, $3 = Zeitlimit (Vorgabe: $SECONDS_RUN)
#
# Die Negativfaelle bekommen bewusst ein KURZES Limit: dort erreicht der Kernel `SELFTEST COMPLETE`
# nie (es gibt keinen Root-Task), er laeuft in den Watchdog. Die Zeilen, um die es geht, stehen
# aber im ersten Boot-Abschnitt. Das volle Limit abzuwarten hiesse, dreimal auf einen Watchdog zu
# warten, dessen Ausgang schon feststeht.
boot() {
    local extra=()
    [ -n "$1" ] && extra=(-initrd "$1")
    timeout "${3:-$SECONDS_RUN}" qemu-system-x86_64 \
        -kernel "$KELF.mb32" -m "$RAM" -smp 4 "${ACCEL[@]}" \
        -machine q35,kernel-irqchip=split -device intel-iommu,caching-mode=on \
        -device virtio-rng-pci "${extra[@]}" \
        -nographic -serial file:"$2" -no-reboot \
        </dev/null >/dev/null 2>&1 || true
}

LOG="$(mktemp)"
echo "== boot ($SECONDS_RUN s) =="
boot build/boot-archive-x86.bin "$LOG"
OUT="$(grep -vE "SeaBIOS|iPXE|Press Ctrl|Booting from|C900|PMM|PnP" "$LOG" 2>/dev/null)"
if [ -z "$OUT" ]; then
    echo "== KEIN OUTPUT -- das ist KEIN Testergebnis, sondern ein Aufbauproblem (QEMU? Zeitlimit?) =="
    rm -f "$LOG"; exit 2
fi
echo "$OUT" | grep -E "^(mbi|mbmod|archive|manifest|root) " || true

echo "== checks =="
fail=0
check() { if echo "$OUT" | grep -q "$1"; then echo "  PASS: $2"; else echo "  FAIL: $2"; fail=1; fi; }
check "mbi     : 1 Modul(e)" \
    "A-1.1: der Bootloader liefert die Startmenge als Multiboot-Modul (Flag Bit 3 ausgewertet)"
check "mbmod   : ALL PASS" \
    "A-1.1: Modulbereiche werden VOR der ersten Allokation aus der Freiliste ausgeschnitten (Rand/Ueberlappung/unsortiert/Vollabdeckung eingespeist)"
check "archive : 2 Modul(e)" \
    "A-1.1/A-1.5: das Archiv liegt an der vom Bootloader gemeldeten Adresse und parst"
check "manifest: ALL PASS" \
    "A-1.2..A-1.4: System-Manifest -- signiert ueber die GESAMTE Nachricht, an DIESES Kernel-Image gebunden, Anti-Downgrade, manipulierte Kopie wird abgewiesen"
check "manifest:   \[1\]init .* ROOT" \
    "A-1.4: Politikfelder (Domaene, Schnittstellenversion, Anfangs-Caps, Prioritaet, NUMA, Affinitaet, Budget) werden gelesen und ausgewiesen"
# ACHTUNG: es gibt MEHRERE `root    :`-Zeilen (eine ueber das Laden, eine ueber das Ergebnis).
# Gegen "root    : ALL PASS" zu pruefen traf die erste -- der Lauf war gruen, waehrend die
# eigentliche Aussage darunter FAILURES meldete. Genau dieser Fehler steht in AGENTS.md.
# Deshalb hier auf den EINDEUTIGEN Text der Ergebniszeile pruefen.
check "root    : ALL PASS (A-2.1" \
    "A-2.1: Root-Task laeuft wirklich (Badge) -- und laedt ueber SEINE Loader-Cap ein weiteres Programm nach"
check "root    : ALL PASS (Startprogramm" \
    "A-2.1: Root-Task aus dem Manifest geladen (Hash geprueft, Wurzel-Caps endowt)"
check "cdelete : ALL PASS" \
    "A-3.1: SYS_CDELETE aus Ring 3 -- beide Ausgaenge belegt (geloescht + Autoritaet weg; mit Ableitungen abgewiesen und weiter benutzbar)"
check "SELFTEST COMPLETE" "sauberes system_off statt Timeout"

# --- Negativfaelle ------------------------------------------------------------------------------
#
# Ein Gate, das nur mit gueltiger Eingabe geprueft wird, ist kein geprueftes Gate. Die drei Faelle
# unten sind genau die, die im Regelbetrieb NIE vorkommen -- und deshalb die, die verrotten.

echo "== Negativfall 1: gar kein Archiv =="
boot "" "$LOG" 25
if grep -q "root    : FAILURES (NoArchive)" "$LOG"; then
    echo "  PASS: ohne Startmenge sagt der Kernel das -- statt still zu idlen"
else
    echo "  FAIL: ohne Startmenge fehlt die Diagnose"; fail=1
fi

echo "== Negativfall 2: Manifest fuer einen ANDEREN Kernel =="
# An ein fremdes Image binden (das mb32 taugt als Stellvertreter: andere Bytes, gueltiges ELF).
# Erwartung: Signatur ist gueltig, Bindung nicht -> abgewiesen.
cp "$KELF" build/fremder-kernel.elf
python3 - "$KELF" build/fremder-kernel.elf <<'EOF' >/dev/null 2>&1
import sys
# Ein Byte im .text-Bereich kippen -> anderer Kernel-Code-Hash, sonst identisches ELF.
d = bytearray(open(sys.argv[1], 'rb').read())
d[0x2000] ^= 0xFF
open(sys.argv[2], 'wb').write(d)
EOF
if build_archive build/boot-archive-fremd.bin build/fremder-kernel.elf 1; then
    boot build/boot-archive-fremd.bin "$LOG" 25
    if grep -q "manifest: FAILURES" "$LOG" && grep -q "root    : FAILURES (NoManifest)" "$LOG"; then
        echo "  PASS: A-1.3 -- ein gueltig signiertes Manifest fuer ein ANDERES Kernel-Image wird abgewiesen"
    else
        echo "  FAIL: A-1.3 -- fremd gebundenes Manifest wurde nicht abgewiesen"; fail=1
    fi
else
    echo "  FAIL: Negativfall 2 liess sich nicht bauen"; fail=1
fi

echo "== Negativfall 3: Modul passt nicht zum Hash im Manifest =="
# Das Manifest gegen `hello` ausstellen, ins Archiv aber ein anderes Binary legen.
python3 tools/sign_manifest.py --kernel "$KELF" --key "$MANKEY" --manifest-version 1 \
    --out build/system.manifest \
    --entry "1:init:0:1:$PROG/hello.elf:loader,ntfn:root:3::any:0" >/dev/null 2>&1
python3 tools/mkarchive.py build/boot-archive-hash.bin --system-manifest build/system.manifest \
    "1:init:0:1:$PROG/init.elf::certs/init-x86.cert" >/dev/null 2>&1
boot build/boot-archive-hash.bin "$LOG" 25
if grep -q "root    : FAILURES (HashMismatch)" "$LOG"; then
    echo "  PASS: A-1.2 -- der erwartete SHA-256 aus dem Manifest wird durchgesetzt, nicht bloss mitgefuehrt"
else
    echo "  FAIL: A-1.2 -- abweichender Modul-Hash wurde nicht bemerkt"; fail=1
fi

rm -f "$LOG"
if [ "$fail" = 0 ]; then echo "== ALL PASS =="; else echo "== FAILURES =="; fi
exit "$fail"
