#!/usr/bin/env bash
# tools/lx_dist.sh — Standard-Distributions-ISO (1- oder 2-Modul-GRUB-ISO).
#
# ZWECK: EIN Aufruf baut aus fertigen Artefakten eine bootfaehige GRUB-ISO.
# Kein Build im Skript (Kernel/Archiv/Treiber kommen als Argumente) — das
# Skript verpackt nur und prueft dabei.
#
# FORM-VORBILDER (gelesen, NICHT geaendert — die ~20 Zeilen Baum+grub.cfg+
# grub-mkrescue unten sind eine bewusste Duplikation daraus):
#   tools/mkgrubiso.sh      (1-Modul-ISO: Multiboot-Check, grub.cfg-Form,
#                            grub-mkrescue-Bau)
#   tools/lx_fahrt4_iso.sh  (2-Modul-Muster: Archiv = Modul 0, Treiber = Modul 1,
#                            grub.cfg mit zwei `module`-Zeilen)
# MANIFEST-BAU (nur Referenz, nicht Teil dieses Skripts):
#   test-qemu-x86-load.sh build_archive (sign_manifest.py + mkarchive.py)
#
# AUFBAU der ISO: Kernel (/boot/kernel.mb32) + Modul 0 (/boot/boot-archive.bin,
# Kommandozeile "boot-archive") + optional Modul 1 (/boot/lxpd-driver.img,
# Kommandozeile "lxpd-driver"). Modulreihenfolge = Ladereihenfolge des Kernels.
#
# Aufruf:
#   bash tools/lx_dist.sh --kernel K.mb32 --archive A.bin --out ISO
#   bash tools/lx_dist.sh --kernel K.mb32 --archive A.bin --driver D.img --out ISO
#   bash tools/lx_dist.sh --selbsttest   # prueft das Skript OHNE QEMU
#   bash tools/lx_dist.sh --help
#
# Danach:  qemu-system-x86_64 -cdrom ISO -boot order=d,menu=off -m 512M ...

if [ -z "${BASH_VERSION:-}" ]; then
    echo "FEHLER: dieses Skript braucht bash, nicht sh/dash." >&2
    exit 2
fi
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

KERNEL=""; ARCHIVE=""; DRIVER=""; OUT=""; SELBSTTEST=0

while [ $# -gt 0 ]; do
    case "$1" in
        --kernel)  KERNEL="$2";  shift 2 ;;
        --archive) ARCHIVE="$2"; shift 2 ;;
        --driver)  DRIVER="$2";  shift 2 ;;
        --out)     OUT="$2";     shift 2 ;;
        --selbsttest) SELBSTTEST=1; shift ;;
        -h|--help) sed -n '2,27p' "$0"; exit 0 ;;
        *) echo "unbekannte Option '$1' (siehe --help)" >&2; exit 2 ;;
    esac
done

