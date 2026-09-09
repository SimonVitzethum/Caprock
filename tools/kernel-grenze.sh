#!/usr/bin/env bash
# **Was darf im Mikrokern liegen — und was nicht.** (Gesetzte Regel, Simon 2026-08-01)
#
# Der Kern enthaelt Mechanismus, keine Geraetetreiber. Treiber laufen als Userland-PD mit
# MMIO-Cap, IRQ-Cap und DMA-Cap (todo A-5.1); nur so tragen Hot-Reload, Fehlereindaemmung und
# Mandantentrennung ueberhaupt.
#
# Diese Regel stand bisher nur in Prosa. Prosa erodiert: am 2026-08-01 ist `virtio.rs` in die HAL
# gewandert, weil es als DMA-Beweisgeraet gebraucht wurde -- richtig als Zwischenschritt, falsch
# als Endzustand, und niemand haette es gemerkt. Deshalb hier eine Liste, die jeder neue
# HAL-Baustein passieren muss.
#
# Die Trennlinie ist nicht "Hardware ja/nein" -- die HAL fasst per Definition Hardware an. Sie
# lautet: **braucht der Kern das Geraet, um seine eigene Aufgabe zu erfuellen?** MMU, Interrupt-
# Controller, Timer und IOMMU sind die Mechanik von Isolation und Einplanung selbst; ohne sie gibt
# es keinen Kern. Ein RNG, eine Netzkarte, eine Platte sind Dienste FUER Mandanten.
if [ -z "${BASH_VERSION:-}" ]; then echo "FEHLER: braucht bash, nicht sh/dash." >&2; exit 2; fi
set -uo pipefail
cd "$(dirname "$0")/.."

