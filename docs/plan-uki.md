# Plan: UKI-Direktstart (x86-64, später aarch64)

Stand: 2026-09-10. Status: **geplant, nicht begonnen** — kommt nach B4b/E2E/WASM,
es sei denn, Z7-Attestierung bekommt Priorität (dann ist UKI deren Fahrzeug).

## Ziel

Caprock bootet auf Blech ohne GRUB dazwischen: ein PE32+-Binary (UKI-Form —
Kernel + Archiv + Cmdline in einem Abbild) startet direkt aus der UEFI-Firmware.
Der einzige Gewinn gegenüber GRUB, der das rechtfertigt: **Measured Boot**
(Kernel + Module in die PCRs messen, bevor gesprungen wird → Z7-Kette).
Kein schnellerer Boot, kein einziges Bit mehr.

## Ausdrücklich nicht enthalten

Kein BIOS-Loader, keine Disk-Treiber im Stub (Archiv als `.initrd`-Section
eingebettet), keine Menüs/Config-Sprache, kein GRUB-Ersatz im BIOS-Sinn.
Der QEMU-Tagesloop (`-kernel`) bleibt unberührt; der Multiboot-Pfad fährt
parallel weiter, bis UKI `SELFTEST` belegt (HEAD-Regel).

## Bausteine

1. **Minimal-Stub** (~300–500 Zeilen, neu, `no_std`): eigene Sections finden,
   `ExitBootServices`, Sprung zum Kernel mit Übergabestruktur (Memory Map,
   Modul-Spannen, RSDP, optional GOP). `uefi`-Crate oder handgerollt.
2. **64-Bit-Einstieg im Kernel**: Long-Mode-Einstieg, der dieselbe
   Übergabestruktur frisst wie der Multiboot-Pfad (Muster: `set_archive_span` —
   der Rest des Kernels merkt nichts). 32-Bit-Trampolin bleibt für Multiboot.
3. **Bau + Test**: PE linken (`x86_64-unknown-uefi`, nachinstallieren),
   UKI per `objcopy --add-section` zusammensetzen (kein `ukify` nötig),
   QEMU mit `-bios OVMF_CODE_4M.fd` (liegt vor, auch aa64-Variante).
   Secure-Boot-Signierung (selbst signiert + MOK) ist Prozedur, kein Code.

## Aufwand (Schätzung 2026-09-10)

* Stub + Einstieg + QEMU-Boot bis `SELFTEST`: 1–2 Wochen (Firmware-Quirks
  unter OVMF klein, Blech später größer).
* Measured Boot obendrauf (PCR-Extend via TCG-Protokoll + Event Log → Z7):
  +1 Woche. Das ist der eigentliche Gewinn.
* aarch64 analog: +Tage (Firmware liegt vor).

## Abnahme

* QEMU/OVMF-Boot bis `SELFTEST COMPLETE` mit identischem Ergebnis wie
  Multiboot-Pfad (kein Sonderverhalten je Pfad).
* Gemessen: PCR-Werte + Event Log gegen bekannte Hashes nachrechenbar
  (sonst ist „measured" behauptet, nicht belegt).
* Danach erst: Multiboot-Pfad als Rückfall behalten oder entfernen (eigene
  Entscheidung, nicht Teil dieses Plans).
