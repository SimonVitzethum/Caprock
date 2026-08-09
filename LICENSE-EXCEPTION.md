# Zusätzliche Erlaubnis: die ABI-Ausnahme

**Kein Rechtsrat.** Das ist die Struktur der Regel, wie sie allgemein verstanden wird. Vor einer
Produkt- oder Vertriebsentscheidung gehört sie vor jemanden mit Zulassung.

SEL4Lake steht unter `GPL-3.0-or-later` (s. `LICENSE`). Dieses Dokument gewährt eine **zusätzliche
Erlaubnis** im Sinne von GPLv3 §7 — es nimmt **nichts** weg.

---

## Was zuerst klar sein muss, damit das Dokument nicht mehr verspricht, als es kann

**Die GPLv3 erlaubt kommerzielle Nutzung, immer, und das kann diese Ausnahme nicht ändern.**
GPLv3 §7 erlaubt zusätzliche *Erlaubnisse*, verbietet aber zusätzliche *Beschränkungen*. Ein Satz
wie „kommerzielle Nutzung bedarf meiner Zustimmung" wäre eine Beschränkung — mit der GPLv3
unvereinbar und vermutlich wirkungslos.

Was hier geregelt wird, ist etwas anderes und Genaueres: **die Klarstellung, dass ein Programm,
das nur die veröffentlichte ABI benutzt, kein abgeleitetes Werk des Kernels ist**, wird
unterschiedlich weit gewährt.

| | ABI-Ausnahme | GPLv3 selbst |
|---|---|---|
| nichtkommerziell | **gilt immer** | gilt |
| kommerziell, mit schriftlicher Zustimmung | **gilt** | gilt |
| kommerziell, ohne Zustimmung | **gilt nicht** | **gilt weiterhin** |

Der dritte Fall heisst also **nicht** „darf nicht". Er heisst: die Nutzung ist erlaubt, aber die
Frage *„ist mein PD ein abgeleitetes Werk?"* bleibt für diesen Nutzer **unbeantwortet**. Das ist
die tatsächliche Wirkung, und sie sollte so benannt sein statt als Verbot — ein Dokument, das ein
Verbot behauptet, das es nicht durchsetzen kann, schadet dem Urheber mehr als dem Nutzer.

## Die Ausnahme

> Ein eigenständiges Programm, das ausschliesslich über die **veröffentlichte
> System-Schnittstelle** von SEL4Lake mit dem Kernel verkehrt, gilt allein aus diesem Grund
> **nicht** als abgeleitetes Werk (*derivative work*) des Kernels, und seine Weitergabe unterliegt
> allein aus diesem Grund **nicht** der GPL.
>
> Zur veröffentlichten System-Schnittstelle zählen: die Syscall-Nummern, Ergebniscodes und
> Registerbelegungen aus `crates/sel4lake-abi`, die IPC-Nachrichtenformate, das Manifestformat,
> die Dienstprotokolle der mitgelieferten PDs — sowie die **Grenz-Crates** (s. `docs/grenze.md`),
> soweit sie unter einer permissiven Lizenz stehen.
>
> Die Ausnahme erstreckt sich **nicht** auf Programme, die Kernelcode enthalten, ihn statisch
> einbinden oder ihn verändern.

## Umfang

**Nichtkommerzielle Nutzung: die Ausnahme gilt unbeschränkt und unwiderruflich für die jeweilige
Fassung.** Sie muss nicht beantragt werden.

**Kommerzielle Nutzung: die Ausnahme gilt nur mit schriftlicher Zustimmung des Urhebers.** Ohne
sie bleibt die GPLv3 in vollem Umfang anwendbar — einschliesslich der Freiheit zur kommerziellen
Nutzung; es fehlt allein die Klarstellung oben.

### Was „kommerziell" hier heisst

Ohne Definition ist eine Erlaubnis vage, und **Vagheit in einer Erlaubnis geht zulasten dessen,
der sie gewährt** — nicht dessen, der sie in Anspruch nimmt. Deshalb, ausdrücklich:

**Kommerziell** ist jede Nutzung, die unmittelbar oder mittelbar auf geldwerten Vorteil gerichtet
ist. Darunter fallen insbesondere:

* der Betrieb als Teil eines entgeltlichen Dienstes (auch wenn die Software selbst nicht
  weitergegeben wird),
* der Vertrieb von Geräten oder Abbildern, die SEL4Lake enthalten,
* die Nutzung im Rahmen der Erwerbstätigkeit eines Unternehmens, auch intern.

**Nicht kommerziell** sind insbesondere: private Nutzung, Lehre, Forschung an
Bildungseinrichtungen, und die Nutzung durch gemeinnützige Einrichtungen — jeweils auch dann, wenn
dafür öffentliche Mittel fliessen.

**Der Grenzfall wird zugunsten des Nutzers ausgelegt:** wer im Zweifel ist, ist nichtkommerziell,
bis der Urheber widerspricht.

## Zustimmung einholen

Formlos schriftlich (E-Mail genügt) an den Urheber. Eine erteilte Zustimmung gilt für die genannte
Fassung und alle späteren, sofern sie nichts anderes sagt.

---

## Zwei Folgen, die zum Dokument gehören

**Erstens: das funktioniert nur, solange es EINEN Urheber gibt.** Eine Ausnahme kann nur gewähren,
wer die Rechte hält. Mit dem ersten gemergten fremden Beitrag hält sie jemand anders mit — und ab
da lässt sich weder umlizenzieren noch eine Ausnahme erteilen, ohne alle zu fragen. Wenn je ein
CLA oder DCO kommen soll, ist der Zeitpunkt **vor** dem ersten externen Beitrag. Dieselbe Logik wie
bei der signierten Manifestfläche: die Regel muss stehen, bevor der erste Fall eintritt.

**Zweitens: für den Betrieb greift die GPL ohnehin kaum.** Die Pflichten der GPLv3 entstehen bei
**Weitergabe**, nicht beim Betrieb. Wer SEL4Lake nur betreibt (etwa als PaaS), gibt nichts weiter
und schuldet nichts — das wäre erst unter **AGPL** anders. Ob das ein Vorteil (Hosting ohne
Copyleft-Zwang) oder eine Lücke ist, ist eine Geschäftsentscheidung; sie sollte bewusst gefallen
sein und nicht als Nebeneffekt der Wahl zwischen drei Kürzeln in `Cargo.toml`.
