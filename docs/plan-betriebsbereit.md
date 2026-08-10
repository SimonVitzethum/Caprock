# Weg zur Betriebsbereitschaft

Stand 2026-07-29. Der Plan, wie aus dem heutigen Kernel ein Basissystem für Cloud-Server wird —
kein Hypervisor darunter, keine VM darin, Isolation und Zeitverwaltung durch den Kern selbst.

Die Punkte selbst stehen in [todo.md](../todo.md); hier steht die **Reihenfolge und warum sie so
ist**. Was hier nicht steht, ist der Aufwand in Personentagen: die Schätzungen wären geraten, und
geratene Zahlen werden später als Zusagen gelesen.

## Der Satz, an dem sich der Plan aufhängt

**Der Kernel hat heute keinen Nicht-Test-Zweck.**

Das ist kein Vorwurf, sondern der gemessene Zustand: Schaltet man das Feature `selftest` ab, baut
der Kernel, bootet, richtet Paging, LAPIC, Timer, SMP, PCI und VT-d ein — und geht dann in eine
Leerschleife. `.text` schrumpft dabei von 0x25000 auf 0x11000, mehr als die Hälfte des
übersetzten Codes ist Prüfinfrastruktur. Es gibt kein Boot-Archiv auf x86, keinen Root-Task, kein
Userland. Jedes Programm, das heute läuft, ist in den Kernel einkompiliert.

Daraus folgt die Gliederung: **Stufe 1 ist nicht Härtung und nicht Skalierung, sondern
überhaupt-etwas-ausführen-können.** Alles andere hängt daran, und zwar nicht organisatorisch,
sondern technisch — ein Treiber, ein Netzstack, ein Tenant-Prozess, ein wandernder Thread: alle
brauchen zuerst einen Weg, Code von außen zu laden und ihm Autorität zu geben.

## Stufe 0 — den Boden geradeziehen (Vorbedingung von allem)

Nicht verhandelbar, weil jede Messung darauf steht.

1. **[D0] Der x86-Lauf muss wieder deterministisch sein.** Gemessen: 4–6 von 8 Läufen desselben
   Images erreichen `SELFTEST COMPLETE`, die übrigen bleiben an wechselnden Stellen stehen. Ein
   Aufbau, der in einem Drittel der Fälle hängt, kann keine Aussage tragen — auch nicht die, die
   er gerade grün meldet. Zwei Verdächtige sind zu trennen: der TCG-Rückfall auf
   `-cpu Skylake-Client` und die durch den Farbtest verschobene Speicherlage.
2. **[Verifikation] Die ARM-Suite muss aus einem frischen Clone laufen.** `test-qemu.sh` signiert
   TrustedSAS-Binaries mit `keys/trusted-test.ed25519`; `/keys/` ist gitignored. Aus einem Clone
   ist die Suite damit nicht lauffähig, und der zweite Architekturzweig bleibt ungeprüft — genau
   die Fehlerform, die dieses Projekt schon dreimal bezahlt hat. Lösungsweg: ein **Testschlüssel
   im Repo** (ausdrücklich als solcher benannt, mit eigenem, im Kernel als `test`
   gekennzeichnetem Key-Slot), oder ein Skript, das beim ersten Lauf ein Paar erzeugt und
   `trusted_keys.rs` daraus generiert. Der Produktivschlüssel bleibt selbstverständlich draußen.
3. **`README.md` sagt, was das Projekt ist.** Sie beschreibt heute einen aarch64-Kernel der Phase
   7 — kein Wort vom x86-Port, von VT-d, von der Kern-Übergabe. Bei einem Open-Source-Projekt ist
   das die teuerste veraltete Datei überhaupt.

## Stufe 1 — der Kernel führt fremden Code aus

Das ist die Stufe, die aus einem Testgerüst ein Betriebssystem macht.

4. **[C6] Boot-Archiv auf x86 über Multiboot-Module.** `SYS_LOAD` scheitert heute sauber, weil es
   nichts zu laden gibt. Auf ARM existiert der Weg (`tools/mkarchive.py`, `caprock-loader`) —
   x86 braucht das Gegenstück über die Multiboot-Modulliste.
