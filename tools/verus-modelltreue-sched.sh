#!/usr/bin/env bash
# **Haelt das Verus-Scheduler-Modell an den echten Quelltext.** (B-7.3, Strang Scheduler)
#
# Gegenstueck zu `tools/verus-modelltreue.sh` (dort: `unlink`), aber mit einer **schwaecheren
# Aussage** -- und der Unterschied ist der Grund, warum das hier eine eigene Datei ist.
#
# ------------------------------------------------------------------------------------------------
# WARUM NICHT DIESELBE FORM WIE BEI `unlink`
# ------------------------------------------------------------------------------------------------
# `CapSpace::unlink` und die Spezifikationen `unlink1/unlink2/unlink_slots` sind eine nahezu
# woertliche Uebertragung: gleiche Verzweigung, gleiche Reihenfolge, gleiche Feldzuweisung. Dort
# ist ein 1:1-Strukturvergleich ehrlich, weil er tatsaechlich Gleichheit verlangen KANN.
#
# `Verification/scheduler/proofs/runqueue.rs` ist etwas anderes. Das Modell ist ein abstraktes
# Einkern-Mitgliedschaftsmodell: `Seq<Thread>` mit einem `in_ready`-**Flag**, sieben Uebergaenge,
# keine Zeit, keine Prioritaeten, keine Donation, keine Migration. `crates/sel4lake-sched/src/lib.rs`
# hat 20 TCB-Felder, intrusive doppelt verkettete Listen je Prioritaet, eine Bitmap, MCS-Perioden,
# Budget-Donation, ein lock-freies Thread-Directory und Zombies. Wer beides auf **dieselbe**
# Ereignisfolge reduziert und Gleichheit verlangt, bekommt entweder einen Waechter, der immer rot
# ist, oder -- schlimmer -- einen, der so grosszuegig normalisiert, dass er nicht mehr fehlschlagen
# kann. Das waere genau die Kopie-mit-Zertifikat, gegen die B-7.2 gebaut wurde.
#
# ------------------------------------------------------------------------------------------------
# WAS DIESER WAECHTER PRUEFT
# ------------------------------------------------------------------------------------------------
# Vier Schichten, jede fuer sich fehlschlagbar:
#
#   [1] FELDER.       Jedes Feld des Modell-`Thread` hat eine benannte Entsprechung in `Tcb`, und
#                     JEDES `Tcb`-Feld ist entweder eine solche Entsprechung oder steht hier
#                     ausdruecklich als „ausserhalb des Modells". Ein neues TCB-Feld schlaegt an --
#                     der Beweis kann von einem Feld, das er nicht kennt, nichts zusichern.
#
#   [2] UEBERGAENGE.  Jede `spec fn ... -> Sched` des Modells ist einer echten Funktion zugeordnet,
#                     und JEDE Stelle in `lib.rs`, die Modellzustand schreibt (`blocked`,
#                     `depleted`, `budget`, `remaining`, `used`, `priority`, `queued`, `current`,
#                     ein ganzer `Tcb`, `enqueue_ready`/`remove_from_ready`), liegt in einer
#                     Funktion, die hier benannt ist -- entweder als Modellpartner oder
#                     ausdruecklich als „vom Modell nicht erfasst". Eine neue Funktion, die
#                     Scheduler-Zustand schreibt, schlaegt an.
#
#   [3] STRUKTUR.     Beide Seiten werden auf dieselbe normalisierte Ereignisfolge reduziert
#                     (Verzweigungen mit normalisierter Bedingung, Feldzuweisungen mit Wert,
#                     Ein-/Ausreihen, Wechsel des laufenden Threads). Verglichen wird NICHT auf
#                     Gleichheit -- sondern die **Differenz** der beiden Folgen gegen die unten
#                     eingetragene, begruendete UEBERTRAGUNGSLUECKE. Aendert sich eine Seite, ohne
#                     dass sich die Luecke gleich mitaendert, schlaegt es an. Aendern sich beide
#                     Seiten gleichsinnig, bleibt es still -- das ist gewollt.
#                     Zusaetzlich: die Liste der Schreibzugriffe, die das Modell **wegabstrahiert**
#                     (`sp`, `period`, `next_refill`, Telemetriezaehler), ist je Paar eingetragen.
#                     Wegabstrahieren ist damit eine gepruefte Behauptung, kein stilles Weglassen.
#
#   [4] AUDIT.        `Scheduler::audit` ist die Laufzeitseite derselben Kopplung. Jeder
#                     Rueckgabecode mit seiner Bedingung ist eingetragen; ausserdem, WELCHE
#                     Teilaussage von `ridx`/`bidx`/`current_valid` von welchem Code getragen wird
#                     -- und welche von KEINEM. Dort sassen die Befunde B2 (behoben, Code 9),
#                     B3 und B4.
#                     Damit diese Zuordnung nicht ueber einem Text redet, der sich unbemerkt
#                     verschiebt, sind die sieben Praedikate des Modells (`runnable`, `ridx`,
#                     `bidx`, `current_valid`, `coupled`, `budget_inv`, `sched_inv`) hier
#                     eingefroren. Wer `runnable` das `!t.depleted` nimmt, schwaecht die bewiesene
#                     Aussage -- ohne dieses Register faellt das nirgends auf.
#                     Und die Gegenrichtung, seit 2026-08-03 fuer B2/B3/B4 vollstaendig: zu jedem
#                     „das hat KEINEN Audit-Code" gehoert eine Meldung, die anschlaegt, sobald es
#                     doch einen gibt. Sie haengt am REGISTER, nicht an der Beobachtung -- sonst
#                     schriee sie ab dem Tag der Behebung fuer immer, wuerde abgeschaltet und
#                     schwiege dann auch beim naechsten echten Fall (CLAUDE.md, Fallenliste).
#                     Zwei Selbsttestfaelle (`erwarte_meldung`) belegen, dass sie sprechen kann.
#
# ------------------------------------------------------------------------------------------------
# WAS DIESER WAECHTER NICHT PRUEFT -- ausdruecklich
# ------------------------------------------------------------------------------------------------
#   * Er beweist nichts. Er stellt keine Semantik-Gleichheit her und kann keine herstellen.
#   * Er prueft **keine Klammer-Schachtelung**: die Ereignisfolge ist flach. Zwei Schreibzugriffe
#     zu tauschen faellt auf; einen Block eine Ebene tiefer zu haengen, ohne die lineare Reihenfolge
#     zu aendern, faellt nicht auf.
#   * `&&`-Konjunkte werden **sortiert** (Kommutativitaet), ihre Reihenfolge ist also nicht Teil der
#     Signatur. `!`, Vergleichsoperatoren und die Menge der Konjunkte sind es sehr wohl.
#   * Lokale Variablennamen sind absorbiert (sie werden ueber ihre Herkunft an Rollen gebunden);
#     Feldnamen, Konstanten und Funktionsnamen sind es NICHT.
#   * Die eingetragene Uebertragungsluecke ist ein **eingefrorener Text**. Wer sie nach einer
#     Aenderung einfach neu einsetzt, ohne das Argument nachzuziehen, hebt den Waechter auf. Das
#     laesst sich nicht mechanisch verhindern -- deshalb steht es hier.
#
# ------------------------------------------------------------------------------------------------
# BEFUNDE beim ersten Lauf (2026-08-03) -- Stand des Registers am 2026-08-03 nach D8 und H-b/D9
# ------------------------------------------------------------------------------------------------
#   B1  BEHOBEN (D8, 2026-08-03). Stand hier als: „`unblock` reiht **ohne** `depleted`-Pruefung
#       wieder ein, das Modell setzt `in_ready: !t.depleted`". Der Waechter fuehrt den Befund
#       weiter, weil die BEDINGUNG dahinter weiter gilt -- der Eintrag steht jetzt am Paar
#       `unblock` unten, samt der Fassung, die es NICHT geworden ist (`if blocked && !depleted`).
#   B2  BEHOBEN (D8, 2026-08-03) als **Audit-Code 9**. Stand hier als: „die Richtung „in der Queue
#       ==> nicht erschoepft" hat keinen Audit-Code". Die Veraltungsmeldung dazu weiter unten ist
#       an das Register gekoppelt, nicht an die Beobachtung -- s. dort, warum das der Unterschied
#       zwischen einem Waechter und einem abgeschalteten Waechter ist.
#   B3  OFFEN. `bidx` (Restbudget <= Budget, erschoepft ==> Rest 0, budget==0 ==> nicht erschoepft)
#       hat ueberhaupt keine Laufzeitentsprechung: `audit` liest weder `budget` noch `remaining`.
#   B4  OFFEN, und H-b hat daran NICHTS geaendert (nachgesehen 2026-08-03): `pause` deplaniert den
#       laufenden Thread weiterhin NICHT (nur `blocked = true`, seit H-b davor noch
#       `budget_blocked = false`); das Modell setzt `current: None`. Der Kommentar am Modell nennt
#       das eine Abstraktion („der naechste Tick deplaniert ihn") -- bis dahin ist `current_valid`
#       im Code verletzt. Auch dieser Eintrag hat jetzt eine an das Register gekoppelte
#       Veraltungsmeldung: faengt `pause` an zu deplanieren, sagt der Waechter, dass B4 weg kann.
#   B5  OFFEN. `switch_to` (IPC-Fastpath) schreibt `blocked` an ZWEI Threads und wechselt
#       `current`, hat aber keinen Uebergang im Modell. Ebenso `exit_current`, `kill`, `spawn*`,
#       `init_core`, Migration und `record_zombie` -- letzteres seit H-b mit deutlich mehr
#       Wirkung (es loest ALLE Empfaenger einer Spende und weckt sie).
#
# ------------------------------------------------------------------------------------------------
# H-b / D9 (2026-08-03) -- WAS DAS NEUE FELD `budget_blocked` DIESEM MODELL KOSTET
# ------------------------------------------------------------------------------------------------
# H-b traegt den GRUND einer Blockade mit: `Tcb.budget_blocked` heisst „blockiert, WEIL das
# belastete Konto leer ist" -- im Unterschied zu einer Blockade aus IPC oder PAUSE. Das Feld ist
# hier als **ausserhalb des Modells** eingetragen (TCB_AUSSERHALB), NICHT als achtes Modellfeld.
#
# Der Grund ist kein Bequemlichkeitsgrund: `budget_blocked` wird ausschliesslich an Stellen
# gesetzt und gelesen, an denen ein Thread gegen ein **fremdes** Konto laeuft (`sc_donor`). Es
# gehoert damit vollstaendig zur Budget-Donation, und die ist laut ADR 0019 bereits ausserhalb --
# aus genau diesem Grund steht der Donation-Zweig in `refill` seit dem ersten Lauf so eingetragen.
# Ein Feld ins Modell zu heben, dessen einziger Zweck ein Mechanismus ist, den das Modell nicht
# kennt, hiesse: sieben neue Zeilen Spezifikation, die ueber nichts reden, was das Modell
# entscheiden kann.
#
# **Die Kehrseite, und sie gehoert benannt statt verdeckt.** Damit gilt die Liveness-Aussage des
# Modells -- `roundrobin_no_starve` („budget == 0 ==> nie erschoepft ==> bleibt einplanbar") und
# `no_lost_thread` -- fuer eine Welt OHNE Spende. In genau der Spende lagen aber D4/D5/D7: ein
# Donee, den niemand mehr weckt, ist gemessen nicht erschoepft (`depleted == false`), nicht in
# einer Liste und `audit() == 0` -- er faellt durch jede dieser Zusicherungen hindurch, weil sie
# ueber ihn gar nicht sprechen. Was H-b behebt, behebt es also UNTERHALB dieses Modells; der
# Beleg dafuer ist `tools/sched-erschoepfung-messen.sh`, nicht Verus.
# Wer das aendern will, braucht ein Modell MIT Donation (`sc_donor` als Feld, `switch_to` und
# `end_donation` als Uebergaenge) -- das ist eine eigene Arbeit, keine Zeile hier.
#
# ------------------------------------------------------------------------------------------------
# D10 (2026-08-03) -- WAS DER ZAEHLER `budget_blocked_count` DIESEM WAECHTER KOSTET
# ------------------------------------------------------------------------------------------------
# Der Weckelauf aus H-b kostete einen VOLLEN Tabellendurchlauf je aufgefuelltem Konto (gemessen:
# 10000 Iterationen bei 10000 Slots, 1000000 in EINEM Timer-Interrupt bei 100 gleichzeitigen
# Refills). Seit D10 laeuft er nur bei `budget_blocked_count > 0`. Drei Stellen aendern sich hier:
#
#   1. `refill` und `set_budget` tragen je eine zusaetzliche Zeile in der Uebertragungsluecke
#      (`WENN #budget_blocked_count > 0`). Sie ist eine KOSTENZEILE, keine Politik, und sie hat
#      je Paar ihre eigene Begruendung -- samt dem Argument, warum sie die Abbildung nicht
#      beruehrt (ist der Zaehler 0, ist auch das Konjunkt `ziel.#budget_blocked` fuer jedes
#      `ziel` falsch; der uebersprungene Lauf haette nichts getan).
#   2. `budget_blocked` wird nicht mehr direkt geschrieben, sondern ueber `set_budget_blocked`.
#      Der Normalisierer FOLGT diesem Einzeiler (`bbset` in `CODE_PAT`) -- sonst waeren die
#      eingetragenen Schreibzugriffe `ziel.#budget_blocked := true/false` einfach verschwunden,
#      und das Register haette weiter gestimmt, ohne noch etwas zu beschreiben.
#   3. `audit` bekommt Code **10**: die Nachzaehlung des Zaehlers gegen die Tabelle. Sie ist der
#      Preis fuer Punkt 1 -- ohne sie waere „der Zaehler sagt die Wahrheit" eine unbelegte
#      Voraussetzung, und genau diese Form (`depleted_count`, D8/M5) hat hier schon einmal
#      gelogen. **Ueber das Modell sagt Code 10 nichts**; er sichert eine Eigenschaft des Codes.
#
# Zwei neue Selbsttestfaelle halten das fest: der Waechter darf den Zaehler nicht mehr lesen
# (Fall a) und die Nachzaehlung darf nicht verschwinden (Fall b).
#
# Aufruf:
#   tools/verus-modelltreue-sched.sh              # pruefen + Selbsttest
#   tools/verus-modelltreue-sched.sh --nur-pruefen
#   tools/verus-modelltreue-sched.sh --selftest
if [ -z "${BASH_VERSION:-}" ]; then echo "FEHLER: braucht bash, nicht sh/dash." >&2; exit 2; fi
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"

