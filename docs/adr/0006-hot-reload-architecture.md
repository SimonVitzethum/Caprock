# ADR 0006 — Hot-Reload-Architektur

**Status:** umgesetzt (Phase 7) · **Datum:** 2026-06-23

> **Stand (Phase 7):** Hot-Reload ist implementiert und in QEMU verifiziert — ein
> Server-PD wird im laufenden System ersetzt (Quiesce: Empfänger zurückziehen +
> Recv-Cap entziehen → Swap: v2 starten + Recv-Cap auf *denselben* Endpoint
> delegieren → Resume), ohne Kernel-Neustart, transparent für den Client (gleiche
> Send-Cap, gleicher Endpoint). Primitive: `EndpointTable::retire_receiver`,
> `PdTable::clear_cap`. **Noch offen:** Zustands-Checkpoint für stateful
> Komponenten (Schritt 2 unten), generischer Reload-Dienst, Abräumen des alten
> PD (TCB/Stack). Siehe `docs/phase-reports/phase-7.md`.

## Motivation

Alles außer Kernel und Microkit-Runtime muss **ohne Kernel-Neustart** stoppbar,
ersetzbar und neu ladbar sein: Treiber, Netzwerkstack, Dateisysteme, Dienste,
Userland. Ein Kernel-Reboot dafür ist nie zulässig.

## Grundidee: Capabilities als stabile Identität, Komponente als austauschbarer Inhalt

Eine Komponente (z. B. ein Treiber) ist ein **Protection Domain** (Microkit-PD),
das ausschließlich über Capabilities mit der Außenwelt verbunden ist: Endpoints
(Dienste, die es anbietet/nutzt), Notifications (IRQs), Device- und
Memory-Capabilities. Der **Capability-Graph ist die stabile Schnittstelle** —
der konkrete Code dahinter ist austauschbar.

## Analysierte Lösungsansätze

### A) Prozess killen + neu starten, Clients verbinden neu
- **−** Verbindungen brechen; Clients müssen Reconnect-Logik haben; Zustand geht
  verloren; nicht transparent. Verworfen als Standard.

### B) Endpoint-Rebind hinter stabiler Cap-Identität (gewählt)
Der **Endpoint** eines Dienstes ist ein eigenständiges Kernelobjekt mit stabiler
Capability. Beim Reload:

1. **Quiesce:** neue Requests am Endpoint werden gepuffert/geblockt (der Endpoint
   bleibt bestehen, der Server wird nur abgekoppelt).
2. **Drain/Checkpoint:** der alte PD beendet laufende Requests; exportiert
   optional Zustand in eine **Memory-Cap** (übergeben an den Nachfolger).
3. **Swap:** neuer PD wird geladen, erhält dieselben Device-/Memory-/Endpoint-Caps
   (Re-Bind) und den Checkpoint.
4. **Resume:** Endpoint wird wieder an den neuen PD gebunden; gepufferte Requests
   laufen weiter. Clients merken nichts (gleiche Cap, gleicher Endpoint).

- **+** Transparent für Clients (stabile Cap/Endpoint-Identität).
- **+** Sauber capability-modelliert: Reload = Caps vom alten PD `revoke`n und an
  neuen PD `delegate`n; Kontrolle hat ein **Reload-Manager** mit der nötigen
  Autorität (selbst ein Userland-Dienst mit besonderen Caps).
- **+** Zustandsübergabe via Memory-Cap ist im SAS zero-copy (ADR 0002/0004).
- **−** Dienste müssen ein **Quiesce/Checkpoint-Protokoll** unterstützen, um
  Zustand nicht zu verlieren (stateless Treiber sind trivial; stateful brauchen
  Kooperation).

### C) Versionierte Module mit Live-Patching im selben PD (Theseus-Stil)
- **+** Feingranular, kein PD-Wechsel.
- **−** Erfordert tiefe Toolchain-/Lader-Integration; später als Optimierung
  denkbar, jetzt zu komplex. Zurückgestellt.

## Entscheidung

**Ansatz B.** Hot-Reload über stabile Endpoint-/Cap-Identität: Quiesce →
Drain/Checkpoint → Swap (Cap-Revoke/Delegate) → Resume. Der Kernel liefert die
Primitiven (Endpoint-Quiesce/-Rebind, Cap-Revoke/-Delegate, Notification-Umlenkung);
ein **Reload-Manager im Userland** orchestriert die Policy. Kernel und
Microkit-Runtime selbst sind **nicht** hot-reloadbar (sie sind die TCB).

## Sicherheitsauswirkungen

- Der Reload-Manager ist eine privilegierte Komponente (definierte Cap-Menge) —
  Least Privilege strikt einhalten; er darf nur die Caps der zu ersetzenden
  Komponente umhängen.
- **Revocation-Korrektheit** ist sicherheitskritisch: nach dem Swap dürfen keine
  „dangling“ Caps des alten PD auf Geräte/Speicher verbleiben (CDT-Revoke, ADR 0003).
- Ein kompromittierter Treiber kann (ohne MMU-Isolation, ADR 0002) im Rahmen
  *seiner* Caps Schaden anrichten, aber Hot-Reload erlaubt schnelles Ersetzen als
  Reaktion.

## Performanceauswirkungen

Quiesce-Fenster sollte klein sein; gepufferte Requests verursachen kurze
Latenzspitzen während des Swaps. Zero-Copy-Checkpoint via Memory-Cap hält den
Zustandstransfer billig. Kein Kernel-Reboot → Verfügbarkeit bleibt erhalten.
