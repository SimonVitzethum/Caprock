# TODO0 — Linux-Userland und lokale Grafik auf Caprock

**Klasse:** Strangplan · **Stand:** 2026-08-17, **nicht begonnen** · **Zuschnitt entschieden,
Aufwand geschätzt und als Schätzung markiert.**

## 0. Das Verhältnis zu `todo.md` — bitte zuerst lesen

`todo.md` ist die Quelle für „was ist offen". **Diese Datei kopiert von dort nichts.** Wo ein
Punkt schon im Register steht, steht hier ein **Zeiger** und der Stand in einem Satz. Eigene
`[ ]`-Einträge (`K*`, `P*`, `G*`, `L*`) gibt es nur für das, was im Register **noch nicht** steht.

Der Grund ist die Falle, die dieses Projekt bereits bezahlt hat: *zwei Zahlen, die aus derselben
Hand kommen, sind keine zwei Quellen.* Zwei Listen mit denselben Punkten driften, und der Tag, an
dem sie sich widersprechen, macht die Frage „was ist offen" unbeantwortbar.

**Regel für diese Datei:** wird ein `K*`/`P*`/`G*`/`L*`-Punkt begonnen, wandert er **nach
`todo.md`** und hier bleibt der Zeiger. Diese Datei ist ein **Plan**, kein zweites Register.

---

## 0b. Wozu dieser Strang gehört

**Das Ziel steht seit 2026-08-17 in `CLAUDE.md`:** Caprock OS am Ende vollständig in Gabbro —
Kern, Treiber, Desktop —, formal verifiziert; daneben Unverifiziertes für die Nutzbarkeit
(Linux-Treiber, Binärkompatibilität). Die Zusage lautet **„das Unbewiesene kann nicht
ausbrechen"**, nicht „alles ist bewiesen".

**Für diesen Strang folgt daraus genau eine Ordnungsregel, und sie ist bindend:**

> **Erst die Einschliessung, dann der Beweis.**
> Jeder Punkt unten wird danach bewertet, ob er die **Grenze** trägt — nicht, ob er später
> beweisbar ist. Ein Linux-Treiber in einer IOMMU-begrenzten PD erfüllt das Ziel heute; ein
> Beweis über ihn wird es nie geben.

Was das konkret entscheidet: **D3** (ein abstürzendes Applet reisst nichts mit) zählt mehr als
jede Zeile Compositor-Code, und **A6/`CAP_IRQ`** zählt mehr als jede Optimierung — weil beide
Grenzen sind. Und: die spätere Gabbro-Fassung betrifft **den Compositor-Kern**, nicht den
Werkzeugkasten (s. §7b).

---

## 1. Der Zuschnitt — was dieser Strang ist und was er nicht ist

**Ziel:** ein System, das auf Caprock aufsetzt, lokal grafisch benutzbar ist (Hyprland als
Zielanwendung) und die Isolationszusage des Kerns behält. Ausdrücklich **ohne CLI-Anspruch** —
das ist eine Erleichterung, keine Lücke, s. §1.2.

### 1.1 Zwei Projekte, die die Frage vermischt

| | „Linux-**Treiber** auf Caprock übersetzt" | „**Hyprland** läuft" |
|---|---|---|
| fehlt | eine **Schicht**, die Linux-Treibercode ohne Linux ausführt | das **Userland-Substrat** |
| Stufe hier | **L** (§8) | **K**, **P**, **G** (§5–§7) |
| Präzedenz | Genode `dde_linux` (erprobt, **GPLv2** → Reibung mit AGPL) | Starnix (BSD-3) — hier aber **nicht nötig**, s. §2 |

**Sie sind unabhängig.** G (Grafik über `virtio-gpu`) braucht **kein** L. L braucht **kein** G.
Wer beides in einen Strang wirft, macht die billige Hälfte von der teuren abhängig.

### 1.2 Was der Verzicht auf CLI einspart

Kein `fork`/`exec`-Prozessmodell, keine Job-Control, keine TTYs, keine Nutzer/Gruppen, kein
`/proc`-Prozessbaum, keine Shell-Semantik. Der Verzicht ist damit der grösste einzelne
Kostensenker dieses Strangs — er streicht genau die Fläche, an der WSL1 gescheitert ist
(Verhaltenstreue bei Dateisystem und Prozessmodell).

> **Nachgemessen 2026-08-17 — und „ein Compositor braucht davon nichts" stimmte nur zufällig.**
> Hyprland importiert `fork`/`execl`/`execv` (für die `exec-once`-Direktive und **Xwayland**) und
> `dlopen`/`dlsym` (für **Plugins** und den Absturzbericht). Beides ist **kein Kernpfad** und
> abschaltbar — der Verzicht hält also, aber **erst nach einer Entscheidung**, nicht von selbst.
> `dlopen` kommt ausserdem zurück, sobald **Mesa** dazukommt: es lädt seine Treiber so.
> Fundstellen und Symbolzählung in `~/Dokumente/Caprock OS/MESSUNGEN.md` M1b.

### 1.3 Was dieser Plan NICHT verspricht

* **Kein** „beliebige Linux-Binaries laufen". Der Weg ist **neu übersetzen** (Z16), nicht
  binäre ABI-Treue.
* **Kein** CUDA. Das ist Z26/Weg A und bleibt hier aussen vor (s. §2).
* **Kein** Blech. Ziel ist QEMU, bis Stufe G steht.
* **Kein** „nur Caprock als TCB", sobald ein Compositor läuft — s. §9. Der Satz ist **nicht**
  verhandelbar und gehört in jede Aussenkommunikation dieses Strangs.

---

## 2. Die Entscheidung, die diesen Strang billig macht — sie ist schon gefallen

`todo.md`/Z26 nennt den Fall wörtlich: *„Nur Darstellung/Compositing → **Weg B**"*. Weg B ist
`nouveau` + `NVK` bzw. `i915` (**728 von 900 Dateien MIT**, gemessen) — alles Quelltext, alles
übersetzbar.

**Folge, und sie ist der grösste Kostenblock der ganzen Frage:** die **binäre Linux-ABI**
(Z26/A3 — `ld.so` für fremde ELFs, glibc-Symbolfläche) bleibt **vom kritischen Pfad**. Sie wird
nur für die proprietären CUDA-Blobs gebraucht, und die stehen in diesem Zuschnitt nicht auf der
Liste.

> **Die protokollierte Entscheidung „CUDA JA → Weg A" (2026-08-09) wird dadurch nicht
> umgekehrt.** Sie gilt für den GPU-Compute-Strang. Dieser Strang hier ist der andere Ast
> derselben Verzweigung und läuft daneben. Wer beide will, zahlt beide — aber nicht in dieser
> Reihenfolge, und nicht als ein Projekt.

---

## 3. Die Abhängigkeitsordnung — sie ist anders, als die Frage nahelegt

**Der härteste Blocker ist nicht die Grafik** — aber er ist auch nicht der, den hier zuerst stand.

> **BERICHTIGT 2026-08-17.** Hier stand: *„Eine PD hat heute EINEN Thread (0 Treffer für
> `SPAWN`/`CLONE` in der ABI)"*. Das war aus `todo.md`/Z14 übernommen und dort **seit dem
> 2026-08-10 überholt**: die Kernelhälfte von Z22 P2 ist gebaut und wird in **jedem** Suitenlauf
> als `pdthrd` gemessen (zwei Threads in PD 1, gemeinsamer Cap-Slot, offene Transaktion).
>
> **Der Blocker ist eine Ebene schmaler: es fehlt der ABI-Weg** (`SPAWN`), mit dem ein
> Userland-Programm sich selbst einen Thread erzeugt — s. K1a. Mesa, Aquamarine, smithay und jede
> JVM brauchen `pthread_create`; die Kernelmechanik darunter steht bereits.

Dass `futex` bei Hyprland ⬛ 67,8 % und bei der JVM ⬛ 96,9 % der Laufzeit trägt, bleibt davon
unberührt — es ist weiterhin die Grösse, an der die Persönlichkeit steht oder fällt.

Daraus die Ordnung. Jede Stufe ist für sich nützlich, auch wenn die nächste nie kommt — das ist
die Abnahmebedingung des Zuschnitts, nicht eine Höflichkeit:

```
K (Substrat)  →  P (Persönlichkeit)  →  G (Grafik)
                                         L (Treiberschicht)  ← unabhängig, nur für Blech
```

