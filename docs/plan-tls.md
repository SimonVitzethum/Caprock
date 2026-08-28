# Plan — TLS, so klein wie moeglich

Written 2026-08-27. Everything below is measured against this branch, not remembered.

> **Stand 2026-08-27: T1–T5 gebaut und gemessen** (Zeile `tls`, beide Architekturen, vier
> Gegenproben in `tools/tls-negativ.sh`). Was daran gelernt wurde, steht in `done.md`; **offen ist
> nur noch E-T1** (`FSGSBASE`). Dieser Plan bleibt als Begruendungstext stehen.

**Das Ziel ist nicht Linux-TLS.** Es ist: *ein Treiber mit `#[thread_local]` laeuft, und zwei
Threads derselben PD sehen verschiedene Werte.* Alles, was darueber hinausgeht, ist ausdruecklich
nicht Teil dieses Plans.

---

## 0. Was heute da ist — gemessen, nicht erinnert

| | Befund |
|---|---|
| `tpidr`, `fs_base`, `IA32_FS_BASE` im Baum | **null Treffer** |
| `.tdata` / `.tbss` in `programs/user.ld`, `programs/user-x86.ld` | **nicht vorhanden** |
| MSR-Zugriff (`rdmsr`/`wrmsr`) | vorhanden, `crates/caprock-hal/src/x86_64/cpu.rs` |
| Naht fuer „pro Thread an die CPU spiegeln" | vorhanden: `sync_fp_trap` / `sync_vspace` in `system.rs::syscall()` |
| Zweiter Thread in derselben PD | vorhanden seit K1a/K1b (`SYS_SPAWN`) — **ohne ihn ist TLS nicht messbar** |
| `FS` und `GS` auf x86 | **beide frei** — kein `swapgs`, keine Per-CPU-Ablage ueber `GS` |
| `TPIDR_EL0` auf aarch64 | **frei** |
| `has-thread-local` in `programs/*-caprock-user.json` | **fehlt** → `#[thread_local]` ist heute gar nicht uebersetzbar |
| `relocation-model` / PIE der Programme | `static` / `false` |

**Drei Folgerungen, jetzt belegt statt angenommen:**

1. **`FS` ist die Wahl auf x86** — nicht weil es ueblich ist, sondern weil beide Register frei sind
   und der Uebersetzer fuer local-exec `%fs:`-relativ adressiert. Dass `GS` frei **bleibt**, ist
   dabei der Nebenertrag: wer spaeter eine Per-CPU-Ablage per `swapgs` will, hat sie noch.
2. **`relocation-model: static` und keine PIE** heisst: diese Binaries bekommen ohnehin
   **local-exec**. Der Umfang aus §4 ist damit keine Sparmassnahme, sondern die Beschreibung
   dessen, was die Werkzeugkette hier tut.
3. **`has-thread-local` fehlt in beiden Ziel-Beschreibungen** — das ist die erste Zeile von T3, und
   ohne sie ist der ganze Rest nicht pruefbar: `#[thread_local]` schlaegt im Uebersetzer fehl,
   nicht erst im Lauf.

---

## 1. Der Schnitt: der Kernel besitzt EIN Register, sonst nichts

> **Der Kernel haelt den Thread-Pointer. Die Aufteilung des Blocks dahinter geht ihn nichts an.**

* **Warum das Register beim Kernel liegt:** es muss den Kontextwechsel ueberleben. Genau das ist
  das Einzige, was ein Programm fuer sich selbst nicht leisten kann.
* **Warum die Aufteilung NICHT beim Kernel liegt:** das TLS-Layout ist eine ABI der
  **Werkzeugkette**, nicht des Kernels — und sie ist **je Architektur eine andere**:

  | | `tp` zeigt auf | Variablen | Selbstzeiger |
  |---|---|---|---|
  | x86-64 (Variante 2) | **Ende** des Blocks | negative Offsets | `tp[0]` |
  | aarch64 (Variante 1) | **Anfang**, plus 16 B reservierter TCB | ab `tp+16` | **keiner** |

  Ein Kernel, der Bloecke anlegt, friert eines der beiden Formate ein und muesste `.tdata`-Groesse,
  Ausrichtung und Konvention kennen. Bleibt er draussen, lebt der Unterschied **ausschliesslich im
  Userspace-Setup** — und das ist nicht eine Nebenwirkung des Schnitts, sondern seine Probe.

  Die erste Fassung dieses Plans schrieb „Variante 2" allgemein hin. Das waere fuer die Haelfte der
  Ziele falsch gewesen, und der Fehler waere auf ARM als **Datenmuell** aufgefallen, nicht als
  Absturz — die teurere Sorte.

**Was das kostet und was es spart:** der Kernel bekommt ein `usize` je TCB und einen MSR-Schreib
je Wechsel (mit Kernpuffer wie bei `sync_vspace` nur bei Aenderung). Er bekommt **keine**
Kenntnis von `.tdata`, keinen Allokator, keine Relokationen.

### Autoritaet: keine — aber eine SCHRANKE, und die ist keine Autoritaetsfrage

