## Lizenz

Caprock steht unter **AGPL-3.0-or-later** (`LICENSE`).

**§13 („Remote Network Interaction") ist der Punkt der Wahl:** wer eine **veränderte** Fassung über
ein Netz anbietet, muss ihren Quelltext den Nutzern anbieten — die Pflicht entsteht beim **Betrieb**,
nicht erst bei der Weitergabe. Ein Kunde, der sein **eigenes Programm** als PD darauf fährt, ist
davon **nicht** erfasst (s. ABI-Ausnahme).

Dazu eine **zusätzliche Erlaubnis** nach GPLv3 §7: die **ABI-Ausnahme** in
`LICENSE-EXCEPTION.md`. Ein Programm, das nur über die veröffentlichte System-Schnittstelle mit
dem Kernel verkehrt, ist allein deswegen kein abgeleitetes Werk. Sie gilt **für jeden,
unbeschränkt und ohne Antrag** — kommerziell wie nichtkommerziell. Dieselbe Konstruktion, die
Linux für seine System-Schnittstelle gewählt hat; der Copyleft-Charakter des Kerns bleibt
unberührt. **Unter AGPL ist sie tragend, nicht bequem** — sie ist es, die §13 von Kundencode
fernhält.

Welche Crates dafür permissiv werden müssen, steht in `docs/grenze.md`.

# Caprock

Ein eigenständiger, capability-basierter **Microkernel in Rust**, inspiriert von **seL4** und der
**seL4-Microkit**-Laufzeit. Er läuft auf **aarch64 und x86_64**.

**Zielbild:** der Kernel ist das **Basissystem eines Cloud-Servers** — kein Hypervisor darunter,
keine VM darin. Isolation, Zeit- und Ressourcenverwaltung macht der Kern selbst. Was daraus folgt
und was dafür noch fehlt, steht in [`todo.md`](todo.md) (Abschnitt Z) und
[`docs/plan-betriebsbereit.md`](docs/plan-betriebsbereit.md).

Ziele: hochsicher · hochperformant · capability-basiert · deterministisch · vollständig modular.

## Schnellstart

```sh
# aarch64 (QEMU virt)
./build.sh && ./run-qemu.sh          # bauen + booten (Beenden: Ctrl-A X)
./test-qemu.sh                       # vollständige Suite (PASS/FAIL)

# x86_64 (QEMU q35, Multiboot)
./build-x86.sh --features selftest
./test-qemu-x86.sh                   # vollständige Suite
RUNS=8 ./test-qemu-x86.sh            # achtmal booten und die Quote melden
```

Voraussetzungen: `rustup` mit **nightly** (+ `rust-src`, wegen `-Z build-std`), `qemu-system-aarch64`
bzw. `qemu-system-x86_64`, `python3` mit `cryptography` (fürs Signieren). Die Toolchain ist über
`rust-toolchain.toml` gepinnt.

**Aus einem frischen Clone lauffähig.** `keys/` ist gitignored; die Suiten erzeugen fehlende
**Testschlüssel** beim ersten Lauf selbst und bauen den Kernel danach neu (die Key-DB wird in ihn
hineinkompiliert). Ein privater Schlüssel im Repo wäre bei einem Open-Source-Projekt kein
Testschlüssel, sondern ein veröffentlichter — der Preis dieser Entscheidung ist ein
maschinenlokaler Wert im Image, und der kostet Reproduzierbarkeit *zwischen* Entwicklern, nicht
*innerhalb* eines Checkouts.

## Das Speichermodell — und wofür es NICHT gilt

Zwei Isolationswege, bewusst getrennt:

* **TrustedSAS** — Single-Address-Space, kein Adressraumwechsel. Sicherheit entsteht *intralingual*
  (safe Rust kann keinen Zeiger auf fremden Speicher erzeugen) plus Capabilities für Autorität. Ein
  Zertifikats-Gate erzwingt die Voraussetzung (`unsafe_status == ALL_PASS`, ADR 0014).
* **Isolierte PDs** — eigener Adressraum je PD, Hardware-Isolation, optional
  **cache-farbpartitioniert**.

> **Gesetzte Regel: TrustedSAS ist ausschließlich für eigenen Code. Kein Kundencode läuft jemals in
> einer TrustedSAS-PD.** Intralinguale Sicherheit lässt sich nur an *Quellcode* prüfen, nie an einem
> fremden Binary. Alles Fremde bekommt den isolierten Pfad — und der ist damit der Produktpfad.

## Stand

**aarch64:** Phasen 0–7 abgeschlossen, plus Ausbaustufen. 8 Kerne mit MMU/W^X, capability-basierter
Allokator, Capability-System mit Derivation-Tree, präemptiver Scheduler, cap-gesicherte IPC,
Protection Domains, Hot-Reload einer Server-Komponente über *dieselbe* Endpoint-Cap, FP/SIMD,
Bitmap-Prioritäten, Thread-Lebenszyklus, Thread-Migration, SMMUv3/DMA-Härtung, Boot-Archiv mit
signierten TrustedSAS-Zertifikaten, adversariale Testdienste aus drei Domänen.

**x86_64** (Zweig `arch/x86_64`, s. [`README-X86.md`](README-X86.md)): derselbe Kernel-Kern —
dieselben Selbsttests, derselbe präemptive Scheduler, dasselbe cap-gesicherte IPC. Capability-
System, Scheduler und IPC tragen **kein einziges `cfg(target_arch)`**; was sich unterscheidet,
liegt in `kernel/src/arch/`. Ausserhalb davon stehen heute 48 solcher `cfg`s im gemeinsamen Code —
nachgezählt, nicht geschätzt: 26 in `system.rs` (der DMA-Enforcer SMMUv3 gegen VT-d, zwei
Stack-Adressen, drei Logzeilen) und 16 im Hochlauf in `main.rs`. Der Gerätepfad ist also
arch-abhängig, die Kernmechanik nicht. Dazu 4-Level-Paging mit W^X, SMP über INIT-SIPI-SIPI,
x2APIC, LAPIC-Timer, PCI über ACPI-MCFG, **VT-d** (Bring-up, Fähigkeiten, DMAR-Auswertung mit
Gruppenbildung), Multiboot-Module als Startmenge, signiertes System-Manifest und Root-Task.

**Cache-Partitionierung zwischen PDs** (`todo.md` A1): die LLC-Geometrie wird *gemessen*
(CPUID-Blatt 4 bzw. CLIDR/CCSIDR), und zwei isolierte PDs bekommen disjunkte Farbsätze — Region,
Kernel-Stack und Seitentabellen. Was das **nicht** abdeckt, steht in
[`docs/invariants.md`](docs/invariants.md) §12; die wichtigste Grenze: gegen
Geschwister-Hyperthreads hilft keine Farbe.

**Kern-Übergabe (Variante B):** ein Linux-Kernelmodul nimmt Kerne offline und bringt sie in den
Long Mode mit eigener GDT/CR3 — wiederholbar, ohne Reboot (`tools/handover/`).

## Bauen ohne Prüfinfrastruktur

Rund die Hälfte des übersetzten Codes ist Prüfinfrastruktur — Selbsttests, Demo-Threads,
DMA-/Farbtests, Testharness. Sie liegt hinter dem Feature `selftest` (`.text` 0x25000 → 0x11000,
also 54 % weniger):

```sh
# ohne Prüfinfrastruktur
cargo build --release --no-default-features --target x86_64-unknown-none -p caprock-kernel
# mit
cargo build --release --features selftest   --target x86_64-unknown-none -p caprock-kernel
```

Die Testsuiten fordern `--features selftest` für das gebootete Image **ausdrücklich** an, statt es
über `default` mitzunehmen — sonst würden sie beim Drehen der Vorgabe still statt rot, und Stille
sieht aus wie Erfolg. **Beide** Konfigurationen werden mitgebaut: ein Bau, den niemand baut,
verrottet still (diese Fehlerform hat hier viermal zugeschlagen). Geprüft wird zusätzlich, dass
`.text` ohne das Feature wirklich schrumpft; sonst wäre das Gating unbemerkt wirkungslos geworden.

## Dokumentation

- [`todo.md`](todo.md) — offene Punkte, **Abschnitt Z: die Zielarchitektur**, an der alles zu
  messen ist
- [`docs/plan-betriebsbereit.md`](docs/plan-betriebsbereit.md) — Reihenfolge und Begründung
- [`done.md`](done.md) — Erledigtes *mit den Annahmen, die sich dabei als falsch erwiesen haben*
- [`docs/invariants.md`](docs/invariants.md) — die tragenden Zusicherungen **und ihre Grenzen**
- [`docs/00-overview.md`](docs/00-overview.md) — Aufbau, Crate-Layout, Vertrauensmodell
- [`docs/adr/`](docs/adr/) — Architekturentscheidungen
- [`docs/verification.md`](docs/verification.md) — Kani/Loom/Verus, Stand und Reichweite
- [`AGENTS.md`](AGENTS.md) / [`STATUS.md`](STATUS.md) — Arbeitsteilung und laufender Stand

## Projektgrenzen

Im Boot-Image liegen genau zwei Dinge: der **Kernel** und **eine Manifestdatei**, die festlegt, was
geladen wird. Treiber, Netzwerkstack, Dateisysteme und Dienste liegen außerhalb und sind zur
Laufzeit austauschbar. Das Manifest ist dabei ein **Autoritätsdokument**, kein Konfigurationsfile:
es legt die gesamte Anfangsverteilung von Autorität fest und ist deshalb signiert und an das
Kernel-Image gebunden.

Was heute noch fehlt, wird nicht verschwiegen: es gibt **keinen Netzstack und kein Blockgerät**.
Der einzige Gerätenachweis ist ein virtio-RNG, und der **DMA-End-to-End-Beleg existiert nur auf
aarch64** (SMMUv3); auf x86 findet der Kernel dasselbe Gerät über PCI, der Weg durch VT-d ist aber
nicht bis zum DMA-Ziel geprüft. VT-d-**Interrupt-Remapping** steht aus — und ohne das darf kein
Gerät an einen Tenant. Die Thread-Migration über Servergrenzen ist Entwurf, nicht Code. Die Liste
steht vollständig in [`todo.md`](todo.md).
