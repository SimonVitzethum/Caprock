# Strang A — Ausführen und Austauschen

**Frage, die dieser Strang beantwortet:** Wie bekommt der Kernel fremden Code zum Laufen, und wie
tauscht man ihn im Betrieb aus, ohne dass IPC-Beziehungen reißen?

Gegenstück: [todo-B-verlaesslichkeit.md](todo-B-verlaesslichkeit.md). Gemeinsame Grundlage:
[todo.md](todo.md) (Abschnitt Z) und [docs/plan-betriebsbereit.md](docs/plan-betriebsbereit.md).
Koordinationsregeln am Ende dieser Datei — **vor der ersten Änderung lesen.**

Dieser Strang ist der **kritische Pfad**. Solange er nicht steht, hat der Kernel keinen
Nicht-Test-Zweck: schaltet man das Feature `selftest` ab, bootet er und geht in eine Leerschleife.
Alles, was Strang B härtet, härtet bis dahin ein System ohne Anwendung.

---

## A-1. Startmenge und Manifest (Z11a, Z11b, C6)

Das Boot-Image enthält genau zwei Dinge: den Kernel und **eine** Datei, die festlegt, was geladen
wird. Alles andere liegt außerhalb.

- [x] **A-1.1 Multiboot-Module lesen.** GRUB übergibt die Startmenge als Multiboot-Module; der
      Kernel muss die Modulliste aus der Multiboot-Info auswerten (Feld `mods_count`/`mods_addr`,
      Flag Bit 3). Auf ARM gibt es das Gegenstück bereits über `-device loader` + das reservierte
      RAM-Fenster; die x86-Seite fehlt vollständig. **Achtung:** die Modulbereiche müssen dem
      `PhysAllocator` als belegt gemeldet werden, bevor irgendetwas alloziert — sonst überschreibt
      die erste Allokation das Modul, das gleich geladen werden soll.
      → `kernel/src/arch/x86_64/multiboot.rs`. Die Bereiche werden **ausgeschnitten**, nicht
      nachträglich markiert: was nie als frei gemeldet wurde, kann nicht vergeben werden. Die
      Grenzfälle (Rand, Überlappung, unsortierte Eingabe, Vollabdeckung) sieht der reale Lauf nie,
      also werden sie eingespeist (`mbmod : ALL PASS`). Die Archivquelle ist nicht mehr fest
      verdrahtet — `loader::set_archive_span`, auf ARM bleibt das statische Fenster der Rückfall.