`SETTLS` wirkt **nur auf den Aufrufer**, wie `YIELD`, `PARK`, `EXIT`. Wer seinen eigenen
Thread-Pointer setzt, gewinnt nichts: die Adresse wird von seinem Code in seinem Adressraum
dereferenziert, und was dort lesbar ist, entscheidet laengst die MMU. Deshalb **keine Cap**.

> **Und trotzdem muss der Wert geprueft werden — weil das Argument die falsche Frage beantwortet.**

Es gilt fuer das, was der Aufrufer **erreicht**. Es sagt nichts darueber, was er den **Kernel tun
laesst**: ein `WRMSR` auf `IA32_FS_BASE` mit **nicht-kanonischem** Wert loest `#GP(0)` **in Ring 0**
aus, an der Schreibstelle. `SETTLS(0x1234_5678_9ABC_DEF0)` aus einer PD **ohne jede Cap** legt damit
den Kern um. (`WRFSBASE` verhaelt sich genauso, falls dieser Weg spaeter kommt — s. E-T1.)

Geprueft wird gegen die **untere Adresshaelfte** (`caprock_abi::USER_VA_TOP = 2^47`), nicht gegen
„kanonisch": strenger, bedeutungsvoller, und ein Thread-Pointer in der oberen Haelfte ist ohnehin
sinnlos. Eigener Code `ERR_BADTLS`.

**Zwei Stellen erreichen das Register, nicht eine:** `SETTLS` und das Restaurieren. Die zweite ist
dadurch gedeckt, dass der Wert **beim Speichern** geprueft wurde — in `Tcb::tls` steht nie etwas,
das den Dispatch nicht passiert hat. Dieser Satz gehoert ausgeschrieben, sonst geht er beim
naechsten Umbau verloren; er steht bei `sync_tls`.

**Auf aarch64 gibt es das Problem nicht** — `TPIDR_EL0` nimmt jeden Wert. Dieselbe Form wie E-B1:
eine Architektur stuetzt sich auf eine Pruefung, die die andere strukturell nicht braucht, und ohne
den Grund an der Stelle liest sie sich als ueberfluessige Zeile.

Den Pointer eines **fremden** Threads zu setzen waere eine andere Sache (der Debugger-Klasse) und
wird ausdruecklich **nicht** angeboten.

---

## 2. Was zu bauen ist

### T1 · Das Register, pro Thread

* `Tcb.tls: usize` (`0` = keiner).
* `Scheduler::set_tls(tid, va)` / `tls_of(tid)`.
* `sync_tls(core, &sched)` neben `sync_fp_trap`/`sync_vspace`: schreibt `IA32_FS_BASE` (x86) bzw.
  `TPIDR_EL0` (aarch64) — **nur bei Aenderung**, mit Kernpuffer.

**Zu pruefen, bevor `FS` gewaehlt wird:** benutzt dieser Kernel `FS` oder `GS` schon fuer etwas?
(`swapgs`/GS-Basis ist auf vielen Kernen die Per-CPU-Ablage.) Steht `GS` in Gebrauch und `FS`
nicht, ist `FS` die Wahl — und sie muss **belegt** werden, nicht angenommen.

### T2 · `SYS_SETTLS`

```
x1 = VA des Thread-Pointers   (0 = abschalten)
```

Kein Cap, kein Badge, keine Ableitung. Ergebnis `OK`, oder `ERR_BADSTACK`-artig, wenn die Adresse
nicht in der eigenen VSpace liegt — **oder gar keine Pruefung**, s. „Autoritaet: keine": eine
unlesbare Adresse faultet beim ersten Zugriff im **Aufrufer**, und das ist die richtige Stelle.
Die billigere und ehrlichere Fassung ist, nicht zu pruefen und das im Kommentar zu sagen.

### T3 · Die Sektionen

`.tdata`/`.tbss` in `programs/user.ld` und `programs/user-x86.ld`, plus Symbole fuer Anfang, Ende
und Ausrichtung. Der Lader kopiert das `.tdata`-Bild **nicht** — das tut das Programm selbst
(s. T4). Der Kernel bleibt draussen.

### T4 · `libcaprock`: der Block und der Selbstzeiger

Eine Funktion, die aus einem vom Programm gestellten Puffer einen brauchbaren Thread-Pointer
macht: `.tdata` hineinkopieren, `.tbss` nullen, `TP` ans **Ende** legen, `*(TP) = TP` (der
Selbstzeiger der Variante 2), dann `SETTLS`.

**Ein Puffer je Thread, gestellt vom Programm.** Fuer einen Treiber mit zwei Threads sind das zwei
`static mut`-Bloecke — kein Allokator, keine Laufzeit.

### T5 · Der Treiber

`virtio-blk` bekommt eine `#[thread_local]`-Variable und einen zweiten Thread ueber `SYS_SPAWN`.
Das ist der Abnahmefall: **nicht** „es kompiliert", sondern „zwei Threads, zwei Werte".

---

## 3. Wie es gemessen wird

Eine Zeile `tls`, arch-neutral (beide Architekturen haben ein Thread-Pointer-Register):

