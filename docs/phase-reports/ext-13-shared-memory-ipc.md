# Ausbaustufe 13 — Shared-Memory-IPC über die Isolationsgrenze

**Datum:** 2026-06-24 · **Status:** umgesetzt & in QEMU verifiziert.

Nutzt den allgemeinen VMM (ext-12), um **denselben physischen Frame in zwei
isolierte VSpaces** zu mappen — kontrolliert per Capability. Damit können zwei
ansonsten vollständig getrennte PDs **zero-copy** Daten austauschen, ohne dass die
VSpaces sonst irgendetwas teilen. Kommunikation läuft über cap-gewährten Frame +
IPC-Synchronisation.

## Mechanismus

- **Ein** Frame F (eine `MemoryCap`), **eine** installierte Cap (`froot`), daraus
  zwei Kind-Caps gemintet — je eine in den Cspace von Writer- und Reader-PD. Jede PD
  mappt F per `MAP`-Syscall (general VMM) in ihre eigene VSpace; durch
  Identity-Mapping liegt F in beiden bei derselben Adresse.
- **Synchronisation** über eine Notification: der Writer schreibt F und
  `SIGNAL`t, der Reader `WAIT`et und liest F danach. (Beides cap-gated; läuft
  kern-/VSpace-übergreifend, da kernel-vermittelt.)
- **Cap-kontrolliertes Teilen:** ohne F-Cap kein Mapping → kein Zugriff. Das
  Capability-System bestimmt, *wer* mit *wem* teilt.

## Verifiziertes Ergebnis

`./test-qemu.sh` → **ALL PASS** (23 Checks). Neuer `shm`-Check:
- Writer (isolierte VSpace A): `MAP` F → schreibt `SHM_SECRET` → `SIGNAL`.
- Reader (isolierte VSpace B): `MAP` F → `WAIT` → liest F → Wert == `SHM_SECRET` →
  meldet Badge SHARED.

`shm : Reader las Writer-Wert via geteiltem Frame (zwei isol. VSpaces)=true; ALL
PASS`. Die beiden PDs sehen **nur** F gemeinsam; jeder Fremdzugriff außerhalb würde
faulten (ext-11/12). 20+ Läufe ohne echten Hang (die Diagnose `DBG pending` löste
nie aus; gelegentliche Truncation nur unter schwerer paralleler Host-Last —
Fenster-Überschreitung, kein Kernel-Hang).

## Gefundener + behobener Bug

Der Writer mintete seine SIGNAL-Cap zunächst mit **Badge 0** → `SIGNAL` machte
`pending |= 0` (No-Op). Signalisierte der Writer **vor** dem `WAIT` des Readers
(kein Waiter), ging das Signal verloren und der Reader blockierte ewig. Behoben:
nicht-null Badge (wie beim bestehenden Notification-Muster).

## Offene Punkte

- Granularität weiterhin 2 MiB (vom allgemeinen VMM geerbt).
- Lese-/Schreibrechte des geteilten Mappings sind aktuell RW für beide; getrennte
  RO/RW-Mappings (z. B. Reader nur-lesend) wären über die Cap-Rechte umsetzbar.
- Nächster Schritt: **natives Code-Laden** je isolierter VSpace (eigenes
  `.user_text` statt der geteilten Demo-`.user_text`).