| Stufe | Nutzen, auch wenn der Strang hier endet |
|---|---|
| **K** | Threads je PD, `mmap`, Fault-Umleitung — jeder Punkt steht ohnehin auf dem Weg zum Server-OS (Z20) und ist für sich abnehmbar |
| **P** | eine `libcaprock`-musl trägt *jedes* aus Quelltext gebaute Programm, nicht nur Compositoren |
| **G** | `virtio-gpu`/`virtio-input` sind zwei weitere Treiber-PDs — dieselbe Form wie `virtio-blk`/`-net`, also Belege für A-5 |
| **L** | schaltet **alle** Linux-Treiber frei (WiFi, Audio, USB), nicht einen |

---

## 4. Stufe 0 — was aus `todo.md` schon gilt (Zeiger, keine Kopien)

| Punkt | Register | Stand in einem Satz |
|---|---|---|
| Syscall-Umleitungsprimitiv | Z26/A3 | **gebaut**, `redirect : ALL PASS` — aber **1,5 von 4 Autoritäten**: Gast-Speicherzugriff und Vspace-Manipulation fehlen |
| Syscall-ABIs als Module | Z28 | entschieden, nicht begonnen; Schnitt entlang des **geteilten Zustands**, nicht der Syscall-Nummer |
| `CAP_IRQ` / IRTE | B-3, Z20 | Kodierung **und** Vergabe stehen als reine Funktion (10 Host-Tests), **verdrahtet ist nichts** |
| Prozessstart-Stack, TLS, Stackgrösse, SSE | Z19/A1–A4 | A4 erledigt, A1–A3 offen — **ohne sie startet kein `main(){return 0;}`** |
| Speicher-Server in Userspace | Z14 Stufe 1 | offen, aber **ohne eine einzige neue Kernelzeile** möglich |
| Dateisystem über FAT16 hinaus | Z20 | offen; heute FAT16 über **eine** Datei |
| Desktop-Einordnung | Z20 | „zwei bis drei Grössenordnungen", **Hobbymeilenstein**, dient der Produktthese nicht |

**Diese Zeilen werden hier nicht gepflegt.** Steht dort etwas anderes, gilt dort.

---

## 5. Stufe K — das Substrat (Kernel; das einzige, was TCB kostet)

Die vier Kerneloperationen aus Z14 sind hier die Grundlage; **drei davon sind offen.** Grobe
Grösse dort: 500–800 Zeilen, ~5–8 % TCB-Wachstum. Jede ist für sich nützlich und einzeln
abzunehmen.

- [~] **K1. Mehrere Threads je PD — die KERNELHÄLFTE IST GEBAUT. Berichtigt 2026-08-17.**

      > **Dieser Eintrag war falsch, und der Fehler ist lehrreich genug, um stehen zu bleiben.**
      > Er sagte „heute hat eine PD einen Thread (0 Treffer für `SPAWN`/`CLONE` in der ABI)" und
      > machte K1 zum **härtesten Blocker des ganzen Strangs**. Übernommen hatte ich das aus
      > `todo.md`/Z14, wo es als Tabellenzeile steht — **und dort ist es seit dem 2026-08-10
      > überholt.** Die Messung stand die ganze Zeit in jedem Suitenlauf, auch in meinen eigenen
      > von heute; ich habe nie danach gegrept. *Ein Register, das Erledigtes führt, macht die
      > Frage „was ist offen" unbeantwortbar* — und dann baut jemand einen Plan darauf.

      **Gebaut (Z22 P2), gemessen in jedem Lauf als `pdthrd`:**

      ```
      pdthrd  : EINE PD, ZWEI Threads: pd(server)=Some(1) pd(client)=Some(1) (PD 1)
                gebundene-Threads=2 (erwartet 2) · erstes-RECV-Code=0
                (beide Threads sehen DENSELBEN Cap-Slot; nur einer gebunden hiesse 4 = ERR_NOPD)
      ```

      Dazu in der PD-Tabelle: `nthreads` je PD, ein **Rückwärts-Index** (tid → pd) und
      `any_thread(pd, prädikat)` als Ersatz für „nimm `thread_of(pd)` und prüfe den" — O(Threads)
      statt O(PDs × Threads), mit benanntem Rückfall auf den Vertreter, solange die Tabelle keinen
      Speicher hat. Und die Z23/S1-Tore sind **an der offenen Transaktion** gemessen, wofür es
      zwei Threads in derselben PD überhaupt erst braucht.

      **Was WIRKLICH fehlt — und es ist viel kleiner:**

- [x] **K1a. Ein ABI-Weg, mit dem eine PD sich selbst einen Thread erzeugt — GEBAUT UND GEMESSEN
      (2026-08-26). Dieser Eintrag war zuletzt in zwei Richtungen falsch.**

      > **Berichtigt 2026-08-26.** Hier stand „die ABI hat 19 Syscalls, kein `SPAWN`". Der
      > Syscall `sys::SPAWN = 20` steht seit dem **2026-08-17** in der ABI, mit
      > `spawncheck::check_stack`, sechs benannten Absagen und `ERR_INUSE`. Die Zeile war seit
      > neun Tagen ueberholt.
      >
      > **Und die andere Richtung war schlimmer:** `SYS_SPAWN` hatte bis zum 2026-08-26
      > **keinen Aufrufer und kein Gatter** — 0 Treffer im ganzen Baum ausserhalb seiner
      > Definition und eines Nummern-Ankers. Er war gebaut und **nie ausgefuehrt**. Ein Register,
      > das „fehlt" sagt, wo „ungemessen" richtig waere, schickt den naechsten Plan an die
      > falsche Stelle — dieselbe Form wie bei K1 selbst.

      **Gemessen als `arena` in JEDEM Lauf beider x86-Suiten** (arch-neutral, aarch64 faehrt sie
      mit):

      ```
      arena  : ALL PASS (fertig=true ganze-region=true vier-fenster=true disjunkt=true
                lebendig=true threads=6 slots=2 ueberlappung-abgewiesen=true
                ausserhalb-abgewiesen=true cap-gesperrt=true | maske=0xf magisch=4/4
                korrupt=0 codes: ganz=0 ueber=7 aus=21 del=18)
      ```

      Die Abnahmebedingung aus diesem Eintrag ist eingeloest, nur mit einer anderen Groesse als
      dem Staffelstab: jedes Kind legt ein aus **seiner** Fensterbasis abgeleitetes Wort ab und
      liest es weiter nach — „`spawn` gab `Some`" zaehlt nicht, und vier Threads auf einer Arena
      saehen ohne diese Zeile genauso aus, wenn sie einander zertrampeln. Der Ueberlauffall
      (`ERR_THREAD_LIMIT`, nicht blockierend) steht in `spawncheck` mit Host-Test.

- [x] **K1b. Mehrere Stapel aus EINER Cap — die Teilregion (2026-08-26).**
      Bis dahin kaufte eine `Memory`-Cap genau **einen** Thread, und sie blieb fuer dessen Leben
      gesperrt. „Wie viele Threads darf eine PD haben" war damit „wie viele Cap-Slots sind noch
      frei" — die falsche Frage: Threads einer PD teilen sich den Adressraum ohnehin, eine eigene
      Cap je Stapel kauft **keine Isolation**. Ein Treiber mit 6 von 8 belegten Slots kam auf
      zwei Threads.

      `x1` von `SYS_SPAWN` traegt jetzt `(offset_pages << 32) | length_pages`; `0` ist **die ganze
      Region** und damit bitgleich zu jedem vorher geschriebenen Aufruf. Vier Absagen mit eigenen
      Namen, darunter `ERR_SUBREGION` (ein Fenster ausserhalb der Cap ist etwas anderes als eine
      Region, die als Stapel nicht taugt).

      **Der Befund unterwegs, und er ist der wichtigere:** die `Overlaps`-Absage war fuer genau
      die Threads, die `SYS_SPAWN` erzeugt, **strukturell unerreichbar**. `pd_mapping_overlaps`
      liest `KSTACKS.ubase_of`, und `spawn_with_stack_parked` traegt dort **absichtlich** nichts
      ein (die Region gehoert der Cap, nicht dem Kernel). Der Pruefer konnte den Fall, gegen den
      er gebaut ist, nicht sehen — und mit mehreren Stapeln in einer Cap ist genau das der
      Hauptfall. Seither gibt es `stack_sibling_overlaps` daneben. Gegenproben:
      `tools/spawnarena-negativ.sh` (M1 ist woertlich der Baum von gestern).

      **Offen geblieben:** das Fenster zwischen `admit` und dem Eintrag in `STACK_CAP_OF` — zwei
      Threads derselben PD auf verschiedenen Kernen koennen beide vor dem jeweils anderen Eintrag
      pruefen. Dasselbe Fenster, das der `ERR_INUSE`-Schutz seit K1a hat; innerhalb einer PD
      dieselbe Vertrauenszone, ein Loch, sobald `SPAWN` je eine PD-Grenze ueberschreitet.