# --- Selbsttest: baut 1- und 2-Modul-ISO aus vorhandenen Artefakten, OHNE QEMU.
# Prueft: ISO entsteht, Module sind per xorriso-Listing drin, extrahierte
# Bytes haben dieselben sha256-Hashes wie die Eingaben (Abgleich mit Manifest).
if [ "$SELBSTTEST" -eq 1 ]; then
    K_DEF="build/target/x86_64-unknown-none/release/caprock-kernel.mb32"
    A_DEF="build/boot-archive-x86.bin"
    D_DEF=""
    for kandidat in build/diag/lx-minelf.img tests/lxpd-boot-qemu/lxpd-test.lxpd; do
        if [ -f "$kandidat" ]; then D_DEF="$kandidat"; break; fi
    done
    [ -f "$K_DEF" ] || { echo "SELBSTTEST: FEHLER: Kernel '$K_DEF' fehlt" >&2; exit 2; }
    [ -f "$A_DEF" ] || { echo "SELBSTTEST: FEHLER: Archiv '$A_DEF' fehlt" >&2; exit 2; }
    command -v xorriso >/dev/null || { echo "SELBSTTEST: FEHLER: 'xorriso' fehlt" >&2; exit 2; }
    TMP="$(mktemp -d)"
    trap 'rm -rf "$TMP"' EXIT
    ok=1
    pruefe_iso() {  # $1 = ISO, $2 = Archiv-Original, $3 = Treiber-Original (oder leer)
        local iso="$1" a_orig="$2" d_orig="$3" liste
        [ -s "$iso" ] || { echo "  FAIL: '$iso' fehlt oder leer"; ok=0; return; }
        echo "  ISO ok: $iso ($(du -h "$iso" | cut -f1))"
        liste="$(xorriso -indev "$iso" -lsl /boot /boot/grub 2>/dev/null)"
        for erwartet in kernel.mb32 boot-archive.bin; do
            if grep -q "$erwartet" <<<"$liste"; then
                echo "  PASS: Modul drin: $erwartet"
            else
                echo "  FAIL: Modul fehlt im ISO-Listing: $erwartet"; ok=0
            fi
        done
        if [ -n "$d_orig" ]; then
            if grep -q "lxpd-driver.img" <<<"$liste"; then
                echo "  PASS: Modul drin: /boot/lxpd-driver.img"
            else
                echo "  FAIL: Modul fehlt im ISO-Listing: /boot/lxpd-driver.img"; ok=0
            fi
        else
            if grep -q "lxpd-driver.img" <<<"$liste"; then
                echo "  FAIL: Treiber-Modul drin, obwohl keines uebergeben wurde"; ok=0
            else
                echo "  PASS: kein Treiber-Modul (1-Modul-ISO wie erwartet)"
            fi
        fi
        # Extrahieren + Hash-Abgleich (derselbe Vergleich wie Manifest gegen Lader).
        xorriso -indev "$iso" \
            -osirrox on -extract /boot/boot-archive.bin "$TMP/got-archive.bin" \
            >/dev/null 2>&1
        if [ "$(sha256sum <"$TMP/got-archive.bin" | cut -d' ' -f1)" = \
             "$(sha256sum <"$a_orig" | cut -d' ' -f1)" ]; then
            echo "  PASS: Archiv-Hash stimmt: $(sha256sum <"$a_orig" | cut -c1-16)…"
        else
            echo "  FAIL: Archiv-Hash weicht ab"; ok=0
        fi
        if [ -n "$d_orig" ]; then
            xorriso -indev "$iso" \
                -osirrox on -extract /boot/lxpd-driver.img "$TMP/got-driver.img" \
                >/dev/null 2>&1
            if [ "$(sha256sum <"$TMP/got-driver.img" | cut -d' ' -f1)" = \
                 "$(sha256sum <"$d_orig" | cut -d' ' -f1)" ]; then
                echo "  PASS: Treiber-Hash stimmt: $(sha256sum <"$d_orig" | cut -c1-16)…"
            else
                echo "  FAIL: Treiber-Hash weicht ab"; ok=0
            fi
        fi
        if grep -q "module /boot/boot-archive.bin boot-archive" \
                <(xorriso -indev "$iso" -osirrox on \
                    -extract /boot/grub/grub.cfg "$TMP/grub.cfg" >/dev/null 2>&1; cat "$TMP/grub.cfg"); then
            echo "  PASS: grub.cfg enthaelt Modul-0-Zeile"
        else
            echo "  FAIL: grub.cfg ohne Modul-0-Zeile"; ok=0
        fi
    }
    echo "== Selbsttest 1/2: 1-Modul-ISO (Kernel + Archiv) =="
    bash tools/lx_dist.sh --kernel "$K_DEF" --archive "$A_DEF" \
        --out "$TMP/dist1.iso" || { echo "  FAIL: Bau 1-Modul"; exit 1; }
    pruefe_iso "$TMP/dist1.iso" "$A_DEF" ""
    if [ -n "$D_DEF" ]; then
        echo "== Selbsttest 2/2: 2-Modul-ISO (mit Treiber $D_DEF) =="
        bash tools/lx_dist.sh --kernel "$K_DEF" --archive "$A_DEF" \
            --driver "$D_DEF" --out "$TMP/dist2.iso" \
            || { echo "  FAIL: Bau 2-Modul"; exit 1; }
        pruefe_iso "$TMP/dist2.iso" "$A_DEF" "$D_DEF"
    else
        echo "== Selbsttest 2/2: UEBERSPRUNGEN (kein Treiber-Artefakt gefunden) =="
    fi
    if [ "$ok" -eq 1 ]; then echo "SELBSTTEST: ALL PASS"; exit 0
    else echo "SELBSTTEST: FAILURES"; exit 1; fi
