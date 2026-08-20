#!/usr/bin/env bash
# **Does the Z8 topology reader see a second node, and does placement use it?**
#
# ## Why this is a separate run
#
# The development machine has exactly ONE node and the suites boot without `-numa`, so every normal
# run reports `nodes=1` and the placement question never arises. The `numa` line is then true
# because its antecedent is false — the RMRR-on-q35 trap, and the same vacuity the `smt` line has
# under `threads=1`. This is the run where the case exists.
#
# ## What it can and cannot show
#
# QEMU emulates the **topology**, not the **latency**: `-numa node,…` produces a real SRAT and SLIT
# (x86) that the kernel parses for real, but the "remote" node is ordinary host memory at ordinary
# host speed. Verifiable here: how many nodes were read, whether the distance matrix arrived,
# whether an allocation that asked for a node got it. NOT verifiable, ever, under QEMU: that it is
# faster. Measuring the benefit needs a real multi-socket machine.
#
# ## The speaking test
#
# Both directions, because one alone proves nothing: with `-numa` the kernel must read TWO nodes
# and a distance matrix; without it, the same kernel must read `nodes=0/1` and still pass. A decoder
# that always claimed two nodes would satisfy the first and fail the second.
set -uo pipefail
cd "$(dirname "$0")/.."

SEK="${SEK:-120}"
KERNEL=build/target/x86_64-unknown-none/release/caprock-kernel.mb32
[ -f "$KERNEL" ] || { echo "numa-messen: FEHLT: $KERNEL -- erst './build-x86.sh --features selftest'"; exit 2; }

if [ -r /dev/kvm ] && [ -w /dev/kvm ]; then
    ACCEL=(-enable-kvm -cpu host,+invtsc,host-cache-info=on)
else
    ACCEL=(-cpu Skylake-Client)
fi
mkdir -p build/diag
fail=0

boot() { # $1 = Ausgabedatei, Rest = zusaetzliche QEMU-Argumente
    local out="$1"; shift
    timeout --signal=KILL "$SEK" qemu-system-x86_64 \
        -machine q35,kernel-irqchip=split -device intel-iommu,intremap=on,caching-mode=on \
        "${ACCEL[@]}" -kernel "$KERNEL" \
        "$@" -serial file:"$out" -display none -no-reboot >/dev/null 2>&1
    return 0
}

erwarte() { # $1 Beschreibung, $2 Datei, $3 Muster
    local got; got="$(grep -m1 "^numa    : init=" "$2" 2>/dev/null)"
    if [ -z "$got" ]; then
        # Fehlend und kaputt duerfen nicht gleich aussehen.
        echo "  FEHLSCHLAG ($1): keine numa-Zeile in $2 -- Boot abgebrochen?"
        fail=1; return
    fi
    if grep -q "$3" <<<"$got"; then
        echo "  OK   ($1): $got"
    else
        echo "  FEHLSCHLAG ($1): erwartet /$3/"
        echo "                   gefunden: $got"
        fail=1
    fi
}

echo "== Z8/N0+N1: sieht der Kernel eine zweite NUMA-Domaene? =="

# --- Fall 1: ZWEI Knoten, mit Distanzmatrix -------------------------------------------------
L1=build/diag/numa-zwei.log
boot "$L1" -m 2G -smp 4,sockets=2,cores=2 \
    -object memory-backend-ram,id=m0,size=1G -object memory-backend-ram,id=m1,size=1G \
    -numa node,nodeid=0,cpus=0-1,memdev=m0 -numa node,nodeid=1,cpus=2-3,memdev=m1 \
    -numa dist,src=0,dst=1,val=21
erwarte "zwei Knoten + Matrix" "$L1" "readable=true trustworthy=true truncated=false nodes=2 .*distances=true"
# Der WERT der Matrix, nicht nur ihre Anwesenheit: `-numa dist,val=21` muss als 21 ankommen.
# (Nicht zeilenanfangs verankern -- `distance(0,1)` steht als ZWEITES Feld der Zeile. Die erste
# Fassung dieses Musters meldete FEHLSCHLAG fuer eine Zeile, die den richtigen Wert enthielt.)
if grep -q "distance(0,1)=Some(21)" "$L1"; then
    echo "  OK   (Distanz gelesen): $(grep -m1 '^numa    : distance' "$L1")"
else
    echo "  FEHLSCHLAG (Distanz): erwartet distance(0,1)=Some(21)"
    echo "               gefunden: $(grep -m1 '^numa    : distance' "$L1" || echo 'keine Zeile')"
    fail=1
fi
# **Die Platzierung muss GESPROCHEN haben.** Sonst sagt die Zeile ueber N2 gar nichts.
if grep -qE "^numa    : placed .* speaking=true" "$L1"; then
    echo "  OK   (Platzierung sprach): $(grep -m1 '^numa    : placed' "$L1")"
else
    echo "  FEHLSCHLAG: die Platzierungsleiter wurde nie gefahren (speaking=false)"
    fail=1
fi

# --- Fall 2: die Gegenprobe -- derselbe Kernel ohne -numa ------------------------------------
L2=build/diag/numa-flach.log
boot "$L2" -m 512M -smp 4
erwarte "ohne -numa" "$L2" "init=true .*truncated=false"
if grep -qE "^numa    : init=true readable=(true nodes=1|false)" "$L2" \
   || grep -qE "nodes=[01] " "$L2"; then
    echo "  OK   (flach): $(grep -m1 '^numa    : init=' "$L2")"
else
    echo "  FEHLSCHLAG (flach): erwartet hoechstens EINEN Knoten ohne -numa"
    fail=1
fi

# --- Das Urteil selbst -----------------------------------------------------------------------
for f in "$L1" "$L2"; do
    if grep -q "^numa    : ALL PASS" "$f"; then
        echo "  OK   (Urteil $(basename "$f")): numa : ALL PASS"
    else
        echo "  FEHLSCHLAG (Urteil $(basename "$f")): $(grep -m1 '^numa    : init=' "$f" || echo 'keine Zeile')"
        fail=1
    fi
done

if [ "$fail" -eq 0 ]; then
    echo "== NUMA-MESSUNG: ALL PASS =="
    echo "   (geprueft ist das LESEN und die PLATZIERUNG; die Latenz emuliert QEMU nicht -- s. Kopf)"
    exit 0
else
    echo "== NUMA-MESSUNG: FAILURES =="
    exit 1
fi