- [x] **A-1.2 Manifestformat.** Ein Eintrag beschreibt: Name, erwarteter SHA-256 des Moduls,
      Domäne (TrustedSAS/HardwareLand/UserLand), Schnittstellenversion, Anfangs-Caps, und die
      Politikfelder aus [A-1.4](#a-14-politikfelder-schnittstelle-zu-strang-b). Format bewusst
      **einfach und selbstbegrenzend** (feste Feldbreiten, Längenpräfixe) — es wird vor jeder
      Signaturprüfung geparst, ist also Angriffsfläche. Kein TOML, kein JSON im Kernel.
      → `crates/sel4lake-loader/src/manifest.rs` (80-B-Kopf, 96-B-Einträge, `entry_len` im Kopf,
      damit ein fremdes Eintragsformat **erkannt** statt verrutscht gelesen wird). Host-getestet
      inkl. Mutations-Durchlauf; Kani-Beweise für Crash-Freiheit und exakte Partitionierung.
- [x] **A-1.3 Manifest ist ein Autoritätsdokument.** Es legt die gesamte Anfangsverteilung von
      Autorität fest. Also: signiert, und die Signatur **an das Kernel-Image gebunden** (Hash des
      Images geht in die signierte Nachricht ein). Wer die Datei tauschen kann, besitzt sonst die
      Maschine. Baut auf ADR 0014 auf, ist aber nicht dasselbe: dort werden Binaries zertifiziert,
      hier die Zuteilung. **Prüfreihenfolge:** Signatur zuerst, Inhalt danach — nie umgekehrt.
      → Eigene Schlüsselmenge (`kernel/src/manifest_keys.rs`, `tools/gen_manifest_key.py`),
      getrennt von `trusted_keys.rs`. Bindung = SHA-256 über `[__text_start, __rodata_end)`;
      Werkzeug-Gegenstück `tools/kernel_hash.py`. **Die Prüfreihenfolge trägt der Typ:**
      `SystemManifest::parse` liefert keinen einzigen Eintrag, Einträge gibt es nur über
      `Verified`, und das entsteht nur aus `verify_with`.
- [x] **A-1.4 Politikfelder** <a id="a-14-politikfelder-schnittstelle-zu-strang-b"></a>
      **(Schnittstelle zu Strang B).** Je Komponente: exklusiver Farbstreifen ja/nein,
      NUMA-Knoten, Kern-Affinität, Priorität, Budget. Strang A besitzt das **Format**, Strang B die
      **Bedeutung** (was ein Streifen ist, wie NUMA vergeben wird). Die konkrete Farbe gehört
      **nicht** ins Manifest — sie ist maschinenlokal.
      → Format steht (`policy_flags`, `numa_node`, `core_affinity`, `priority`, `budget_us`); der
      Kernel liest und weist sie aus.
      **Nachtrag 2026-07-30: „noch nicht angewandt" ist kein neutraler Zustand.** Wer
      `POLICY_EXCLUSIVE_STRIPE` in ein signiertes Dokument schreibt, glaubt danach an eine
      Cache-Trennung, die niemand herstellt — das ist schlimmer als gar kein Feld. Der Kernel
      **weist ein solches Manifest jetzt ab** (`UnsupportedPolicy`), genau wie A-2.1 es bei
      `CAP_PD_CONTROL` tut: lieber ablehnen als weniger geben, ohne es zu sagen.
      Warum es heute nicht einhaltbar ist: `alloc_colored` weist mehr als `MASK_BITS` Seiten ab,
      ein Streifen trägt `MASK_BITS/PARTITIONS` Seiten (64 KiB), und Segmente/Stack eines
      geladenen Programms kommen physisch **zusammenhängend** aus `mem_alloc`. Nötig wäre
      stückweise Allokation aus demselben Streifen mit seitenweisem Mapping, plus eine
      Freigabe-Buchhaltung, die mitwächst (`seglist`, `MAX_IMG_SEGS`). **Teilweise gefärbt ist
      nicht gefärbt** — deshalb ablehnen statt halb liefern.
      `POLICY_NO_HOTRELOAD` **wird durchgesetzt** (s. A-4.5).
- [x] **A-1.5 `SYS_LOAD` auf x86 zum Laufen bringen.** Scheitert heute sauber, weil es nichts zu
      laden gibt. Der ELF-Lader existiert (`sel4lake-loader`); es fehlt die Quelle.
      → Loader baut auf beiden Architekturen, `load_by_index` ist kein `None`-Stub mehr.
      Nebenbefund: der ELF-Parser kannte nur `EM_AARCH64` — ein x86-Binary war für ihn schlicht
      kein ELF. Jetzt `EXPECTED_MACHINE` per `cfg(target_arch)`, mit einem Test gegen die jeweils
      **andere** Architektur.

## A-2. Root-Task (F2)

- [x] **A-2.1 Ein Startprogramm aus der Startmenge laden und ihm die Wurzel-Caps übergeben.** Der
      seL4-Weg. Ab hier ist jede weitere Fähigkeit ein Userland-Programm statt eines
      Kernel-Patches — das ist der eigentliche Hebel des ganzen Plans.
      → `loader::start_root_task` + `programs/trusted/init`. Der Root-Task signalisiert, dass er
      läuft, und lädt dann **über seine eigene Loader-Cap** die restliche Startmenge nach; erst das
      zweite Badge belegt die Aussage, um die es geht. Boot-Argument ist bewusst das Minimum
      (`(Anzahl << 32) | eigener Index`) — alles Weitere gehört hinter eine Capability, nicht in
      ein Register.
      **Offen und benannt:** `CAP_PD_CONTROL` ist nicht erteilbar, weil eine PdControl-Cap eine
      Ziel-PD bezeichnet, die beim Start des Root-Tasks noch nicht existiert. Der Kernel weist ein
      Manifest, das sie verlangt, **ab** (`UnsupportedAuthority`) statt still weniger zu geben. Der
      ehrliche Weg wäre, dass `SYS_LOAD` die PdControl-Cap der neu erzeugten PD zurückgibt — das
      ist eine ABI-Erweiterung und gehört zu A-3.2 (wählbarer Empfangs-Slot).
- [x] **A-2.2 erledigt (2026-07-30). `default = []`.** Vorher wäre das Gating kein schlankerer
      Kernel gewesen, sondern ein leerer; seit A-2.1 hat der Vorgabebau einen Zweck
      (`start_root_task_reported()` liegt ausserhalb jedes Features). Beide Suiten fordern
      `--features selftest` für den gebooteten Bau **ausdrücklich** an, nicht über `default` —
      sonst wären sie beim Dreh still geworden statt rot, und Stille sieht wie Erfolg aus.
      Belegt von B: volle x86-Suite mit dem Dreh im Baum, **einziger FAIL `x2APIC`** (unverändert
      gegenüber dem Lauf davor), beide F1-Prüfungen mit gedrehten Vorzeichen (`.text` ohne Feature
      `0x24000`, mit `0x38000`), und die Lade-Suite `== ALL PASS ==` inklusive aller drei
      Negativfälle. Details in AGENTS.md, Mitteilung 6.

## A-3. Caps, die man wieder loswird (A4, A3, C3)

- [x] **A-3.1 `SYS_CDELETE`** (eigener Slot, cap-gegatet auf den eigenen Cspace). Ohne das kann
      ein langlebiger Dienst, der Caps per IPC empfängt, seine Slots nicht freigeben und läuft
      gegen `CAP_BUDGET_PER_PD`. Der Root-Task ist genau so ein Dienst: mit A-2 wird aus der
      ABI-Lücke ein Betriebsproblem.
      → Syscall 14, **ohne** zusätzliche Cap: Autorität abzugeben darf nie an einer Erlaubnis
      hängen. Reihenfolge Slot räumen → löschen → bei Misserfolg zurücklegen; ein Cap mit
      CDT-Kindern bleibt unverändert liegen (`ERR_HASCHILDREN`) statt halb entfernt zu werden.
      Geprüft aus **Ring 3** (im `init`), und zwar **beide** Ausgänge: der erfolgreiche (und die
      Autorität ist danach wirklich weg — ein weiteres `SYS_LOAD` wird abgewiesen) und der
      abgelehnte (der Cap bleibt benutzbar).
- [x] **A-3.2 `SYS_CMOVE`/`SYS_CCOPY`** und ein **wählbarer Empfangs-Slot** für Grants statt des
      festen `GRANT_RECV_SLOT`.
      → Syscalls 15/16/17. `CCOPY` schneidet die Rechte mit denen des Originals (eine Kopie darf nie
      mehr können als die Vorlage) und nimmt ein **Badge**; `CMOVE` überschreibt ein belegtes Ziel
      **nicht** (das wäre ein Cap-Verlust, den niemand angeordnet hat — wer räumen will, ruft
      `CDELETE`); `SETRECV` legt den Empfangs-Slot **je PD** fest, und zwar beim **Empfänger**:
      dürfte der Sender ihn wählen, könnte ein Server jeden Cap seines Clients verdrängen — eine
      Schreiboperation in fremdes Eigentum, verkleidet als Antwort.
      **Warum das Badge dazugehört und nicht Beiwerk ist:** `SYS_SIGNAL` verodert das Badge der
      benutzten **Cap** in `pending`; das Nachrichtenwort spielt keine Rolle. Ohne badgbare Kopien
      kann ein Programm dem Kernel genau **eine** Tatsache melden. Dasselbe gilt beim Weiterreichen,
      deshalb hat `SYS_LOAD` jetzt ein Badge-Argument (`x4`). Erlaubt ist beides, weil der Aufrufer
      das Objekt bereits besitzt — er vergibt ein Etikett auf eigener Autorität, er erwirbt keine.
      **Nicht erledigt:** `SYS_LOAD` gibt weiterhin keine PdControl-Cap der neu erzeugten PD zurück
      (s. A-2.1). Der wählbare Empfangs-Slot ist die Vorbedingung dafür; die Rückgabe selbst steht
      noch aus.
- [x] **A-3.3 `ReplyFinal` vom Kernelstack lösen.** Hält heute ein `[(u32,u64); NOBJECTS]`-Array
      auf dem 2-KiB-Kernelstack; `NOBJECTS` hochzuziehen koppelt an die Stackgröße. Vorbedingung
      von A-3.4.
      → `Finalized` (der heutige Name) leiht seinen Speicher jetzt vom Aufrufer statt ihn zu
      besitzen; der Kernel hält **einen** statischen Puffer (`FinalizeBuf`, äusserste Sperre).
      Kommen die Tabellen in A-3.4 aus dem RAM, kommt der Puffer aus derselben Quelle — **ohne
      Änderung an der Cap-Crate**. Es war mehr als das eine Array: `dma_finalize` hielt eine
      **Kopie** derselben Regionen (nur um daraus einen zusammenhängenden Slice zu machen), und
      **jede** `finalize`-Implementierung der Enforcer nochmals ein `[usize; 128]`.
      **Zwei Funde:** `Finalized::overflowed()` — daneben der Kommentar, sie stehe da, „damit «kann
      nicht vorkommen» prüfbar ist statt behauptet" — wurde von **niemandem** gerufen. Solange die
      Arrays im Typ lagen, war die Kapazität eine Typeigenschaft; jetzt ist sie eine Entscheidung
      des Aufrufers, also wird sie geprüft (Audit-Code 70, plus sofortige Meldung: ein Überlauf
      heisst, ein in `CALL` blockierter Aufrufer wird nie entblockt, und das sieht man hinterher
      an nichts mehr). Und die Schranke stand doppelt (`MAX_FINALIZE` im Kernel „dieselbe Schranke
      wie in der Cap-Crate") — eine Kopie, die beim Wachsen still auseinanderläuft.
- [x] **A-3.4 erledigt (2026-07-30/31), in vier Teilen + Abschluss.** Threads als Zusage
      (`TARGET_THREADS = 10_000`, vorher `cores * 256` — eine Eigenschaft des Testaufbaus);
      `CapSpace`, `PdTable`, Endpoints und Notifications aus dem Boot-RAM statt `.bss`
      (`Slab`-Muster aus ext-30). Gemessen: 80256 Slots/Objekte, 10000 PDs, 10064 Endpoints,
      10000 Thread-Slots, zusammen rund 42 MiB. **Der Fund, der A-3.4 begruendete, ist
      geschlossen:** `CAP_BUDGET_PER_PD` deckelte den Einzelverbrauch, die **Summe** prüfte
      niemand — 256 Slots gegen 2048 zugesagte, 32 PDs füllten die Tabelle. Jetzt wird die Summe
      geprüft (`d1f5ed8`). *(Alter Text:)* **A-3.4 Cap-/PD-/Endpoint-/Notification-Tabellen dynamisch.** Solange die Cap-Tabelle global
      und fest ist, bestimmt ein Tenant die Dichte aller anderen. **(Berührt Strang B:** die
      Streifenbuchhaltung aus A1 zählt PDs; wächst die PD-Zahl dynamisch, muss die
      Streifen-Freiliste das mitmachen.)

## A-4. Hot-Reload, der den Namen verdient (Z11d, Z11e, Z11f)

Auf ARM existiert der Fall (Phase 7: eine Server-PD wird über *dieselbe* Endpoint-Cap ersetzt).
Für einen Betriebsanspruch fehlen drei Dinge.

- [x] **A-4.1 erledigt (2026-07-31, `a25fa23`).** Prüfung und Tausch laufen unter *einem* Lock.
      Ohne vorherige Stilllegung (A-4.2) wird abgewiesen — der Befund „keine offene Transaktion"
      wäre sonst eine Momentaufnahme, die im nächsten Takt nicht mehr gilt. Ein fremder Empfänger
      blockiert den Tausch, statt still verdrängt zu werden; im überlappenden Fall ist durchgehend
      genau ein Empfänger gebunden. Belegt: `rebind : ALL PASS`.
- [x] **A-4.2 erledigt (2026-07-31, `4fca286`).** Vor A-4.1 gebaut, weil dessen ehrliche Variante
      den Begriff *ruhend* voraussetzt. Stilllegen weist **neue** Transaktionen mit
      `ERR_QUIESCING` ab (nicht `ERR_BADCAP`: „kommt gleich wieder" ist für den Client eine andere
      Lage als „gibt es nicht"), laufende dürfen abschließen; ein zweiter Austausch am selben
      Endpoint wird abgewiesen, doppelte Freigabe wird gemeldet. Belegt: `quiesce : ALL PASS`.
      Ursprüngliche Fassung: Ein Client, der in `CALL` blockiert, hält eine Reply-Cap auf den
      alten Server. Entweder der neue erbt die offenen Replys, oder der Austausch findet nur ohne
      offene Transaktion statt. Die zweite Variante ist ehrlich und für den Anfang wahrscheinlich
      richtig — sie braucht aber einen Begriff von „ruhend", den es heute nicht gibt.
      **Derselbe Begriff trägt später [Z4a](todo.md#z4-checkpointrestore-eines-threads)
      (Thread einfrieren).** Einmal bauen, zweimal benutzen — und deshalb hier gleich allgemein
      genug entwerfen.
- [x] **A-4.3 erledigt (2026-07-31).** Gewählt ist die **Region, die den Austausch überlebt** —
      und damit ist ihr Format eine ABI, also mit **versioniertem Kopf** (`state.rs` in
      `sel4lake-region`). Der Kopf trägt `state_version`, `program_id` und einen
      Übernahmezähler. **Der Punkt, an dem die Sache hängt:** eine abweichende Version wird
      ABGEWIESEN, statt die Bytes der alten Fassung im eigenen Sinn zu lesen. Das ist der
      Unterschied zwischen Datenverlust (merkt man) und fehlinterpretiertem Zustand (merkt man
      nicht). Ebenso weist eine fremde `program_id` ab; eine frische Region meldet `NoState`
      statt „Version 0", weil „noch keiner" und „der nullte" verschiedene Lagen sind. Der
      Übernahmezähler zählt nur bei tatsächlicher Übernahme, nicht bei Abweisungen — sonst
      belegte er das Gegenteil dessen, was er behauptet.
      **Belegt in zwei Stufen:** die Torlogik auf x86 (`state : ALL PASS`), der Ernstfall auf
      aarch64 (`ckpt : ALL PASS`): v1 rechnet `+1` und hinterlässt `[1, 2, 3]`, nach dem Reload
      rechnet v2 mit `+10` weiter und kommt auf `[13, 23]` — also auf den ÜBERNOMMENEN Werten,
      nicht auf frisch initialisierten, Übernahmegeneration 1. Der Ernstfall läuft nicht auf
      x86, weil er an der arch-neutralen Thread-Demo hängt, die dort nicht startet; im x86-Skript
      steht das als Anmerkung, damit ein grünes `state` nicht mehr verspricht, als es prüft.
      *(Alter Text:)* **A-4.3 Zustandsübergabe.** Die neue Fassung braucht den Zustand der alten.
      Entweder eine Region, die den Austausch überlebt (dann ist ihr Format eine ABI und muss
      versioniert werden), oder ein ausdrückliches Übergabeprotokoll. Ohne Festlegung ist
      Hot-Reload ein Neustart mit Datenverlust, der anders heißt.
- [x] **A-4.4 erledigt (2026-07-30), mit einer benannten Grenze.** Versionssperre im Lader:
      `iface_gate` hält beim ersten Laden einer `program_id` ihre `iface_version` fest und weist
      jeden weiteren Ladevorgang mit abweichender Version ab (`IfaceVersionChanged`). Verglichen
      wird gegen die **erste**, nicht gegen die vorige — sonst driftete die Schnittstelle in
      kleinen Schritten beliebig weit weg, obwohl kein einzelner Schritt erlaubt war. Die Version
      kommt aus dem **signierten** Manifest, nicht aus dem Image: sie ist eine Aussage über
      Zuteilung. Der Gate hängt in **beiden** Ladepfaden (`load_image`, `load_program_into_pd`),
      sonst wäre er umgehbar. Läuft die Buchhaltung voll, wird abgewiesen (`IfaceTableFull`) statt
      still nicht mehr zu prüfen — eine Prüfung, die unbemerkt aussetzt, sieht von aussen aus wie
      eine bestandene.
      **Die Grenze, und sie ist wichtig:** über das Manifest ist der Abweisungszweig heute
      **nicht erreichbar**. Pro Boot gibt es genau ein Manifest, jeder Ladevorgang derselben ID
      liest also dieselbe Version. Erreichbar wird er erst, wenn ein Austausch zur Laufzeit ein
      *anderes* Image mitbringt — das ist A-4.1/A-4.3 und existiert nicht. Damit der Zweig nicht
      bis dahin ungeprüft bleibt (ungeprüft heisst: vermutlich kaputt, wenn er zum ersten Mal
      gebraucht wird), ist die Buchhaltung als `iface_record_or_check` herausgezogen und wird vom
      Selbsttest direkt gefüttert: `iface : ALL PASS` belegt **beide** Ausgänge plus, dass eine
      andere `program_id` unberührt bleibt.
- [x] **A-4.5 erledigt (2026-07-30).** `docs/invariants.md` **§13**, normativ formuliert: der
      Kernel selbst (das Manifest ist an sein Image gebunden — ein getauschter Kernel entwertet
      jede Signatur, die auf ihn lautet), IOMMU-Kontexte und aktive DMA-Regionen (werden von der
      *Hardware* gelesen, nicht vom Kernel; „die PD ist weg" ist für ein Busmaster-Gerät keine
      Aussage), gebundene IRQ-Zustellung. Das Teardown-Token (ext-37) ist die richtige Grundlage:
      austauschbar ist eine PD erst, wenn sie ihre Geräte abgegeben hat.
      **Beim Schreiben dazugekommen:** eine gefärbte PD ist nur hot-reloadbar, wenn ein Streifen
      **frei** ist. A-4.1 verlangt atomares Umbinden, also existieren alte und neue Instanz
      kurzzeitig gleichzeitig und brauchen je einen disjunkten Satz. Bei vier Streifen und vier
      gefärbten PDs schlägt jeder Austausch fehl — sauber (B-4.2), aber er schlägt fehl. Die neue
      Instanz den Streifen der alten erben zu lassen ist **kein** Ausweg: solange beide leben,
      teilten sie sich die Farben.

## A-5. Etwas, das sich lohnt zu laden (Z10)

- [ ] **A-5.1 Treiberrahmen als Userland-PD.** MMIO-Cap, IRQ-Cap, DMA-Cap — die Stücke existieren
      einzeln; es fehlt die Zusammenfassung zu „so schreibt man hier einen Treiber".
- [~] **A-5.2 virtio auf x86** — der **Transport** steht und ist auf x86 belegt (2026-08-01).
      Offen bleiben **Netz und Blockgerät**.

      Der Treiber lag unter `hal/aarch64/`, obwohl nichts daran ARM-spezifisch war: virtio-pci ist
      ein PCI-Standard, und die beiden Berührungspunkte — `cpu::dsb_sy()` (auf x86 `mfence`, war
      da) und `pcie::cap_ptr` (fehlte, drei Zeilen) — gibt es auf beiden Zweigen. Er liegt jetzt
      arch-neutral in `crates/sel4lake-hal/src/virtio.rs`.

      **Gemessen, beide Richtungen** (`virtio`-Zeile, x86-Suite):

          Transport (vor VT-d):  Caps=1  Geraet-DMA=1 (64 Byte)
          Sperre  (nach VT-d):   Caps=1  Geraet-DMA=0 (0 Byte)  VT-d-Faults 0 -> 1

      Der erste Teil steht **vor** dem VT-d-Aufbau und belegt den Transport in voller Länge:
      Capability-Liste, Handshake, Feature-Aushandlung einschließlich
      `VIRTIO_F_ACCESS_PLATFORM`, Virtqueue — und dass das Gerät wirklich Bytes per Bus-Master-DMA
      liefert. Der zweite läuft nach dem Aufbau: dasselbe Gerät, keine Zuteilung, kommt am
      Default-Block nicht mehr durch, und die Einheit protokolliert den Fault. Ein Test, der nur
      den Erfolgsfall zeigt, könnte nicht sagen, ob die Sperre wirkt; einer, der nur die Sperre
      zeigt, nicht, ob überhaupt etwas funktioniert hätte.

      QEMU-Detail, das eine Runde gekostet hat: `virtio-rng-pci` ist auf x86 per Vorgabe
      *transitional*, und dort gibt es `iommu_platform` nicht (`VIRTIO_F_IOMMU_PLATFORM was
      supported by neither legacy nor transitional device`). Es braucht `disable-legacy=on`.

      **Zu tun für das eigentliche A-5.2:** virtio-net und virtio-blk. Beides braucht mehr als den
      Transport (mehrere Queues, Anfrageformate) — und für nutzbaren DMA die VT-d-Zuteilung aus
      B-3.3/B-3.4, die heute noch `None` liefert.
- [ ] **A-5.3** Die Geräte-Zuteilung darf erst scharf werden, wenn **Interrupt Remapping** steht —
      das liegt in Strang B (B-3). Bis dahin nur Geräte ohne DMA-Fähigkeit oder ohne
      Tenant-Zugriff.

---

## Koordinationsregeln

**Dateibesitz.** Wer eine Datei besitzt, ändert sie ohne Rückfrage; wer sie nicht besitzt, meldet
sich vorher.

* **Strang A besitzt:** `kernel/src/loader.rs`, `crates/sel4lake-loader/`, `crates/sel4lake-cap/`,
  `crates/sel4lake-ipc/`, `crates/sel4lake-abi/`, `crates/sel4lake-microkit/`,
  `kernel/src/arch/x86_64/bootinfo.rs`, `tools/`, `programs/`, `kernel/Cargo.toml`.
* **Strang B besitzt:** `crates/sel4lake-sync/`, `crates/sel4lake-mem/`, `crates/sel4lake-hal/`,
  `kernel/src/colors.rs`, `kernel/src/dmatests.rs`, `test-qemu*.sh`, `build*.sh`, `docs/`.
* **Geteilt, deshalb mit Ansage:** `kernel/src/system.rs`, `kernel/src/arch/x86_64/bringup.rs`,
  `kernel/src/main.rs`, `docs/invariants.md`, `todo.md`.
  In `system.rs` gilt grob: A ändert Syscall-/Cap-/Loader-Pfade, B die Speicher-, Farb- und
  Telemetriepfade.

**Vier echte Kopplungen** — alles andere ist unabhängig:

1. **Politikfelder im Manifest** (A-1.4 ↔ B-4): A besitzt das Format, B die Bedeutung.
2. **Der Root-Task erzeugt PDs** (A-2.1 ↔ B-4.1): er muss den *gefärbten isolierten* Pfad
   benutzen, sobald B ihn zum Normalfall gemacht hat. Bis dahin den heutigen.
3. **Ruhender Punkt** (A-4.2 ↔ Z4a): einmal bauen, A besitzt ihn.
4. **Dynamische Tabellen** (A-3.4 ↔ B-4.2): mehr PDs als Streifen muss sauber scheitern.

**Reihenfolge über die Stränge hinweg:** B-1 (Determinismus) zuerst, von beiden abzuwarten. Ein
Testaufbau, der sporadisch hängt, macht jede Messung beider Stränge wertlos.

**Vor jeder Übergabe:** `./test-qemu-x86.sh` grün (bzw. begründet, was nicht), und die
Host-Unit-Tests: `rustc --test --edition 2021 -O crates/sel4lake-mem/src/lib.rs -o /tmp/t && /tmp/t`.