| Konjunkt | sagt |
|---|---|
| `tp-gesetzt` | das Register traegt den Wert, **zurueckgelesen** aus dem Register, nicht aus dem TCB |
| `beide-liefen` | **Sprechprobe.** Fortschrittszaehler beider Threads > 0 — ohne sie sagen die uebrigen nichts, weil die Schleife nicht lief |
| `getrennt` | Thread A schreibt 0xA, Thread B 0xB, **beide lesen ihr eigenes** zurueck. **Das ist die eigentliche Aussage**: mit einem Thread allein ist eine `#[thread_local]`-Variable von einer globalen nicht zu unterscheiden |
| `ueberlebt-wechsel` | der Wert steht nach mindestens einem `YIELD`-Rundlauf noch. Trennt „gesetzt" von „gehalten" — und **nur** das misst, ob der Kernel restauriert |
| `ueberlebt-syscall` | der Wert steht nach einem Syscall **ohne** Threadwechsel noch. Das deckt genau die Luecke, die die Optimierung „MSR-Schreib nur bei Aenderung" aufmacht: ohne Wechsel wird **nichts** geschrieben, und faesst irgendein Kernelpfad `FS` an, ist der Wert weg, **ohne dass die Restaurierung je greift**. `ueberlebt-wechsel` sieht das nicht |

### Gegenproben (`tools/tls-negativ.sh`)

* **M1 — `sync_tls` tut nichts.** `getrennt=false` bei gruenem `tp-gesetzt`: der erste Thread setzt,
  der zweite laeuft auf demselben Block. *Der Zustand vor T1.*
* **M2 — gesetzt, aber nicht restauriert** (nur beim Spawn schreiben). `ueberlebt-wechsel=false`,
  `tp-gesetzt` und `getrennt` bleiben gruen — die Mutation, die zeigt, dass die beiden Konjunkte
  verschiedene Dinge messen.
* **M3 — beide Threads bekommen denselben Puffer** (Fehler im PROGRAMM, nicht im Kernel).
  `getrennt=false`, alles andere gruen. Ohne sie waere `getrennt` auch dann wahr, wenn der Kernel
  richtig ist und das Programm falsch — und die Zeile behauptete etwas ueber den Kernel, was sie
  ueber das Programm gemessen hat.

  **Dieselbe Bewegung wie M8 bei `irqmsi`:** nicht den Schluss pruefen, sondern die **Praemisse**.
  Das ist die Klasse aus todo D18, und sie ist hier von vornherein mitgebaut statt nachgereicht.
* **M4 — der MSR-Puffer wird nie invalidiert** (`sync_tls` schreibt nur beim ersten Mal).
  `ueberlebt-syscall` faellt, `tp-gesetzt` bleibt gruen — die Mutation zur Optimierung, nicht zur
  Sache.

---

## 3a. E-T1 · `FSGSBASE`: ein Gewinn, der eine Option verteuert — OFFEN

`WRMSR` **serialisiert** (ueber hundert Zyklen), `WRFSBASE` sind ein paar. Bei einem Schreib je
Wechsel ist der Unterschied real, und die Voraussetzung ist ein CPUID-Bit plus `CR4.FSGSBASE`.

**Der Haken ist keine Umsetzungsfrage:** `CR4.FSGSBASE` erlaubt auch **`wrgsbase` aus Ring 3**.
Solange `GS` ungenutzt ist, folgenlos. Sobald `GS` die Per-CPU-Ablage werden soll — die Option, die
sich dieser Plan als Nebenertrag gerade **offenhaelt** —, ist die GS-Basis beim Kerneleintritt ein
**vom Benutzer gewaehlter** Wert, und der `swapgs`-Pfad muss das aushalten. Linux hat dafuer seinen
Eintrittspfad umgebaut.

Also: **als Entscheidung fuehren, nicht als Optimierung durchwinken.** Der Gewinn ist echt, der
Preis ist eine Option, deren Wert heute niemand kennt.

---

## 4. Ausdruecklich NICHT dabei

* `__tls_get_addr`, DTV, dynamisches Laden — der **local-exec**-Modell reicht, und statisch
  gelinkte Programme bekommen ihn ohnehin.
* TLS fuer Kernel-Threads.
* Ein Allokator fuer TLS-Bloecke.
* `%gs` / eine zweite Achse.
* `set_tls` fuer fremde Threads.

---

## 5. Die Reihenfolge

1. **T1 + T2** — Register und Syscall, messbar allein ueber `tp-gesetzt` und `ueberlebt-wechsel`
   (mit zwei Threads, die noch keine `#[thread_local]`-Variable brauchen: es reicht, den Pointer
   zu setzen und zurueckzulesen).
2. **T3 + T4** — Sektionen und Selbstzeiger; ab hier uebersetzt `#[thread_local]`.
3. **T5** — der Treiber, und damit `getrennt`.
4. Die Gegenproben. **M1 zuerst**: sie ist der Zustand von heute, und wenn sie nicht rot wird,
   sagt nichts darunter etwas.