5. **[F2] Root-Task.** Der seL4-Weg: ein Startprogramm aus dem Archiv laden und ihm die
   Wurzel-Caps übergeben. **Erst danach** darf `default = []` werden (todo F2) — vorher wäre das
   Gating kein schlankerer Kernel, sondern ein leerer.
6. **[A4] `SYS_CDELETE`, dann `CMOVE`/`CCOPY`.** Ohne das kann ein langlebiger Dienst, der Caps
   per IPC empfängt, seine Slots nicht freigeben und läuft gegen `CAP_BUDGET_PER_PD`. Ein
   Root-Task ist genau so ein Dienst — der Punkt wird mit Stufe 1 von einer ABI-Lücke zu einem
   Betriebsproblem.

Am Ende von Stufe 1 gilt: ein extern gebautes Programm startet, bekommt Autorität, gibt sie
zurück. Ab hier ist jede weitere Fähigkeit ein Userland-Programm und kein Kernel-Patch.

## Stufe 2 — ein Server, der etwas tut

7. **[Z10] Treiberrahmen + virtio auf x86.** `virtio` ist bis heute aarch64-only. Ohne Netz und
   Blockgerät ist es keine Cloud. Als Userland-PDs, nicht im Kernel — das ist die Projektgrenze
   aus der `README.md`, und sie ist richtig.
8. **[E, Schritt 3b+4] Queued Invalidation, dann Interrupt Remapping mit abgeschaltetem CFI.**
   Reihenfolge ist erzwungen: die Invalidierung des Interrupt-Entry-Cache existiert **nur** als
   QI-Deskriptor. Und ohne IR kann ein durchgereichtes Gerät beliebige Interrupt-Nachrichten
   erzeugen — das ist der Standardausbruch aus einer Geräte-Zuteilung und muss **vor** dem ersten
   Tenant-Gerät stehen, nicht danach.
9. ~~**[Z9] Fehlerdomäne festlegen und aufschreiben.**~~ **Erledigt 2026-08-02 (B-6.2):**
   [fehlerdomaene.md](fehlerdomaene.md), Invariante §14 in [invariants.md](invariants.md).
   Festgelegt ist die billige Variante — *der Knoten ist die Fehlerdomäne, Redundanz über Knoten*,
   und alle TrustedSAS-PDs eines Knotens bilden untereinander **eine** Domäne.
   **Die Prämisse dieses Punktes war falsch:** ein Kernel-Panic reißt den Knoten heute *nicht*
   zuverlässig mit — er wird in den häufigsten Fällen verschluckt, weil das `halt()` im Panic-Pfad
   die Interrupts nicht maskiert und der Timer-Tick den Kern zurückholt. Gemessen, mit vier
   verschiedenen Ausgängen für dieselbe Ursache. Für ein Sicherheitsprodukt ist das der schlechtere
   Ausgang: der Kernel arbeitet nach einer nachweislich verletzten Invariante weiter. Die drei
   billigen Gegenmaßnahmen sind benannt und **nicht** gebaut — sie sind eine Entscheidung
   (Verfügbarkeit gegen Ehrlichkeit), keine Reparatur.

## Stufe 3 — mehr als ein Tenant, verantwortbar

10. **[Z1] Der isolierte, gefärbte Pfad wird der Normalfall.** Heute ist `spawn_isolated` regulär
    und ungefärbt, `spawn_isolated_colored` die Ausnahme. Dazu gehört die Entscheidung über die
    Regionsgröße: Färbung verträgt sich nicht mit dem 2-MiB-Blockdeskriptor (512 Seiten
    überstreichen alle 256 gemessenen Farben), also entweder kleinere Regionen mit seitenweisem
    Mapping oder mehrere gefärbte Läufe je PD.
11. **[A1-Rest] Streifen-Freiliste.** `mask_for` vergibt heute rundläufig, **ohne** zu prüfen, ob
    ein Streifen belegt ist: mehr gleichzeitige PDs als Partitionen heißt stille
    Farbüberschneidung. Nötig ist ein sauberer Fehlschlag statt einer stillen Aufweichung.
