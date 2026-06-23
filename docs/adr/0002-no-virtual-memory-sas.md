# ADR 0002 — Kein virtueller RAM: Single-Address-Space mit Capability-Isolation

**Status:** akzeptiert (Phase 0, umzusetzen ab Phase 1) · **Datum:** 2026-06-23

Dies ist die *definierende* Architekturentscheidung von SEL4Lake und der größte
Unterschied zu seL4.

## Anforderung (aus der Spezifikation)

- Jede Speicheradresse ist eine echte physische Adresse.
- Keine per-Prozess-Adressräume.
- Keine klassische MMU-basierte Speicherisolation.
- Keine virtuelle Speicherübersetzung.
- Speichersicherheit ausschließlich über Capabilities + Besitzrechte + Rust.

## Hintergrund: was „MMU aus“ auf aarch64 real bedeutet

Ein wichtiger Hardware-Fakt prägt die Entscheidung: Ist die MMU auf aarch64
**vollständig deaktiviert** (`SCTLR_ELx.M = 0`), behandelt die CPU **jeden**
Datenzugriff als `Device-nGnRnE`. Konsequenzen:

- **Keine Caches** → jeder Zugriff geht in den DRAM. Faktor 10–50× langsamer.
- **Keine Speculation, kein Reordering, kein Write-Combining.**
- **Strikte Ausrichtung erzwungen** → unausgerichtete Zugriffe faulten.

„MMU komplett aus“ widerspricht damit direkt dem Ziel *hochperformant*.

## Analysierte Lösungsansätze

### A) MMU vollständig deaktiviert (wörtliche Auslegung)
- **+** Maximal einfach konzeptionell; Adresse == Phys-Adresse trivial.
- **+** Kein Page-Table-Code, keine TLB-Verwaltung.
- **− Performance katastrophal** (alles uncached, siehe oben).
- **−** Strikte Ausrichtung überall nötig; kein `W^X`/NX, keine
  Read-only-Durchsetzung von Code/`.rodata` möglich.
- **Bewertung:** nur als Phase-0-Übergangszustand akzeptabel, nicht als Ziel.

### B) MMU an, **eine einzige globale Identity-Map**, Caches an (gewählt)
Die MMU ist aktiv, aber es gibt **genau einen** Übersetzungssatz für das ganze
System, der **virtuell == physisch** abbildet (Identity-/Flat-Mapping). Es gibt
**keine per-Prozess-Page-Tables und keinen Adressraumwechsel** beim
Kontextwechsel (kein `TTBR`-Wechsel, kein ASID-Roundtrip).

- **+** Adresse, die ein Prozess sieht, **ist** die physische Adresse — die
  Spezifikationsanforderung „echte physische Adresse / keine Übersetzung“ bleibt
  semantisch erfüllt (Identity-Map = keine *Umrechnung* von Bedeutung).
- **+** Caches/Speculation aktiv → **volle Performance**.
- **+** Kein TTBR-Wechsel beim Context-Switch → **sehr niedrige Switch-Latenz**
  und **keine TLB-Shootdowns** zwischen Prozessen (gut für Determinismus und
  Multicore-Skalierung).
- **+** Grobe Schutzattribute global setzbar: Code als `RX`, `.rodata` als `RO`,
  Rest `RW`+`XN` (No-Execute) → härtet den Kernel selbst (`W^X`), ohne
  Prozess-Isolation einzuführen.
- **−** Bietet **keine** per-Prozess-Hardware-Isolation (genau gewollt) — Trennung
  zwischen Komponenten kommt allein aus Rust + Capabilities (siehe Vertrauensmodell).
- **−** Page-Table-Setup einmalig nötig (kleiner, statischer Code in der erlaubten
  `unsafe`-Domäne „MMU-Init“). Bei 4 GiB kontinuierlichem RAM genügen wenige
  Block-Einträge (1-GiB-/2-MiB-Blocks), kein mehrstufiger per-Prozess-Walk.

### C) MMU an, per-Prozess-VSpaces (klassisch, wie seL4)
- **+** Echte Hardware-Isolation, kann auch unsichere native Prozesse einsperren.
- **−** Widerspricht der Spezifikation direkt (per-Prozess-Adressräume, Übersetzung).
- **−** TTBR-Wechsel + TLB-Verwaltung beim Context-Switch → höhere Latenz,
  TLB-Shootdowns auf Multicore, mehr Nichtdeterminismus. **Verworfen.**

## Entscheidung

**Ansatz B.** SEL4Lake fährt einen **Single-Address-Space (SAS)** mit aktiver
MMU, aber **einer einzigen, statischen Identity-Map** und aktivierten Caches.
Es gibt keine per-Prozess-Adressräume und keine Adressübersetzung im
semantischen Sinn. Speicher-Autorität wird über **Memory-Capabilities**
vergeben (ADR 0003), Speichersicherheit innerhalb von Komponenten durch **Rust**
garantiert. In Phase 0 läuft der Kernel noch mit deaktivierter MMU (reiner
Bring-up); die Identity-Map + Caches werden in Phase 1 aktiviert.

## Vertrauensmodell (zwingende Konsequenz)

Ohne per-Prozess-Isolation gilt: **alle Komponenten müssen speichersicher sein**
(sicheres Rust, vertrauenswürdige Toolchain). Ein Prozess mit beliebigem
`unsafe`/rohen Zeigern könnte ohne MMU fremden Speicher korrumpieren.
Capabilities begrenzen, *welche* Ressourcen eine Komponente überhaupt ansprechen
darf (Least Privilege), ersetzen aber **nicht** die Speichersicherheit *innerhalb*
nicht vertrauenswürdigen nativen Codes. Daher:

1. Userland-Komponenten werden als sicheres Rust ausgeliefert.
2. `unsafe` ist global auf die erlaubten Low-Level-Domänen beschränkt und wird
   begründet/auditierbar gehalten.
3. Die grobe globale MMU-Konfiguration (`W^X`, `XN` für Daten, `RO` für `.rodata`)
   fängt ganze Fehlerklassen ab, ohne Prozess-Isolation einzuführen.

## Sicherheitsauswirkungen

- **+** Großer Teil klassischer Speicherfehler wird durch Rust *eliminiert*, nicht
  nur abgefangen.
- **+** `W^X`/`XN`/`RO` global härtet die TCB.
- **−** Kein Schutz gegen bösartigen nativen Code → das Bedrohungsmodell schließt
  „nicht vertrauenswürdige native Binaries“ aus. Muss prominent dokumentiert und
  ggf. künftig durch optionale Hardware-Domänen (z. B. MTE, PAN, oder ein
  optionaler Sandbox-VSpace für untrusted Code) ergänzt werden.

## Performanceauswirkungen

- **+** Caches aktiv; kein TTBR-Wechsel/TLB-Shootdown beim Switch → niedrige,
  vorhersagbare Kontextwechsel-Latenz (gut für *deterministisch* + *Multicore*).
- **+** IPC kann Daten per Referenz/Move statt per Kopie übergeben (eine
  Adresse ist global gültig) — sehr niedrige IPC-Latenz möglich (ADR 0004).
- **−** Einmaliger MMU-Setup-Aufwand und etwas statischer `unsafe`-Code.

## Offene Punkte für spätere Phasen

- Genaues Page-Table-Layout der Identity-Map (Blockgrößen, MAIR-Attribute,
  Cacheability für Device-MMIO-Regionen vs. Normal-RAM).
- Optionaler Hardware-Härtungspfad für nicht vertrauenswürdige Komponenten
  (MTE/PAN/Sandbox-VSpace) — bewusst außerhalb des Kern-Bedrohungsmodells.
