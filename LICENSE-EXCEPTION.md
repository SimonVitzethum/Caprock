# Zusätzliche Erlaubnis: die ABI-Ausnahme

**Kein Rechtsrat.** Das ist die Struktur der Regel, wie sie allgemein verstanden wird. Vor einer
Produkt- oder Vertriebsentscheidung gehört sie vor jemanden mit Zulassung.

Caprock steht unter `AGPL-3.0-or-later` (s. `LICENSE`). Dieses Dokument gewährt eine
**zusätzliche Erlaubnis** im Sinne von (A)GPLv3 §7 — es nimmt **nichts** weg und beschränkt niemanden.

---

## Die Ausnahme

> Ein eigenständiges Programm, das ausschliesslich über die **veröffentlichte
> System-Schnittstelle** von Caprock mit dem Kernel verkehrt, gilt allein aus diesem Grund
> **nicht** als abgeleitetes Werk (*derivative work*) des Kernels, und seine Weitergabe unterliegt
> allein aus diesem Grund **nicht** der GPL.
>
> Zur veröffentlichten System-Schnittstelle zählen: die Syscall-Nummern, Ergebniscodes und
> Registerbelegungen aus `crates/caprock-abi`, die IPC-Nachrichtenformate, das Manifestformat,
> die Dienstprotokolle der mitgelieferten PDs — sowie die **Grenz-Crates** (s. `docs/grenze.md`),
> soweit sie unter einer permissiven Lizenz stehen.
>
> Die Ausnahme erstreckt sich **nicht** auf Programme, die Kernelcode enthalten, ihn statisch
> einbinden oder ihn verändern.

## Umfang: allgemein

**Die Ausnahme gilt für jeden, unbeschränkt, ohne Antrag und ohne Unterscheidung nach dem Zweck der
Nutzung** — kommerziell wie nichtkommerziell.

Das ist eine bewusste Entscheidung und nicht das Weglassen einer Einschränkung. Eine Ausnahme, die
nur einem Teil der Nutzer gilt, hätte drei Nachteile, die ihren Zweck untergraben:

* **Sie hätte nichts verboten.** GPLv3 §7 erlaubt zusätzliche *Erlaubnisse* und verbietet
  zusätzliche *Beschränkungen*. Die GPLv3 gestattet kommerzielle Nutzung ohnehin; eine
  eingeschränkte Ausnahme hätte sie nicht untersagt, sondern nur die **Klarstellung**
  vorenthalten.
* **Sie hätte Unsicherheit erzeugt, wo Sicherheit der ganze Zweck ist.** Wer erst prüfen muss, ob
  sein Programm unter die Ausnahme fällt, hat genau das Problem, das die Ausnahme beseitigen soll.
* **„Kommerziell" ist nicht scharf** — und Vagheit in einer Erlaubnis geht zulasten dessen, der sie
  gewährt, nicht dessen, der sie in Anspruch nimmt.

Das Vorbild ist dieselbe Konstruktion, die Linux für seine System-Schnittstelle gewählt hat: die
Ausnahme gilt allen, und der Copyleft-Charakter des Kerns bleibt davon unberührt.

## Was die Ausnahme NICHT tut

* Sie ändert **nichts** an der GPLv3 für Caprock selbst. Wer den Kernel oder kernnahe Crates
  verändert und weitergibt, gibt unter GPLv3 weiter.
* Sie macht **keine** Aussage über Programme, die Kernelcode enthalten oder statisch einbinden.
* Sie ist **keine** Aussage über fremden Code: eine Linux-Kompatibilitätsschicht enthält
  Linux-Code und ist damit ein abgeleitetes Werk des **Linux**-Kernels — dafür gilt GPLv2, und
  daran ändert diese Ausnahme nichts (s. das getrennte Repo `caprock-linux-compat`).

---

## Zwei Folgen, die zum Dokument gehören

**Erstens: eine Ausnahme kann nur gewähren, wer die Rechte hält.** Solange das eine Person ist, ist
sie ein Commit. Mit dem ersten gemergten fremden Beitrag hält sie jemand anders mit — und ab da
lässt sich weder umlizenzieren noch eine Ausnahme ändern, ohne alle zu fragen. Wenn je ein CLA oder
DCO kommen soll, ist der Zeitpunkt **vor** dem ersten externen Beitrag. Dieselbe Logik wie bei der
signierten Manifestfläche: die Regel muss stehen, bevor der erste Fall eintritt.

**Zweitens: unter AGPL greift das Copyleft AUCH beim Betrieb — und genau deshalb ist diese
Ausnahme jetzt tragend statt nur bequem.**

`AGPL-3.0` §13 („Remote Network Interaction") verlangt: wer eine **veränderte** Fassung betreibt
und sie über ein Netz anbietet, muss den Nutzern den **Quelltext dieser Fassung** anbieten. Die
Pflicht entsteht damit beim **Betrieb**, nicht erst bei der Weitergabe — das ist der Unterschied
zur GPLv3 und der Grund für den Wechsel.

Was das konkret trennt:

| | trifft §13 |
|---|---|
| jemand **verändert** den Kernel und betreibt ihn als Dienst | **ja** — Quelltext an die Nutzer |
| jemand betreibt Caprock **unverändert** als Dienst | nein (nichts verändert) |
| ein Kunde fährt sein **eigenes Programm** als PD darauf | **nein** — es ist kein abgeleitetes Werk, s. die Ausnahme oben |

**Die dritte Zeile ist der Grund, warum die ABI-Ausnahme jetzt tragend ist.** Ohne sie stünde für
jeden Kunden die Frage im Raum, ob sein Workload durch §13 erfasst wird — und eine PaaS, deren
Kunden erst einen Anwalt fragen müssen, ob ihr Code offengelegt werden muss, hat kein
Lizenzproblem, sondern ein Vertriebsproblem. Mit der Ausnahme ist die Antwort ein Satz.

**Der Preis, der dazugehört:** etliche Unternehmen schliessen AGPL-Software pauschal aus, ohne die
Unterscheidung oben zu prüfen. Das ist eine reale Kostenseite der Wahl und keine Rechtsfrage.