- [ ] **KB. Stufe B (`CAP_IRQ`) — geplant 2026-08-26, `docs/plan-cap-irq.md`.**
      ⬛ Der Eintrag „IRQ-Zustellung fehlt" war in **beide** Richtungen falsch, dieselbe Form wie
      K1a: die Zustellmechanik ist gebaut und wird auf aarch64 in jedem Lauf als `irq : ALL PASS`
      gemessen (RTC über GIC-SPI); die IRTE-Kodierung samt SVT/SID und Hardwarezugriff steht
      ebenfalls. Was fehlt, ist schmaler: **MSI-X-Programmierung** (`pcie.rs` kennt kein MSI),
      **ein Aufrufer** für `vtd::irte_vergib`, und ein **Cap-Riegel** an `bind_irq` — dessen
      Doku-Kommentar „über die IRQ-Cap autorisiert" behauptet, was seine Signatur nicht kann.

      Die tragende Entwurfsentscheidung steht im Plan: **der Treiber schreibt seine MSI-X-Tabelle
      selbst** (er hält die BAR-Cap ohnehin), und die IRTE-Indirektion ist der Riegel — ein Handle,
      den er nicht bekommen hat, trifft einen nicht-präsenten Eintrag oder eine fremde SID, und
      VT-d weist ab. Ohne `SVT_SID` wäre die Interruptzustellung genau der Kanal, den A-5.4 auf der
      DMA-Achse geschlossen hat.

      **Entschieden 2026-08-26 (E10-E12), alle drei kleiner als sie aussahen:** der Re-Trigger-
      Schutz ist eine **Zusicherung** statt einer gemeinsamen Implementierung (aarch64 maskiert,
      MSI ist edge — geprüft wird auf beiden gleich: zweimal auslösen vor dem Drain, höchstens eine
      Zustellung); der **Kernel** schreibt die MSI-X-Zeile und weist ein Gerät ab, dessen Tabelle in
      der angebotenen BAR liegt (gemessen: der Treiber bekommt heute genau die BAR mit der
      Common-Config, alles andere wäre ein Zufall der Auswahlregel); und die Bindung hängt an der
      **Zuteilung** statt an `NIRQ_BIND` — ein Vektor je Gerät ist bereits die Obergrenze, ein Konto
      braucht es nicht.

      **Und die Reihenfolge A→B stimmt nur halb:** die positive Richtung („der Treiber kam ohne
      eine einzige Poll-Runde voran") ist heute messbar; die Gegenprobe braucht eine Frist, aber
      **die des Messenden, nicht die der ABI**. Stufe B ist damit vor Stufe A abnehmbar. Was A
      wirklich braucht, ist das Produkt: ein Treiber muss sich von einem ausbleibenden Interrupt
      **erholen** können, und das geht ohne `ERR_TIMEOUT` nicht. **Als Schuld benannt:** *vor Stufe A
      hängt ein Treiber-PD, dessen Interrupt ausbleibt, unwiderruflich* — eine Eigenschaft des
      ausgelieferten Zustands, kein latenter Mangel. Ein Poll-Fallback als Zwischenlösung ist
      **verboten**: er unterliefe ausgerechnet `poll-runden == 0`, also das tragende Konjunkt von
      `irqmsi`. Keine Zwischenlösung zwischen B und A.

- [ ] **K1d. Das PD-lokale Präemptions-Gatter — die einzige Anforderung, die aus E3 folgt.**
      Entschieden 2026-08-26, Begründung in `docs/linux-kompatibilitaet-caprock.md` §5.

      Caprock ist **präemptiv, aber ohne Parallelität innerhalb einer PD** (`SYS_SPAWN` legt jeden
      Thread auf den Kern des Aufrufers — gemessen, nicht angenommen). Das ist
      `CONFIG_SMP=n` + `CONFIG_PREEMPT=y`, eine unterstützte Linux-Konfiguration, und ihre Antwort
      steht in `include/linux/spinlock_up.h`: `spin_lock() → preempt_disable()`, kein Spinnen.
      **Damit bleiben `spin_lock` und `rcu_read_lock` in Klasse A** — die A-Schicht braucht keine
      echten Sperren.

      **Der Zuschnitt ist die Entscheidung, nicht das Ob:**
      * **kein globaler Scheduling-Override** — das wäre ein System-DoS aus einem Treiber-PD, also
        genau die Autorität, die eine PD nicht haben darf;
      * es bedeutet *nicht auf einen anderen Thread **derselben PD** umschalten*. Verdrängung durch
        eine fremde PD ist unschädlich, weil kein Zustand geteilt wird;
      * **gedeckelt durch das verbleibende SC-Budget**, sonst ist „gehalten" unbegrenzt und es
        entsteht ein neues Fehlerbild statt eines benannten (D11-Form).

      **Abnahme:** zwei Threads derselben PD, einer hält das Gatter und schreibt einen
      Zwei-Wort-Zustand, der zwischendurch inkonsistent ist; der andere liest ihn in einer Schleife.
      Ohne Gatter muss der Leser die Inkonsistenz **sehen** (Positivkontrolle — ein Test, der sie
      nie sieht, misst nichts), mit Gatter nie. Dazu: ein Thread einer **fremden** PD verdrängt
      weiterhin (sonst ist es doch ein globaler Override), und das Gatter fällt beim
      Budget-Ende von selbst.

      **Voraussetzung, die mit der Entscheidung steht und fällt:** sobald `SYS_SPAWN` eine Kernwahl
      bekommt, gilt `CONFIG_SMP=y` und `spin_lock` verlässt Klasse A. Die Kernwahl gibt es heute
      nicht — wer sie einführt, muss diesen Eintrag mit aufmachen.

- [ ] **K1c. `NCAPS = 16` ist der ECHTE Deckel je PD — nicht das Budget.**
      Am 2026-08-26 gefunden, und zwar vom Selbsttest, nicht beim Gegenlesen: `CAP_BUDGET_MAX`
      stand auf 64, der **lokale Cspace einer PD ist aber ein Array von `NCAPS` = 16 Slots**
      (`Pd::cspace`). `install_cap` weist jeden Slot `>= NCAPS` ab, **ohne das Budget je zu
      fragen** — ein Budget von 20 war keine grosszuegige Zusage, sondern eine unerfuellbare.
      `CAP_BUDGET_MAX` ist seither `NCAPS`, mit `const _: () = assert!` daneben.

      Damit steht die Zahl, die eine Treiberumgebung wirklich begrenzt: **16 Slots je PD, hart.**
      Das Budget-Konto (s. A3) hebt die erreichbare Zahl von 8 auf 16, ohne die anderen
      zehntausend PDs etwas zu kosten; darueber hinaus braucht es einen **variablen** Cspace je
      PD. Das ist eine eigene Aenderung und beruehrt `Pd`, `PdTable` und jede Stelle, die
      `[Option<CapPtr>; NCAPS]` als Wert herumreicht (`caps_of`).

      **Vier Entscheidungen stehen davor, und keine ist beliebig:**
      1. **Woher der Stack?** Aus einer Cap des Aufrufers (dann trägt er die Kosten und die
         Politik bleibt draussen) oder aus dem Kernel (bequem, aber eine zweite Speicherpolitik).
      2. **Die Obergrenze je PD — und ihr NAME.** *Wer eine Kapazität einführt, muss den Überlauf
         benennen* (D11 wörtlich). Ohne eigenen Fehlercode ist eine Schranke kein Schutz, sondern
         ein Loch — und ein Programm, das unbegrenzt Threads erzeugt, ist ein DoS-Kanal wie
         `AUFTRAEGE_MAX` bei C8.
      3. **Zulassungsreihenfolge.** `spawn_parked` → Stack/TLS setzen → `bind_pd` → `admit`. Ein
         Thread, der lauffähig ist, bevor er seine Autorität hat, ist **D0 wörtlich**.
      4. **TLS-Basis je Thread** (Z19/A2) — ohne sie ist `errno` in musl kaputt, also die libc
         als solche, nicht „Threading".

      **Abnahme** (unverändert gültig): ein Staffelstab, den **beide** Threads abwechselnd
      fortschreiben müssen, plus ihre **eigenen** Verdrängungszahlen — *„`spawn` gab `Some`"
      zählt nicht.* Gegenprobe: ein Thread stillgelegt → der Staffelstab steht. Dazu ein
      Überlauffall, der den benannten Fehlercode bekommt **und nicht blockiert** (D11).
      **TCB:** ja — ein Syscall.

- [ ] **K2. Mappen/Entmappen in eine FREMDE PD.**
      `SYS_MAP` bildet heute nur in die **eigene** VSpace ab. Ohne das gibt es kein `mmap`, kein
      `mprotect`, keinen Lader für nachgeladene Bibliotheken — und keinen Speicher-Server, der
      seinem Klienten etwas *einblendet* statt eine Cap zu reichen.

      **Abnahme.** PD A bildet eine Region in PD B ab; B liest, was A geschrieben hat; **PD C
      sieht sie nicht** (Positivkontrolle über denselben Pfad, nur eine Cap wandert — dieselbe
      Form wie A-5.4). **TCB:** ja, cap-gated wie `PDCTL`.

- [ ] **K3. Fault-Umleitung an eine PD.**
      Heute **beendet ein Fault den Thread im Kern**. Gebraucht für COW, Demand-Paging und
      `MAP_SHARED`. Z14 nennt es zurecht „der erste Schritt zu einem Pager in Userspace".

      **Abnahme.** Ein Gast faultet auf eine nicht abgebildete Seite, ein Handler in einer
      anderen PD bekommt Adresse und Zugriffsart, bildet ab, der Gast **läuft weiter und sieht
      seinen Wert** — dieselbe Abnahmeform, mit der die Nutzlast des Umleitungsprimitivs belegt
      wurde. Sprechprobe: ohne Handler-Cap muss der Thread wie bisher sterben, und die Zeile muss
      das **unterscheiden** können. **TCB:** ja.

- [ ] **K4. Gast-Speicherzugriff für Handler (Nachtrag 2 zu Z26/A3, hier nur verzeigert).**
      Ohne ihn ist **jeder Syscall mit Zeiger** nicht implementierbar — `read`, `write`, `ioctl`.
      Steht im Register; hier als Vorbedingung von P notiert, damit die Kette vollständig ist.

- [ ] **K5. Zeit als Schnittstelle.**
      Ein Compositor ist durch und durch zeitabhängig (Bildtakt, Eingabezeitstempel, Animation).
      Zyklenabrechnung existiert (`caprock-sched/src/cycles.rs`); eine **Uhr** für Userland nicht.
      Z19/B nennt `clock_gettime` als vDSO-Seite — der billigste Weg, weil er den Kern nicht
      anfasst.

      **Abnahme.** Monotonie über 10⁶ Abfragen, und die **Auflösung** als Zahl im Bericht (nicht
      „funktioniert"). Untergrenze vorab, sonst ist Null grün. **TCB:** eine geteilte, nur
      lesbare Seite.

---

## 6. Stufe P — die Persönlichkeit (Userland; TCB-neutral)

**Grundsatz:** kein Kernelwachstum. Alles hier ist eine PD.

- [ ] **P1. `libcaprock`-musl.** Z16-Linie: derselbe Quelltext, **neu gelinkt**. Setzt Z19/A1–A3
      voraus (`argc`/`auxv`, TLS, ein Stack, der nicht 16 KiB ist — üblich sind **8 MiB**, und
      das gehört ins Manifest, nicht in eine Konstante).

      **Abnahme.** Ein unverändertes Fremdprogramm aus Quelltext (Vorschlag: `zlib`-Selbsttest,
      weil abhängigkeitsarm und mit eingebautem Urteil) baut und besteht.

      **Die Umfangsmessung ist gefahren — aber in ZWEI Fassungen, und nur die zweite gilt hier.**
      `strace -c -f` ergab **84 distinkte Syscalls** und beantwortet das Breitentor
      („Persönlichkeit oder etwas Kleineres?", Schwelle 150). Für den **Neuübersetzungsweg** ist
      es die falsche Grösse: neu gelinkt setzt Hyprland keinen Linux-Syscall ab, es ruft libc.
      Massgeblich ist die **Symbolfläche**: ⬛ **1 069 undefinierte Symbole, davon 139 aus libc
      und 140 aus libstdc++**. Diese 139 sind der Umfang von P2, nicht die 84.

- [ ] **P2. Die fünf tragenden Syscalls einer Wayland-Ereignisschleife.**
      `futex`, `epoll`/`poll`, `eventfd`, `timerfd`, `memfd` (Wayland-shm nutzt `memfd` mit
      Sealing). Nach Z28 fallen sie in die Gruppen *Synchronisation* (klein) und *fd/Dateien*
      (**gross**) — der Schnitt folgt dem geteilten Zustand.

      **Abnahme.** Je Syscall ein Fall, der ihn **braucht** und ohne ihn nachweislich hängt
      (Gegenprobe mit genau einem offenen Konjunkt, Hausform).

- [ ] **P3. Unix-Sockets mit fd-Übergabe — und das ist strukturell billig.**
      **Wayland *ist* `SCM_RIGHTS`**: jeder Puffer, jedes dmabuf wandert als fd über den Socket.
      Ein fd zu übergeben ist **eine Cap zu übergeben**, und Cap-Transfer im IPC steht seit
      langem. Das ist der eine Punkt, an dem Caprocks Modell dem Ziel *entgegenkommt* statt im
      Weg zu stehen.

      **Abnahme.** Zwei PDs, ein Socket, ein Puffer wandert; die empfangende PD schreibt hinein,
      die sendende liest es. **Und eine dritte PD, die denselben Socket nicht hat, sieht nichts.**

- [ ] **P4. VFS mit Pfaden und Gerätknoten.**
      Gebraucht: `/dev/dri/*`, `/dev/input/*`, `/sys/class/drm` (udev-Enumeration), plus ein Ort
      für Mesas Shadercache. Heute: FAT16 über **eine** Datei. Z20 führt das als „gross, aber
      abhängigkeitsfrei wie `caprock-part`/`-fat`" — die Bauart steht also, der Umfang nicht.

      **Abnahme.** Ein Programm öffnet einen Pfad, den es nicht kennt, über eine Enumeration —
      nicht über eine einkompilierte Konstante. Sonst prüft der Test die Konstante.

- [ ] **P5. Speicher-Server (Z14 Stufe 1, hier nur verzeigert).**
      Ohne neue Kernelzeile möglich, Abnahme steht dort. Vorbedingung für `malloc`.

---

## 7. Stufe G — Grafik, und der Befund, der im Register fehlt

### G0. Der Abkürzungsbefund: **DRM muss nicht emuliert werden**

Hyprlands Hardwarezugriff geht nicht direkt an DRM, sondern durch **Aquamarine** (früher
`wlroots`) — eine **Backend-Abstraktion** mit austauschbaren Rückseiten (DRM, verschachteltes
Wayland, headless). Ein **natives Caprock-Backend** ersetzt die gesamte `/dev/dri`-`ioctl`-Fläche
durch eine Cap-Schnittstelle mit rund einem Dutzend Operationen.

**Warum das gross ist:** `ioctl` ist kein Syscall, sondern eine **unbegrenzte API je Treiber**.
Wer DRM emuliert, emuliert Modesetting, Atomic-Commit, GEM, dma-buf, `drm_syncobj` und
Vblank-Ereignisse — eine Fläche ohne Rand. Wer ein Backend schreibt, implementiert **eine**
benannte Schnittstelle. Dieselbe Bewegung wie A-5.1: die Politik wandert in eine PD, statt dass
der Kern eine fremde Fläche nachbaut.

> **Damit fällt Weg A doppelt weg** — nicht nur für CUDA, sondern auch für die Grafik-`ioctl`s.

**ENTSCHIEDEN am 2026-08-17, gemessen** (Protokoll: `~/Dokumente/Caprock OS/MESSUNGEN.md` M2).
`IBackendImplementation` hat **16 rein virtuelle Methoden in EINEM Header**. Die vorhandenen
Implementierungen geben die Kostenskala direkt her: **Null 89 · Headless 264 · Wayland 887 ·
DRM ~4 526** Zeilen. **Gegenprobe an einem unabhängigen Baum:** wlroots' `backend/headless/` ist
**246** Zeilen — praktisch dieselbe Zahl bei anderem Code, anderer Sprache, anderem Projekt.

> **Zwei unabhängige Quellen sagen: ein minimales Backend kostet rund 250 Zeilen.** Eingespart
> wird die DRM-Fläche von ~4 500 Zeilen — und, wichtiger, deren Charakter.

**G0 ist damit keine Hypothese mehr.** Die Abbruchbedingung in §11 dazu ist erledigt.

- [ ] **G1. `virtio-gpu` und `virtio-input` in `crates/caprock-virtio`.**
      Dasselbe `Transport`+`Queue`-Gerüst, das `blk` und `net` schon tragen; dasselbe QEMU-Gerüst,
      in dem beide Suiten laufen. **Die billigste Zeile dieses ganzen Plans**, weil sie nichts
      Neues erfindet.

      **Abnahme, und sie muss der Hausregel genügen** — *ein Schreiber, der sein eigenes Ergebnis
      bestätigt, bestätigt nichts.* Der Rahmen wird **nicht** vom Treiber zurückgelesen, sondern
      über QMP `screendump` aus QEMU geholt und **ausserhalb** gegen ein erwartetes Muster
      geprüft (anderes Werkzeug, Muster dort noch einmal hingeschrieben statt importiert — wie
      `tools/checkfat.py`).

- [ ] **G2. Weiches Compositing zuerst.** Pixman statt Mesa. Langsam, aber es trennt „das Bild
      steht" von „die GPU rechnet" — zwei Aussagen, die sonst gemeinsam durchfallen und dann
      nicht auseinanderzuhalten sind.

- [ ] **G3. Der Compositor — und die Wahl ist am 2026-08-17 GEDREHT.**
      Nicht Hyprland zuerst, sondern **smithay** (Rust). Gemessen: **alle** C-Bindungen sind
      optionale Cargo-Features, `wayland-rs` läuft vorgabemässig in reinem Rust, und `smallvil`
      ist ein vollständiger Compositor in **1 373 Zeilen**. Caprock übersetzt Rust für ein
      eigenes Ziel bereits — Cargo-Crates gegen **141 C-Bibliotheken** ist nicht dieselbe Art
      Arbeit. Begründung und Gegenrede: `~/Dokumente/Caprock OS/COMPOSITOR-WAHL.md`.
      **Hyprland bleibt Stufe 2** (Aquamarine-Backend, 264–887 Zeilen Präzedenz) — wenn die Frage
      von „läuft es" auf „ist es benutzbar" wechselt.

- [ ] **G4. Mesa.** Gross, aber **Userland** — also die gute Sorte gross: es lebt in einer PD und
      ist keine TCB. Braucht dmabuf-Semantik (→ P3), shm (→ P2), `dlopen` und einen Cache-Pfad
      (→ P4).

### G5. Was der Demo-Pfad NICHT braucht — meine Einschätzung, nicht gemessen

`todo.md` nennt drei unbedingte Vorbedingungen (**`CAP_IRQ`**, **grosse/zusammenhängende DMA**,
**Firmware aus einem Dateisystem**). Die gelten für eine **echte GPU auf Blech**. Für das erste
Bild unter QEMU gilt nach meiner Einschätzung:

| Vorbedingung | für G1–G3 nötig? | Begründung |
|---|---|---|
| `CAP_IRQ` | **nein** | `virtio-gpu`/`-input` sind pollbar, genau wie `blk`/`net` es heute sind |
| grosse DMA | **nein** | ein Framebuffer in QEMU ist mit vorhandenen Mitteln zu decken |
| Firmware | **nein** | `virtio-gpu` lädt keine |

**Das ist eine Einschätzung und gehört gemessen, bevor darauf geplant wird.** Sobald echtes Blech
oder Beschleunigung dazukommt, gelten alle drei wieder unverändert — dann ist `CAP_IRQ` für eine
GPU nach eigener Registeraussage „nicht gangbar" ohne.

---

## 7b. Stufe D — der eigene Desktop (Zuschnitt entschieden 2026-08-17)

**Zuschnitt:** stellt Fenster dar · Taskbar · KDE-artiges Aussehen · Miniprogramme wie KDE ·
**kein** Dateimanager und dergleichen · **die Miniprogramme laufen unabhängig vom Desktop** ·
Wayland als Protokoll. Vollständige Herleitung mit allen Messwerten:
`~/Dokumente/Caprock OS/EIGENER-DESKTOP.md`.

**Der Bauplan existiert und ist gemessen: COSMIC** (System76, Rust, auf smithay) — cosmic-comp
⬛ 68 537 · cosmic-panel ⬛ 19 212 · 15 eigenständige Applet-Crates ⬛ 20 830 · libcosmic ⬛ 60 289.
Lizenzen GPL-3.0-only bzw. MPL-2.0, **mit AGPL vereinbar**.

- [ ] **D1. Leiste über `wlr_layer`, Taskbar über `foreign_toplevel_list`.**
      Beide Protokolle sind in smithay ⬛ **vorhanden** — Taskbar und Leiste sind auf
      Protokollebene gelöste Probleme, es fehlt das Zeichnen.
      **Abnahme:** ein zweites Fenster erscheint **in der Liste**, nicht nur auf dem Schirm.

- [ ] **D2. Ein Miniprogramm als eigene PD.** ⬛ Vorbild `cosmic-applet-time`: **887 Zeilen**
      (battery 2 082, audio 2 131).
      **Abnahme:** es tickt — **und es hat keine Cap auf Platte oder Netz**, nachgewiesen durch
      einen Versuch, der fehlschlägt.

- [ ] **D3. Die Zeile, die den ganzen Zuschnitt rechtfertigt.** Ein Applet wird absichtlich zum
      Absturz gebracht; Leiste, Compositor und alle übrigen Applets laufen **nachweislich**
      weiter — am Rundenzähler, nicht am Augenschein.
      **Das ist der Test, den KDE strukturell nicht bestehen kann:** Plasmoids sind QML **in**
      `plasmashell`. `cosmic-panel` dagegen legt selbst ein `wayland_server::Display` an
      (⬛ `main.rs:315`) — die Leiste ist ein verschachtelter Compositor, jedes Applet ein eigener
      Prozess. **Auf Caprock ist das keine Bauweise, sondern die Vorgabe.**

- [ ] **D4. Aussehen.** ⬜ Der billigste Punkt der Liste — Farben, Schriften, Ecken, Anordnung
      sind bei `iced`/`libcosmic` eine Themendatei. Teuer sind Fensterverwaltung, Eingabe und
      Bildtakt, und die sieht niemand.

**Kostenrahmen ⬜** (aus den Vorbildern hergeleitet, ausdrücklich Schätzung): Compositor
10 000–20 000 · Backend **250–900** (⬛ Präzedenz) · Leiste+Taskbar 3 000–8 000 · je Applet
1 000–2 000 · Werkzeugkasten **0 geschrieben**. **Grössenordnung selbst geschrieben:
15 000–30 000 Zeilen Rust** — gegen ⬛ kwins **356 922**.

**Was ausdrücklich fehlt und warum das trägt:** kein Dateimanager, keine Einstellungs-App, kein
Terminal, kein Browser. ⬛ 227 Prozesse und 530 Pakete einer KDE-Sitzung sind fast vollständig
**Anwendungen**, nicht Desktop. Wer sie weglässt, lässt 95 % der Arbeit weg und behält die Aussage.

---

## 8. Stufe L — die Linux-Treiberschicht (unabhängig; nur für Blech)

- [ ] **L1. Bewertung vor Bau.** Genodes `dde_linux` ist der Präzedenzfall — erprobt, mit
      Intel-Grafik. **Aber GPLv2**, also Reibung mit AGPL: eine eigene Schicht wäre nötig, kein
      Übernehmen. Z20 sagt das Entscheidende: *„Wer den Desktop will, baut nicht i915, sondern
      `dde_linux`"* — die Schicht ist die Investition, die Treiber sind danach billig, und sie
      schaltet **alle** frei (WiFi, GPU, Audio, USB).

      **Was die Schicht mindestens tragen muss:** `kmalloc`/`vmalloc`, die DMA-API
      (`dma_alloc_coherent` → Caprocks IOVA-Fenster, **strukturell passend**), IRQ-Registrierung
      (→ braucht `CAP_IRQ`), Workqueues/Tasklets/Softirq, schlafende **und** Spin-Sperren, Timer,
      das PCI-Probe-Modell.

      **Abnahme der Bewertung** (nicht des Baus): ein **gemessener** Zeilenumfang für genau einen
      Zieltreiber und die Liste der Subsysteme, an denen er hängt — nicht eine Schätzung aus dem
      Gedächtnis. Dieselbe Form wie die Z26-Messung, die die Erwartung gedreht hat.

---

## 9. Die TCB-Aussage, die mitgeliefert werden muss

**„Nur Caprock als TCB" ist in dem Moment falsch, in dem ein Compositor läuft — und zwar nicht
wegen Caprock.** Ein Compositor sieht die Pixel *jedes* Fensters. Hyprland sind sechsstellig viele
Zeilen. Das ändert kein Kernel.

Die tragfähige Fassung ist feiner und stärker:

> **Die TCB ist je EIGENSCHAFT verschieden.**
> *Isolation zwischen PDs* → der Kern allein.
> *Verfügbarkeit der Grafik* → Kern + Treiber-PD.
> *Vertraulichkeit des Bildschirms* → Kern + Compositor.
>
> **Ein abstürzender Grafiktreiber reisst seine PD ab und nicht das System** — bei Linux ist ein
> GPU-Treiberfehler ein Kernelfehler. Das ist der echte Vorteil, und er bleibt.

`todo.md`/Z14 führt diesen Satz bereits für den Linux-Mandanten (*„wer ‚kleine TCB' als
Produktversprechen führt, muss diesen Satz mitliefern, sonst ist er unehrlich"*). **Für den
Compositor steht er dort noch nicht und gehört ergänzt.**

---

## 10. Schwellen, die VORAB stehen

Nach Hausregel: eine Schwelle nach der Messung ist keine Schwelle. Diese hier stehen vor dem
ersten Lauf und dürfen nur mit Eintrag geändert werden.

| Grösse | Schwelle | Begründung |
|---|---|---|
| umgeleiteter `getpid` | **≤ 2000 Zyklen** | steht schon im Register (verankert an Z18) — hier nur übernommen, nicht neu erfunden |
| Threadwechsel in derselben PD (K1) | **≤ Kosten eines PD-Wechsels** | ein Wechsel ohne Adressraumwechsel darf nicht teurer sein als einer mit |
| Bildrate G2 (weich, 1280×720) | **≥ 10 fps** | Untergrenze, nicht Ziel — sie trennt „läuft" von „steht", und ohne Untergrenze ist **Null grün** |
| verschiedene Syscalls, die Hyprland ruft (P1) | **keine** — die Zahl **ist** das Ergebnis | **gemessen 2026-08-17: 84** (Hauptprozess), 119 (ganze Sitzung). Abbruchschwelle 150 **nicht gerissen** |

**Jede einseitig verglichene Grösse braucht eine Plausibilitätsuntergrenze** — oder Null muss
ausdrücklich als „nicht gemessen" ausscheiden. Das ist die F1-Lehre und gilt hier für jede Zeile.

---

## 11. Abbruch- und Umkehrbedingungen

* ~~**G0 widerlegt**~~ — **erledigt 2026-08-17: bestätigt**, mit Gegenprobe an einem zweiten Baum.
* ~~**P1 misst > ~150 verschiedene Syscalls**~~ — **erledigt 2026-08-17: 84 gemessen.** Der
  Zuschnitt „Compositor statt Linux-Persönlichkeit" hält seiner eigenen Prüfung stand.
* ~~**M6**~~ — **gefahren 2026-08-17, BESTANDEN.** Bau mit `--no-default-features --features
  wayland_frontend,desktop` läuft in 17 s durch: **62 Crates, 0 undefinierte `wl_*`-Symbole
  (libwayland wird nicht verlinkt), 0 `dlopen`, 0 C-Übersetzeraufrufe, 7 undefinierte C-Symbole**
  (`ceil floor memcmp memcpy memmove memset raise`), **`unsafe` 461 → 53 im gebauten Teil (11 %)**.
  Gegenprobe: mit `renderer_pixman` ist C wieder im Baum — die Feature-Grenze trennt wirklich.
  Damit ruht Stufe 1 auf einem Bau statt auf einer Lesart. **Offen bleibt M7** (reiner
  Rust-Renderer; `pixman` bindet C).
* **Die Hülle wächst statt zu schrumpfen**: die „61 statt 141" sind geschätzt. Bleiben es über
  ~100, ist der Zuschnitt ein Distributionsprojekt und keine Portierung.
* **K1 wächst über ~800 Zeilen Kernel**: dann ist das TCB-Versprechen berührt und die
  Entscheidung gehört vor den Bau, nicht danach.
* **Kein Abbruch wegen Aufwand allein.** Jede Stufe ist für sich nützlich (§3) — endet der Strang
  nach K oder P, ist nichts verloren, was nicht ohnehin auf dem Weg zum Server-OS steht.

---

## 12. Was dieser Plan an offenen Fragen HAT — und nicht versteckt

- [x] ~~**G0 ist ungeprüft**~~ — **gefahren 2026-08-17, bestätigt** (16 Methoden; 264/246 Zeilen
      aus zwei unabhängigen Bäumen).
- [ ] **§7/G5 ist eine Einschätzung**, keine Messung. `CAP_IRQ` könnte für flüssige Eingabe doch
      nötig sein — Pollen bei Mauszeigerbewegung ist eine Latenzfrage, und Latenz habe ich nicht
      gemessen.
- [ ] **Die Messreihe hat drei Posten aufgedeckt, die in diesem Plan fehlten** (Einzelheiten in
      `~/Dokumente/Caprock OS/KOMPONENTEN.md`):
      **(a) Text und Schrift** — 14 unvermeidbare Bibliotheken (freetype, harfbuzz, pango, cairo,
      ICU …), der zweitgrösste Block nach der libc, und er kommt in **keiner** bisherigen
      Caprock-Planung vor;
      **(b) die Werkzeugkette** — 141 C-Bibliotheken bauen heisst meson + cmake + autotools +
      `pkg-config` + `dlopen`, je Bibliothek eine eigene Bauart;
      **(c) `xkbcommon`** — ohne Tastaturbelegung keine Zuordnung Scancode→Zeichen, und sie
      entfällt bei keinem Backend.
- [ ] **Nebenbefund mit Folgen für Z26: die Maschine hat ZWEI GPUs** (`Intel Raptor Lake-S UHD`
      neben der `NVIDIA GB206M`). Z26 ist vollständig um die NVIDIA geplant. Für
      Darstellung/Compositing ist die **Intel-iGPU** der billigere Weg — Z26 nennt i915 selbst mit
      728 von 900 Dateien MIT. Ändert die Entscheidung nicht, die **Ausgangslage** schon.
- [ ] **Die Zeilenumfänge fehlen vollständig.** Dieser Plan nennt **keine** Aufwandszahlen ausser
      denen, die aus `todo.md` übernommen sind. Das ist Absicht: erfundene Zahlen in einem Plan
      sind teurer als fehlende, weil sie später als gemessen gelesen werden.
- [ ] **Der Compositor-TCB-Satz gehört nach `todo.md`/Z14** (§9) — hier steht er, dort fehlt er.

---

*Erstellt 2026-08-17. Diese Datei ist ein Plan und kein Register. Sobald ein Punkt begonnen wird,
wandert er nach `todo.md`.*

## 13. Stufe T — NATIVE Caprock drivers instead of Linux ones (decided 2026-08-17)

**The instruction: write native drivers for the local hardware — Ethernet, WLAN, USB, graphics —
rewriting rather than porting, and adapting only where recompiling does not suffice.**

Two measurements were taken before planning, and together they overturn the obvious order.

### The hardware that is actually here (2026-08-17, `lspci -nn`)

| Class | Device | Linux driver |
|---|---|---|
| Ethernet | Realtek RTL8111/8168 `[10ec:8168]` | `r8169` |
| WLAN | MediaTek **MT7925 802.11be** `[14c3:7925]` | `mt7925e` |
| Graphics | Intel Raptor Lake-S UHD `[8086:a78b]` **and** NVIDIA **GB206M / RTX 5070** `[10de:2d18]` | `i915`, `nvidia` |
| USB | Intel 700-series **xHCI** `[8086:7a60]` | `xhci_hcd` |

### What QEMU can emulate — i.e. what is testable before metal boots Caprock

`e1000`, **`e1000e` (82574L)**, **`igb` (82576)**, `rtl8139`, `tulip`, `pcnet`, `vmxnet3`,
`virtio-net` · **`qemu-xhci` / `nec-usb-xhci`** · `bochs-display`, `ramfb`, `VGA`, `virtio-gpu`.

**Not emulable: RTL8168, MT7925, Intel Xe, NVIDIA Blackwell.**

### The verdict, per class — and it differs per class

| Class | Native writable? | Testable before metal? | Call |
|---|---|---|---|
| **Ethernet** | yes, medium | **only for a chip QEMU has** | **do it — but pick `igb`/`e1000e`, not RTL8168** |
| **USB (xHCI)** | yes, large but bounded — the spec is **public** | **yes** (`qemu-xhci`) | do it, after Ethernet |
| **Graphics: scanout** | yes, small (GOP framebuffer) | **yes** (`bochs-display`/`ramfb`) | do it — and it is *enough for a desktop* |
| **Graphics: 3D** | **no** — Xe and Blackwell, both firmware-gated | no | an honest **never**, not a "later" |
| **WLAN** | **effectively no** — signed firmware, an 802.11be MLME stack, regulatory, crypto | **no emulation exists** | keep the contained Linux-driver PD |

### The premise that has to be corrected first

> *"…oder passe an, wenn auf Caprock kompilieren nicht reicht"*

For **drivers, recompiling was never an option at all** — not "not quite enough". A Linux driver is
not a program that makes syscalls; it is a plugin into `net_device`/NAPI/`sk_buff`, or into
DRM/GEM/TTM. The graphics measurement of 2026-08-17 put a number on it (**461 DRM ioctls**, and no
compatibility layer emulates DRM); the network side has the same shape. Recompiling is the path for
**applications**, and it was never the path for drivers.

So the real choice per device is only ever: **native driver**, or **contained Linux driver in a PD**
— and the second is exactly what §0b already blesses (*"das Unbewiesene kann nicht ausbrechen"*).

### Why Ethernet first, and why not the Realtek in this machine

The socket already exists and is measured: A-5.1 put a driver **outside the kernel** as a
replaceable service, A-5.3 moved device assignment into the **manifest**, A-5.4 measured that one
driver PD's device cannot reach the other's DMA region, and `crates/caprock-virtio` already carries
a working `net` driver. A native NIC driver is **slotted into an existing socket**, not the socket.

But a driver for the RTL8168 in this laptop **cannot be tested at all** until metal boots Caprock —
QEMU has no model for it. `igb`/`e1000e` can be developed and regression-tested in the suite *and*
exist on real server boards (i210/i350/X550 are the igb/igc family). That is the same rule that
deferred CAT/MPAM: *buildable but not verifiable is not a starting point.* The Realtek driver is the
**second** one, written once the shape is proven, and it will be the first thing that needs metal.

### Order

- [ ] **T1 — native `igb`/`e1000e` NIC driver as a PD.** Ring setup, descriptors, MMIO, link state,
      RX/TX. Reuses the DMA-region and manifest-assignment machinery unchanged. Testable in the
      suite from day one against `-device igb`.
- [ ] **T2 — scanout.** `bochs-display`/`ramfb` under QEMU, EFI GOP on metal. No GPU driver, no 3D
      — a linear framebuffer is enough for the compositor of Stufe D, and it removes the single
      biggest dependency of the graphics strand.
- [ ] **T3 — xHCI.** Public specification, emulated by QEMU. Brings HID (keyboard/mouse — which the
      desktop needs) and mass storage.
- [ ] **T4 — RTL8168.** After T1, and the first driver that can only be finished on metal.
- [ ] **WLAN stays contained.** Not a native driver. If wireless has to be native later, the honest
      route is a USB dongle with a documented chip **after T3**, not MT7925.

### The threshold that decides whether T1 was worth it

`igb` must move real frames through a driver PD with the **IOMMU on**, with the same evidence
A-5.2/A-5.4 demanded of virtio: a receive path that proves the device *read* our memory (not just
wrote to it), a counter that moves, and a negative case where the device is denied and the transfer
demonstrably does not happen. A NIC that "initialises" is not a NIC that works — *jede gezählte
Einheit muss eine Arbeit nachweisen, die nur ein laufender Träger leisten kann.*

### 13a. The reordering: T2 comes BEFORE T1 — measured 2026-08-17

**Question asked: what would booting Caprock on real metal verify?**
**Measured answer: today, nothing — the run would be silent, and would probably not reach ACPI.**

Four independent facts, each sufficient on its own:

| # | Fact | Where |
|---|---|---|
| 1 | The machine is **UEFI 64-bit** (efivars present); a modern Lenovo with an RTX 5070 has no CSM to fall back on | `/sys/firmware/efi` |
| 2 | Caprock boots via **Multiboot 1** — a BIOS-era protocol | `.multiboot` section, `MultibootInfo` |
| 3 | The RSDP is found by **scanning the legacy BIOS area** (EBDA pointer at `0x40E`, then `0xE0000..0x100000`). Under UEFI the RSDP lives in the EFI configuration table; those addresses are typically empty | `x86_64/acpi.rs:39` |
| 4 | The console is **COM1 port-I/O only** (`0x3F8`) — and this machine has **no UART** behind it (`/sys/class/tty/ttyS0/type = 0`, `iomem_base = 0x0`) | `x86_64/console.rs` |

Fact 4 is the decisive one and it is the project's own rule turned on the project: **a checker that
cannot speak has not tested anything.** Every report line, every `ALL PASS`, the whole `all_done()`
gate — all of it goes out of a serial port that does not physically exist here.

**Therefore T2 (scanout) is not a graphics feature. It is the precondition for every metal
measurement**, and it moves ahead of T1. A framebuffer is the only output path on this machine that
does not itself require a driver we have not written (USB-serial needs xHCI = T3, writing results to
disk needs an NVMe driver — both circular).

- [ ] **T0 — make a metal boot able to SPEAK.** Three pieces, and the second and third fall out of
      the first: a **Multiboot2** header alongside the existing MB1 one (GRUB-EFI boots `multiboot2`
      from UEFI), the **RSDP from the MB2 ACPI tag** instead of the legacy scan, and a
      **framebuffer console** from the MB2 framebuffer tag. Until this exists, no metal run can
      report anything, and nothing below is reachable.

#### What metal verifies that QEMU structurally CANNOT — the actual reason to boot it

Not drivers. The isolation claims:

- **A1 cache colouring — the whole claim is unverified.** Already recorded in `CLAUDE.md`: as a
  guest the host rewrites the colour bits, measured `disjunkt=234` against `gleichfarbig=210`, i.e.
  **no protection at all**. Everything `color : ALL PASS` says today is a statement about
  bookkeeping, not about caches.
- **B-4.5 Prime+Probe is `SKIP` for a structural reason** — an undisturbed chain link cost 0 cycles,
  below the lower bound of 3, because that measures the emulator's instruction count and not a
  cache. On metal it produces real latencies for the first time.
- **Z6/SMT: the channel, not the policy.** `tools/smt-messen.sh` proves a sibling is recognised and
  suppressed; whether that removes a side channel is not observable under QEMU, and this machine has
  2-way SMT.
- **IOMMU with real RMRRs.** q35 has **0**; on real hardware (legacy USB, BMC, graphics) they are
  the normal case, so the RMRR exclusion path has never run against a real one.
- **D13** — whether the leftover deviations are really measurement-stand artifacts (3.2× vCPU
  overcommit) or something in the kernel.

NUMA is **not** on this list: this machine has one node.

### 13b. T0 in progress — the two pure halves are built and host-tested (2026-08-17)

kexec settles the boot problem: **`kexec-tools` 2.0.32 supports `multiboot-x86` and
`multiboot2-x86`** (measured), so Caprock is loaded straight out of a running Linux — no GRUB, no
UEFI stub, no CSM. What kexec does *not* do is discover ACPI or the display. Linux already knows
both, so the launcher passes them on the command line — the same mechanism Linux itself uses for
kexec under EFI (`acpi_rsdp=`).

**Landed, and host-tested (21 tests):**

| Module | What | Tests |
|---|---|---|
| `caprock_hal::bootparams` | `acpi_rsdp=0x…` and `fb=base,w,h,pitch,bpp`, pure over the command line | 11 |
| `caprock_hal::fbtext` | text-grid arithmetic over a linear framebuffer, `&mut [u8]`, font-agnostic | 10 |

Two rules carry them, and both are this project's own findings applied one level down:

- **A malformed value must never become a plausible one.** `0` is a typable framebuffer base and
  the address of the real-mode IVT; `fb=garbage → base 0` would scribble over low memory and call
  it a display. Every field is `Option`, one bad field discards the **whole** parameter (four of
  five fields is not four fifths of a framebuffer, it is a wrong one), and malformed values are
  **counted** — otherwise a launcher typo and an absent parameter are the same observation.
- **`pitch` is read, never recomputed.** On the measured machine `2560 px × 4 B = 10240 = pitch`
  exactly — which is the condition under which the mistake is invisible. Same shape as *„unten
  zuerst war ein Zufall der Groessenrelation"*. The tests therefore run against a framebuffer whose
  pitch is deliberately **wider** than its line, and one test asserts the wrong answer is not
  produced (`assert_ne!(offset(0,1), Some(64))`).

`fbtext` deliberately takes **numbers** rather than `bootparams::Framebuffer`: the host harness
compiles each module as a single file, so a module that imports a sibling is one nobody can test
alone. Same shape as `cache_decode::colors_from(sets, line, PAGE)`.

*(One test of mine was wrong and the code was right: it claimed `width=1, pitch=u32::MAX` was
impossible. Absurd is not impossible, and a checker may only refuse the second. Recorded in the
test.)*

- [ ] **T0a — wire it up.** Read the Multiboot command line into `bootparams`, use `acpi_rsdp` when
      the legacy scan fails, and add a framebuffer console behind `konsole` alongside COM1. **Both**
      outputs, not one: under QEMU the serial port is the only thing the suites can read, so
      replacing it would make every existing measurement unreadable.
- [ ] **T0b — a bitmap font.** `fbtext` is font-agnostic by design; it needs one 8×16 table.
- [ ] **T0c — `tools/kexec-caprock.sh`.** Assembles the command line from Linux's own knowledge
      (RSDP from `/sys/firmware/efi/systab`, framebuffer from the EFI GOP region) and **fails
      loudly** if it cannot determine a value. A launcher that guesses a framebuffer address is a
      launcher that writes into whatever is there.
      **Open question, and it is the risky one:** with `i915` bound, `fb0` is a DRM framebuffer in
      GTT-mapped stolen memory, not a linear physical one. The reliable source is the **EFI GOP**
      region (`efifb`) — so the first metal attempt boots Linux with `nomodeset`, or reads the GOP
      base out of the EFI memory map. This must be settled before anything is written to it.
- [ ] **T0d — NVMe as the result store.** `/dev/nvme0n1` exists here and **QEMU emulates `-device
      nvme`**, so it is testable in the suite first — the same rule that picked `igb` over RTL8168.
      NVMe is the most tractable modern storage interface (admin queue + one I/O queue, public
      spec). Results go to a **designated LBA range outside every partition**, guarded by a magic
      and read back independently — exactly the Z4 checkpoint pattern (sector 32710), and for the
      same reason: **a write to the wrong LBA destroys the developer's disk.** No write path lands
      before the negative test that proves it refuses an unguarded target.
- [ ] **T0e — the hardware test suite.** Only reachable once T0a–T0c work. What it must measure is
      already listed in §13a: A1 colouring on real caches (today the claim is entirely unverified),
      `pprobe` with real latencies instead of `SKIP`, the SMT *channel* rather than the policy, the
      IOMMU against real RMRRs (q35 has 0), and whether D13 is the measurement stand or the kernel.

---

## 14. Strand P — PostgreSQL on Caprock (measured 2026-08-21)

**The measurement corrected the estimate, and it corrected it downwards where it counts.** Source
clone at `~/Dokumente/Caprock OS/quellen/postgres` (PostgreSQL 20devel, built here against glibc so
the inventory below is read rather than guessed).

### 14.1 Postgres is the SMALL half — with numbers

`fork()` is the one structural obstacle: Caprock has none (0 hits in the tree), and should not get
one — it duplicates address space *and* cspace implicitly, which is the operation a capability
system cannot name. seL4, Fuchsia and Capsicum all refuse it.

**Postgres solved this itself.** `src/include/pg_config_manual.h:122`:

> If EXEC_BACKEND is defined, the postmaster uses an alternative method for starting subprocesses:
> Instead of simply using fork() … **This must be enabled on Windows (because there is no fork()).**

And the seams are *named files*, not scattered logic:

| seam | Unix | Windows |
|---|---|---|
| shared memory | `sysv_shmem.c` 993 | `win32_shmem.c` 650 |
| semaphores | `posix_sema.c` 381 | `win32_sema.c` 234 |

**All four together: 2258 lines.** `EXEC_BACKEND` touches 50 files, concentrated in
`launch_backend.c` and `syslogger.c` (13 hits each). The platform header template is
`win32_port.h` (589 lines) plus 16 `win32*.c` replacements in `src/port/` (63 files total).

So a Caprock port is **a third entry beside `win32` and `sysv`**: an estimated 2500–4000 lines
against 1 208 303 lines in `src/backend`+`src/common`+`src/port` — **0.3 %**. Weeks, not months.

### 14.2 The libc surface is an INVENTORY, not an estimate

`nm -D --undefined-only` over the built `postgres` + `initdb`: **277 symbols.**

| group | n | note |
|---|---|---|
| file/syscall | **45** | the real item |
| math | 28 | libm — portable or vendored |
| string/mem | 25 | trivial |
| process/signal | 22 | includes `fork`, which `EXEC_BACKEND` removes |
| stdio | 20 | |
| glibc internals `__*` | 18 | disappear with musl or an own libc |
| network | 17 | milestone 3, not 1 |
| time | 11 | |
| locale | 9 | a **decision**, see 14.4 |
| **pthread** | **5** | `sem_*` only — **not one `pthread_*`** |
| dl | 4 | avoidable: build extensions statically |

**Postgres brings its own concurrency entirely.** That was the number most likely to sink the
port, and it is the smallest one.

Nothing exotic in the remainder: `getopt_long`, `syslog`, `backtrace` (diagnostics, droppable),
`getpwuid`/`getgrnam` (a stub on Caprock), `tcgetattr` (initdb's terminal only), `syncfs`.

### 14.3 What Postgres needs from the filesystem

`fd.c` + `md.c` measured 17 operations. That figure is a **floor, not a ceiling** — it is the
storage-manager path, and `xlog.c`/`xlogrecovery.c` (WAL segments, `pg_control`), `initdb` (the
directory tree), `postmaster.pid` with lock semantics, tablespaces (`symlink`), `pg_stat` files and
the syslogger all go past it. The `nm` inventory above supersedes the grep: **45 file/syscall
symbols** is the number to design the interface against.

Caprock today has none of them: the fs PD does `INFO/READ/WRITE/FLUSH/SCAN` on **sectors**, and
`caprock-fat` is read-only — no `mkdir`, no `rename`, no `create`.

### 14.4 Two decisions that must fall BEFORE the first line of port code

- **Collation.** Build against ICU or against C collation only, and write it down. Otherwise B-tree
  ordering hangs on our `strcoll`, and a later fix there **silently invalidates existing indexes**.
- **`wal_sync_method` is pinned, never auto-detected.** The eleven optional `HAVE_*` degrade
  cleanly; the sync-method choice does not — it decides which path the durability measurement
  actually exercises. An auto-detected value means the measurement and production may differ.

### 14.5 Four items that are not in `fd.c` and are therefore easy to miss

Measured present, with size:

| item | where | size |
|---|---|---|
| abnormal backend exit → reset the **whole** shared memory | `CleanupBackend`/`HandleChildCrash` | 20 hits in `postmaster.c` |
| postmaster-death detection in the backend | `PostmasterDeathWatchHandle`/`PostmasterIsAlive` | 7 files |
| `statement_timeout` / `deadlock_timeout` | `timeout.c` | 830 lines |
| shared-region placement | `win32_shmem.c` retry loop against ASLR | 12 hits |

The last one is the instructive one: on Caprock placement **can** be deterministic — but that
belongs in the region cap as a **requirement**, not as an accident of the allocator. Read the
Windows retry loop before writing `caprock_shmem.c`.

The second one has its template in the tree too: on Windows it is an event handle, which is what a
notification-based version looks like.

### 14.6 Milestone 0 is the crash probe — BEFORE libc and filesystem

**Twenty lines of synthetic writer: `write` → `flush`, then a real power cut on target hardware with
NVMe and the volatile write cache ENABLED.** Not QEMU-virtio, not a process kill — those exercise a
different cache.

If the chain does not hold, libc and filesystem are lost person-months, and we find out at the end
instead of at the start. Same displacement as chapter 18.

Postgres deliberately **PANICs** when `fsync` fails (`data_sync_retry` in `fd.c`), because a
swallowed error means silent corruption — the fsyncgate aftermath. So the specification comes with
it, and it is one line:

> **An fsync error is STICKY.** Every subsequent call reports it again, until the file is closed and
> reopened. That is what makes `data_sync_retry` and the PANIC path work — and it makes us better
> than the original at exactly this point, where Linux dropped the error and lost the page.

### 14.7 The named assumption — and it is not a footnote

**`EXEC_BACKEND` has NO CI gate in this tree.** Measured: `.cirrus.yml` is gone (CI is
`.github/workflows/pg-ci.yml`), and `EXEC_BACKEND` appears **zero** times in `meson.build`,
`configure.ac` or `src/tools/ci/`. The only switch is `#if defined(WIN32)` in
`pg_config_manual.h`, or a hand-passed `-DEXEC_BACKEND`.

So: it **is** maintained — through the Windows build, and Windows is a supported platform. But the
**Unix** variant of `EXEC_BACKEND`, which is the one a Caprock port resembles, has no automated
coverage; the header calls it *"only useful for verifying those otherwise Windows-specific code
paths"*.

**We are therefore a co-maintainer of the half nobody builds.** That is not a veto, it is a priced
assumption: if the Unix `EXEC_BACKEND` path breaks in a release, upstream will not notice, and the
report comes from us. Budget for it, or carry a patch.

### 14.8 Order

| # | milestone | what it proves |
|---|---|---|
| **0** | crash probe on real NVMe, write cache on | the durability chain carries a database at all |
| 1 | `initdb` + `postgres --single` (3594 lines, single process, no network) | libc subset + filesystem with the 45 operations + `fsync` |
| 2 | `EXEC_BACKEND` with ONE backend | shared region via a cap, latches via notifications |
| 3 | several backends + local client | the concurrency path |
| 4 | power cut mid-commit, database consistent afterwards | the only line that says the stack can hold a database |

Milestone 1 is roughly half the total effort — and almost all of it is libc and filesystem, **not
Postgres**. Which makes Postgres a good acceptance criterion and a bad strand of its own: both
pieces are needed for every other serious program anyway.