12. **[Z6] SMT.** Cache-Coloring trennt den LLC und **prinzipiell nicht** L1/L2/TLB/Store-Buffer
    zwischen Geschwister-Hyperthreads. Entweder SMT aus (kostet Durchsatz, ist ehrlich) oder ein
    physischer Kern gehört zu jedem Zeitpunkt genau einem Tenant. Für den zweiten Weg muss der
    Scheduler die CPU-Topologie kennen; die liest heute niemand.
13. **[A3/C3] Cap-/PD-/Endpoint-Tabellen dynamisch**, davor `ReplyFinal` vom Kernelstack lösen.
    Solange die Cap-Tabelle global und fest ist, bestimmt ein Tenant die Dichte aller anderen.
14. **[Z7] Messbarer Boot und Attestierung.** Vorbedingung für alles, was Tenant-Zustand über das
    Netz bewegt.

## Stufe 4 — die Eigenschaften, die VMs ersetzen

15. **[Z2] Abrechnung entkoppelt vom Zeittakt.** Verbrauch per Zyklenstempel beim Ein- und
    Auswechseln (per-TCB `consumed_cycles`, todo D), Monitoring-Cap darüber. **Ein schnellerer
    Tick ist die falsche Antwort** — er erhöht Auflösung *und* Overhead; ein Zyklenstempel erhöht
    nur die Auflösung.
16. **[Z5] Tickless für Rechenkerne.** Timer nur armieren, wenn es etwas zu verdrängen gibt.
    Zusammen mit 15 ergibt das: Abrechnung per Zyklen, Verdrängung per Bedarf — die beiden werden
    entkoppelt, und genau das ist der Punkt.
17. **[Z8] NUMA**, gemeinsam mit der Farbvergabe entschieden. Zwei getrennt entwickelte Politiken
    kämpfen sonst um dieselbe Physadresse, und wer zuerst zuteilt, gewinnt.
18. **[Z4] Checkpoint/Restore eines Threads**, in der Reihenfolge Z4a–Z4f. Das ist der
    aufwendigste Punkt des Plans, und er steht bewusst am Ende: er setzt Stufe 1 (Prozesse
    überhaupt), Stufe 3 (Isolation, die man mitnehmen kann) und Z7 (eine Zielmaschine, der man
    trauen kann) voraus.
19. **[C5] GICv3 auf ARM**, falls ARM Zielplattform bleibt — sonst bleibt ARM bei 8 Kernen. Auf
    x86 ist das mit x2APIC erledigt.

## Was ich an diesem Plan für riskant halte

**Z4 (Thread-Migration) kann Entwurfsentscheidungen erzwingen, die weiter vorn liegen.** Caps sind
heute Indizes in eine globale Tabelle plus CDT-Kante; über Maschinengrenzen bedeutet ein Index
nichts. Wenn sich beim Entwurf von Z4b herausstellt, dass eine externe, maschinenunabhängige
Cap-Darstellung nötig ist, betrifft das die Cap-Crate, die ABI und die Verifikation. Es lohnt
sich, **früh einen Wegwerf-Prototyp** von Z4b zu bauen — nur die Serialisierung eines
Cap-Space, ohne Netz, ohne Migration — um herauszufinden, ob das Modell trägt. Das kostet wenig
und kann teure Umwege ersparen.

**Der zweite Architekturzweig verrottet, solange er nicht läuft.** Solange die ARM-Suite aus einem
Clone nicht startet, ist jede aarch64-Zeile ungeprüft — die `hal::cache`-Fassung für ARM ist
geschrieben, übersetzt und nie ausgeführt worden. Deshalb steht Punkt 2 in Stufe 0 und nicht
irgendwo hinten.

**Die Reihenfolge Härtung-vor-Funktion ist verlockend und wäre falsch.** A1, SMT, NUMA sind
sichtbare, gut abgrenzbare Arbeit mit schönen Testergebnissen. Aber ein Kernel, der nichts
ausführt, braucht keine Cache-Partitionierung zwischen Tenants, die es nicht gibt. Erst Stufe 1,
dann der Rest.