CODE_STD="$ROOT/crates/sel4lake-sched/src/lib.rs"
MODELL_STD="$ROOT/Verification/scheduler/proofs/runqueue.rs"

# Der Selbsttest arbeitet ausschliesslich auf KOPIEN -- die Originale werden nie beschrieben.
# Das Aufraeumen haengt trotzdem an EXIT/INT/TERM und nicht nur an RETURN: ein Abbruch mitten im
# Lauf soll kein Verzeichnis mit halbmutiertem Quelltext hinterlassen, das jemand spaeter fuer
# echten Code haelt.
W=""
aufraeumen() { [ -n "$W" ] && rm -rf "$W"; W=""; }
trap aufraeumen EXIT INT TERM

waechter() {   # waechter <lib.rs> <runqueue.rs>
    python3 - "$1" "$2" <<'PY'
import io, re, sys, difflib

CODE, MODELL = sys.argv[1], sys.argv[2]

def fehler(*z):
    for s in z:
        print(s, file=sys.stderr)
    sys.exit(1)

# ================================================================================================
# DIE ZUORDNUNG -- alles Deklarative steht hier oben beisammen.
# ================================================================================================

# [1] Modellfeld -> TCB-Feld. `in_ready` ist im Code kein Feld, sondern die Liste selbst
#     (`queued` + `queues[p]` + `bitmap`); es wird als ENQ/DEQ modelliert.
FELDPAAR = {'used': 'used', 'blocked': 'blocked', 'depleted': 'depleted',
            'budget': 'budget', 'remaining': 'remaining', 'prio': 'priority',
            'in_ready': 'queued'}

# Jedes TCB-Feld, das KEINE Modellentsprechung hat -- mit dem Grund.
TCB_AUSSERHALB = {
    'gid':         'globaler Thread-Slot (Directory-Index) -- Identitaet, nicht Einplanung',
    'gen':         'Generation gegen stale Handles',
    'sp':          'gesicherter Trap-Frame -- Kontextwechsel liegt im HAL-TCB',
    'stack_base':  'Rueckgewinnung beim Beenden (Zombie/Reap, nicht modelliert)',
    'stack_len':   'dito',
    'qnext':       'intrusive Verkettung -- das Modell hat ein Flag statt einer Liste',
    'qprev':       'dito',
    'period':      'MCS-Periodenlaenge -- das Modell kennt keine Zeit',
    'next_refill': 'Refill-Zeitpunkt -- dito',
    'sc_donor':    'Budget-Donation (ADR 0019: ausserhalb)',
    'sc_donee':    'dito',
    # H-b/D9, 2026-08-03. Die Entscheidung ist im Dateikopf begruendet -- kurz: das Feld wird
    # ausschliesslich dort gesetzt und gelesen, wo ein Thread gegen ein FREMDES Konto laeuft, es
    # gehoert also ganz zur Donation, und die ist bereits ausserhalb. Der Preis dafuer steht
    # ebenfalls im Kopf: die Liveness-Aussage des Modells gilt damit fuer eine Welt ohne Spende.
    'budget_blocked':
                   'H-b: der GRUND einer Blockade („wartet auf den Refill eines FREMDEN Kontos") '
                   '-- Teil der Budget-Donation, ADR 0019: ausserhalb',
    'cyc':         'Zyklenabrechnung (B-5.1) -- Messung, nicht Einplanung',
    'stamp':       'dito',
}

# [2] Funktionen in `lib.rs`, die Modellzustand schreiben.
#     `partner`  -- hat einen Uebergang im Modell (s. PAARE).
#     `ausserhalb` -- schreibt Modellzustand, hat aber KEINEN Uebergang im Modell (Befund B5).
#     `primitiv` -- die Realisierung von `in_ready` bzw. von `pick` selbst.
CODE_PARTNER = ['block_current', 'unblock', 'pause', 'on_tick', 'refill_depleted', 'set_budget']
CODE_PRIMITIV = {
    'enqueue_ready':    'Realisierung von in_ready := true',
    'remove_from_ready':'Realisierung von in_ready := false',
    'dequeue_highest':  'Realisierung von `pick` (Auswahl der hoechsten Prioritaet -- Code 6-Gebiet)',
}
CODE_AUSSERHALB = {
    'init_core':            'Idle-Thread beim Hochlauf -- kein Uebergang im Modell',
    'spawn':                'Thread-Erzeugung -- kein Uebergang im Modell',
    'spawn_user_at':        'dito',
    'alloc_tcb':            'Slot-Belegung + gid-Vergabe -- kein Uebergang im Modell',
    'exit_current':         'Selbstbeendigung -- Zombie/Reap-Lebenszyklus (README 10)',
    'kill':                 'Fremdbeendigung -- dito',
    'record_zombie':        'dito',
    'switch_to':            'IPC-Fastpath: blockiert den Aufrufer UND macht den Server laufend, '
                            'dazu Budget-Donation -- ADR 0019 ausdruecklich ausserhalb (Befund B5)',
    'detach_for_migration': 'Migration -- ausserhalb (Einkern-Modell)',
    'attach_migrated':      'dito',
}

# [3] Die Paare Modell <-> Code samt eingefrorener Uebertragungsluecke.
#     `luecke`     -- die erwartete Differenz (`-` nur Code, `+` nur Modell, ` ` Kontext).
#     `ausserhalb` -- die Schreibzugriffe, die das Modell wegabstrahiert.
#     `grund`      -- warum die Luecke die Uebertragung nicht kaputtmacht (bzw. dass sie es tut).
PAARE = [
    dict(
        name='block_current',
        modell=[(r'pub open spec fn block_current\(', {}, None),
                (r'pub open spec fn pick\(', {'n': 'naechster'}, None)],
        code=[(r'pub fn block_current\(&mut self', {}, None)],
        luecke=[
            ' SETZE laeufer.blocked := true',
            '+DEQ laeufer',
            '+LAEUFT := keiner',
            ' DEQ naechster',
        ],
        ausserhalb=['laeufer.#sp := frame'],
        grund='Der Code verschmilzt `block_current` mit `pick`. Die zwei Modellzeilen mehr sind '
              'genau die Naht: `DEQ laeufer` ist unter `ridx` wirkungslos (der laufende Thread '
              'steht nie in einer Liste -- audit-Code 4), und `LAEUFT := keiner` wird von der '
              'unmittelbar folgenden Auswahl ueberschrieben. Der gesicherte Frame (`sp`) ist '
              'Kontextwechsel und liegt im HAL-TCB.',
    ),
    dict(
        name='unblock',
        modell=[(r'pub open spec fn unblock\(', {'i': 'ziel'}, None)],
        code=[(r'pub fn unblock\(&mut self', {}, None)],
        luecke=[
            '-WENN #aufloesbar',
            '-WENN ziel.blocked',
            '-WENN ziel.#budget_blocked',
            '-WENN konto != ziel && konto.depleted',
            ' SETZE ziel.blocked := false',
        ],
        ausserhalb=['ziel.#budget_blocked := true'],
        grund='Vier Zeilen, und sie zerfallen in zwei Paare mit sehr verschiedenem Gewicht. '
              '(a) `WENN #aufloesbar` + `WENN ziel.blocked` sind die Aufloesung des Handles und '
              'die Idempotenz; das Modell sagt in seinem Kommentar ausdruecklich, es spezifiziere '
              'nur den blockierten Fall. Das ist eine Abstraktion, keine Abweichung. '
              'BEFUND B1 IST BEHOBEN (2026-08-03, D8): hier stand '
              '`+WENN !ziel.depleted` als echte Abweichung -- der Code reihte BEDINGUNGSLOS ein, '
              'das Modell nur, wenn nicht erschoepft. Gemessen mit '
              '`tools/sched-erschoepfung-messen.sh` (D8): ein erschoepfter Thread lief danach eine '
              'volle Zeitscheibe auf leerem Konto, mit bereitstehender Alternative und `audit()==0`; '
              'erreichbar OHNE Cap ueber die IPC-Donation. `unblock` traegt den Waechter jetzt '
              'INNERHALB des Rumpfes -- die Fassung `if blocked && !depleted` waere schaedlich '
              'gewesen (RESUME verschluckt, zusammen mit dem refill-Waechter: vollstaendiges '
              'Verhungern, gemessen). Das Einreihen selbst deckt sich seitdem. '
              '(b) NEU seit H-b (2026-08-03, D9): `WENN ziel.#budget_blocked`, '
              '`WENN konto != ziel && konto.depleted` und der wegabstrahierte '
              '`ziel.#budget_blocked := true`. Alle drei reden ueber das KONTO, gegen das der '
              'Thread belastet wird (`sc_donor.unwrap_or(s)`) -- also ueber Donation, ADR 0019: '
              'ausserhalb. Das Modell hat fuer diesen Fall keinen Begriff; es kennt nur '
              '`t.depleted` AM THREAD SELBST, und genau das war D6: der D8-Waechter fragte den '
              'Thread, belastet wurde das fremde Konto. '
              'Warum das `ridx` nicht bricht: in beiden neuen Zweigen kehrt `unblock` zurueck, '
              'OHNE `blocked` zu loeschen und OHNE einzureihen -- beide Seiten der Kopplung '
              'bleiben unveraendert, der Thread ist weiter blockiert und weiter nicht in der '
              'Liste. Fuer das Modell sieht das aus wie ein `unblock`, das nichts tut. '
              'Was es aber sehr wohl beruehrt, ist LIVENESS, und die traegt dieses Modell hier '
              'nicht: dass den Thread jemand wieder weckt, haengt daran, dass '
              '`refill_depleted`/`set_budget`/`record_zombie` genau `budget_blocked` aufloesen. '
              'Bewiesen ist das nirgends, gemessen ist es (D5 6 Ticks, D7 9, D4 299).',
    ),
    dict(
        name='pause',
        modell=[(r'pub open spec fn pause\(', {'i': 'ziel'}, None)],
        code=[(r'pub fn pause\(&mut self', {}, None)],
        luecke=[
            '-WENN #aufloesbar',
            '-WENN !ziel.blocked',
            ' SETZE ziel.blocked := true',
            ' DEQ ziel',
            '+WENN LAEUFT == ziel',
            '+LAEUFT := keiner',
        ],
        ausserhalb=['ziel.#budget_blocked := false'],
        grund='Handle-Aufloesung + Idempotenz auf der Codeseite; auf der Modellseite die '
              'Deplanierung des laufenden Threads (BEFUND B4). Der Code laesst einen pausierten '
              '`current` stehen, bis der naechste Tick ihn nicht wieder einreiht -- in diesem '
              'Fenster ist `current_valid` im Code falsch. Das Modell nennt das eine Abstraktion; '
              'es ist eine, aber eine mit einem beobachtbaren Fenster. '
              'NEU seit H-b (2026-08-03, D9): der wegabstrahierte `ziel.#budget_blocked := false` '
              '-- **PAUSE UEBERNIMMT die Blockade**. Er steht VOR `if !ziel.blocked` und ist '
              'deshalb kein Teil der Verzweigung: ohne ihn waere `pause` an einem Thread, der '
              'schon auf ein leeres fremdes Konto geblockt ist, ein reines No-Op (er ist ja '
              'bereits `blocked`), und der naechste Refill des Kontos hoebe die PAUSE mit auf --'
              ' obwohl `pause` Erfolg gemeldet hat. Das ist D9/D1, gemessen: nach dem Refill '
              '`blocked = 0`, wird `current`, verbraucht Budget, `audit() == 0`. '
              'Im Modell ist das unsichtbar, weil `budget_blocked` dort nicht existiert (Donation, '
              'ADR 0019) -- die Ereignisfolge des Modells bleibt Zeile fuer Zeile dieselbe. '
              'BEFUND B4 ist davon NICHT beruehrt und bleibt offen: die Zeile schreibt kein '
              '`current`. Sie macht die Abstraktion des Modells auch nicht schlimmer, aber sie '
              'macht das beobachtbare Fenster laenger begruendungsbeduerftig, denn ein pausierter '
              '`current` haelt jetzt zusaetzlich seine Nicht-Weckbarkeit fest.',
    ),
    dict(
        name='tick_charge',
        modell=[(r'pub open spec fn tick_charge\(', {}, None)],
        code=[(r'pub fn on_tick\(&mut self', {}, None)],
        luecke=[
            '-WENN tick',
            '-WENN #depleted_count > 0',
            '-WENN LAEUFT != keiner',
            '-MERKER MERKER1 := !laeufer.blocked',
            '-WENN konto.budget > 0 && tick',
            '-SETZE konto.remaining := konto.remaining - 1',
            '-WENN konto.remaining == 0',
            '-SETZE konto.depleted := true',
            '-MERKER MERKER1 := false',
            '-WENN !laeufer.blocked && konto != laeufer',
            '-SETZE laeufer.blocked := true',
            '-WENN MERKER1',
            '-ENQ laeufer',
            '-DEQ naechster',
            '-LAEUFT := naechster',
            '+WENN laeufer.remaining == 1',
            '+SETZE laeufer.remaining := 0',
            '+SETZE laeufer.depleted := true',
            '+DEQ laeufer',
            '+LAEUFT := keiner',
            '+SONST',
            '+SETZE laeufer.remaining := laeufer.remaining - 1',
        ],
        ausserhalb=['#now', 'laeufer.#sp := frame',
                    'konto.#next_refill := #now + konto.#period', '#depleted_count', '#depletions',
                    'laeufer.#budget_blocked := true'],
        grund='HIER DECKT SICH NICHTS -- und das ist die ehrliche Auskunft. `on_tick` verschmilzt '
              'vier Dinge: den Refill-Anstoss, die Budgetbelastung, das Wiedereinreihen des '
              'laufenden Threads und `pick`. Das Modell beschreibt nur das zweite, und auch das in '
              'anderer Form: es verzweigt VOR der Zuweisung (`remaining == 1`), der Code danach '
              '(`saturating_sub` und dann `== 0`) -- gleiche Wirkung, andere Struktur. Dazu '
              'belastet der Code das **Konto** (`sc_donor.unwrap_or(cur)`), das Modell immer den '
              'laufenden Thread; Donation ist laut ADR 0019 ausserhalb. Fuer dieses Paar leistet '
              'Schicht [3] nur noch, was ein eingefrorener Text leisten kann: JEDE Aenderung an '
              'einer der beiden Seiten faellt auf und verlangt eine neue Herleitung. '
              'ZWEI ZEILEN sind seit H-b neu (2026-08-03, D9), beide im Donee-Zweig '
              '(`konto != laeufer`), also im bereits ausserhalb liegenden Teil: '
              '(a) `WENN !laeufer.blocked && konto != laeufer` statt `WENN konto != laeufer`. '
              'Ohne den Konjunkt schriebe `on_tick` eine Blockade aus IPC oder PAUSE still in eine '
              'Budget-Blockade um -- und der naechste Refill des Kontos hoebe sie mit auf. Der '
              'Zusatz sagt: nur wer nicht schon aus einem ANDEREN Grund blockiert ist, wird hier '
              'blockiert. (b) der wegabstrahierte `laeufer.#budget_blocked := true`: der Grund '
              'wird mitgeschrieben, damit der Wecker spaeter BENANNT ist. Vor H-b stand hier ein '
              'blankes `blocked = true` -- und `refill_depleted` hob es an einem Thread wieder auf, '
              'dessen Blockade es nicht gesetzt hatte (D9/D1) bzw. gar nicht mehr auf (D5/D4/D7). '
              'Fuer das Modell aendert sich dadurch nichts: es kennt weder `konto != laeufer` noch '
              '`budget_blocked`; seine Ereignisfolge ist Zeile fuer Zeile dieselbe geblieben.',
    ),
    dict(
        name='refill',
        modell=[(r'pub open spec fn refill\(', {'i': 'ziel'}, None)],
        code=[(r'fn refill_depleted\(&mut self', {}, None)],
        luecke=[
            '-WENN #now >= ziel.#next_refill && ziel.budget > 0 && ziel.depleted && ziel.used',
            ' SETZE ziel.remaining := ziel.budget',
            ' SETZE ziel.depleted := false',
            '-WENN #budget_blocked_count > 0',
            '-WENN ziel != ziel && ziel.#budget_blocked && ziel.#sc_donor == Some(ziel) && ziel.used',
            '-SETZE ziel.blocked := false',
            ' ENQ ziel',
            '-WENN donee != ziel',
            '-MERKER MERKER1 := donee',
            '-SONST',
            '-WENN !ziel.blocked && LAEUFT != ziel',
            '-ENQ ziel',
        ],
        ausserhalb=['#depleted_count', '#refills', 'ziel.#budget_blocked := false'],
        grund='Der Kern deckt sich Zeile fuer Zeile (`remaining := budget`, `depleted := false`, '
              'einreihen). Die Luecke ist (a) die Ausloesebedingung -- das Modell kennt keine Zeit '
              'und nimmt `refill` als bereits ausgeloest an (seine Vorbedingungen `used`, '
              '`depleted`, `budget > 0` stehen im `requires`, nicht im Rumpf) -- und (b) der '
              'Donation-Zweig: laeuft ein Donee gegen das Konto, wird DER wieder bereit. Beides '
              'ausserhalb (ADR 0019). '
              '(c) seit D8: `WENN !ziel.blocked && LAEUFT != ziel`. Das Modell kennt '
              'kein `pause` im Refill-Pfad, der Code muss es kennen -- BEFUND D8/M4, gemessen: '
              'ohne diesen Waechter reihte der Refill einen PAUSIERTEN Thread wieder ein, er wurde '
              '`current` und verbrauchte eine Zeitscheibe, `audit()==0`. PAUSE hielt also nicht, '
              'und dafuer brauchte es nicht einmal ein `unblock`. Der `LAEUFT`-Teil verhindert '
              'zusaetzlich Audit-Code 4 (`current` steht zugleich in einer Ready-Liste). '
              '(d) NEU seit H-b (2026-08-03, D9) -- und hier stand bis heute „OFFEN und '
              'ausdruecklich NICHT geprueft: der Donee-Zweig setzt `donee.blocked` weiterhin '
              'BEDINGUNGSLOS zurueck". Das ist gemessen und behoben. Der `match sc_donee`-Zweig '
              'weckt jetzt NIEMANDEN mehr (`let _ = d;` -- daher `MERKER MERKER1 := donee` in der '
              'Luecke, eine Zuweisung ohne Wirkung); davor steht ein LAUF ueber alle TCBs, der '
              'genau die weckt, die `budget_blocked` sind UND `sc_donor == slot` fuehren. '
              'Der Grund ist D5: die Spende ist ein **Stapel** (fs -> Blockdienst -> Treiber), '
              '`sc_donee` nur seine Spitze -- der zweite CALL ueberschreibt sie, das innere REPLY '
              'loescht sie, und der Zweig weckte danach den Falschen bzw. gar keinen (0 Ticks in '
              '3 Perioden, `blocked = 1`, `audit() == 0`, ohne jedes Privileg herstellbar). '
              'Die Zeile `WENN ziel != ziel && ...` ist KEIN Tippfehler und keine tote Bedingung: '
              'der Normalisierer bindet sowohl die aeussere Schleife (`for slot`) als auch die '
              'innere (`for d`) an die Rolle `ziel`, weil beide dieselbe Herkunft haben '
              '(`for _ in 0..self.tcbs.len()`). Im Quelltext steht `d != slot`. Wer das '
              'auseinanderziehen will, braucht eine zweite Rolle im Normalisierer -- solange sie '
              'fehlt, steht der Hinweis hier, damit die Zeile nicht als Befund missverstanden '
              'wird. '
              'Das Modell traegt von alldem NICHTS: sein `refill` weckt genau einen Thread, den '
              'erschoepften selbst. Der ganze Lauf liegt in der Donation und damit ausserhalb '
              '(ADR 0019) -- was er behebt, belegt `tools/sched-erschoepfung-messen.sh` (D5 auf '
              '6 Ticks, D4 auf 299, D7 auf 9), nicht Verus. '
              '(e) NEU seit D10 (2026-08-03): `WENN #budget_blocked_count > 0` VOR dem Lauf. '
              'Das ist keine Politik, sondern eine KOSTENZEILE, und sie hat ihr eigenes Argument. '
              'Gemessen (`sched-erschoepfung-messen.sh`, L-Reihe): der Lauf kostet einen vollen '
              'Tabellendurchlauf je aufgefuelltem Konto -- 10000 Iterationen bei 10000 Slots, und '
              '1000000 in EINEM Timer-Interrupt, sobald 100 Konten im selben Tick auffuellen '
              '(erreichbar: die Periode legt `next_refill` fest, und `attach_migrated` rechnet sie '
              'auf die eigene Uhr um -- L3a..L3d). Warum die Zeile die ABBILDUNG nicht beruehrt: '
              'sie ist genau dann falsch, wenn KEIN Thread `budget_blocked` traegt, und dann ist '
              'auch das Konjunkt `ziel.#budget_blocked` der Zeile darunter fuer jedes `ziel` '
              'falsch -- der uebersprungene Lauf haette nichts getan. Das gilt aber NUR, solange '
              'der Zaehler die Wahrheit sagt; deshalb zaehlt `audit` ihn nach (Code 10, s. [4]). '
              'Ohne diese Nachzaehlung waere die Zeile eine unbelegte Voraussetzung -- und genau '
              'diese Form (`depleted_count`, D8/M5) hat in diesem Projekt schon einmal gelogen. '
              'Gegengeprueft mit der Fassung `ohne-Zaehler`: alle 208 Verhaltensmesswerte '
              'unveraendert, nur die Kosten fallen (L3a/L3d 1000000 -> 0, L3b -> 10000).',
    ),
    dict(
        name='set_budget',
        modell=[(r'pub open spec fn set_budget\(', {'i': 'ziel'}, 'b')],
        code=[(r'pub fn set_budget\(&mut self', {}, 'budget')],
        luecke=[
            '-WENN #aufloesbar',
            '-MERKER MERKER1 := ziel.depleted',
            ' SETZE ziel.budget := PARAM',
            ' SETZE ziel.depleted := false',
            '-WENN MERKER1',
            '-WENN #budget_blocked_count > 0',
            '-WENN ziel != ziel && ziel.#budget_blocked && ziel.#sc_donor == Some(ziel) && ziel.used',
            '-SETZE ziel.blocked := false',
            '-ENQ ziel',
            ' WENN !ziel.blocked && LAEUFT != ziel',
            ' ENQ ziel',
            '-SONST',
        ],
        ausserhalb=['ziel.#period := period.max(1)', 'ziel.#next_refill := #now + period',
                    '#depleted_count', 'ziel.#budget_blocked := false'],
        grund='Die drei Zuweisungen und die Einreihbedingung decken sich. Die Luecke ist die '
              'Handle-Aufloesung und der zusaetzliche Waechter `WENN war_erschoepft`: der Code '
              'reiht NUR dann wieder ein, wenn der Thread vorher erschoepft war. Das ist nur '
              'deshalb dasselbe, WEIL `ridx` gilt -- ein nicht erschoepfter, nicht blockierter, '
              'nicht laufender Thread steht bereits in einer Liste, `enqueue_ready` waere ein '
              'No-Op. Die Aequivalenz haengt also an der Invariante, die hier gerade bewiesen '
              'wird. Genau diese Art Kreisschluss ist der Grund, warum die Luecke aufgeschrieben '
              'gehoert statt wegnormalisiert. '
              'NEU seit H-b (2026-08-03, D9): derselbe Lauf ueber alle TCBs wie in `refill`, in '
              'demselben `war_erschoepft`-Zweig, plus der wegabstrahierte '
              '`ziel.#budget_blocked := false`. Zur Doppelrolle `ziel != ziel` s. das Paar '
              '`refill` -- es ist dieselbe Normalisierung, nicht ein zweiter Befund. '
              'Der Grund ist D7 und er ist praeziser als „auch hier wecken": `set_budget` loescht '
              '`depleted` am Konto und senkt `depleted_count`. Damit verschwindet der ANLASS, aus '
              'dem `refill_depleted` diesen Slot je wieder anfassen wuerde -- wer auf dieses Konto '
              'geblockt war, verliert seinen Wecker genau hier, ohne dass ihn jemand aufgeweckt '
              'haette. Gemessen: 0 Ticks in 3 Perioden, `audit() == 0`; mit dem Lauf 9 Ticks. '
              'Dieselbe Form wie `record_zombie` (dort stirbt das Konto ganz, D4). '
              'Das Modell traegt auch das nicht: sein `set_budget` fasst genau `i` an. Der Lauf '
              'liegt in der Donation, ADR 0019: ausserhalb. '
              'NEU seit D10 (2026-08-03): `WENN #budget_blocked_count > 0` VOR dem Lauf -- '
              'dasselbe Argument wie in `refill`, aber mit einer eigenen Zahl: `set_budget` auf '
              'ein erschoepftes Konto kostete 10000 Iterationen bei 10000 Slots (L5), jetzt 0. '
              'Diese Stelle liegt NICHT im Tick-Pfad; sie ist trotzdem mitgezogen, weil sonst '
              'zwei Laeufe mit derselben Frage verschieden begruendet waeren -- und ein Waechter, '
              'der zwei gleiche Faelle verschieden fuehrt, ist der Anfang einer Beschriftung, die '
              'neben der Sache herlaeuft.',
    ),
]

# [4] `Scheduler::audit` -- Rueckgabecode und Bedingung, eingefroren.
AUDIT_QUEUE = [
    (5, '((self.bitmap >> p) & 1 == 1) != (q.count > 0)'),
    (1, '#tcbs.get schlaegt fehl'),
    (1, '!t.used'),
    (2, 't.blocked'),
    (9, 't.depleted'),  # seit 2026-08-03 (D8/B2): die Gegenrichtung zu Code 7
    (6, 't.queued as usize != p || t.priority as usize != p'),
    (3, 't.qprev != prev'),
    (4, 'self.current == Some(s)'),
    (5, 'n > q.count'),
    (5, 'n != q.count || q.tail != prev'),
]
AUDIT_REST = [
    (8, '#dir_load passt nicht'),
    (7, '!t.blocked && !t.depleted && self.current != Some(local) && t.queued == NOT_QUEUED'),
    # Seit 2026-08-03 (D10). Code 10 ist die NACHZAEHLUNG von `budget_blocked_count` gegen die
    # Tabelle -- und sie ist der Preis dafuer, dass `refill_depleted`/`set_budget` den teuren
    # Weckelauf jetzt an einem Zaehler aufhaengen. Ohne sie waere der Waechter im Code eine
    # unbelegte Voraussetzung: luegt der Zaehler nach oben, laeuft der Scan wieder in jedem Tick
    # (die `depleted_count`-Form aus D8/M5); luegt er nach unten, bleibt ein Donee liegen, den
    # niemand mehr weckt (die D5-Form). Beide Richtungen waren vorher unbeobachtbar.
    # **Ueber das Modell sagt Code 10 nichts** -- `budget_blocked` ist als ausserhalb eingetragen
    # (ADR 0019, Donation). Er sichert eine Eigenschaft des CODE, nicht der Abbildung.
    (10, 'bb != self.budget_blocked_count'),
]
# Die Praedikate, ueber die INVARIANTE unten redet -- eingefroren. Ohne das waere die Tabelle
# darunter eine Behauptung ueber einen Text, der sich unbemerkt aendern kann: wer `runnable` das
# `!t.depleted` nimmt, schwaecht die bewiesene Aussage, ohne dass irgendeine Zeile hier anschlaegt.
PRAEDIKATE = {
    'runnable':
        'pub open spec fn runnable(t: Thread) -> bool { t.used && !t.blocked && !t.depleted }',
    'ridx':
        'pub open spec fn ridx(s: Sched, i: int) -> bool { s.threads[i].in_ready <==> '
        '(runnable(s.threads[i]) && s.current != Some(i as nat)) }',
    'bidx':
        'pub open spec fn bidx(s: Sched, i: int) -> bool { (s.threads[i].budget > 0 ==> '
        's.threads[i].remaining <= s.threads[i].budget) && (s.threads[i].depleted ==> '
        's.threads[i].remaining == 0) && (s.threads[i].budget == 0 ==> !s.threads[i].depleted) }',
    'current_valid':
        'pub open spec fn current_valid(s: Sched) -> bool { s.current is Some ==> { let c = '
        's.current->Some_0 as int; 0 <= c < s.threads.len() && s.threads[c].used && '
        '!s.threads[c].blocked && !s.threads[c].depleted } }',
    'coupled':
        'pub open spec fn coupled(s: Sched) -> bool { forall|i: int| #![trigger s.threads[i]] '
        '0 <= i < s.threads.len() ==> ridx(s, i) }',
    'budget_inv':
        'pub open spec fn budget_inv(s: Sched) -> bool { forall|i: int| #![trigger s.threads[i]] '
        '0 <= i < s.threads.len() ==> bidx(s, i) }',
    'sched_inv':
        'pub open spec fn sched_inv(s: Sched) -> bool { coupled(s) && budget_inv(s) && '
        'current_valid(s) }',
}
# Welche Teilaussage der Modellinvariante traegt welcher Audit-Code? `None` = KEINER (Befund).
INVARIANTE = [
    ('ridx: in_ready ==> used',            1,    None),
    ('ridx: in_ready ==> !blocked',        2,    None),
    ('ridx: in_ready ==> nicht laufend',   4,    None),
    ('ridx: in_ready ==> !depleted',       9,    None),  # B2 behoben 2026-08-03 (D8): Code 9
    ('ridx: lauffaehig ==> in_ready',      7,    None),
    ('bidx: remaining <= budget',          None, 'BEFUND B3: `audit` liest `budget`/`remaining` nie'),
    ('bidx: depleted ==> remaining == 0',  None, 'BEFUND B3'),
    ('bidx: budget == 0 ==> !depleted',    None, 'BEFUND B3'),
    ('current_valid',                      None, 'BEFUND B4: `pause` laesst einen blockierten '
                                                 '`current` stehen; kein Audit-Code dafuer'),
]

# ================================================================================================
# Werkzeug
# ================================================================================================

def lies(p):
    try:
        return io.open(p, encoding='utf-8').read()
    except OSError as e:
        fehler("FEHLER: %s nicht lesbar (%s). Ein Waechter ohne Eingabe ist kein Ergebnis." % (p, e))

def strippe(z):
    z = re.sub(r'"(?:[^"\\]|\\.)*"', '""', z)   # Strings zuerst: sonst reisst ein `//` darin
    return re.sub(r'//.*$', '', z)              # den Zeilenrest weg

def rumpf(text, muster, pfad):
    """Rumpf ab der ersten Zeile, die `muster` trifft, bis die Klammertiefe wieder 0 ist --
    als EINE Zeile mit normalisiertem Weissraum."""
    zeilen = text.splitlines()
    for k, z in enumerate(zeilen):
        if re.search(muster, z):
            raus, tiefe, begonnen = [], 0, False
            for z2 in zeilen[k:]:
                o = strippe(z2)
                raus.append(o)
                tiefe += o.count('{') - o.count('}')
                if o.count('{'):
                    begonnen = True
                if begonnen and tiefe <= 0:
                    return re.sub(r'\s+', ' ', ' '.join(raus)).strip()
            fehler("FEHLER: Rumpf zu %r in %s nicht geschlossen." % (muster, pfad))
    fehler("FEHLER: %r in %s NICHT GEFUNDEN." % (muster, pfad),
           "        Der Waechter liest ins Leere -- das ist kein bestandener Test, sondern ein",
           "        kaputter Waechter. Wurde die Funktion umbenannt oder entfernt?")

def strukturfelder(text, muster, pfad):
    m = re.search(muster, text)
    if not m:
        fehler("FEHLER: Struktur %r in %s NICHT GEFUNDEN." % (muster, pfad))
    i = text.index('{', m.start())
    tiefe, j = 0, i
    while j < len(text):
        if text[j] == '{':
            tiefe += 1
        elif text[j] == '}':
            tiefe -= 1
            if tiefe == 0:
                break
        j += 1
    koerper = re.sub(r'//.*$', '', text[i + 1:j], flags=re.M)
    return re.findall(r'^\s*(?:pub\s+)?(\w+)\s*:', koerper, flags=re.M)

def bis_zu(s, i, auf, zu):
    tiefe = 0
    while i < len(s):
        if s[i] == auf:
            tiefe += 1
        elif s[i] == zu:
            tiefe -= 1
            if tiefe == 0:
                return i + 1
        i += 1
    fehler("FEHLER: unbalancierte Klammern beim Zerlegen -- der Waechter kann nicht urteilen.")

def top_split(s, trenner=','):
    teile, tiefe, akk = [], 0, ''
    for c in s:
        if c in '([{':
            tiefe += 1
        elif c in ')]}':
            tiefe -= 1
        if c == trenner and tiefe == 0:
            teile.append(akk); akk = ''
        else:
            akk += c
    if akk.strip():
        teile.append(akk)
    return [t.strip() for t in teile]

# ================================================================================================
# Der Normalisierer. Beide Seiten werden auf dieselbe Ereignisfolge abgebildet:
#
#   WENN <bedingung>            -- Verzweigung (Konjunkte sortiert, `!`/Vergleiche bleiben)
#   SONST                       -- der andere Zweig
#   SETZE <rolle>.<feld> := <w> -- Zuweisung an ein Modellfeld
#   MERKER <n> := <w>           -- lokale Hilfsvariable (positionsbenannt, Name absorbiert)
#   ENQ <rolle> / DEQ <rolle>   -- in_ready := true / false
#   LAEUFT := <rolle|keiner>    -- Wechsel des laufenden Threads
#
# Rollen: `laeufer` (der laufende), `ziel` (der benannte), `naechster` (der gewaehlte),
#         `konto` (das belastete SC-Konto), `donee` (der geliehene Laeufer).
# Sie werden aus der HERKUNFT gebunden (`self.current.expect`, `self.resolve`, `dequeue_highest`,
# ...) -- deshalb ist ein Umbenennen einer lokalen Variablen wirkungslos.
# ================================================================================================

class Kontext:
    def __init__(self, rollen, param=None):
        self.rollen = dict(rollen)
        self.param = param
        self.merker = {}
    def rolle(self, v):
        return self.rollen.get(v, '?' + v)
    def merk(self, v):
        if v not in self.merker:
            self.merker[v] = 'MERKER%d' % (len(self.merker) + 1)
        return self.merker[v]

def normfeld(f):
    UMKEHR = {v: k for k, v in FELDPAAR.items()}
    if f in UMKEHR:
        return UMKEHR[f]
    if f in FELDPAAR:
        return f
    return '#' + f     # ausserhalb des Modells -- bleibt sichtbar, wird nicht verschluckt

def normausdruck(a, k):
    a = a.strip()
    a = re.sub(r'\s+as\s+(?:nat|int|usize|u32|u64|u8|u16)\b', '', a)
    a = re.sub(r'\.saturating_sub\(\s*1\s*\)', ' - 1', a)
    a = re.sub(r'self\.tcbs\[\s*(\w+)\s*\]\.(\w+)',
               lambda m: k.rolle(m.group(1)) + '.' + normfeld(m.group(2)), a)
    a = re.sub(r'\b(\w+)\.(\w+)\b',
               lambda m: (k.rolle(m.group(1)) + '.' + normfeld(m.group(2)))
               if m.group(1) in k.rollen else m.group(0), a)
    a = re.sub(r'(?:self|s)\.current\s*(==|!=)\s*Some\(\s*(\w+)\s*\)',
               lambda m: 'LAEUFT %s %s' % (m.group(1), k.rolle(m.group(2))), a)
    a = re.sub(r'(?:self|s)\.current\b', 'LAEUFT', a)
    a = re.sub(r'self\.(\w+)', lambda m: '#' + m.group(1), a)
    a = re.sub(r'\b(\w+)\b', lambda m: k.rolle(m.group(1)) if m.group(1) in k.rollen else m.group(0), a)
    a = re.sub(r'\b(\w+)\b', lambda m: k.merker[m.group(1)] if m.group(1) in k.merker else m.group(0), a)
    if k.param:
        a = re.sub(r'\b%s\b' % re.escape(k.param), 'PARAM', a)
    a = re.sub(r'^\((.*)\)$', r'\1', a.strip())
    return re.sub(r'\s+', ' ', a).strip()

def normbed(b, k):
    teile = [t for t in (normausdruck(x, k) for x in top_split(b, '&')) if t]
    return ' && '.join(sorted(teile))

CODE_PAT = re.compile(r'''
    (?P<b_cur_e>let\s+(?P<bce>\w+)\s*=\s*self\.current\.expect\s*\()
  | (?P<b_cur_i>if\s+let\s+Some\(\s*(?P<bci>\w+)\s*\)\s*=\s*self\.current\b)
  | (?P<b_ziel_l>let\s+Some\(\s*(?P<bzl>\w+)\s*\)\s*=\s*self\.resolve\s*\()
  | (?P<b_ziel_i>if\s+let\s+Some\(\s*(?P<bzi>\w+)\s*\)\s*=\s*self\.resolve\s*\()
  | (?P<b_konto>let\s+(?P<bk>\w+)\s*=\s*self\.tcbs\[\s*\w+\s*\]\.sc_donor\.unwrap_or\s*\()
  | (?P<b_next_l>let\s+(?P<bnl>\w+)\s*=\s*self\s*\.\s*dequeue_highest\s*\(\s*\))
  | (?P<b_next_m>match\s+self\.dequeue_highest\s*\(\s*\)\s*\{\s*Some\(\s*(?P<bnm>\w+)\s*\))
  | (?P<b_slot>for\s+(?P<bfs>\w+)\s+in\s+0\.\.self\.tcbs\.len\s*\(\s*\))
  | (?P<a_donee>Some\(\s*(?P<bd>\w+)\s*\)\s*if\s+(?P=bd)\s*!=\s*(?P<bd2>\w+)\s*=>)
  | (?P<a_sonst>_\s*=>)
  | (?P<enq>self\.enqueue_ready\s*\(\s*(?P<eq>\w+)\s*\))
  | (?P<deq>self\.remove_from_ready\s*\(\s*(?P<dq>\w+)\s*\))
  | (?P<cur_set>self\.current\s*=(?!=)\s*(?P<cv>Some\(\s*\w+\s*\)|None))
  | (?P<tcb_ganz>self\.tcbs\[\s*(?P<tw>\w+)\s*\]\s*=(?!=)\s*(?P<twv>[^;]+))
  | (?P<setf>self\.tcbs\[\s*(?P<sft>\w+)\s*\]\.(?P<sff>\w+)\s*=(?!=)\s*(?P<sfv>[^;]+))
  | (?P<bbset>self\.set_budget_blocked\s*\(\s*(?P<bbt>\w+)\s*,\s*(?P<bbv>true|false)\s*\))
  | (?P<selfop>self\.(?P<so>\w+)\s*(?:\+=|-=|=(?!=))\s*[^;]+)
  | (?P<els>\}\s*else\s*\{)
  | (?P<iff>if\s+(?P<ic>[^{]+?)\s*\{)
  | (?P<letm>let\s+(?:mut\s+)?(?P<lm>\w+)\s*=(?!=)\s*(?P<lmv>[^;]+))
  | (?P<setm>\b(?P<am>[a-z_][a-z0-9_]*)\s*=(?!=)\s*(?P<amv>[^;]+))
''', re.X)

def code_ereignisse(text, rollen, param):
    k = Kontext(rollen, param)
    ev, ausserhalb, pos = [], [], 0
    while True:
        m = CODE_PAT.search(text, pos)
        if not m:
            break
        pos = m.end()
        if m.group('b_cur_e'):
            k.rollen[m.group('bce')] = 'laeufer'
        elif m.group('b_cur_i'):
            k.rollen[m.group('bci')] = 'laeufer'
            ev.append('WENN LAEUFT != keiner')
        elif m.group('b_ziel_l') or m.group('b_ziel_i'):
            k.rollen[m.group('bzl') or m.group('bzi')] = 'ziel'
            ev.append('WENN #aufloesbar')
        elif m.group('b_konto'):
            k.rollen[m.group('bk')] = 'konto'
        elif m.group('b_next_l'):
            k.rollen[m.group('bnl')] = 'naechster'
            ev.append('DEQ naechster')
        elif m.group('b_next_m'):
            k.rollen[m.group('bnm')] = 'naechster'
            ev.append('DEQ naechster')
        elif m.group('b_slot'):
            k.rollen[m.group('bfs')] = 'ziel'
        elif m.group('a_donee'):
            k.rollen[m.group('bd')] = 'donee'
            ev.append('WENN donee != %s' % k.rolle(m.group('bd2')))
        elif m.group('a_sonst'):
            ev.append('SONST')
        elif m.group('enq'):
            ev.append('ENQ %s' % k.rolle(m.group('eq')))
        elif m.group('deq'):
            ev.append('DEQ %s' % k.rolle(m.group('dq')))
        elif m.group('cur_set'):
            v = m.group('cv')
            ev.append('LAEUFT := keiner' if v == 'None'
                      else 'LAEUFT := %s' % k.rolle(re.search(r'Some\(\s*(\w+)', v).group(1)))
        elif m.group('tcb_ganz'):
            ev.append('SETZE %s.* := %s' % (k.rolle(m.group('tw')), normausdruck(m.group('twv'), k)))
        elif m.group('setf'):
            f, ziel = m.group('sff'), '%s.%s' % (k.rolle(m.group('sft')), normfeld(m.group('sff')))
            wert = normausdruck(m.group('sfv'), k)
            (ev if f in FELDPAAR.values() else ausserhalb).append(
                ('SETZE %s := %s' if f in FELDPAAR.values() else '%s := %s') % (ziel, wert))
        elif m.group('bbset'):
            # D10 (2026-08-03): `budget_blocked` wird nicht mehr direkt geschrieben, sondern
            # ueber `set_budget_blocked`, weil dort der Zaehler `budget_blocked_count`
            # mitlaeuft. Der Waechter FOLGT diesem Einzeiler, statt die Schreibstelle aus dem
            # Register verschwinden zu lassen -- sonst haette das Verlegen hinter einen Helfer
            # eine eingetragene Wirkung stillschweigend getilgt, und das Register haette
            # weiterhin gestimmt, ohne dass es noch etwas beschreibt.
            ausserhalb.append('%s.#budget_blocked := %s'
                              % (k.rolle(m.group('bbt')), m.group('bbv')))
        elif m.group('selfop'):
            ausserhalb.append('#%s' % m.group('so'))
        elif m.group('els'):
            ev.append('SONST')
        elif m.group('iff'):
            ev.append('WENN %s' % normbed(m.group('ic'), k))
        elif m.group('letm'):
            ev.append('MERKER %s := %s' % (k.merk(m.group('lm')), normausdruck(m.group('lmv'), k)))
        elif m.group('setm') and m.group('am') in k.merker:
            ev.append('MERKER %s := %s' % (k.merker[m.group('am')], normausdruck(m.group('amv'), k)))
    return ev, ausserhalb

MODELL_PAT = re.compile(r'''
    (?P<b_cur>let\s+(?P<bc>\w+)\s*=\s*s\.current->Some_0\s+as\s+int)
  | (?P<b_t>let\s+(?P<bt>\w+)\s*=\s*s\.threads\[\s*(?P<bti>\w+)\s*\])
  | (?P<upd>s\.threads\.update\(\s*(?P<ui>\w+)\s*,\s*Thread\s*\{)
  | (?P<curf>current:\s*)
  | (?P<els>\}\s*else\s*\{)
  | (?P<iff>if\s+(?P<ic>[^{]+?)\s*\{)
''', re.X)

def modell_ereignisse(text, rollen, param):
    k = Kontext(rollen, param)
    ev, pos = [], 0
    while True:
        m = MODELL_PAT.search(text, pos)
        if not m:
            break
        pos = m.end()
        if m.group('b_cur'):
            k.rollen[m.group('bc')] = 'laeufer'
        elif m.group('b_t'):
            k.rollen[m.group('bt')] = k.rolle(m.group('bti'))
        elif m.group('upd'):
            rolle = k.rolle(m.group('ui'))
            ende = bis_zu(text, m.end() - 1, '{', '}')
            pos = ende
            for f in top_split(text[m.end():ende - 1]):
                if f.startswith('..'):
                    continue
                name, _, wert = f.partition(':')
                name, wert = name.strip(), wert.strip()
                if name == 'in_ready':
                    w = normausdruck(wert, k)
                    if w == 'true':
                        ev.append('ENQ %s' % rolle)
                    elif w == 'false':
                        ev.append('DEQ %s' % rolle)
                    else:
                        ev.append('WENN %s' % normbed(wert, k))
                        ev.append('ENQ %s' % rolle)
                else:
                    ev.append('SETZE %s.%s := %s' % (rolle, normfeld(name), normausdruck(wert, k)))
        elif m.group('curf'):
            rest, tiefe, e = text[m.end():], 0, 0
            for e, c in enumerate(rest):
                if c in '([{':
                    tiefe += 1
                elif c in ')]}':
                    if tiefe == 0:
                        break
                    tiefe -= 1
                if c == ',' and tiefe == 0:
                    break
            wert = rest[:e].strip().rstrip(',').strip()
            pos = m.end() + e
            mm = re.match(r'if\s+(.+?)\s*\{\s*(.*?)\s*\}\s*else\s*\{\s*(.*?)\s*\}$', wert)
            def laeuft(x):
                x = x.strip()
                if x == 'None':
                    return 'LAEUFT := keiner'
                s2 = re.match(r'Some\(\s*(\w+)', x)
                return 'LAEUFT := %s' % (k.rolle(s2.group(1)) if s2 else normausdruck(x, k))
            if mm:
                bed, dann, sonst = mm.groups()
                ev.append('WENN %s' % normbed(bed, k))
                ev.append(laeuft(dann))
                if sonst.strip() != 's.current':
                    ev.append('SONST')
                    ev.append(laeuft(sonst))
            else:
                ev.append(laeuft(wert))
        elif m.group('els'):
            ev.append('SONST')
        elif m.group('iff'):
            ev.append('WENN %s' % normbed(m.group('ic'), k))
    return ev

# ================================================================================================
# Die vier Schichten
# ================================================================================================
ct, mt = lies(CODE), lies(MODELL)
befunde, zeilen, fehlt = [], [], []

def mangel(schicht, *text):
    fehlt.append((schicht, list(text)))

# ---- [1] Felder --------------------------------------------------------------------------------
tcb = strukturfelder(ct, r'struct Tcb \{', CODE)
thread = strukturfelder(mt, r'pub struct Thread \{', MODELL)
if not tcb or not thread:
    fehler("FEHLER: leere Feldliste -- ein leerer Lauf ist kein Ergebnis.")
for f in thread:
    if f not in FELDPAAR:
        mangel(1, "Modellfeld `Thread.%s` hat hier keine Zuordnung." % f,
                  "Entweder ist es neu (dann gehoert es in FELDPAAR UND in den Code),",
                  "oder das Modell beschreibt etwas, das der Scheduler nicht fuehrt.")
    elif FELDPAAR[f] not in tcb:
        mangel(1, "Modellfeld `Thread.%s` zeigt auf `Tcb.%s` -- das gibt es nicht (mehr)."
                  % (f, FELDPAAR[f]))
for f in FELDPAAR:
    if f not in thread:
        mangel(1, "FELDPAAR kennt `Thread.%s`, das Modell nicht (mehr)." % f)
zugeordnet = set(FELDPAAR.values())
for f in tcb:
    if f not in zugeordnet and f not in TCB_AUSSERHALB:
        mangel(1, "`Tcb.%s` ist weder einem Modellfeld zugeordnet noch als ausserhalb erklaert."
                  % f,
                  "Ein Feld, von dem der Beweis nichts weiss, kann er auch nicht zusichern.")
for f in TCB_AUSSERHALB:
    if f not in tcb:
        mangel(1, "TCB_AUSSERHALB fuehrt `Tcb.%s`, das es nicht (mehr) gibt." % f)
zeilen.append("  [1] Felder ......... %d Modellfelder, %d TCB-Felder, %d davon erklaert ausserhalb"
              % (len(thread), len(tcb), len(TCB_AUSSERHALB)))

# ---- [2] Uebergaenge ---------------------------------------------------------------------------
uebergaenge = re.findall(r'pub open spec fn (\w+)\([^)]*\)\s*->\s*Sched', mt)
if not uebergaenge:
    fehler("FEHLER: keine `spec fn ... -> Sched` im Modell gefunden -- der Waechter liest ins Leere.")
erwartet = set(p['name'] for p in PAARE) | {'pick'}   # `pick` ist in block_current/on_tick verschmolzen
for u in uebergaenge:
    if u not in erwartet:
        mangel(2, "Modell-Uebergang `%s` hat hier keinen zugeordneten Code." % u,
                  "Ein Uebergang, dem nichts entspricht, beweist etwas ueber niemanden.")
for u in erwartet:
    if u not in uebergaenge:
        mangel(2, "Zugeordnet ist `%s`, das Modell hat diesen Uebergang nicht (mehr)." % u)

# Jede Schreibstelle am Modellzustand muss in einer benannten Funktion liegen.
SCHREIBT = re.compile(
    r'self\.tcbs\[\s*\w+\s*\]\.(?:used|blocked|depleted|budget|remaining|priority|queued)\s*=(?!=)'
    r'|self\.tcbs\[\s*\w+\s*\]\s*=(?!=)'
    r'|\btcb\.(?:used|blocked|depleted|budget|remaining|priority|queued)\s*=(?!=)'
    r'|self\.enqueue_ready\s*\('
    r'|self\.remove_from_ready\s*\('
    r'|self\.current\s*=(?!=)')
benannt = set(CODE_PARTNER) | set(CODE_PRIMITIV) | set(CODE_AUSSERHALB)
# WICHTIG: Fundstellen und Funktionsgrenzen muessen aus DEMSELBEN Text kommen. Wer die
# Schreibstellen in einer kommentarfreien Kopie sucht und die `fn`-Grenzen im Original, ordnet
# jede Fundstelle der falschen Funktion zu -- und meldet lauter Funde, die es nicht gibt.
ct_roh = re.sub(r'//.*$', '', ct, flags=re.M)
fn_pos = [(m.start(), m.group(1)) for m in re.finditer(r'\bfn\s+(\w+)\s*[(<]', ct_roh)]
def umgebende_fn(p):
    name = '<Datei-Ebene>'
    for start, n in fn_pos:
        if start < p:
            name = n
        else:
            break
    return name
n_schreib, gesehen = 0, set()
for m in SCHREIBT.finditer(ct_roh):
    n_schreib += 1
    f = umgebende_fn(m.start())
    gesehen.add(f)
    if f not in benannt:
        mangel(2, "`%s` schreibt Scheduler-Zustand (%s), ist hier aber nicht benannt."
                  % (f, m.group(0).strip()),
                  "Entweder ist es ein Uebergang, den das Modell abbilden muss, oder er gehoert",
                  "ausdruecklich in CODE_AUSSERHALB -- stillschweigend geht es nicht.")
for f in benannt:
    if f not in gesehen:
        mangel(2, "`%s` ist als zustandsschreibend benannt, schreibt aber nichts (mehr)." % f)
zeilen.append("  [2] Uebergaenge .... %d Modell-Uebergaenge, %d Partnerfunktionen, "
              "%d Schreibstellen in %d Funktionen"
              % (len(uebergaenge), len(CODE_PARTNER), n_schreib, len(gesehen)))

# ---- [3] Struktur ------------------------------------------------------------------------------
# Die Ereignisfolgen werden aufgehoben: Schicht [4] fragt sie fuer die Veraltungsmeldung zu
# BEFUND B4 noch einmal ab (fuer eine Aussage ueber `pause` braucht man `pause`, nicht `audit`).
PAAR_EREIGNISSE = {}
for p in PAARE:
    cev, caus = [], []
    for muster, rollen, param in p['code']:
        a, b = code_ereignisse(rumpf(ct, muster, CODE), rollen, param)
        cev += a
        caus += b
    mev = []
    for muster, rollen, param in p['modell']:
        mev += modell_ereignisse(rumpf(mt, muster, MODELL), rollen, param)
    if not cev or not mev:
        fehler("FEHLER: leere Ereignisfolge fuer %s -- ein leerer Lauf ist kein Ergebnis." % p['name'])
    PAAR_EREIGNISSE[p['name']] = (cev, caus)
    ist = [z for z in difflib.unified_diff(cev, mev, n=1, lineterm='')
           if not z.startswith(('---', '+++', '@@'))]
    if ist != p['luecke']:
        mangel(3, "Paar `%s`: die Uebertragungsluecke ist nicht mehr die eingetragene." % p['name'],
                  "  eingetragen:", *["    %s" % z for z in p['luecke']],
                  "  gemessen:", *["    %s" % z for z in ist],
                  "  (Zum Nachtragen -- ABER erst, wenn das Argument daneben wieder stimmt:)",
                  *["            %r," % z for z in ist])
    if caus != p['ausserhalb']:
        mangel(3, "Paar `%s`: die wegabstrahierten Schreibzugriffe stimmen nicht mehr." % p['name'],
                  "  eingetragen: %s" % (p['ausserhalb'] or '(keine)'),
                  "  gemessen:    %s" % (caus or '(keine)'))
zeilen.append("  [3] Struktur ....... %d Paare, Uebertragungsluecke + wegabstrahierte Zugriffe"
              % len(PAARE))

# ---- [4] Audit ---------------------------------------------------------------------------------
# Zuerst die Praedikate: die Zuordnung „Teilaussage -> Audit-Code" weiter unten redet ueber SIE.
for name, soll in PRAEDIKATE.items():
    ist = rumpf(mt, r'pub open spec fn %s\(' % re.escape(name), MODELL)
    if ist != soll:
        mangel(4, "Das Praedikat `%s` ist nicht mehr das eingetragene." % name,
                  "  eingetragen: %s" % soll,
                  "  gemessen:    %s" % ist,
                  "Damit stimmt auch die Zuordnung Teilaussage -> Audit-Code nicht mehr.")

ab = rumpf(ct, r'pub fn audit\(&self\)', CODE)
teile = re.split(r'for local in 0\.\.self\.tcbs\.len\(\)', ab)
if len(teile) != 2:
    fehler("FEHLER: `audit` laesst sich nicht in Queue-Lauf und Rest zerlegen.",
           "        Der Waechter kann die Zuordnung der Codes dann nicht beurteilen.")
AUDIT_PAT = re.compile(
    r'if\s+([^{]+?)\s*\{\s*return\s+(\d+)'
    r'|let\s+Some\(\s*\w+\s*\)\s*=\s*self\.tcbs\.get\([^)]*\)\s*else\s*\{\s*return\s+(\d+)'
    r'|_\s*=>\s*return\s+(\d+)')
def audit_codes(s, ersatz):
    raus = []
    for m in AUDIT_PAT.finditer(s):
        code = int(m.group(2) or m.group(3) or m.group(4))
        raus.append((code, re.sub(r'\s+', ' ', (m.group(1) or ersatz).strip())))
    return raus
ist_q = audit_codes(teile[0], '#tcbs.get schlaegt fehl')
ist_r = audit_codes(teile[1], '#dir_load passt nicht')
if not ist_q or not ist_r:
    fehler("FEHLER: keine Audit-Codes gefunden -- ein leerer Lauf ist kein Ergebnis.")
for was, ist, soll in (('Queue-Lauf', ist_q, AUDIT_QUEUE), ('verlorene Threads', ist_r, AUDIT_REST)):
    if ist != soll:
        mangel(4, "`audit` (%s) hat sich geaendert." % was,
                  "  eingetragen: %s" % soll, "  gemessen:    %s" % ist)
# Die Behauptung „diese Teilaussage hat KEINEN Audit-Code" muss ueberpruefbar bleiben.
alle = ist_q + ist_r
vorhandene_codes = set(c for c, _ in alle)
for name, code, notiz in INVARIANTE:
    if code is not None and code not in vorhandene_codes:
        mangel(4, "%s soll von Audit-Code %d getragen werden -- den gibt es nicht (mehr)."
                  % (name, code))
    if code is None:
        befunde.append("%-36s -> KEIN Audit-Code (%s)" % (name, notiz))
# Diese Meldung darf NUR anschlagen, solange das Register die Teilaussage noch als
# „kein Audit-Code" fuehrt. Sonst feuert sie ab dem Tag, an dem der Befund behoben ist, fuer
# immer -- ein Waechter, der nach seiner eigenen Behebung weiterschreit, wird abgeschaltet, und
# dann schweigt er auch beim naechsten echten Fall. (B2 ist seit 2026-08-03 als Code 9
# eingetragen; die Meldung bleibt fuer den Fall, dass jemand den Code wieder herausnimmt.)
_b2_noch_offen = any(n == 'ridx: in_ready ==> !depleted' and c is None for n, c, _ in INVARIANTE)
if _b2_noch_offen and any('depleted' in b for _, b in ist_q):
    mangel(4, "Der Queue-Lauf von `audit` prueft jetzt `depleted` -- das Register oben (BEFUND B2)",
              "ist damit veraltet. Gute Nachricht, aber sie gehoert eingetragen.")
# B3 -- 2026-08-03 an das Register GEKOPPELT. Vorher hing diese Meldung allein an der Beobachtung
# („`audit` liest budget/remaining"), also an genau der Bedingung, die nach der Behebung des
# Befundes DAUERHAFT wahr waere. Sie haette ab dem Tag der Behebung fuer immer geschrien -- und ein
# Waechter, der das tut, wird abgeschaltet und schweigt dann auch beim naechsten echten Fall.
# Dieselbe Falle wie bei B2, dieselbe Form der Behebung. (`budget_blocked` faellt hier nicht
# hinein: `\bbudget\b` greift daran nicht, weil `_` ein Wortzeichen ist -- nachgesehen, nicht
# angenommen.)
_b3_noch_offen = any(n.startswith('bidx:') and c is None for n, c, _ in INVARIANTE)
if _b3_noch_offen and any(re.search(r'\b(budget|remaining)\b', b) for _, b in alle):
    mangel(4, "`audit` liest jetzt `budget`/`remaining` -- BEFUND B3 ist veraltet und gehoert",
              "nachgetragen (dann traegt `bidx` erstmals auch zur Laufzeit).")
# B4 -- ebenso gekoppelt, und die Beobachtung kommt nicht aus `audit`, sondern aus `pause` selbst:
# der Befund lautet „`pause` deplaniert den laufenden Thread nicht". Faengt `pause` an, `current`
# zu loeschen, ist er weg und das Register veraltet. Nachgesehen am 2026-08-03: H-b aendert daran
# nichts (die neue Zeile schreibt `budget_blocked`, nicht `current`) -- der Eintrag bleibt offen.
_b4_noch_offen = any(n == 'current_valid' and c is None for n, c, _ in INVARIANTE)
if _b4_noch_offen and 'LAEUFT := keiner' in PAAR_EREIGNISSE.get('pause', ([], []))[0]:
    mangel(4, "`pause` deplaniert jetzt den laufenden Thread -- BEFUND B4 ist damit veraltet und",
              "gehoert nachgetragen (`current_valid` gilt dann auch im Code ohne Fenster).")
zeilen.append("  [4] Audit .......... %d Praedikate eingefroren, %d Rueckgabestellen, "
              "%d Teilaussagen ohne Laufzeitpruefung"
              % (len(PRAEDIKATE), len(alle), sum(1 for _, c, _ in INVARIANTE if c is None)))

# ---- Bericht -----------------------------------------------------------------------------------
if '--leise' not in sys.argv:
    for z in zeilen:
        print(z)
    # Die Ueberschrift hiess bis 2026-08-03 „(deklariert, `lib.rs` unangetastet)". Das stimmte beim
    # ersten Lauf und danach nicht mehr: D8 und H-b/D9 haben `lib.rs` sehr wohl angefasst. Eine
    # Beschriftung, die neben der Sache herlaeuft, erzeugt Arbeit, die es nicht braucht.
    print("  -- Befunde (deklariert, OHNE Laufzeitpruefung -- Stand des Registers im Dateikopf) --")
    for b in befunde:
        print("     %s" % b)
    for p in PAARE:
        if p['luecke']:
            print("     Paar %-14s Uebertragungsluecke: %d Zeilen" % (p['name'], len(p['luecke'])))
if fehlt:
    if '--leise' not in sys.argv:
        print("  ABWEICHUNG -- Modell und Code sind nicht mehr so verbunden wie eingetragen:",
              file=sys.stderr)
        for schicht, text in fehlt:
            for i, z in enumerate(text):
                print("    [%d] %s" % (schicht, z) if i == 0 else "        %s" % z, file=sys.stderr)
    sys.exit(1)
PY
}

