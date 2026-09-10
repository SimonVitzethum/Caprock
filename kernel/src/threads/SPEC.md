# SPEC: Gang-Scheduling und Trust-Domänen (Z6 Stufen 2/3) — die §12a-Entscheidung

Stand: 2026-09-09 · Strang TICKLESS/SMT (NOHZ + SMT-1b) · Nachfolger von `docs/invariants.md` §12a
„The decision that stage 3 will have to make — recorded before it is needed".

Diese Datei **trifft** die Entscheidung, die §12a offengelassen hat. Sie baut **keinen**
Mechanismus: Stufen 2/3 ohne diese Entscheidung gießen die Granularität in Beton (s. Z6),
und ein Gang, dessen Zusicherung breiter klingt als sie ist, wiederholt genau die Form,
derentwegen §12 existiert. Kein Code ohne Entscheidung — hier ist sie.

## 1. Die Entscheidung

**Gewählt: Option 2 — die Zusicherung wird schriftlich auf Tenant-gegen-Tenant verengt.**

Sobald zwei Geschwister einer Domäne gemeinsam laufen, läuft bei jedem Syscall und jedem
Interrupt auf der einen Seite **Kernelcode** neben **Usercode** der anderen. Der Kernel
hält tenantfremde Geheimnisse (Trusted-Key-Material, fremde Frames auf dem
Sidecar-Pfad). Wer das abdeckt, braucht Option 1 (Kerineintritt stoppt das Geschwister).
Wer Option 1 nicht bezahlt, deckt Tenant-gegen-Kernel nicht ab — und das steht ab heute
**hier und in §12a** (Patch-Text an B, s. §5), nicht zwischen den Zeilen.

### Warum nicht Option 1

Drei Gründe, in Reihenfolge ihres Gewichts:

1. **Der Preis hängt am häufigsten Pfad, nicht am seltensten.** Dies ist ein
   IPC-lastiger Mikrokern: Kerneintritt ist der Normalfall, nicht die Ausnahme. Das
   Geschwister bei jedem Eintritt zu stoppen (IPI + Warten + Wiederanlauf) besteuert
   genau die Operation, die dieses System am meisten ausführt. Bei Linux war das
   Geschwister-Stoppen die teure Hälfte des Core-Schedulings und ist nie gelandet —
   dort ist Kerneintritt selten. Hier wäre es der Preispfad.
2. **Der Preis ist strukturell, nicht einstellbar.** Kein Schwellwert macht ihn klein:
   Jeder Eintritt zahlt, auch der Nanosekunden-Syscall. Eine „nur bei Geheimnissen"-
   Variante scheitert daran, dass der Kern beim Eintritt noch nicht weiß, ob der Pfad
   Geheimnisse berührt — die Klassifikation käme nach dem Eintritt, also nach dem
   Zeitpunkt, an dem das Geschwister hätte stoppen müssen.
3. **Der Nutzen ist begrenzt, solange §12a-1b gilt.** Stufe 1 (ein Kern, eine CPU) deckt
   beides heute vollständig ab. Stufe 3 kauft Durchsatz zurück (×1,36…×1,89, gemessen
   2026-08-17). Wer dafür die Tenant-gegen-Kernel-Abdeckung aufgibt, zahlt gemessene
   Sicherheit für ungemessenen Durchsatz — ohne Lastmodell (s. §4) ist das kein Handel,
   sondern ein Wunsch.

### Was Option 2 dann schuldet (kein Freibrief, sondern zwei benannte Folgen)

Die Verengung ist nur ehrlich, wenn die Lücke einen anderen Eigentümer bekommt:

- **F1 — Kernel-Geheimnisse brauchen eine eigene Antwort.** Trusted-Key-Material und
  fremde Sidecar-Frames dürfen während laufender User-Ausführung auf dem Geschwister
  nicht lesbar residieren. Zwei Wege (Entscheidung offengehalten, Aufwand klein gegen
  Option 1): schlüssellose Kernelpfade (pro-Tenant-Material, der Kern hält nur
  öffentliche Anteile) oder Kopieren-und-Löschen-Disziplin (fremde Frames nur in
  Registern/engem Fenster, danach scrubben — die Switch-Form, die Stufe 1 gerade nicht
  braucht, aber nie verboten hat).
- **F2 — die Zeile muss sagen, was sie nicht sagt.** `smt : ALL PASS` heißt ab Stufe 3:
  „keine zwei Tenants teilen einen physischen Kern", und NIEMALS „kein Tenant teilt
  ihn mit dem Kernel". Der Wortlaut steht im Patch-Text (§5).

### Revisionsbedingung (wann diese Entscheidung fällt)

Fällt, sobald eine der drei Prämissen bricht — und nur dann:

- Die Hardware bietet billiges Geschwister-Stoppen (z. B. architektonisches
  Core-Stun ohne IPI-Roundtrip), oder