# Erlaubt: Modulname -> Begruendung, warum der KERN es selbst braucht.
declare -A ERLAUBT=(
  [mmu]="Adressraumtrennung -- die Isolation selbst"
  [cpu]="Privilegstufen, Barrieren, Merkmale"
  [exception]="Trap-Eintritt; ohne ihn gibt es keinen Syscall"
  [intc]="Interrupt-Controller: Einplanung + IRQ-Zustellung an PDs"
  [gic]="Interrupt-Controller (aarch64)"
  [timer]="Zeitscheibe + Budgets"
  [fp]="Lazy-FP-Kontext gehoert zum Kontextwechsel"
  [gdt]="Segmentierung (x86); Ring-Wechsel"
  [syscall]="Syscall-Eintritt"
  [console]="frueher Debug-Ausgang, vor jeder PD"
  # Kein Treiber, sondern die SCHREIBORDNUNG ueber den beiden Konsolentreibern (C9b): wer haelt
  # den Port, wie lange am Stueck, und was geschieht mit einer Ausgabe aus dem Trap-Kontext. Sie
  # liegt hier und nicht in einer PD aus demselben Grund wie `console` selbst -- sie muss vor
  # jeder PD sprechen koennen, auch im Panikpfad. Und sie liegt arch-NEUTRAL, weil die Regel es
  # ist: zwei Architekturen, die dieselbe Ordnung getrennt hinschreiben, sind der Riss, durch den
  # eine von beiden zurueckfaellt.
  [konsole]="Schreibordnung der Debug-Konsole (C9b) -- Politik, kein Geraet"
  [power]="system_off/Reset -- Kernaufgabe"
  [psci]="dito (aarch64)"
  [cache]="Cache-Geometrie fuer die Farbzuteilung (A1)"
  [cache_decode]="reine Feldzerlegung dazu, host-getestet"
  [fault]="Fault-Weiterreichung an PDs"
  # C4. Zwei Atomics; die HAL kennt den Scheduler nicht, also reicht der Kern die beiden Adressen
  # herein und die HAL nur die Zahl heraus. Der Modulname steht hier und nicht bei `exception`,
  # weil der Inhalt arch-neutral ist: er lag bis 2026-08-13 NUR in der x86-HAL und hat den
  # aarch64-Bau gerissen. Kein Geraet, keine Hardware -- eine Messgroesse des Trap-Eintritts.
  [irqtiefe]="Stackverbrauch des IRQ-Pfads (C4): zweiter Summand der Stackrechnung"
  [hook]="typisierter atomarer Trap-Hook-Slot -- Mechanik des Trap-Eintritts, kein Geraet"
  [iommu]="DMA-Eindaemmung -- Isolation gegenueber Geraeten"
  [smmu]="IOMMU (aarch64)"
  [vtd]="IOMMU (x86)"
  # Z22 P1. Eine IRTE entscheidet, WELCHES Geraet WELCHEN Vektor auf WELCHEM Kern ausloesen darf --
  # das ist Autoritaet, nicht Geraetesteuerung, und damit genau das Kriterium dieser Grenze. Die
  # Treiber-PD schreibt zwar ihre MSI-X-Tabelle selbst (sie besitzt das Fenster), aber der EINTRAG,
  # gegen den die Einheit prueft, gehoert dem Kern: sonst duerfte eine PD den Handle einer fremden
  # IRTE benutzen. Die Datei selbst fasst keine Hardware an (reine Bitrechnung, host-getestet) --
  # sie steht hier, weil ihr ERGEBNIS eine Autoritaetsentscheidung ist.
  [irte]="Interrupt-Remapping-Eintraege (x86): wer darf welchen Vektor ausloesen"
  [dmar]="IOMMU-Entdeckung aus ACPI"
  [acpi]="Plattformentdeckung (Kerne, ECAM) beim Hochlauf"
  [pcie]="Bus-ENUMERATION + RID-Ermittlung fuer die IOMMU -- nicht Geraetetreiber"
  [virtio]="nur noch das AUFFINDEN der virtio-Strukturen im Konfigurationsraum (Enumeration, wie pcie). Die Treiberlogik liegt seit 2026-08-01 in crates/caprock-virtio -- ohne jede Abhaengigkeit, damit sie in eine Userland-PD kann (A-5.1)"
  # T0/Blech-Arbeit (2026-08-17), Liste am 2026-09-09 nachgezogen -- bis dahin meldete der
  # Waechter zu Recht 5 Verletzungen (fbtext, iommu_health, numa, bootparams, smt). Die Module
  # kamen ohne Listeneintrag dazu; die Pruefung war richtig, die Liste veraltet.
  # Z8/N0-N2: NUMA-Topologie lesen (SRAT/SLIT/DTB-Dekoder, reine Byte-Arithmetik, host-getestet
  # wie dmar/irte) + Platzierungsregeln. Der Kern platziert danach Speicher UND Kerne -- ohne
  # sie gibt es keine NUMA-Platzierung, nur Zufall.
  [numa]="Topologie-Dekoder + Platzierung (Z8): wonach der Kern Speicher und Kerne platziert"
  # Z6 Stufe 1: SMT-Topologie lesen + Zulassungspolitik (ein logischer Kern je physischem Kern).
  # Einplanung selbst -- welcher Kern ueberhaupt online darf --, kein Geraet, keine Hardware
  # (reine Arithmetik ueber injizierte Registerwerte, host-getestet wie dmar).
  [smt]="SMT-Topologie + Zulassungspolitik (Z6): welche Kerne der Scheduler online nehmen darf"
  # T0: was der kexec-Launcher dem Kern auf der Befehlszeile uebergibt (RSDP, Framebuffer-
  # Geometrie). Reiner Parser, keine Hardware. Auf UEFI-Blech findet der Kern ohne ihn weder
  # ACPI (kein Legacy-Scan) noch Anzeige -- Plattformentdeckung beim Hochlauf, wie acpi.
  [bootparams]="Boot-Uebergabe vom Launcher (T0): RSDP + Framebuffer-Geometrie -- Plattformentdeckung"
  # Arch-neutrales IOMMU-Urteil ueber beiden Fassaden (x86_64/iommu.rs, aarch64/iommu.rs reichen
  # die Werte herein). Reine Typ-Arithmetik, kein Registerzugriff -- wie irte eine
  # Autoritaetsaussage (DMA-Eindaemmung = Isolation gegenueber Geraeten), kein Geraet.
  [iommu_health]="arch-neutrales IOMMU-Gesundheitsurteil (Aussage ueber die DMA-Eindaemmung)"
)