pruefen() {   # pruefen <lib.rs> <runqueue.rs> [--leise]
    local code="$1" modell="$2" leise="${3:-}"
    local aus rc
    aus="$(waechter "$code" "$modell" $leise 2>&1)"; rc=$?
    [ -n "$leise" ] || echo "$aus"
    return "$rc"
}

# ------------------------------------------------------------------------------------------------
# Selbsttest. Zwei Haelften, und beide sind noetig:
#   * kann dieser Waechter ueberhaupt ausloesen -- auf BEIDEN Seiten, und auch dann, wenn eine
#     Funktion gar nicht mehr da ist (sonst liest er ins Leere und meldet Gleichheit);
#   * haelt er still, wenn sich nur die Form aendert? Ohne diese Gegenprobe waere er wertlos:
#     einer, der auf alles anschlaegt, wird abgeschaltet.
# ------------------------------------------------------------------------------------------------
selbsttest() {
    W="$(mktemp -d)"
    local fehler=0 n=0 mut_rc=0

    mutieren() {   # mutieren <ziel: code|modell> <python-programm auf `s`>
        cp "$CODE_STD" "$W/lib.rs"; cp "$MODELL_STD" "$W/runqueue.rs"
        local datei="$W/lib.rs"; [ "$1" = "modell" ] && datei="$W/runqueue.rs"
        # Greift das Suchmuster nicht mehr, ist der Testfall wirkungslos -- und ein wirkungsloser
        # Testfall sieht von aussen aus wie „der Waechter schweigt". Deshalb hart getrennt.
        mut_rc=0
        python3 - "$datei" "$2" <<'PY'
import io, sys
p, prog = sys.argv[1], sys.argv[2]
s = io.open(p, encoding='utf-8').read()
ns = {'s': s}
exec(prog, ns)
if ns['s'] == s:
    sys.exit("FEHLER im Selbsttest: die Mutation hat NICHTS geaendert (%s).\n"
             "       Ein Testfall, der die Datei nicht anfasst, prueft nichts." % p)
io.open(p, 'w', encoding='utf-8').write(ns['s'])
PY
        mut_rc=$?
    }
    erwarte() {   # erwarte <kracht|still> <name>
        n=$((n+1))
        if [ "$mut_rc" -ne 0 ]; then
            echo "  FEHLER: '$2' -- die Mutation selbst ist fehlgeschlagen (Muster veraltet)." >&2
            fehler=1; mut_rc=0; return
        fi
        pruefen "$W/lib.rs" "$W/runqueue.rs" --leise
        local rc=$?
        if [ "$1" = "kracht" ]; then
            if [ "$rc" -eq 0 ]; then echo "  FEHLER: '$2' -- der Waechter schweigt." >&2; fehler=1
            else echo "  erkannt : $2"; fi
        else
            if [ "$rc" -ne 0 ]; then
                echo "  FEHLER: '$2' -- der Waechter schlaegt grundlos an." >&2
                pruefen "$W/lib.rs" "$W/runqueue.rs" >&2
                fehler=1
            else echo "  still   : $2"; fi
        fi
    }
    # Dritte Form neben `kracht`/`still`. Bei einer VERALTUNGSMELDUNG ist nicht das Anschlagen die
    # Leistung, sondern der Text: „der Befund im Register ist weg, trag ihn aus". Ein Fall, der nur
    # `kracht` prueft, waere schon bestanden, sobald irgendeine andere Schicht meckert -- und
    # genau das tut sie hier immer mit. Die Meldung koennte dann spurlos verschwinden, ohne dass
    # etwas auffaellt. Deshalb wird sie namentlich verlangt.
    erwarte_meldung() {   # erwarte_meldung <regex> <name>
        n=$((n+1))
        if [ "$mut_rc" -ne 0 ]; then
            echo "  FEHLER: '$2' -- die Mutation selbst ist fehlgeschlagen (Muster veraltet)." >&2
            fehler=1; mut_rc=0; return
        fi
        local aus
        aus="$(pruefen "$W/lib.rs" "$W/runqueue.rs" 2>&1)"
        if printf '%s' "$aus" | grep -qE -- "$1"; then
            echo "  gemeldet: $2"
        else
            echo "  FEHLER: '$2' -- die erwartete Meldung fehlt (/$1/)." >&2
            fehler=1
        fi
    }

    # -- Mutationen am ECHTEN Code ---------------------------------------------------------------
    mutieren code 's = s.replace("if self.tcbs[acct].remaining == 0 {",
                                 "if self.tcbs[acct].remaining > 0 {", 1)'
    erwarte kracht "Code: Vergleichsoperator gedreht (remaining == 0 -> > 0)"

    mutieren code 's = s.replace("            self.tcbs[s].depleted = false;\n", "", 1)'
    erwarte kracht "Code: die Zuweisung depleted = false faellt weg (set_budget)"

    mutieren code 's = s.replace("""            self.tcbs[s].budget = budget;
            self.tcbs[s].period = period.max(1);
            self.tcbs[s].remaining = budget;""",
    """            self.tcbs[s].remaining = budget;
            self.tcbs[s].period = period.max(1);
            self.tcbs[s].budget = budget;""", 1)'
    erwarte kracht "Code: zwei Schreibzugriffe getauscht (budget <-> remaining)"

    mutieren code 's = s.replace("        if !self.tcbs[s].blocked {\n            self.tcbs[s].blocked = true;",
                                 "        if self.tcbs[s].blocked {\n            self.tcbs[s].blocked = true;", 1)'
    erwarte kracht "Code: Verzweigung invertiert (pause: !blocked -> blocked)"

    # Muster nachgezogen am 2026-08-03: seit der D8-Behebung steht das Einreihen in `unblock`
    # hinter `if !self.tcbs[s].depleted`. Das ALTE Muster traf danach nichts mehr -- und der
    # Waechter hat das als harten Fehler gemeldet statt als "schweigt". Genau dafuer ist die
    # Regel da.
    mutieren code 's = s.replace("""            if !self.tcbs[s].depleted {
                self.enqueue_ready(s);
            }""", "", 1)'
    erwarte kracht "Code: unblock reiht nicht mehr ein"

    # Und die Gegenrichtung, neu seit der Behebung: faellt der Waechter WEG (also wieder
    # bedingungsloses Einreihen wie vor D8), muss es ebenfalls anschlagen. Ohne diesen Fall
    # koennte die Behebung still zurueckgedreht werden, ohne dass etwas meldet.
    mutieren code 's = s.replace("""            if !self.tcbs[s].depleted {
                self.enqueue_ready(s);
            }""", "            self.enqueue_ready(s);", 1)'
    erwarte kracht "Code: der D8-Waechter in unblock faellt weg (Rueckfall auf bedingungsloses Einreihen)"

    # -- H-b / D9 (2026-08-03): fuenf Faelle, die es vor H-b nicht geben KONNTE ------------------
    # Sie halten die Behebung fest, ohne die der Waechter sie nur beschreibt. Jeder einzelne dreht
    # genau ein Stueck von H-b zurueck -- und weil das Feld `budget_blocked` als „ausserhalb"
    # eingetragen ist, faellt das nicht ueber Schicht [1] auf, sondern nur ueber die
    # Uebertragungsluecke bzw. die Liste der wegabstrahierten Zugriffe. Genau deshalb ist die
    # AUSSERHALB-Liste je Paar gepflegt und nicht bloss ein Sammelbecken: sie ist hier die einzige
    # Stelle, an der ein weggelassener Schreibzugriff noch bemerkt wird.
    mutieren code 's = s.replace("                        self.set_budget_blocked(cur, true);\n", "", 1)'
    erwarte kracht "Code: on_tick schreibt den GRUND der Blockade nicht mehr mit (H-b/D9)"

    # Die Gegenrichtung im selben `on_tick`: ohne den `!blocked`-Konjunkt schreibt der Tick eine
    # Blockade aus IPC oder PAUSE still zu einer Budget-Blockade um -- und der naechste Refill hebt
    # sie mit auf. Das ist D9/D1, und es steht als Zeile in der Uebertragungsluecke.
    mutieren code 's = s.replace("if acct != cur && !self.tcbs[cur].blocked {", "if acct != cur {", 1)'
    erwarte kracht "Code: on_tick ueberschreibt wieder eine fremde Blockade (!blocked-Konjunkt weg)"

    # Der Kern von D9: der Refill-Lauf ueber ALLE `sc_donor == slot` faellt auf den einzelnen
    # `sc_donee` zurueck -- also auf die Spitze eines Stapels. Gemessen war das: niemand weckt den
    # Richtigen (D5: 0 Ticks in 3 Perioden, `audit() == 0`).
    # **Neu angesetzt am 2026-08-03 (D10).** Der Anker war der ganze Rumpf; seit der Waechter
    # `budget_blocked_count > 0` davorsteht und die Zuweisung hinter `set_budget_blocked` liegt,
    # passte er nicht mehr. Er ist jetzt die SACHE selbst und nicht ihre Formatierung: geweckt
    # wird nur noch, wer zugleich die SPITZE des Stapels ist -- also genau der Rueckfall auf
    # `sc_donee`, den D5 widerlegt hat.
    mutieren code 's = s.replace("""                            && self.tcbs[d].sc_donor == Some(slot)""",
"""                            && self.tcbs[d].sc_donor == Some(slot)
                            && self.tcbs[slot].sc_donee == Some(d)""", 1)'
    erwarte kracht "Code: der Refill-Lauf faellt auf den einzelnen sc_donee zurueck (H-b/D9)"

    # PAUSE uebernimmt die Blockade nicht mehr: die Zeile steht VOR `if !blocked` und ist deshalb
    # kein Zweig, sondern ein wegabstrahierter Schreibzugriff -- ohne sie hoebe der Refill eine
    # PAUSE auf, die Erfolg gemeldet hat.
    mutieren code 's = s.replace("        self.set_budget_blocked(s, false);\n", "", 1)'
    erwarte kracht "Code: pause uebernimmt die Budget-Blockade nicht mehr (H-b/D9)"

    # Und der Waechter in `unblock`, der eine Budget-Blockade stehen laesst. Faellt er weg, ist D6
    # wieder da: RESUME am Donee -> in die Liste, wird `current`, ein voller Tick auf leerem Konto.
    mutieren code 's = s.replace("""            if self.tcbs[s].budget_blocked {
                return true;
            }