- die Kerneintrittsrate fällt um Größenordnungen (Batch-IPC, gepufferte Syscalls), oder
- F1 wird so teuer, dass Option 1 unterm Strich billiger ist.

Bis dahin ist jede Re-Diskussion ohne neue Messung nur die alte mit neuem Datum.

## 2. Stufe 2 — die Trust-Domäne benennen (nach dieser Entscheidung, nicht davor)

Die Domäne ist die **Tenant-Schließung**: Anwendungs-PD + ihre Treiber-PDs + ihre
FS-PD — alles, was denselben Tenant bedient. Der Kernel kennt heute PDs, keine
Tenants; die Domäne ist deshalb ein neues, benanntes Konzept, kein umbenanntes altes:

- **Granularität:** Tenant, nicht PD. Eine App-PD, ihre Treiber-PD und die FS-PD
  gehören zusammen (s. Z6). Wer pro PD gängt, serialisiert einen Tenant gegen sich
  selbst — der ×1,0-Fall bei vollem Hardwarepreis.
- **Herkunft:** Manifest. Die Zugehörigkeit steht im signierten Manifest (neues Feld,
  Vorschlag `tenant_id:u32`, `0` = keine Domäne). **Kopplung beachten:** Das
  Manifest-Format besitzt Strang A, die Bedeutung (Farbe, NUMA-Knoten) Strang B
  (AGENTS.md-Kopplung 1). Dasselbe Muster gilt hier: A prägt das Feld, B die
  Scheduler-Bedeutung. Kein Feld ohne beide.
- **Niemand gehört dazu:** Kernel-Threads (Idle, Verifizierer, Flusher) und
  domänenlose PDs laufen als „nobody". Ein Geschwister neben „nobody" läuft frei —
  und eine Domäne mit nur einem lauffähigen Thread zwingt ihr Geschwister in den
  Leerlauf (×1,0 bei vollem Preis). Das ist kein Fehler, sondern der dokumentierte
  Preis; wer ihn nicht zahlen will, stellt keine Ein-Thread-Domänen auf SMT-Kerne.

## 3. Stufe 3 — Geschwister einer Domäne gemeinsam einplanen

- **Mechanismus:** benannter Grund in `BlockReasons` (Z24), nie ein viertes Bit und
  nie ein stilles Überspringen. Ein Thread, der lauffähig ist und nicht laufen darf,
  ist wörtlich `sched_audit`-Code 7 — die Regression, die der D0-Umbau produziert
  hat. Der Gang meldet sich dort, wo jede andere Warteursache steht.
- **Prüfer:** zählt die **Gelegenheit**, nicht den Treffer. Bei jedem Switch-in: gehört
  das Geschwister zur selben Domäne oder zu niemandem? Ein Trefferzähler wäre bei
  einem seltenen Ereignis in fast jedem Lauf stumm (die `pdbind`-Lehre, D18-Klasse).
- **Voraussetzung, unverhandelbar:** erst das Lastmodell. Der realisierbare Anteil der
  ×1,36…×1,89 hängt vollständig am Tenant-Mix. Einen Mechanismus bauen, dessen Wert
  zwischen 0 % und 90 % liegt, ohne zu wissen wo, ist keine Technik.

## 4. Fahrplan (Reihenfolge ist Inhalt)

1. ✅ Diese Entscheidung (hier).
2. §12a-Nachtrag in `docs/invariants.md` (B-Besitz — Patch-Text, §5).
3. Manifest-Feld `tenant_id` (A-Format + B-Bedeutung, Kopplung 1 — Mitteilung an beide).
4. `BlockReasons`-Bit + Scheduler-Gang + Gelegenheits-Prüfer (Stufe 3, mit Lastmodell).
5. F1-Antwort für Kernel-Geheimnisse (eigener Eintrag, sobald Stufe 3 beginnt).

## 5. Patch-Texte (fremder Besitz, hier nur übergeben)

- **B (`docs/invariants.md` §12a):** Nachtrag „Entscheidung 2026-09-09: Option 2 —
  Tenant-gegen-Tenant; Tenant-gegen-Kernel ab Stufe 3 nicht abgedeckt, s. F1/F2" plus
  `smt`-Zeilenwortlaut F2.
- **B (`test-qemu*.sh`, `build*.sh`):** Suite-Pinning `threads=1` (s. Ergebnisbericht).
- **B (`kernel/src/arch/x86_64/bringup.rs`):** `cores`-Konjunkt + `sched`-Bericht wie
  die ARM-Seite (s. Ergebnisbericht).
- **Gemeinsam (`kernel/src/system.rs`):** `system::nohz_stand(core)`-Accessor +
  Tick-seitige Umprogrammierung in `reschedule` (s. Ergebnisbericht).

## Simon-Entscheidung 2026-09-10: §12a-Bestätigung

Option 2 bleibt gültig (Zusicherung auf Tenant-gegen-Tenant verengt). Tenant-gegen-Kernel
(F1/F2) bleibt offen und gehört zu Stufe 3, nicht in diesen Schritt.
