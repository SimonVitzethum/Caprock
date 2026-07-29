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

- [ ] **A-1.1 Multiboot-Module lesen.** GRUB übergibt die Startmenge als Multiboot-Module; der
      Kernel muss die Modulliste aus der Multiboot-Info auswerten (Feld `mods_count`/`mods_addr`,
      Flag Bit 3). Auf ARM gibt es das Gegenstück bereits über `-device loader` + das reservierte
      RAM-Fenster; die x86-Seite fehlt vollständig. **Achtung:** die Modulbereiche müssen dem
      `PhysAllocator` als belegt gemeldet werden, bevor irgendetwas alloziert — sonst überschreibt
      die erste Allokation das Modul, das gleich geladen werden soll.
- [ ] **A-1.2 Manifestformat.** Ein Eintrag beschreibt: Name, erwarteter SHA-256 des Moduls,
      Domäne (TrustedSAS/HardwareLand/UserLand), Schnittstellenversion, Anfangs-Caps, und die
      Politikfelder aus [A-1.4](#a-14-politikfelder-schnittstelle-zu-strang-b). Format bewusst
      **einfach und selbstbegrenzend** (feste Feldbreiten, Längenpräfixe) — es wird vor jeder
      Signaturprüfung geparst, ist also Angriffsfläche. Kein TOML, kein JSON im Kernel.
- [ ] **A-1.3 Manifest ist ein Autoritätsdokument.** Es legt die gesamte Anfangsverteilung von
      Autorität fest. Also: signiert, und die Signatur **an das Kernel-Image gebunden** (Hash des
      Images geht in die signierte Nachricht ein). Wer die Datei tauschen kann, besitzt sonst die
      Maschine. Baut auf ADR 0014 auf, ist aber nicht dasselbe: dort werden Binaries zertifiziert,
      hier die Zuteilung. **Prüfreihenfolge:** Signatur zuerst, Inhalt danach — nie umgekehrt.
- [ ] **A-1.4 Politikfelder** <a id="a-14-politikfelder-schnittstelle-zu-strang-b"></a>
      **(Schnittstelle zu Strang B).** Je Komponente: exklusiver Farbstreifen ja/nein,
      NUMA-Knoten, Kern-Affinität, Priorität, Budget. Strang A besitzt das **Format**, Strang B die
      **Bedeutung** (was ein Streifen ist, wie NUMA vergeben wird). Die konkrete Farbe gehört
      **nicht** ins Manifest — sie ist maschinenlokal.
- [ ] **A-1.5 `SYS_LOAD` auf x86 zum Laufen bringen.** Scheitert heute sauber, weil es nichts zu
      laden gibt. Der ELF-Lader existiert (`sel4lake-loader`); es fehlt die Quelle.

## A-2. Root-Task (F2)

- [ ] **A-2.1 Ein Startprogramm aus der Startmenge laden und ihm die Wurzel-Caps übergeben.** Der
      seL4-Weg. Ab hier ist jede weitere Fähigkeit ein Userland-Programm statt eines
      Kernel-Patches — das ist der eigentliche Hebel des ganzen Plans.
- [ ] **A-2.2 Erst danach `default = []`** in `kernel/Cargo.toml` (Feature `selftest`). Vorher
      wäre das Gating kein schlankerer Kernel, sondern ein leerer. Der `--no-default-features`-Bau
      wird bereits von `test-qemu-x86.sh` mitgebaut und der `.text`-Schrumpf geprüft — die
      Prüfungen bleiben, nur die Vorgabe dreht sich.

## A-3. Caps, die man wieder loswird (A4, A3, C3)

- [ ] **A-3.1 `SYS_CDELETE`** (eigener Slot, cap-gegatet auf den eigenen Cspace). Ohne das kann
      ein langlebiger Dienst, der Caps per IPC empfängt, seine Slots nicht freigeben und läuft
      gegen `CAP_BUDGET_PER_PD`. Der Root-Task ist genau so ein Dienst: mit A-2 wird aus der
      ABI-Lücke ein Betriebsproblem.
- [ ] **A-3.2 `SYS_CMOVE`/`SYS_CCOPY`** und ein **wählbarer Empfangs-Slot** für Grants statt des
      festen `GRANT_RECV_SLOT`.
- [ ] **A-3.3 `ReplyFinal` vom Kernelstack lösen.** Hält heute ein `[(u32,u64); NOBJECTS]`-Array
      auf dem 2-KiB-Kernelstack; `NOBJECTS` hochzuziehen koppelt an die Stackgröße. Vorbedingung
      von A-3.4.
- [ ] **A-3.4 Cap-/PD-/Endpoint-/Notification-Tabellen dynamisch.** Solange die Cap-Tabelle global
      und fest ist, bestimmt ein Tenant die Dichte aller anderen. **(Berührt Strang B:** die
      Streifenbuchhaltung aus A1 zählt PDs; wächst die PD-Zahl dynamisch, muss die
      Streifen-Freiliste das mitmachen.)

## A-4. Hot-Reload, der den Namen verdient (Z11d, Z11e, Z11f)

Auf ARM existiert der Fall (Phase 7: eine Server-PD wird über *dieselbe* Endpoint-Cap ersetzt).
Für einen Betriebsanspruch fehlen drei Dinge.

- [ ] **A-4.1 Atomares Umbinden.** Zwischen „alter Server weg" und „neuer Server empfangsbereit"
      darf kein Zustand liegen, in dem ein `CALL` mit `NoEndpoint` scheitert. Der Endpoint ist
      bereits ein eigenes Objekt mit eigener Cap — das trägt; der Austausch muss unter *einem*
      Lock geschehen.
- [ ] **A-4.2 Ruhender Punkt.** Ein Client, der in `CALL` blockiert, hält eine Reply-Cap auf den
      alten Server. Entweder der neue erbt die offenen Replys, oder der Austausch findet nur ohne
      offene Transaktion statt. Die zweite Variante ist ehrlich und für den Anfang wahrscheinlich
      richtig — sie braucht aber einen Begriff von „ruhend", den es heute nicht gibt.
      **Derselbe Begriff trägt später [Z4a](todo.md#z4-checkpointrestore-eines-threads)
      (Thread einfrieren).** Einmal bauen, zweimal benutzen — und deshalb hier gleich allgemein
      genug entwerfen.
- [ ] **A-4.3 Zustandsübergabe.** Die neue Fassung braucht den Zustand der alten. Entweder eine
      Region, die den Austausch überlebt (dann ist ihr Format eine ABI und muss versioniert
      werden), oder ein ausdrückliches Übergabeprotokoll. Ohne Festlegung ist Hot-Reload ein
      Neustart mit Datenverlust, der anders heißt.
- [ ] **A-4.4 Schnittstellenversion prüfen, nicht hoffen.** Ein Austausch, der die Version ändert,
      wird **abgewiesen**. Sonst redet ein neuer Server mit alten Clients in einer Sprache, die
      beide für dieselbe halten.
- [ ] **A-4.5 Negativliste.** Was **nicht** austauschbar ist, muss benannt sein: der Kernel
      selbst, und alles, was eine Cap auf maschinenlokale Hardware hält, während sie in Benutzung
      ist (IOMMU-Kontexte, aktive DMA-Regionen). Gehört in `docs/invariants.md`.

## A-5. Etwas, das sich lohnt zu laden (Z10)

- [ ] **A-5.1 Treiberrahmen als Userland-PD.** MMIO-Cap, IRQ-Cap, DMA-Cap — die Stücke existieren
      einzeln; es fehlt die Zusammenfassung zu „so schreibt man hier einen Treiber".
- [ ] **A-5.2 virtio auf x86.** `virtio` ist bis heute aarch64-only. Netz und Blockgerät sind die
      Mindestmenge für „Cloud".
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