fi

# --- Normaler Bau -------------------------------------------------------------
[ -n "$KERNEL" ]  || { echo "FEHLER: --kernel fehlt (siehe --help)" >&2; exit 2; }
[ -n "$ARCHIVE" ] || { echo "FEHLER: --archive fehlt (siehe --help)" >&2; exit 2; }
[ -n "$OUT" ]     || { echo "FEHLER: --out fehlt (siehe --help)" >&2; exit 2; }

for w in grub-mkrescue grub-file xorriso; do
    command -v "$w" >/dev/null || { echo "FEHLER: '$w' fehlt (Paket grub / xorriso)." >&2; exit 1; }
done
[ -f "$KERNEL" ]  || { echo "FEHLER: Kernel '$KERNEL' fehlt (fertiges Artefakt uebergeben, kein Build im Skript)." >&2; exit 1; }
[ -f "$ARCHIVE" ] || { echo "FEHLER: Boot-Archiv '$ARCHIVE' fehlt." >&2; exit 1; }
if [ -n "$DRIVER" ]; then
    [ -f "$DRIVER" ] || { echo "FEHLER: Treiber-Bild '$DRIVER' fehlt." >&2; exit 1; }
fi

# Wie mkgrubiso.sh: der Bootloader laedt nur, was er als Multiboot erkennt.
# Pruefung HIER statt vor schwarzem Bildschirm — dort saehe man nur Stille.
grub-file --is-x86-multiboot "$KERNEL" \
    || { echo "FEHLER: '$KERNEL' ist kein Multiboot1-Image (Header fehlt oder liegt zu spat)." >&2; exit 1; }

# Modul-Hashes VOR dem Verpacken — zum Abgleich mit dem Manifest (der Kernel
# prueft sha256 je Eintrag; weicht ein Modul ab, weist er es ab statt es zu laden).
H_ARCHIVE="$(sha256sum "$ARCHIVE" | cut -d' ' -f1)"
if [ -n "$DRIVER" ]; then H_DRIVER="$(sha256sum "$DRIVER" | cut -d' ' -f1)"; fi

# Duplikat aus mkgrubiso.sh / lx_fahrt4_iso.sh (s. Quellenangabe oben):
# Baum anlegen, Kernel + Module kopieren, grub.cfg schreiben, grub-mkrescue.
# `timeout=0`, ein Eintrag: Testgegenstand, kein Menue.
BAUM="$(mktemp -d)"
trap 'rm -rf "$BAUM"' EXIT
mkdir -p "$BAUM/boot/grub"
cp "$KERNEL" "$BAUM/boot/kernel.mb32"
cp "$ARCHIVE" "$BAUM/boot/boot-archive.bin"
if [ -n "$DRIVER" ]; then cp "$DRIVER" "$BAUM/boot/lxpd-driver.img"; fi

{
    echo "set timeout=0"
    echo "set default=0"
    echo ""
    echo 'menuentry "Caprock-Dist" {'
    echo "    multiboot /boot/kernel.mb32"
    echo "    module /boot/boot-archive.bin boot-archive"
    if [ -n "$DRIVER" ]; then
        echo "    module /boot/lxpd-driver.img lxpd-driver"
    fi
    echo "    boot"
    echo "}"
} > "$BAUM/boot/grub/grub.cfg"

grub-mkrescue -o "$OUT" "$BAUM" >/dev/null 2>&1 \
    || { echo "FEHLER: grub-mkrescue fehlgeschlagen." >&2; exit 1; }
[ -s "$OUT" ] || { echo "FEHLER: '$OUT' ist leer." >&2; exit 1; }

echo "== ISO: $OUT ($(du -h "$OUT" | cut -f1)) =="
echo "   Kernel  : $KERNEL"
echo "   Modul 0 : $ARCHIVE  -> /boot/boot-archive.bin"
echo "             sha256=$H_ARCHIVE"
if [ -n "$DRIVER" ]; then
    echo "   Modul 1 : $DRIVER  -> /boot/lxpd-driver.img"
    echo "             sha256=$H_DRIVER"
else
    echo "   Modul 1 : KEINES (1-Modul-ISO)"
fi