# Bekannte Ausnahmen: liegen im Kern, gehoeren dort NICHT hin, mit benanntem Ausgang.
# Leer -- und das ist der Punkt. Am 2026-08-01 stand hier `virtio`, weil das Protokoll im Kern
# lag. Es liegt jetzt in `crates/caprock-virtio` (keine Abhaengigkeiten); in der HAL blieb nur das
# Auffinden der Strukturen, also Enumeration. Eine Ausnahme weniger, nicht eine Ausnahme
# umgeschrieben.
declare -A AUSNAHME=(
  # T0: Text in einen linearen Framebuffer -- reine Pixelarithmetik ueber einen injizierten
  # Slice, kein Hardwarezugriff, host-getestet. Dem ZWECK nach wie `console` (frueher
  # Debug-Ausgang auf UART-losen Maschinen, vor jeder PD), aber NOCH NICHT VERDRAHTET: kein
  # Aufrufer im Kern, keine Anbindung an die Konsole. Ausgang: beim Blech-Bring-up anbinden
  # oder entfernen -- bis dahin als Ausnahme benannt statt stillschweigend erlaubt.
  [fbtext]="noch nicht an die Konsole angebunden (T0-Vorlauf); Ausgang: Blech-Bring-up oder entfernen"
)

NUR_PRUEFEN=0
[ "${1:-}" = "--nur-pruefen" ] && NUR_PRUEFEN=1
fehler=0
gefunden=()
while IFS= read -r f; do
    m="$(basename "$f" .rs)"
    [ "$m" = "lib" ] && continue
    [ "$m" = "mod" ] && continue
    gefunden+=("$m")
    if [ -n "${ERLAUBT[$m]:-}" ]; then continue; fi
    if [ -n "${AUSNAHME[$m]:-}" ]; then
        echo "  AUSNAHME: $m -- ${AUSNAHME[$m]}"
        continue
    fi
    echo "  FEHLER: '$m' liegt in der HAL, steht aber weder auf der Erlaubnisliste noch als" >&2
    echo "          benannte Ausnahme. Entweder gehoert es in eine Userland-Treiber-PD (A-5.1)," >&2
    echo "          oder es gehoert auf die Liste in $0 -- MIT Begruendung, warum der KERN es" >&2
    echo "          selbst braucht. Stillschweigend aufnehmen ist der Weg, auf dem aus einem" >&2
    echo "          Mikrokern ein Monolith wird." >&2
    fehler=1
done < <(find crates/caprock-hal/src -name '*.rs' -not -path '*/tests/*')

echo "  geprueft: ${#gefunden[@]} HAL-Module"
if [ "$fehler" -ne 0 ]; then echo "== KERNGRENZE VERLETZT =="; exit 1; fi
[ "$NUR_PRUEFEN" -eq 1 ] && exit 0
# **Kann dieser Waechter ueberhaupt ausloesen?** Ein Pruefer, der ueber Abwesenheit entscheidet,
# muss sprechfaehig sein. Also einmal ein Modul unterschieben, das dort nichts zu suchen hat, und
# nachsehen, ob er es findet. Ohne diesen Schritt waere "keine Verletzung" auch dann die Antwort,
# wenn der `find`-Aufruf ins Leere liefe oder die Schleife nie durchlaufen wuerde.
PROBE=crates/caprock-hal/src/caprock_regressionsgeraet.rs
trap 'rm -f "$PROBE"' EXIT
printf '// Wegwerfdatei des Waechter-Selbsttests.\n' > "$PROBE"
if "$0" --nur-pruefen >/dev/null 2>&1; then
    echo "  FEHLER: der Waechter meldet KEINE Verletzung, obwohl ein fremdes Modul in der HAL" >&2
    echo "          liegt -- er ist leer. Das ist schlimmer als eine Verletzung: er wuerde jede" >&2
    echo "          kuenftige auch nicht sehen." >&2
    rm -f "$PROBE"
    exit 1
fi
rm -f "$PROBE"
trap - EXIT
echo "  Selbsttest: ein untergeschobenes Modul wird erkannt -- der Waechter ist sprechfaehig"
echo "== Kerngrenze eingehalten (Ausnahmen oben sind benannt, nicht genehmigt) =="
