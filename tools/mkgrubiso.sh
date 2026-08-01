#!/usr/bin/env bash
# SEL4Lake — Boot-Image für einen ECHTEN Bootloader bauen (GRUB, Multiboot1, El-Torito-ISO).
#
# WARUM es das gibt. Bisher wird x86 ausschliesslich über QEMUs `-kernel`/`-initrd` gestartet.
# Das ist bequem, aber es ist kein Bootloader: QEMU hat einen eingebauten Minimal-Multiboot-
# Lader, der Dinge liefert, die er laut Spezifikation nicht liefern müsste. Zwei Folgen:
#
#   1. Der Weg auf echte Hardware existiert nicht. Mit `-kernel` kommt man dort nie hin.
#   2. Was der Kernel über den Bootloader annimmt, ist gegen genau eine Implementierung
#      geprüft. Der Multiboot-Header verlangt heute NICHTS (`Flags = 0`) — weder
#      seitenausgerichtete Module (Bit 0) noch Speicherinformationen (Bit 1) — obwohl der
#      Kernel beides benutzt. QEMU gibt es freiwillig. Ob GRUB dasselbe tut, war bis zu
#      diesem Skript eine Vermutung.
#
# Die Struktur folgt Z11: im Image liegen **genau zwei Dinge** — der Kernel und eine Datei,
# die festlegt, was geladen wird. Alles Übrige ist ein Multiboot-Modul, das der Bootloader
# anliefert; der Kernel prüft gegen das Manifest, dass genau das ankam, was dort steht.
#
# Aufruf:
#   bash tools/mkgrubiso.sh                          # Kernel + Boot-Archiv (Vorgabe)
#   bash tools/mkgrubiso.sh --kernel K --archive A   # abweichende Pfade
#   bash tools/mkgrubiso.sh --no-archive             # nur Kernel (Negativfall: keine Startmenge)
#   bash tools/mkgrubiso.sh --out build/x.iso
#
# Danach:  qemu-system-x86_64 -cdrom build/sel4lake-grub.iso -nographic ...

if [ -z "${BASH_VERSION:-}" ]; then
    echo "FEHLER: dieses Skript braucht bash, nicht sh/dash." >&2
    exit 2
fi
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

KERNEL="build/target/x86_64-unknown-none/release/sel4lake-kernel.mb32"
ARCHIVE="build/boot-archive-x86.bin"
OUT="build/sel4lake-grub.iso"
MIT_ARCHIV=1

while [ $# -gt 0 ]; do
    case "$1" in
        --kernel)     KERNEL="$2"; shift 2 ;;
        --archive)    ARCHIVE="$2"; shift 2 ;;
        --out)        OUT="$2"; shift 2 ;;
        --no-archive) MIT_ARCHIV=0; shift ;;
        -h|--help)    sed -n '2,25p' "$0"; exit 0 ;;
        *) echo "unbekannte Option '$1'" >&2; exit 2 ;;
    esac
done

for w in grub-mkrescue grub-file xorriso; do
    command -v "$w" >/dev/null || { echo "FEHLER: '$w' fehlt (Paket grub / xorriso)." >&2; exit 1; }
done
[ -f "$KERNEL" ] || { echo "FEHLER: Kernel '$KERNEL' fehlt -- zuerst ./build-x86.sh" >&2; exit 1; }

# Der Bootloader lädt nur, was er als Multiboot erkennt. Das hier zu prüfen statt es dem
# GRUB-Start zu überlassen, spart die Fehlersuche an einem schwarzen Bildschirm: dort sähe man
# nur, dass nichts passiert.
grub-file --is-x86-multiboot "$KERNEL" \
    || { echo "FEHLER: '$KERNEL' ist kein Multiboot1-Image (Header fehlt oder liegt zu spät)." >&2; exit 1; }

BAUM="$(mktemp -d)"
trap 'rm -rf "$BAUM"' EXIT
mkdir -p "$BAUM/boot/grub"
cp "$KERNEL" "$BAUM/boot/kernel.mb32"

# `timeout=0` und ein einziger Eintrag: dieses Image ist ein Testgegenstand, kein Menü. Eine
# Wartezeit würde jeden automatisierten Lauf um ihre Dauer verlängern, ohne etwas zu prüfen.
{
    echo "set timeout=0"
    echo "set default=0"
    echo ""
    echo 'menuentry "SEL4Lake" {'
    echo "    multiboot /boot/kernel.mb32"
} > "$BAUM/boot/grub/grub.cfg"

if [ "$MIT_ARCHIV" -eq 1 ]; then
    [ -f "$ARCHIVE" ] || { echo "FEHLER: Boot-Archiv '$ARCHIVE' fehlt (test-qemu-x86-load.sh baut es)." >&2; exit 1; }
    cp "$ARCHIVE" "$BAUM/boot/boot-archive.bin"
    # Der zweite Teil des Boot-Images. Der Modulname ist NICHT beliebig: der Kernel gleicht die
    # angelieferte Menge gegen das Manifest ab, und ein Modul, das dort nicht steht, ist ein
    # Fehler und keine Zugabe.
    echo "    module /boot/boot-archive.bin boot-archive" >> "$BAUM/boot/grub/grub.cfg"
fi

{
    echo "    boot"
    echo "}"
} >> "$BAUM/boot/grub/grub.cfg"

# grub-mkrescue redet viel über das Dateisystem, das es gerade schreibt. Für den Aufrufer zählt
# genau eine Frage: liegt am Ende ein ISO da.
grub-mkrescue -o "$OUT" "$BAUM" >/dev/null 2>&1 \
    || { echo "FEHLER: grub-mkrescue fehlgeschlagen." >&2; exit 1; }
[ -s "$OUT" ] || { echo "FEHLER: '$OUT' ist leer." >&2; exit 1; }

echo "== ISO: $OUT ($(du -h "$OUT" | cut -f1)) =="
echo "   Kernel : $KERNEL"
if [ "$MIT_ARCHIV" -eq 1 ]; then
    echo "   Modul  : $ARCHIVE  -> /boot/boot-archive.bin"
else
    echo "   Modul  : KEINES (--no-archive: Negativfall ohne Startmenge)"
fi