""", "", 1)'
    erwarte kracht "Code: unblock hebt eine Budget-Blockade wieder auf (H-b/D9)"

    mutieren code 's = s.replace("    pub fn set_budget(&mut self", "    pub fn set_budget_v2(&mut self", 1)'
    erwarte kracht "Code: Funktion umbenannt (Waechter liest NICHT ins Leere)"

    mutieren code 's = s.replace("    /// Lastmass dieses Kerns", "    /// X", 1) if False else s
s = s.replace("""    pub fn load(&self) -> usize {""",
"""    /// Neu erfundener Uebergang -- vom Modell nicht erfasst.
    pub fn erfundener_uebergang(&mut self, i: usize) {
        self.tcbs[i].blocked = true;
        self.enqueue_ready(i);
    }

    pub fn load(&self) -> usize {""", 1)'
    erwarte kracht "Code: neue Funktion schreibt Scheduler-Zustand (Schicht 2)"

    mutieren code 's = s.replace("    depleted: bool,", "    depleted: bool,\n    neues_feld: u32,", 1)'
    erwarte kracht "Code: neues TCB-Feld, dem Modell unbekannt (Schicht 1)"

    mutieren code 's = s.replace("                if t.blocked {\n                    return 2;",
                                 "                if t.depleted {\n                    return 2;", 1)'
    erwarte kracht "Code: audit prueft depleted statt blocked (Schicht 4 + Befund-Register)"

    mutieren code 's = s.replace("            self.tcbs[s].next_refill = self.now + period as u64;\n", "", 1)'
    erwarte kracht "Code: ein wegabstrahierter Schreibzugriff verschwindet (AUSSERHALB-Liste)"

    # -- D10 (2026-08-03): zwei Faelle, die es vor D10 nicht geben KONNTE ------------------------
    # Der Weckelauf haengt seit D10 an einem ZAEHLER, und ein Zaehler ist genau die Konstruktion,
    # die in diesem Projekt schon einmal gelogen hat (`depleted_count`, D8/M5). Beide Faelle
    # nehmen einer Haelfte dieser Konstruktion den Halt.
    #
    # (a) Der Waechter liest den Zaehler nicht mehr -- die Kostenzeile faellt weg. Fuer die
    #     ABBILDUNG ist das folgenlos (der Lauf tut dann hoechstens mehr, nie weniger), aber die
    #     Ereignisfolge des Codes ist eine andere als die eingetragene, und genau das soll der
    #     Waechter merken: eine Zeile, die im Register mit einer eigenen Begruendung steht, darf
    #     nicht stillschweigend verschwinden.
    mutieren code 's = s.replace("                if self.budget_blocked_count > 0 {", "                if true {", 1)'
    erwarte kracht "Code: der D10-Waechter im Refill liest den Zaehler nicht mehr"

    # (b) Die NACHZAEHLUNG faellt weg. Das ist der schwerere der beiden: ohne sie ist der Zaehler
    #     eine unbelegte Voraussetzung des Waechters aus (a) -- luegt er nach oben, laeuft der
    #     Scan wieder in jedem Tick; luegt er nach unten, bleibt ein Donee liegen. Beides war vor
    #     Audit-Code 10 unbeobachtbar, und beides ist am echten Quelltext gemessen
    #     (`sched-erschoepfung-messen.sh`, Fassungen `Luegner-hoch` / `Luegner-runter`).
    mutieren code 's = s.replace("""        if bb != self.budget_blocked_count {
            return 10;
        }
""", "", 1)'
    erwarte kracht "Code: die Nachzaehlung von budget_blocked_count faellt weg (Audit-Code 10)"

    # -- Mutationen am MODELL --------------------------------------------------------------------
    mutieren modell 's = s.replace("Thread { blocked: false, in_ready: !t.depleted, ..t }",
                                   "Thread { blocked: false, in_ready: true, ..t }", 1)'
    erwarte kracht "Modell: unblock reiht bedingungslos ein"

    mutieren modell 's = s.replace("Thread { remaining: t.budget, depleted: false, in_ready: true, ..t }",
                                   "Thread { remaining: 0, depleted: false, in_ready: true, ..t }", 1)'
    erwarte kracht "Modell: refill fuellt auf 0 statt auf budget"

    mutieren modell 's = s.replace("Thread { remaining: 0, depleted: true, in_ready: false, ..t }",
                                   "Thread { depleted: true, remaining: 0, in_ready: false, ..t }", 1)'
    erwarte kracht "Modell: zwei Feldzuweisungen getauscht (tick_charge)"

    mutieren modell 's = s.replace("""        threads: s.threads.update(c, Thread { blocked: true, in_ready: false, ..t }),
        current: None,""",
    """        threads: s.threads.update(c, Thread { blocked: true, in_ready: false, ..t }),
        current: s.current,""", 1)'
    erwarte kracht "Modell: block_current deplaniert nicht mehr"

    mutieren modell 's = s.replace("pub open spec fn pause(", "pub open spec fn pause_v2(", 1)'
    erwarte kracht "Modell: pause gibt es nicht mehr (Waechter liest NICHT ins Leere)"

    mutieren modell 's = s.replace("    pub prio: nat,        // Prioritaet (0..NPRIO-1)\n", "", 1)'
    erwarte kracht "Modell: Feld prio entfernt (Schicht 1)"

    mutieren modell 's = s.replace("""// ===================== Bewiesene Eigenschaften =====================""",
"""pub open spec fn erfundener_uebergang(s: Sched, i: int) -> Sched {
    let t = s.threads[i];
    Sched { threads: s.threads.update(i, Thread { blocked: true, ..t }), ..s }
}

// ===================== Bewiesene Eigenschaften =====================""", 1)'
    erwarte kracht "Modell: neuer Uebergang ohne Code-Partner (Schicht 2)"

    mutieren modell 's = s.replace("    t.used && !t.blocked && !t.depleted", "    t.used && !t.blocked", 1)'
    erwarte kracht "Modell: runnable verliert !depleted (Praedikat-Register, Schicht 4)"

    mutieren modell 's = s.replace("(s.threads[i].budget == 0 ==> !s.threads[i].depleted)",
                                   "(s.threads[i].budget == 1 ==> !s.threads[i].depleted)", 1)'
    erwarte kracht "Modell: bidx-Konjunkt aufgeweicht (keine Aushungerung nur noch bei budget==1)"

    # -- Sprechprobe der beiden VERALTUNGSMELDUNGEN im Register (B3/B4) --------------------------
    # Beide sind an das Register gekoppelt und feuern im Normalbetrieb deshalb NIE. Ein Melder,
    # von dem niemand je etwas gehoert hat, ist keiner -- genau die Form, die dieses Projekt bei
    # der leeren Ereigniswarteschlange ohne `CD.R` schon einmal bezahlt hat. Die zwei Faelle
    # zeigen, dass er sprechen KANN, und sie halten zugleich fest, WAS er sagt.
    mutieren code 's = s.replace("            self.remove_from_ready(s); // No-Op, falls er gerade `current` ist",
      "            self.remove_from_ready(s);\n            if self.current == Some(s) {\n                self.current = None;\n            }", 1)'
    erwarte_meldung "BEFUND B4 ist damit veraltet" "Sprechprobe: pause deplaniert -> Register B4 meldet sich veraltet"

    mutieren code 's = s.replace("""                if t.depleted {
                    return 9;""",
    """                if t.depleted || t.remaining > t.budget {
                    return 9;""", 1)'
    erwarte_meldung "BEFUND B3 ist veraltet" "Sprechprobe: audit liest remaining -> Register B3 meldet sich veraltet"

    # -- Und die Gegenprobe: Kosmetik auf BEIDEN Seiten darf NICHT ausloesen ----------------------
    # Umbenannte lokale Variablen (Code + Modell), ein Kommentar, eine Leerzeile.
    cp "$CODE_STD" "$W/lib.rs"; cp "$MODELL_STD" "$W/runqueue.rs"
    mut_rc=0
    python3 - "$W/lib.rs" <<'PY' || mut_rc=1
import io, sys
p = sys.argv[1]; s0 = io.open(p, encoding='utf-8').read(); s = s0
s = s.replace("""        let cur = self.current.expect("kein laufender Thread");
        self.tcbs[cur].sp = frame;
        self.tcbs[cur].blocked = true;
        let next = self
            .dequeue_highest()
            .expect("Idle-Thread sollte immer bereit sein");
        self.current = Some(next);
        self.tcbs[next].sp""",
"""        // Kommentar des Selbsttests.

        let laeufer = self.current.expect("kein laufender Thread");
        self.tcbs[laeufer].sp = frame;

        self.tcbs[laeufer].blocked = true;
        let nachfolger = self
            .dequeue_highest()
            .expect("Idle-Thread sollte immer bereit sein");
        self.current = Some(nachfolger);
        self.tcbs[nachfolger].sp""", 1)
s = s.replace("let was_depleted = self.tcbs[s].depleted;",
              "let war_erschoepft = self.tcbs[s].depleted;   // umbenannt", 1)
s = s.replace("if was_depleted {", "if war_erschoepft {", 1)
if s == s0:
    sys.exit("FEHLER im Selbsttest: die Gegenprobe hat NICHTS geaendert (lib.rs).")
io.open(p, 'w', encoding='utf-8').write(s)
PY
    python3 - "$W/runqueue.rs" <<'PY' || mut_rc=1
import io, sys
p = sys.argv[1]; s0 = io.open(p, encoding='utf-8').read(); s = s0
s = s.replace("""    let c = s.current->Some_0 as int;
    let t = s.threads[c];
    Sched {
        threads: s.threads.update(c, Thread { blocked: true, in_ready: false, ..t }),
        current: None,
    }""",
"""    // Kommentar des Selbsttests.

    let lauf = s.current->Some_0 as int;

    let th = s.threads[lauf];
    Sched {
        threads: s.threads.update(lauf, Thread { blocked: true, in_ready: false, ..th }),
        current: None,
    }""", 1)
s = s.replace("""pub open spec fn unblock(s: Sched, i: int) -> Sched {
    let t = s.threads[i];
    Sched {
        threads: s.threads.update(i, Thread { blocked: false, in_ready: !t.depleted, ..t }),
        ..s
    }
}""",
"""pub open spec fn unblock(s: Sched, i: int) -> Sched {

    let th = s.threads[i];      // umbenannt
    Sched {
        threads: s.threads.update(i, Thread { blocked: false, in_ready: !th.depleted, ..th }),
        ..s
    }
}""", 1)
if s == s0:
    sys.exit("FEHLER im Selbsttest: die Gegenprobe hat NICHTS geaendert (runqueue.rs).")
io.open(p, 'w', encoding='utf-8').write(s)
PY
    erwarte still "Kosmetik beidseitig (lokale Namen, Kommentare, Leerzeilen)"

    echo "  Selbsttest: $n Faelle"
    aufraeumen
    return "$fehler"
}

MODUS="${1:-alles}"
case "$MODUS" in
    --nur-pruefen)
        pruefen "$CODE_STD" "$MODELL_STD" --leise; exit $? ;;
    --selftest)
        selbsttest; exit $? ;;
    alles|"")
        echo "== Modell-Treue: Verus-runqueue gegen sel4lake-sched::Scheduler =="
        pruefen "$CODE_STD" "$MODELL_STD" || { echo "== MODELL-TREUE (SCHED) VERLETZT ==" >&2; exit 1; }
        echo "-- Selbsttest --"
        selbsttest || { echo "== WAECHTER NICHT SPRECHFAEHIG ==" >&2; exit 1; }
        echo "== Modell und Code stehen zueinander wie eingetragen =="
        ;;
    *) echo "Aufruf: $0 [--nur-pruefen|--selftest]" >&2; exit 2 ;;
esac
