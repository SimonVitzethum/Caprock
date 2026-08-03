#!/usr/bin/env bash
# **Haelt das Verus-IPC-Modell an den echten Endpoint.** (B-7.3/D7, IPC-Strang)
#
# ================================================================================================
# WAS HIER GEPRUEFT WIRD -- und was ausdruecklich NICHT
# ================================================================================================
#
# `Verification/ipc/proofs/endpoint.rs` beweist etwas ueber ein Modell mit NEUN Feldern und ACHT
# Operationen. `crates/sel4lake-ipc/src/lib.rs::Endpoint` hat SECHS Felder (`used`, `quiescing`,
# `senders`, `receivers`, `caller`, `reply_owner`) und rund fuenfzehn Operationen. Alle sechs haben
# im Modell inzwischen ein Gegenstueck; die drei weiteren (`delivered`, `dropped_senders`,
# `dropped_receivers`) sind Buchhaltung, die im Code nirgends steht und deshalb **gemessen** wird.
#
# Ein struktureller 1:1-Vergleich waere hier trotzdem eine LUEGE (anders als bei `unlink`,
# tools/verus-modelltreue.sh): er schlaege entweder immer an oder muesste so weit aufgeweicht
# werden, dass er nicht mehr anschlagen KANN -- und ein Pruefer, der nicht fehlschlagen kann, laesst
# eine Aussage wahr *aussehen*. Deshalb wird die Abstraktion hier **hingeschrieben und ausgefuehrt**
# statt behauptet:
#
#   1. Das ausfuehrbare Modell wird aus der Verus-Datei **uebersetzt**, nicht abgeschrieben.
#      Eine handgeschriebene Zweitfassung koennte still auseinanderlaufen -- genau die Falle,
#      die B-7.2 bezahlt hat. Der Uebersetzer ist fail-closed: was er nicht kennt, ist ein Fehler,
#      kein Ueberspringen; und er prueft die Menge der `spec fn`/`proof fn` gegen die Liste, die
#      dieser Kopf benennt.
#   2. Der ECHTE Quelltext von `sel4lake-ipc` wird **unveraendert** uebernommen (eine einzige,
#      geprueft vorhandene Zeile `#![no_std]` faellt weg, damit ein Host-Binary entsteht) und
#      gegen Stellvertreter fuer HAL/Scheduler/ABI gelinkt.
#   3. Die Abbildung `echter Endpoint -> Modell-Endpoint` (`alpha`) steht an EINER Stelle:
#         used, quiescing     := die gleichnamigen Felder
#         senders, receivers  := die Thread-IDs der blockierten Sender bzw. geparkten Empfaenger,
#                                in FIFO-Reihenfolge
#         caller, reply_owner := die gleichnamigen Felder
#         delivered           := **effektbasiert**: eine Nachricht gilt als zugestellt, sobald ihr
#                                Wort erstmals im Frame eines ANDEREN Fadens steht
#         dropped_*           := **effektbasiert**: ein Faden, der blockiert wurde (oder fuer den
#                                die Operation Erfolg meldete), danach aber in KEINER Warteschlange
#                                steht und kein Token haelt -- also niemanden mehr hat, der ihn
#                                wecken koennte
#         Ergebniscode        := ein Sentinel im Ergebnisregister zeigt, ob die Operation ueberhaupt
#                                einen Code gegeben hat; `ERR_BADCAP`/`ERR_QUIESCING` werden auf die
#                                abstrakten Codes 1/2 des Modells abgebildet
#      Die Buchhaltungsfelder aus dem Code ABZULESEN waere die Falle aus CLAUDE.md („`rx_used` sagt,
#      dass das Geraet gehandelt hat, nicht dass Daten ankamen") -- deshalb die Wirkung.
#   4. Dann wird gefahren: jede echte Operation muss unter `alpha` dasselbe tun wie ihr
#      Modellschritt, und `ep_inv`/`token_inv`/`msgs_total`/`gate` (ebenfalls uebersetzt) muessen am
#      abgebildeten Zustand gelten.
#
# **GEPRUEFT wird damit:**
#   * `call`/`recv` in allen Zweigen (Rendezvous, Parken, Abweisung, Kapazitaetsueberlauf), ueber
#     beide Kern-Pfade (`switch_to` und `unblock`), FIFO-Reihenfolge beider Warteschlangen.
#   * `reply`: Konsum des Reply-Tokens, kein Doppel-Reply (an der **Wirkung** im Frame des
#     Aufrufers gemessen, nicht am Rueckgabewert), und die A-4.2-Asymmetrie -- `REPLY` wirkt am
#     stillgelegten Endpoint weiter, waehrend `CALL`/`RECV` abgewiesen werden.
#   * das Stilllegungstor: dass abgewiesen wird, dass die Abweisung WIRKUNGSLOS ist (kein Eintrag,
#     keine Blockade), und dass die beiden Gruende UNTERSCHEIDBARE Codes tragen -- samt der
#     Gegenprobe, dass sie es im echten ABI ueberhaupt sind.
#   * die Kapazitaetsschranke `QUEUE_CAP` auf beiden Warteschlangen -- inklusive der Frage, ob die
#     Schranke des Modells noch die des Codes ist.
#   * `bind_receiver`, `migrate_owner`, `begin_quiesce`, `end_quiesce` als eigene Modellschritte.
#   * **Sprechproben am Modell allein** (ohne Code dahinter): ob `gate`, `ep_inv`, `ep_inv_strong`,
#     `token_inv` und `msgs_total` ueberhaupt noch urteilen koennen. Ohne sie bliebe ein auf `true`
#     aufgeweichtes Praedikat unbemerkt, weil der Code den verletzenden Zustand nur ueber drei Ecken
#     erreicht.
#   * dass die Nebenbedingungen der Abbildung **tragen**: fuer jede Luecke wird gemessen, dass die
#     Entsprechung ohne sie zerbricht (G1..G4).
#
# **NICHT geprueft wird:**
#   * der Leichen-Zweig (`frame_of == None`), `purge_thread`, `owner_died`, `abort_call` -- das
#     Modell kennt weder Tod noch Serverausfall. Das ist als Luecke GEMESSEN (G1..G4), nicht
#     stillschweigend ausgelassen.
#   * `retire_receiver`, `rebind_server`, `audit`, `Notification` (SIGNAL/WAIT) -- keine
#     Modell-Entsprechung.
#   * Nebenlaeufigkeit. Modell und Waechter sind sequentiell; die Locks des Kernels bleiben
#     Concurrency-TCB (so steht es auch im Kopf der Beweisdatei).
#   * die HAL (Frame-Register) und der Scheduler. Beide sind hier **Stellvertreter**. Was ein
#     echter `switch_to` tut, sagt dieser Lauf nicht -- und ob ein blockierter Faden je wieder
#     laeuft, erst recht nicht: **Liveness ist keine Aussage dieses Waechters.** Er misst, dass
#     niemand mehr da ist, der wecken koennte; dass daraus dauerhaftes Haengen folgt, ist eine
#     Eigenschaft des Schedulers.
#
# **Die Befunde, die dieser Waechter FESTHAELT.** Sie werden als Tatsachen geprueft, damit sie nicht
# stillschweigend verschwinden -- und die Kopplung laeuft ueber das REGISTER, nicht ueber die
# Beobachtung: trifft ein Befund nicht mehr zu, schlaegt der Waechter fehl und verlangt, dass der
# Eintrag hier heraus und ins `done.md` wandert. Ein Melder, der nach seiner eigenen Behebung
# weiterschreit, wird sonst abgeschaltet (CLAUDE.md).
#   B1a/B1b  `ep_inv_strong` gilt am echten Endpoint NICHT: `bind_receiver` sieht die
#            Sender-Warteschlange nicht an, `migrate_owner` nicht die Empfaengerseite. Beides ist
#            gewollt (A-4.1/A-4.3) -- die Invariante haelt deshalb nicht der TYP, sondern die
#            Aufrufdisziplin, und die steht jetzt als Vorbedingung im Modell (`ep_inv`).
#   B2       Der 33. Sender an EINEM Endpoint wird von `TidQueue::enqueue` still verworfen. Der
#            Faden wird trotzdem blockiert, bekommt KEINEN Ergebniscode, steht in keiner Struktur
#            des Endpoints, wird von 33 nachfolgenden `RECV` nie geweckt, und `quiescence_of`
#            meldet ihn als RUHIG. `purge_thread` findet ihn nicht, `audit` meldet nichts.
#   B2b/c/d  Dieselbe Schranke auf der Empfaengerseite, in `bind_receiver` (meldet ERFOLG, obwohl
#            verworfen) und in `migrate_owner` (loescht die Antwortpflicht und verliert den
#            Aufrufer).
#   B3       Ein zweites `RECV` desselben Servers vor dem `REPLY` UEBERSCHREIBT das Reply-Token.
#            Der uebergangene Aufrufer wird nie geweckt -- und `is_idle()` meldet den Endpoint
#            danach als RUHIG, ein Austausch nach A-4.1 traefe also scheinbar niemanden.
#   B4       Nach `migrate_owner` bei geparktem Empfaenger ist ein Rendezvous FAELLIG, aber beide
#            Seiten sind blockiert; ein dritter Aufrufer ueberholt den migrierten.
#   KEIN Kernel-Quelltext wird deshalb geaendert -- so wie beim `unlink`-Waechter.
#
# Aufruf:
#   tools/verus-modelltreue-ipc.sh              # pruefen + Selbsttest
#   tools/verus-modelltreue-ipc.sh --nur-pruefen
#   tools/verus-modelltreue-ipc.sh --selftest
#
# Rueckgabe: 0 = deckungsgleich · 1 = Abweichung im Lauf · 2 = Werkzeugfehler (Uebersetzung/Bau).
if [ -z "${BASH_VERSION:-}" ]; then echo "FEHLER: braucht bash, nicht sh/dash." >&2; exit 2; fi
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"

CODE_STD="$ROOT/crates/sel4lake-ipc/src/lib.rs"
MODELL_STD="$ROOT/Verification/ipc/proofs/endpoint.rs"
ABI_STD="$ROOT/crates/sel4lake-abi/src/lib.rs"
RUSTC="${RUSTC:-rustc}"

# **Alle Wegwerfdateien unter EINER Wurzel, mit einem Aufraeumer, der auch bei Abbruch greift.**
# Der Selbsttest arbeitet ausschliesslich auf Kopien; die Originale (`sel4lake-ipc/src/lib.rs`,
# `Verification/ipc/proofs/endpoint.rs`) werden nie beschrieben. Ein Ctrl-C mitten im Lauf darf
# weder etwas stehenlassen noch etwas zuruecklassen, das nach Zustand aussieht.
WURZEL="$(mktemp -d)"
trap 'rm -rf "$WURZEL"' EXIT
trap 'rm -rf "$WURZEL"; exit 130' INT TERM

# ================================================================================================
# 1. Der Uebersetzer: Verus-Spezifikation -> ausfuehrbares Rust.
# ================================================================================================
modell_uebersetzen() {   # modell_uebersetzen <endpoint.rs>
    python3 - "$1" <<'PY'
import io, re, sys

pfad = sys.argv[1]
try:
    roh = io.open(pfad, encoding='utf-8').read()
except OSError as e:
    sys.exit("FEHLER: %s nicht lesbar (%s)." % (pfad, e))

def fehler(*zeilen):
    sys.exit("FEHLER (Modell-Uebersetzer, %s):\n  %s" % (pfad, "\n  ".join(zeilen)))

m = re.search(r'verus!\s*\{', roh)
if not m:
    fehler("kein `verus! {`-Block gefunden -- das ist keine Beweisdatei.")
i, tiefe = m.end(), 1
while i < len(roh) and tiefe > 0:
    if roh[i] == '{': tiefe += 1
    elif roh[i] == '}': tiefe -= 1
    i += 1
if tiefe != 0:
    fehler("der `verus!`-Block ist nicht geschlossen.")
text = re.sub(r'//.*$', '', roh[m.end():i-1], flags=re.M)

# -- Die Oberflaeche, die dieser Waechter kennt. Weicht sie ab, liest er ins Leere oder deckt
#    weniger ab, als sein Kopf behauptet -- beides ist ein Fehler, kein stiller Erfolg.
SPEC_ERWARTET  = {'queue_cap', 'gate', 'ep_inv_strong', 'ep_inv', 'token_inv', 'msgs_total',
                  'send', 'recv', 'reply', 'bind_receiver', 'migrate_owner',
                  'begin_quiesce', 'end_quiesce'}
PROOF_ERWARTET = {
    'send_preserves_inv', 'recv_preserves_inv', 'reply_preserves_inv',
    'bind_receiver_keeps_inv_under_discipline', 'bind_receiver_breaks_strong_inv',
    'migrate_owner_keeps_inv_under_discipline', 'migrate_owner_breaks_strong_inv',
    'end_quiesce_breaks_inv',
    'send_accounts_every_message', 'send_no_loss', 'send_drops_above_cap',
    'recv_drops_above_cap', 'recv_delivers_once', 'rendezvous_progress',
    'gate_rejects_are_noops', 'gate_distinguishes',
    'send_preserves_token_inv', 'recv_preserves_token_inv', 'reply_preserves_token_inv',
    'migrate_owner_preserves_token_inv', 'reply_consumes_token', 'no_double_reply',
    'reply_not_gated_by_quiescing', 'recv_overwrites_token'}
spec_reihenfolge = re.findall(r'\bspec\s+fn\s+(\w+)\s*\(', text)
spec_gefunden  = set(spec_reihenfolge)
proof_gefunden = set(re.findall(r'\bproof\s+fn\s+(\w+)\s*\(', text))
if spec_gefunden != SPEC_ERWARTET:
    fehler("die Menge der `spec fn` weicht ab.",
           "fehlt    : %s" % (", ".join(sorted(SPEC_ERWARTET - spec_gefunden)) or "(nichts)"),
           "neu      : %s" % (", ".join(sorted(spec_gefunden - SPEC_ERWARTET)) or "(nichts)"),
           "Fehlt eine, laese der Waechter ins Leere. Kam eine dazu, beruehrt sie kein Testfall.")
if proof_gefunden != PROOF_ERWARTET:
    fehler("die Menge der `proof fn` weicht ab.",
           "fehlt    : %s" % (", ".join(sorted(PROOF_ERWARTET - proof_gefunden)) or "(nichts)"),
           "neu      : %s" % (", ".join(sorted(proof_gefunden - PROOF_ERWARTET)) or "(nichts)"),
           "Der Kopf dieses Waechters benennt genau diese Zusicherungen -- eine weitere waere",
           "ungedeckt, eine fehlende eine Zusicherung, die es nicht mehr gibt.")

ms = re.search(r'pub\s+struct\s+Endpoint\s*\{', text)
if not ms:
    fehler("`pub struct Endpoint` nicht gefunden.")
j, tiefe = ms.end(), 1
while j < len(text) and tiefe > 0:
    if text[j] == '{': tiefe += 1
    elif text[j] == '}': tiefe -= 1
    j += 1
FELDER = []
for fz in text[ms.end():j-1].split(','):
    fz = fz.strip()
    if not fz:
        continue
    mf = re.match(r'^pub\s+(\w+)\s*:\s*(.+)$', fz, re.S)
    if not mf:
        fehler("Feld-Deklaration nicht verstanden: %r" % fz)
    FELDER.append((mf.group(1), mf.group(2).strip()))
FELDTYP = dict(FELDER)

def rust_typ(t, als_param=False):
    t = t.strip()
    if t == 'nat':         return 'u64'
    if t == 'bool':        return 'bool'
    if t == 'Seq<nat>':    return 'Vec<u64>'
    if t == 'Option<nat>': return 'Option<u64>'
    if t == 'Endpoint':    return '&Endpoint' if als_param else 'Endpoint'
    fehler("unbekannter Typ %r -- bekannt sind nat, bool, Seq<nat>, Option<nat>, Endpoint." % t)

# -- Ausdruecke tragen ihren Modelltyp mit. Das ist keine Bequemlichkeit: `Endpoint` ist im
#    erzeugten Rust einmal `&Endpoint` (ein Parameter) und einmal ein eigener Wert (ein
#    Struktur-Literal). Ohne die Unterscheidung waere `if gate(ep) != 0 { ep } else { .. }`
#    nicht uebersetzbar -- und ein Uebersetzer, der eine Stelle raet, ist keine Bruecke mehr.
# `ref` heisst: das ist ein geliehener PLATZ (ein Parameter, ein Feld), kein eigener Wert.
# Wer ihn als Wert braucht, muss kopieren -- bei `Vec` und `Endpoint` sonst ein Bewegen aus
# einer Ausleihe, also ein Uebersetzungsfehler statt einer Bruecke.
NICHT_COPY = ('Endpoint', 'Seq<nat>')

def wert(e):
    code, typ, ref = e
    return code + '.clone()' if (ref and typ in NICHT_COPY) else code

def referenz(e):
    code, typ, ref = e
    if typ != 'Endpoint':
        return code
    return code if ref else '&(%s)' % code

TOK = re.compile(r'\s+|(\d+)|([A-Za-z_][A-Za-z0-9_]*)|(==|!=|>=|<=|->|\|\||&&|[{}(),:.+><!])')

def zerlegen(s):
    toks, p = [], 0
    while p < len(s):
        m2 = TOK.match(s, p)
        if not m2:
            fehler("unverstaendliches Zeichen %r in einem spec-Rumpf." % s[p])
        if   m2.group(1) is not None: toks.append(('num', m2.group(1)))
        elif m2.group(2) is not None: toks.append(('id',  m2.group(2)))
        elif m2.group(3) is not None: toks.append(('op',  m2.group(3)))
        p = m2.end()
    toks.append(('eof', ''))
    return toks

class P:
    def __init__(self, toks, params, signaturen):
        self.t, self.i = toks, 0
        self.params = dict(params)
        self.sig = signaturen
    def sieh(self):  return self.t[self.i]
    def nimm(self):
        t = self.t[self.i]; self.i += 1; return t
    def erwarte(self, art, w=None):
        t = self.nimm()
        if t[0] != art or (w is not None and t[1] != w):
            fehler("erwartet %r, bekommen %r." % (w or art, t[1]))
        return t
    def ist(self, art, w):
        return self.sieh()[0] == art and self.sieh()[1] == w

    def expr(self):
        if self.ist('id', 'if'):
            self.nimm()
            bed = self.expr()
            if bed[1] != 'bool':
                fehler("die Bedingung eines `if` ist kein bool, sondern %s." % bed[1])
            self.erwarte('op', '{'); dann = self.expr(); self.erwarte('op', '}')
            self.erwarte('id', 'else')
            if self.ist('id', 'if'):
                sonst = self.expr()
            else:
                self.erwarte('op', '{'); sonst = self.expr(); self.erwarte('op', '}')
            if dann[1] != sonst[1]:
                fehler("die Zweige eines `if` haben verschiedene Typen (%s / %s)." % (dann[1], sonst[1]))
            return ("if %s { %s } else { %s }" % (wert(bed), wert(dann), wert(sonst)), dann[1], False)
        return self.oder()
    def oder(self):
        l = self.und()
        while self.ist('op', '||'):
            self.nimm(); r = self.und()
            l = ("%s || %s" % (wert(l), wert(r)), 'bool', False)
        return l
    def und(self):
        l = self.vergleich()
        while self.ist('op', '&&'):
            self.nimm(); r = self.vergleich()
            l = ("%s && %s" % (wert(l), wert(r)), 'bool', False)
        return l
    def vergleich(self):
        l = self.summe()
        if self.sieh()[0] == 'op' and self.sieh()[1] in ('==', '!=', '>', '<', '>=', '<='):
            op = self.nimm()[1]; r = self.summe()
            if l[1] != r[1]:
                fehler("Vergleich zwischen %s und %s." % (l[1], r[1]))
            return ("%s %s %s" % (wert(l), op, wert(r)), 'bool', False)
        return l
    def summe(self):
        l = self.unaer()
        while self.ist('op', '+'):
            self.nimm(); r = self.unaer()
            l = ("%s + %s" % (wert(l), wert(r)), 'nat', False)
        return l
    def unaer(self):
        if self.ist('op', '!'):
            self.nimm(); e = self.unaer()
            if e[1] != 'bool':
                fehler("`!` auf einem Nicht-bool (%s)." % e[1])
            return ("!%s" % wert(e), 'bool', False)
        return self.nachsatz()
    def nachsatz(self):
        e = self.atom()
        while True:
            if self.ist('op', '.'):
                self.nimm()
                name = self.erwarte('id')[1]
                if self.ist('op', '('):
                    e = self.methode(e, name)
                else:
                    if e[1] != 'Endpoint':
                        fehler("Feldzugriff .%s auf einem Nicht-Endpoint (%s)." % (name, e[1]))
                    if name not in FELDTYP:
                        fehler("`struct Endpoint` hat kein Feld %r." % name)
                    e = ("%s.%s" % (e[0], name), FELDTYP[name], True)
                continue
            if self.ist('op', '->'):
                self.nimm()
                v = self.erwarte('id')[1]
                if v != 'Some_0':
                    fehler("`->%s` -- bekannt ist nur `->Some_0`." % v)
                if e[1] != 'Option<nat>':
                    fehler("`->Some_0` auf einem Nicht-Option (%s)." % e[1])
                e = ("%s.unwrap()" % e[0], 'nat', False)
                continue
            if self.ist('id', 'is'):
                self.nimm()
                v = self.erwarte('id')[1]
                if e[1] != 'Option<nat>':
                    fehler("`is %s` auf einem Nicht-Option (%s)." % (v, e[1]))
                if   v == 'Some': e = ("%s.is_some()" % e[0], 'bool', False)
                elif v == 'None': e = ("%s.is_none()" % e[0], 'bool', False)
                else: fehler("`is %s` -- bekannt sind `is Some` und `is None`." % v)
                continue
            break
        return e
    def methode(self, e, name):
        self.erwarte('op', '(')
        args = []
        while not self.ist('op', ')'):
            args.append(self.expr())
            if self.ist('op', ','): self.nimm()
        self.erwarte('op', ')')
        if e[1] != 'Seq<nat>':
            fehler("Methodenaufruf .%s() auf einem Nicht-Seq-Ausdruck (%s)." % (name, e[1]))
        b = e[0]
        if name == 'len'        and not args:      return ("(%s.len() as u64)" % b, 'nat', False)
        if name == 'first'      and not args:      return ("mseq_first(&%s)" % b, 'nat', False)
        if name == 'drop_first' and not args:      return ("mseq_drop_first(&%s)" % b, 'Seq<nat>', False)
        if name == 'push'       and len(args) == 1: return ("mseq_push(&%s, %s)" % (b, wert(args[0])), 'Seq<nat>', False)
        if name == 'contains'   and len(args) == 1: return ("%s.contains(&%s)" % (b, wert(args[0])), 'bool', False)
        fehler("Seq-Methode %r mit %d Argument(en) -- bekannt: len(), first(), drop_first(), "
               "push(x), contains(x)." % (name, len(args)))
    def atom(self):
        t = self.sieh()
        if t[0] == 'num':
            self.nimm(); return (t[1], 'nat', False)
        if t[0] == 'op' and t[1] == '(':
            self.nimm(); e = self.expr(); self.erwarte('op', ')')
            return ("(%s)" % wert(e), e[1], False)
        if t[0] != 'id':
            fehler("unerwartetes Token %r in einem spec-Rumpf." % (t[1],))
        name = self.nimm()[1]
        if name in ('true', 'false'):
            return (name, 'bool', False)
        if name == 'None':
            return ('None', 'Option<nat>', False)
        if name == 'Some':
            self.erwarte('op', '('); e = self.expr(); self.erwarte('op', ')')
            if e[1] != 'nat':
                fehler("`Some(..)` mit %s statt nat." % e[1])
            return ("Some(%s)" % wert(e), 'Option<nat>', False)
        if name == 'Endpoint' and self.ist('op', '{'):
            self.nimm()
            gesetzt, teile = [], []
            while not self.ist('op', '}'):
                f = self.erwarte('id')[1]
                if f not in FELDTYP:
                    fehler("Struktur-Literal setzt unbekanntes Feld %r." % f)
                self.erwarte('op', ':')
                v = self.expr()
                if v[1] != FELDTYP[f]:
                    fehler("Feld %s bekommt %s statt %s." % (f, v[1], FELDTYP[f]))
                teile.append("%s: %s" % (f, wert(v))); gesetzt.append(f)
                if self.ist('op', ','): self.nimm()
            self.erwarte('op', '}')
            if set(gesetzt) != set(FELDTYP):
                fehler("Struktur-Literal setzt %s, der struct hat %s." % (gesetzt, list(FELDTYP)),
                       "Ein weggelassenes Feld waere eine stille Uebernahme.")
            return ("Endpoint { %s }" % ", ".join(teile), 'Endpoint', False)
        if name in self.sig and self.ist('op', '('):
            self.nimm()
            args = []
            while not self.ist('op', ')'):
                args.append(self.expr())
                if self.ist('op', ','): self.nimm()
            self.erwarte('op', ')')
            ps, rt = self.sig[name]
            if len(args) != len(ps):
                fehler("%s(..) mit %d statt %d Argument(en)." % (name, len(args), len(ps)))
            teile = []
            for a, (pn, pt) in zip(args, ps):
                if a[1] != pt:
                    fehler("%s(..): Argument %s ist %s statt %s." % (name, pn, a[1], pt))
                teile.append(referenz(a) if pt == 'Endpoint' else wert(a))
            return ("%s(%s)" % (name, ", ".join(teile)), rt, False)
        if name not in self.params:
            fehler("Bezeichner %r ist weder Parameter dieser spec fn (%s) noch eine bekannte spec fn."
                   % (name, ", ".join(self.params) or "keine"))
        return (name, self.params[name], self.params[name] in NICHT_COPY)

def spec_rumpf(name):
    ms2 = re.search(r'pub\s+open\s+spec\s+fn\s+%s\s*\(([^)]*)\)\s*->\s*(\w+(?:<\w+>)?)\s*\{' % name,
                    text)
    if not ms2:
        fehler("`pub open spec fn %s(...) -> ...` NICHT GEFUNDEN." % name,
               "Der Waechter laese hier ins Leere -- das ist kein bestandener Test.")
    params = []
    for teil in ms2.group(1).split(','):
        teil = teil.strip()
        if not teil: continue
        mp = re.match(r'^(\w+)\s*:\s*(.+)$', teil)
        if not mp: fehler("Parameter nicht verstanden: %r" % teil)
        params.append((mp.group(1), mp.group(2).strip()))
    k, tf = ms2.end(), 1
    while k < len(text) and tf > 0:
        if text[k] == '{': tf += 1
        elif text[k] == '}': tf -= 1
        k += 1
    if tf != 0: fehler("Rumpf von %s nicht geschlossen." % name)
    return params, ms2.group(2), text[ms2.end():k-1]

# Erst ALLE Signaturen einsammeln (ein Aufruf darf vor der Definition stehen), dann uebersetzen.
rumpfe, signaturen = {}, {}
for name in spec_reihenfolge:
    params, rtyp, rumpf_txt = spec_rumpf(name)
    for _, pt in params:
        rust_typ(pt, als_param=True)
    rust_typ(rtyp)
    signaturen[name] = (params, rtyp)
    rumpfe[name] = (params, rtyp, rumpf_txt)

aus = []
for name in spec_reihenfolge:
    params, rtyp, rumpf_txt = rumpfe[name]
    p = P(zerlegen(rumpf_txt), params, signaturen)
    koerper = p.expr()
    if p.sieh()[0] != 'eof':
        fehler("im Rumpf von %s bleibt unuebersetzter Text ab %r stehen." % (name, p.sieh()[1]))
    if koerper[1] != rtyp:
        fehler("%s liefert %s, deklariert ist %s." % (name, koerper[1], rtyp))
    sig = ", ".join("%s: %s" % (n, rust_typ(t, als_param=True)) for n, t in params)
    aus.append("pub fn %s(%s) -> %s {\n    %s\n}" % (name, sig, rust_typ(rtyp), wert(koerper)))

if not aus or not FELDER:
    fehler("leeres Ergebnis -- ein leerer Lauf ist kein Testergebnis.")

print("""// ERZEUGT aus der Verus-Spezifikation. NICHT von Hand aendern -- die Quelle ist die
// Beweisdatei. Wer hier etwas aendern will, aendert dort.
#![allow(dead_code)]

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Endpoint {
%s
}

/// `Seq::drop_first` ist in Verus funktional (liefert eine NEUE Folge) -- hier ebenso.
pub fn mseq_drop_first(s: &Vec<u64>) -> Vec<u64> { s[1..].to_vec() }
/// `Seq::push` ebenso: haengt an und liefert eine NEUE Folge.
pub fn mseq_push(s: &Vec<u64>, v: u64) -> Vec<u64> { let mut n = s.clone(); n.push(v); n }
/// `Seq::first` -- in Verus mit `recommends len() > 0`; hier bricht es hart ab statt zu raten.
pub fn mseq_first(s: &Vec<u64>) -> u64 { s[0] }

%s""" % ("\n".join("    pub %s: %s," % (n, rust_typ(t)) for n, t in FELDER), "\n\n".join(aus)))
PY
}

# ================================================================================================
# 2. Der Stellvertreter fuer `sel4lake-abi` -- **extrahiert**, nicht abgeschrieben.
#    Die Registerindizes entscheiden mit, wo eine Nachricht landet; sie hier noch einmal
#    hinzuschreiben waere dieselbe Kopie-Falle wie ein handgeschriebenes Modell.
# ================================================================================================
abi_extrahieren() {   # abi_extrahieren <abi/lib.rs>
    python3 - "$1" <<'PY'
import io, re, sys
pfad = sys.argv[1]
try:
    s = io.open(pfad, encoding='utf-8').read()
except OSError as e:
    sys.exit("FEHLER: %s nicht lesbar (%s)." % (pfad, e))

def block(start):
    m = re.search(start, s)
    if not m:
        sys.exit("FEHLER: %r in %s NICHT GEFUNDEN -- der Stellvertreter kann die echten\n"
                 "        Registerindizes/Ergebniscodes nicht uebernehmen." % (start, pfad))
    i, t = m.end(), 1
    while i < len(s) and t > 0:
        if s[i] == '{': t += 1
        elif s[i] == '}': t -= 1
        i += 1
    if t != 0:
        sys.exit("FEHLER: Block zu %r in %s nicht geschlossen." % (start, pfad))
    return s[m.start():i]

mw = re.search(r'pub const MSG_WORDS\s*:\s*usize\s*=\s*\d+\s*;', s)
if not mw:
    sys.exit("FEHLER: `pub const MSG_WORDS` in %s NICHT GEFUNDEN." % pfad)
teile = [mw.group(0), block(r'pub mod reg\s*\{'), block(r'pub mod result\s*\{')]
print("// Aus crates/sel4lake-abi/src/lib.rs uebernommen (extrahiert, nicht abgeschrieben).")
print("mod sel4lake_abi {")
for t in teile:
    print("\n".join("    " + z if z.strip() else z for z in t.splitlines()))
print("}")
PY
}

# ================================================================================================
# 3. Der Anbau: Stellvertreter fuer HAL/Scheduler, die Abbildung `alpha` und die Faelle.
#    Alles hier ist NEU -- es bildet weder Modell noch Code nach, es verbindet sie.
# ================================================================================================
anbau_schreiben() {   # anbau_schreiben <zieldatei>
    cat > "$1" <<'RSEOF'

// ================================================================================================
// Ab hier: der Anbau des Modell-Treue-Waechters. Nicht Teil von `sel4lake-ipc`.
// ================================================================================================

/// Stellvertreter fuer `sel4lake-hal`: ein Frame ist ein Index in eine globale Registerablage.
/// Was ein echter Frame tut, sagt dieser Lauf NICHT -- hier zaehlt nur, dass ein `transfer`
/// beobachtbar wird.
mod sel4lake_hal {
    pub mod exception {
        use std::sync::Mutex;
        pub const NREG: usize = 8;
        pub static FRAMES: Mutex<Vec<[u64; NREG]>> = Mutex::new(Vec::new());
        /// Neuen Frame anlegen; die Nummerierung ist 1-basiert, damit 0 „kein Frame" bleibt.
        pub fn frame_neu() -> usize {
            let mut f = FRAMES.lock().unwrap();
            f.push([0u64; NREG]);
            f.len()
        }
        pub fn frame_reg(frame: usize, idx: usize) -> u64 {
            FRAMES.lock().unwrap()[frame - 1][idx.min(NREG - 1)]
        }
        pub fn frame_set_reg(frame: usize, idx: usize, val: u64) {
            FRAMES.lock().unwrap()[frame - 1][idx.min(NREG - 1)] = val;
        }
    }
}

/// Stellvertreter fuer `sel4lake-sched`. `ThreadId` ist quelltextgleich zum Original (die
/// Packung nach `u64` ist die Bruecke zu den `nat`s des Modells); `SchedOps` traegt genau die
/// Methoden, die `sel4lake-ipc` aufruft -- ruft es eine weitere, bricht die Uebersetzung ab.
mod sel4lake_sched {
    use std::sync::Mutex;

    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub struct ThreadId {
        slot: usize,
        gen: u32,
    }
    impl ThreadId {
        pub fn to_raw(self) -> u64 {
            (self.slot as u64) | ((self.gen as u64) << 32)
        }
        pub fn from_raw(raw: u64) -> Self {
            Self { slot: (raw & 0xffff_ffff) as usize, gen: (raw >> 32) as u32 }
        }
        pub fn slot(self) -> usize {
            self.slot
        }
    }

    pub trait SchedOps {
        fn current_id(&mut self, core: usize) -> ThreadId;
        fn frame_of(&mut self, tid: ThreadId) -> Option<usize>;
        fn block_current(&mut self, core: usize, frame: usize) -> usize;
        fn switch_to(&mut self, core: usize, frame: usize, target: ThreadId) -> usize;
        fn unblock(&mut self, tid: ThreadId);
        fn end_donation(&mut self, core: usize);
    }

    pub static HEIMATKERN: Mutex<Vec<(u64, usize)>> = Mutex::new(Vec::new());
    pub fn setze_kern(tid: ThreadId, kern: usize) {
        HEIMATKERN.lock().unwrap().push((tid.to_raw(), kern));
    }
    /// Wie im Original lock-frei befragt; entscheidet in `call` ueber Fastpath vs. IPI.
    pub fn owner_core(tid: ThreadId) -> Option<usize> {
        let r = tid.to_raw();
        HEIMATKERN.lock().unwrap().iter().rev().find(|(t, _)| *t == r).map(|(_, k)| *k)
    }
}

#[path = "modell.rs"]
mod modell;

mod treue {
    use crate::modell;
    use crate::sel4lake_abi::{reg, result};
    use crate::sel4lake_hal::exception::{frame_neu, frame_reg, frame_set_reg};
    use crate::sel4lake_sched::{owner_core, setze_kern, SchedOps, ThreadId};
    use crate::{Endpoint, QUEUE_CAP};
    use std::collections::{HashMap, HashSet};

    /// Ein Wert, den weder ABI noch Code je schreiben. Steht er nach der Operation noch im
    /// Ergebnisregister, hat sie dem Aufrufer KEINEN Code gegeben -- sie hat ihn also nicht
    /// abgewiesen. So wird das Tor an seiner **Wirkung** gemessen und nicht am Quelltext.
    const SENTINEL: u64 = 0xDEAD_BEEF;
    /// Antwortwoerter des REPLY-Falls -- ausserhalb des Nachrichtenbereichs, damit die
    /// effektbasierte Zustellzaehlung sie nicht mitzaehlt.
    const ANTWORT1: u64 = 0x9000_0001;
    const ANTWORT2: u64 = 0x9000_0002;

    /// Der echte Ergebniscode -> der abstrakte Code des Modells. **Das ist die Zuordnung**, um die
    /// es bei A-4.2 geht; sie steht hier und nicht im Beweis, damit der Beweis nicht von
    /// ABI-Zahlen abhaengt. Ein unbekannter Code wird zu 99 und passt damit auf nichts.
    fn modellcode(echt: u64) -> u64 {
        if echt == SENTINEL || echt == result::OK {
            0
        } else if echt == result::ERR_BADCAP {
            1
        } else if echt == result::ERR_QUIESCING {
            2
        } else {
            99
        }
    }

    /// Ein Schritt: der Zustand davor, was das Modell daraus macht, was der Code daraus macht,
    /// und der Ergebniscode, den der Code dem Aufrufer gegeben hat (schon abgebildet).
    pub struct Schritt {
        vorher: modell::Endpoint,
        erwartet: modell::Endpoint,
        nachher: modell::Endpoint,
        code: u64,
    }

    /// Die Welt um den Endpoint: Faeden mit Frames und Heimatkernen, dazu die **effektbasierte**
    /// Buchhaltung ueber angekommene Nachrichten und ueber gestrandete Faeden.
    pub struct Welt {
        laufend: HashMap<usize, ThreadId>,
        frames: HashMap<u64, usize>,
        tot: HashSet<u64>,
        leerlauf: usize,
        /// Nachricht -> Absender. Solange sie nur dort liegt, ist sie nicht zugestellt.
        offen: HashMap<u64, u64>,
        angekommen: HashSet<u64>,
        zugestellt: u64,
        /// Faeden, fuer die `block_current` gerufen wurde -- in Aufrufreihenfolge.
        blockiert: Vec<u64>,
        /// Faeden, fuer die `unblock` gerufen wurde.
        geweckt: Vec<u64>,
        verworfene_sender: u64,
        verworfene_empfaenger: u64,
        naechste_id: u64,
        naechste_msg: u64,
    }

    impl SchedOps for Welt {
        fn current_id(&mut self, core: usize) -> ThreadId {
            *self.laufend.get(&core).expect("kein laufender Faden auf diesem Kern")
        }
        fn frame_of(&mut self, tid: ThreadId) -> Option<usize> {
            self.frame_von(tid)
        }
        fn block_current(&mut self, core: usize, _frame: usize) -> usize {
            if let Some(t) = self.laufend.get(&core).copied() {
                self.blockiert.push(t.to_raw());
            }
            self.leerlauf
        }
        fn switch_to(&mut self, _core: usize, _frame: usize, target: ThreadId) -> usize {
            let f = self.frame_von(target);
            f.unwrap_or(self.leerlauf)
        }
        fn unblock(&mut self, tid: ThreadId) {
            self.geweckt.push(tid.to_raw());
        }
        fn end_donation(&mut self, _core: usize) {}
    }

    impl Welt {
        pub fn neu() -> Welt {
            Welt {
                laufend: HashMap::new(),
                frames: HashMap::new(),
                tot: HashSet::new(),
                leerlauf: frame_neu(),
                offen: HashMap::new(),
                angekommen: HashSet::new(),
                zugestellt: 0,
                blockiert: Vec::new(),
                geweckt: Vec::new(),
                verworfene_sender: 0,
                verworfene_empfaenger: 0,
                naechste_id: 1,
                naechste_msg: 0x1000,
            }
        }

        pub fn faden(&mut self, kern: usize) -> ThreadId {
            let t = ThreadId::from_raw(self.naechste_id | (1u64 << 32));
            self.naechste_id += 1;
            let f = frame_neu();
            self.frames.insert(t.to_raw(), f);
            setze_kern(t, kern);
            t
        }

        /// Einen Faden sterben lassen: er hat keinen Frame mehr (`frame_of == None`) und bleibt
        /// als Leiche in der Warteschlange -- der Fall, den `call`/`recv` ueberspringen.
        pub fn toeten(&mut self, t: ThreadId) {
            self.tot.insert(t.to_raw());
        }

        fn frame_von(&self, t: ThreadId) -> Option<usize> {
            if self.tot.contains(&t.to_raw()) {
                None
            } else {
                self.frames.get(&t.to_raw()).copied()
            }
        }

        pub fn geweckt_worden(&self, t: ThreadId) -> bool {
            self.geweckt.contains(&t.to_raw())
        }

        /// **Die Abbildung.** Sie steht an genau dieser Stelle, damit sie ueberprueft und
        /// veraendert werden kann, statt in den Faellen verstreut zu sein. Die sechs Codefelder
        /// werden abgelesen, die drei Buchhaltungsfelder gemessen (s. `nachzaehlen`/`strandet`).
        pub fn alpha(&self, ep: &Endpoint) -> modell::Endpoint {
            let mut senders = Vec::new();
            ep.senders.for_each(|t| senders.push(t.to_raw()));
            let mut receivers = Vec::new();
            ep.receivers.for_each(|t| receivers.push(t.to_raw()));
            modell::Endpoint {
                used: ep.used,
                quiescing: ep.quiescing,
                senders,
                receivers,
                caller: ep.caller.map(|t| t.to_raw()),
                reply_owner: ep.reply_owner.map(|t| t.to_raw()),
                delivered: self.zugestellt,
                dropped_senders: self.verworfene_sender,
                dropped_receivers: self.verworfene_empfaenger,
            }
        }

        /// **Zustellung ist eine Wirkung, keine Absicht.** Gezaehlt wird erst, wenn das
        /// Nachrichtenwort im Frame eines ANDEREN Fadens steht.
        fn nachzaehlen(&mut self) {
            let paare: Vec<(u64, usize)> = self.frames.iter().map(|(a, b)| (*a, *b)).collect();
            for (tid, f) in paare {
                let m = frame_reg(f, reg::MSG0);
                if let Some(absender) = self.offen.get(&m).copied() {
                    if absender != tid && self.angekommen.insert(m) {
                        self.zugestellt += 1;
                    }
                }
            }
        }

        /// **Stranden ist ebenfalls eine Wirkung.** Ein Faden gilt als gestrandet, wenn er nach
        /// der Operation in KEINER der beiden Warteschlangen steht und KEIN Token haelt -- dann
        /// gibt es niemanden mehr, der ihn wecken koennte. Das wird gemessen und nicht aus
        /// `enqueue` abgelesen: „die Warteschlange war voll" ist eine Absicht, „niemand kann ihn
        /// mehr wecken" ist die Folge.
        fn strandet(&self, ep: &Endpoint, t: ThreadId) -> bool {
            !ep.senders.contains(t)
                && !ep.receivers.contains(t)
                && ep.caller != Some(t)
                && ep.reply_owner != Some(t)
        }

        /// Ein echter `call` -- mit dem Modellschritt, den er unter `alpha` bewirken muss.
        pub fn tue_call(&mut self, ep: &mut Endpoint, c: ThreadId, kern: usize) -> Schritt {
            let msg = self.naechste_msg;
            self.naechste_msg += 1;
            let vorher = self.alpha(ep);
            let erwartet = modell::send(&vorher, c.to_raw());
            let f = self.frame_von(c).expect("Aufrufer ohne Frame");
            frame_set_reg(f, reg::MSG0, msg);
            frame_set_reg(f, reg::SYSNO_RESULT, SENTINEL);
            self.offen.insert(msg, c.to_raw());
            self.laufend.insert(kern, c);
            let b0 = self.blockiert.len();
            ep.call(&mut *self, kern, f);
            if self.blockiert[b0..].contains(&c.to_raw()) && self.strandet(ep, c) {
                self.verworfene_sender += 1;
            }
            self.nachzaehlen();
            let nachher = self.alpha(ep);
            Schritt { vorher, erwartet, nachher, code: modellcode(frame_reg(f, reg::SYSNO_RESULT)) }
        }

        /// Ein echter `recv` -- mit seinem Modellschritt.
        pub fn tue_recv(&mut self, ep: &mut Endpoint, s: ThreadId, kern: usize) -> Schritt {
            let vorher = self.alpha(ep);
            let erwartet = modell::recv(&vorher, s.to_raw());
            let f = self.frame_von(s).expect("Empfaenger ohne Frame");
            frame_set_reg(f, reg::SYSNO_RESULT, SENTINEL);
            self.laufend.insert(kern, s);
            let b0 = self.blockiert.len();
            ep.recv(&mut *self, kern, f);
            if self.blockiert[b0..].contains(&s.to_raw()) && self.strandet(ep, s) {
                self.verworfene_empfaenger += 1;
            }
            self.nachzaehlen();
            let nachher = self.alpha(ep);
            Schritt { vorher, erwartet, nachher, code: modellcode(frame_reg(f, reg::SYSNO_RESULT)) }
        }

        /// Ein echter `reply`. `antwort` wird vorher in den Frame des Servers gelegt, damit die
        /// **Wirkung** der Antwort im Frame des Aufrufers nachweisbar ist.
        pub fn tue_reply(&mut self, ep: &mut Endpoint, s: ThreadId, kern: usize, antwort: u64)
            -> Schritt {
            let vorher = self.alpha(ep);
            let erwartet = modell::reply(&vorher);
            let f = self.frame_von(s).expect("Server ohne Frame");
            frame_set_reg(f, reg::MSG0, antwort);
            frame_set_reg(f, reg::SYSNO_RESULT, SENTINEL);
            self.laufend.insert(kern, s);
            ep.reply(&mut *self, kern, f);
            self.nachzaehlen();
            let nachher = self.alpha(ep);
            Schritt { vorher, erwartet, nachher, code: modellcode(frame_reg(f, reg::SYSNO_RESULT)) }
        }

        /// `bind_receiver` (A-4.1). Meldet die Operation Erfolg, ohne dass der Faden danach
        /// irgendwo steht, ist er gestrandet -- dieselbe Wirkung wie eine volle Warteschlange.
        pub fn tue_bind(&mut self, ep: &mut Endpoint, t: ThreadId)
            -> (modell::Endpoint, modell::Endpoint, bool) {
            let vorher = self.alpha(ep);
            let erwartet = modell::bind_receiver(&vorher, t.to_raw());
            let ok = ep.bind_receiver(t);
            if ok && self.strandet(ep, t) {
                self.verworfene_empfaenger += 1;
            }
            (erwartet, self.alpha(ep), ok)
        }

        /// `migrate_owner` (A-4.3). Gelingt die Migration, ohne dass der ehemalige Aufrufer
        /// danach irgendwo steht, ist er gestrandet.
        pub fn tue_migrate(&mut self, ep: &mut Endpoint, alt: ThreadId)
            -> (modell::Endpoint, modell::Endpoint, bool) {
            let vorher = self.alpha(ep);
            let erwartet = modell::migrate_owner(&vorher, alt.to_raw());
            let vorher_caller = ep.caller();
            let ok = ep.migrate_owner(alt);
            if ok {
                if let Some(c) = vorher_caller {
                    if self.strandet(ep, c) {
                        self.verworfene_sender += 1;
                    }
                }
            }
            (erwartet, self.alpha(ep), ok)
        }

        pub fn tue_begin_quiesce(&mut self, ep: &mut Endpoint)
            -> (modell::Endpoint, modell::Endpoint) {
            let erwartet = modell::begin_quiesce(&self.alpha(ep));
            ep.begin_quiesce();
            (erwartet, self.alpha(ep))
        }

        pub fn tue_end_quiesce(&mut self, ep: &mut Endpoint)
            -> (modell::Endpoint, modell::Endpoint) {
            let erwartet = modell::end_quiesce(&self.alpha(ep));
            ep.end_quiesce();
            (erwartet, self.alpha(ep))
        }

        // -- Auskuenfte fuer die Faelle (Wirkungen, nicht Absichten) ---------------------------
        /// Das Nachrichtenwort im Frame dieses Fadens.
        pub fn wort(&self, t: ThreadId) -> u64 {
            frame_reg(self.frames[&t.to_raw()], reg::MSG0)
        }
        /// Die zuletzt von `tue_call` vergebene Nachricht.
        pub fn letzte_msg(&self) -> u64 {
            self.naechste_msg - 1
        }
        pub fn blockiert_worden(&self, t: ThreadId) -> bool {
            self.blockiert.contains(&t.to_raw())
        }
        pub fn weck_zahl(&self, t: ThreadId) -> usize {
            self.geweckt.iter().filter(|x| **x == t.to_raw()).count()
        }
    }

    /// Ein synthetischer Modellzustand -- **ohne** Code dahinter. Damit laesst sich pruefen, ob
    /// die Praedikate des Modells ueberhaupt noch urteilen koennen: ein `ep_inv`, das auf `true`
    /// aufgeweicht wurde, faellt sonst nirgends auf, weil der Code den verletzenden Zustand nur
    /// ueber drei Ecken erreicht.
    fn synth(used: bool, quiescing: bool, s: u64, r: u64,
             caller: Option<u64>, owner: Option<u64>) -> modell::Endpoint {
        modell::Endpoint {
            used,
            quiescing,
            senders: (1..=s).collect(),
            receivers: (101..=100 + r).collect(),
            caller,
            reply_owner: owner,
            delivered: 0,
            dropped_senders: 0,
            dropped_receivers: 0,
        }
    }

    struct Bericht {
        n: u32,
        fehler: u32,
    }

    impl Bericht {
        /// Entsprechung fuer `call`/`recv`: der echte Schritt tut unter `alpha` dasselbe wie der
        /// Modellschritt, `ep_inv`/`token_inv` gelten am Ergebnis, die Buchhaltung stimmt, und
        /// der Ergebniscode ist der, den das Tor des Modells vorhersagt.
        ///
        /// `sendend` traegt die bewiesene Buchhaltung mit: ein zugelassenes `send` erhoeht
        /// `msgs_total` um genau 1 (zugestellt ODER eingereiht ODER als verworfen gezaehlt --
        /// `send_accounts_every_message`), ein abgewiesenes gar nicht, `recv` nie.
        fn deckt(&mut self, name: &str, s: &Schritt, sendend: bool) {
            let erwartete_summe = modell::msgs_total(&s.vorher)
                + u64::from(sendend && modell::gate(&s.vorher) == 0);
            self.urteil(name, s, erwartete_summe, modell::gate(&s.vorher));
        }

        /// Entsprechung fuer `reply`: **nicht** vom Stilllegungstor betroffen (A-4.2), nur von
        /// `used`. Die Summe bleibt unveraendert -- eine Antwort ist keine neue Nachricht.
        fn deckt_reply(&mut self, name: &str, s: &Schritt) {
            let summe = modell::msgs_total(&s.vorher);
            let code = if s.vorher.used { 0 } else { 1 };
            self.urteil(name, s, summe, code);
        }

        fn urteil(&mut self, name: &str, s: &Schritt, summe_soll: u64, code_soll: u64) {
            self.n += 1;
            let gleich = s.erwartet == s.nachher;
            let inv = modell::ep_inv(&s.nachher);
            let tinv = modell::token_inv(&s.nachher);
            let summe = modell::msgs_total(&s.nachher) == summe_soll;
            let tor = s.code == code_soll;
            if gleich && inv && tinv && summe && tor {
                println!("  deckt   : {}", name);
            } else {
                self.fehler += 1;
                println!("  ABWEICHUNG: {}", name);
                if !gleich {
                    println!("              Modell {:?}", s.erwartet);
                    println!("              Code   {:?}", s.nachher);
                }
                if !inv {
                    println!("              ep_inv verletzt: {:?}", s.nachher);
                }
                if !tinv {
                    println!("              token_inv verletzt: caller={:?} reply_owner={:?}",
                             s.nachher.caller, s.nachher.reply_owner);
                }
                if !summe {
                    println!("              msgs_total {} statt {}",
                             modell::msgs_total(&s.nachher), summe_soll);
                }
                if !tor {
                    println!("              Ergebniscode {} statt {} (Modellcodes: 0 zulassen, \
                              1 BADCAP, 2 QUIESCING, 99 unbekannt)", s.code, code_soll);
                }
            }
        }

        /// Entsprechung fuer die Operationen ohne Frame (`bind_receiver`, `migrate_owner`,
        /// `begin_quiesce`, `end_quiesce`): nur Zustandsgleichheit + `token_inv`. `ep_inv` wird
        /// hier bewusst NICHT verlangt -- genau diese Operationen duerfen sie verletzen, und das
        /// steht als eigener Befund unten.
        fn deckt_zustand(&mut self, name: &str, erwartet: &modell::Endpoint,
                         nachher: &modell::Endpoint) {
            self.n += 1;
            if erwartet == nachher && modell::token_inv(nachher) {
                println!("  deckt   : {}", name);
            } else {
                self.fehler += 1;
                println!("  ABWEICHUNG: {}", name);
                println!("              Modell {:?}", erwartet);
                println!("              Code   {:?}", nachher);
            }
        }

        /// Eine **Sprechprobe am Modell allein**: gilt die Aussage an einem hingeschriebenen
        /// Zustand? Ohne sie koennte ein aufgeweichtes Praedikat unbemerkt bleiben, weil der Code
        /// den verletzenden Zustand nie erzeugt.
        fn probe(&mut self, name: &str, wahr: bool) {
            self.n += 1;
            if wahr {
                println!("  Probe   : {}", name);
            } else {
                self.fehler += 1;
                println!("  FEHLER  : {} -- die Sprechprobe schlaegt fehl. Das Modell urteilt", name);
                println!("              hier nicht mehr (aufgeweicht?) oder die Probe misst nicht,");
                println!("              was sie messen soll.");
            }
        }

        /// Gegenprobe: hier MUSS die Entsprechung zerbrechen. Eine Nebenbedingung, deren
        /// Verletzung folgenlos bliebe, waere keine Nebenbedingung.
        fn weicht_ab(&mut self, name: &str, erwartet: &modell::Endpoint,
                     nachher: &modell::Endpoint) {
            self.n += 1;
            if erwartet != nachher {
                println!("  Luecke  : {}", name);
                println!("              Modell {:?}", erwartet);
                println!("              Code   {:?}", nachher);
            } else {
                self.fehler += 1;
                println!("  FEHLER  : {} -- die Entsprechung haelt hier, obwohl sie nicht", name);
                println!("              halten kann. Entweder ist der Fall nicht mehr erreichbar");
                println!("              (dann gehoert der Kopf dieses Waechters geaendert), oder");
                println!("              die Gegenprobe misst nicht, was sie messen soll.");
            }
        }

        /// Befund: eine Aussage gilt am echten Endpoint NICHT. Wird als Tatsache geprueft, damit
        /// sie nicht stillschweigend verschwindet -- **und damit ein behobener Befund auffliegt**
        /// statt fuer immer weiterzuschreien: trifft er nicht mehr zu, schlaegt der Waechter fehl
        /// und verlangt, dass er hier heraus und ins `done.md` wandert.
        fn befund(&mut self, name: &str, gilt_nicht: bool) {
            self.n += 1;
            if gilt_nicht {
                println!("  Befund  : {}", name);
            } else {
                self.fehler += 1;
                println!("  FEHLER  : {} -- der Befund trifft nicht mehr zu.", name);
                println!("              Entweder ist er behoben (dann gehoert er hier heraus und");
                println!("              ins done.md), oder die Aussage, die ihn nachweist, ist");
                println!("              aufgeweicht worden.");
            }
        }
    }

    impl Bericht {
        /// Wie `deckt`, aber ohne Zeile -- fuer lange Ketten, deren Ergebnis danach als EINE
        /// Aussage berichtet wird. Ein Fehlschlag verschwindet damit nicht, er kippt die Aussage.
        fn deckt_leise(&self, s: &Schritt, sendend: bool) -> bool {
            let soll = modell::msgs_total(&s.vorher)
                + u64::from(sendend && modell::gate(&s.vorher) == 0);
            s.erwartet == s.nachher
                && modell::ep_inv(&s.nachher)
                && modell::token_inv(&s.nachher)
                && modell::msgs_total(&s.nachher) == soll
                && s.code == modell::gate(&s.vorher)
        }
        fn deckt_reply_leise(&self, s: &Schritt) -> bool {
            s.erwartet == s.nachher
                && modell::token_inv(&s.nachher)
                && modell::msgs_total(&s.nachher) == modell::msgs_total(&s.vorher)
                && s.code == if s.vorher.used { 0 } else { 1 }
        }
    }

    pub fn run() -> i32 {
        let mut b = Bericht { n: 0, fehler: 0 };

        // -- S: Sprechproben. Kann das Modell ueberhaupt noch urteilen? -------------------------
        //    Diese Faelle haben KEINEN Code dahinter. Sie sind noetig, weil die verletzenden
        //    Zustaende ueber die oeffentliche Schnittstelle nur ueber drei Ecken erreichbar sind:
        //    ein auf `true` aufgeweichtes Praedikat fiele sonst nirgends auf.
        b.probe(&format!("die Schranke des Modells ist die des Codes (queue_cap {} == QUEUE_CAP {})",
                         modell::queue_cap(), QUEUE_CAP),
                modell::queue_cap() == QUEUE_CAP as u64);
        b.probe("die zwei Abweisungsgruende sind im ECHTEN ABI verschiedene Codes \
                 (sonst waere die Unterscheidung nur eine Absicht)",
                result::ERR_BADCAP != result::ERR_QUIESCING
                    && result::ERR_BADCAP != result::OK
                    && result::ERR_QUIESCING != result::OK);
        {
            let unbelegt = synth(false, false, 0, 0, None, None);
            let still = synth(true, true, 0, 0, None, None);
            let offen = synth(true, false, 0, 0, None, None);
            b.probe("Tor: unbelegt wird abgewiesen", modell::gate(&unbelegt) != 0);
            b.probe("Tor: stillgelegt wird abgewiesen", modell::gate(&still) != 0);
            b.probe("Tor: der gesunde Endpoint laesst durch (Positivkontrolle)",
                    modell::gate(&offen) == 0);
            b.probe("Tor: die beiden Abweisungsgruende sind UNTERSCHEIDBAR (A-4.2)",
                    modell::gate(&unbelegt) != modell::gate(&still));
        }
        {
            let faellig = synth(true, false, 2, 2, None, None);
            let gesund = synth(true, false, 2, 0, None, None);
            b.probe("ep_inv_strong urteilt: bei Sendern UND Empfaengern ist es verletzt",
                    !modell::ep_inv_strong(&faellig));
            b.probe("ep_inv_strong urteilt: am gesunden Zustand gilt es (Positivkontrolle)",
                    modell::ep_inv_strong(&gesund));
            b.probe("ep_inv urteilt: ausserhalb der Stilllegung ist derselbe Zustand verletzt",
                    !modell::ep_inv(&faellig));
            b.probe("ep_inv urteilt: UNTER Stilllegung ist er zugelassen (die Aufrufdisziplin)",
                    modell::ep_inv(&synth(true, true, 2, 2, None, None)));
        }
        {
            let halb = synth(true, false, 0, 0, Some(7), None);
            let halb2 = synth(true, false, 0, 0, None, Some(9));
            let ganz = synth(true, false, 0, 0, Some(7), Some(9));
            b.probe("token_inv urteilt: ein Aufrufer ohne Antwortpflicht ist verletzt",
                    !modell::token_inv(&halb));
            b.probe("token_inv urteilt: eine Antwortpflicht ohne Aufrufer ist verletzt",
                    !modell::token_inv(&halb2));
            b.probe("token_inv urteilt: beides gesetzt ist zulaessig (Positivkontrolle)",
                    modell::token_inv(&ganz));
        }
        {
            let mut m = synth(true, false, 3, 0, None, None);
            m.delivered = 5;
            m.dropped_senders = 2;
            b.probe("msgs_total zaehlt den VERLUST mit (5 + 3 + 2 == 10)",
                    modell::msgs_total(&m) == 10);
        }

        // -- A: die Entsprechung unter den Nebenbedingungen ------------------------------------
        {
            let mut w = Welt::neu();
            let mut ep = Endpoint::EMPTY;
            ep.mark_used();
            let s = w.faden(0);
            let c = w.faden(0);

            let sch = w.tue_recv(&mut ep, s, 0);
            b.deckt("RECV auf leeren Endpoint -> Empfaenger parkt", &sch, false);
            let sch = w.tue_call(&mut ep, c, 0);
            b.deckt("CALL trifft wartenden Empfaenger (gleicher Kern, switch_to)", &sch, true);

            let sch = w.tue_reply(&mut ep, s, 0, ANTWORT1);
            b.deckt_reply("REPLY konsumiert das Token", &sch);
            b.probe("REPLY wirkt: die Antwort steht im Frame des Aufrufers, und er wird geweckt",
                    w.wort(c) == ANTWORT1 && w.weck_zahl(c) == 1);

            let sch = w.tue_reply(&mut ep, s, 0, ANTWORT2);
            b.deckt_reply("zweites REPLY findet kein Token", &sch);
            b.probe("kein Doppel-Reply (Wirkung): der Aufrufer traegt weiter die ERSTE Antwort \
                     und wurde kein zweites Mal geweckt",
                    w.wort(c) == ANTWORT1 && w.weck_zahl(c) == 1);
        }
        {
            let mut w = Welt::neu();
            let mut ep = Endpoint::EMPTY;
            ep.mark_used();
            let c = w.faden(0);
            let s = w.faden(0);

            let sch = w.tue_call(&mut ep, c, 0);
            b.deckt("CALL auf leeren Endpoint -> Sender parkt", &sch, true);
            let sch = w.tue_recv(&mut ep, s, 0);
            b.deckt("RECV holt wartenden Sender", &sch, false);
        }
        {
            // FIFO beider Warteschlangen: das Modell nimmt `drop_first`, der Code `dequeue`.
            let mut w = Welt::neu();
            let mut ep = Endpoint::EMPTY;
            ep.mark_used();
            let c1 = w.faden(0);
            let c2 = w.faden(0);
            let c3 = w.faden(0);
            let s1 = w.faden(0);
            let s2 = w.faden(0);
            for (i, c) in [c1, c2, c3].iter().enumerate() {
                let sch = w.tue_call(&mut ep, *c, 0);
                b.deckt(&format!("CALL {} von drei parkt in Reihenfolge", i + 1), &sch, true);
            }
            for (i, s) in [s1, s2].iter().enumerate() {
                let sch = w.tue_recv(&mut ep, *s, 0);
                b.deckt(&format!("RECV {} nimmt den AELTESTEN Sender (FIFO)", i + 1), &sch, false);
            }
        }
        {
            // Der zweite Kern-Pfad: Empfaenger auf einem FREMDEN Kern -> unblock statt switch_to.
            let mut w = Welt::neu();
            let mut ep = Endpoint::EMPTY;
            ep.mark_used();
            let s = w.faden(3);
            let c = w.faden(0);
            let sch = w.tue_recv(&mut ep, s, 3);
            b.deckt("RECV auf Kern 3 parkt", &sch, false);
            let sch = w.tue_call(&mut ep, c, 0);
            b.deckt("CALL trifft Empfaenger auf FREMDEM Kern (unblock+IPI-Pfad)", &sch, true);
        }
        {
            let mut w = Welt::neu();
            let mut ep = Endpoint::EMPTY;
            ep.mark_used();
            let s1 = w.faden(0);
            let s2 = w.faden(0);
            let c1 = w.faden(0);
            let c2 = w.faden(0);
            for (i, s) in [s1, s2].iter().enumerate() {
                let sch = w.tue_recv(&mut ep, *s, 0);
                b.deckt(&format!("RECV {} von zwei parkt", i + 1), &sch, false);
            }
            for (i, c) in [c1, c2].iter().enumerate() {
                let sch = w.tue_call(&mut ep, *c, 0);
                b.deckt(&format!("CALL {} nimmt den AELTESTEN Empfaenger (FIFO)", i + 1), &sch, true);
            }
        }
        {
            // Eine laengere gemischte Kette -- ein einzelner Uebergang kann zufaellig stimmen.
            let mut w = Welt::neu();
            let mut ep = Endpoint::EMPTY;
            ep.mark_used();
            let a = w.faden(0);
            let bb = w.faden(1);
            let c = w.faden(0);
            let d = w.faden(1);
            let plan: [(u8, ThreadId, usize); 8] = [
                (0, a, 0), (0, bb, 1), (1, c, 0), (1, d, 1),
                (1, c, 0), (0, a, 0), (0, bb, 1), (1, d, 1),
            ];
            for (k, (art, t, kern)) in plan.iter().enumerate() {
                let sendend = *art == 1;
                let sch = if sendend {
                    w.tue_call(&mut ep, *t, *kern)
                } else {
                    w.tue_recv(&mut ep, *t, *kern)
                };
                b.deckt(&format!("gemischte Kette, Schritt {} ({})", k + 1,
                                 if sendend { "CALL" } else { "RECV" }), &sch, sendend);
            }
        }

        // -- A-4.2: das Stilllegungstor -------------------------------------------------------
        {
            let mut w = Welt::neu();
            let mut ep = Endpoint::EMPTY;
            ep.mark_used();
            let s = w.faden(0);
            let c = w.faden(0);
            let s2 = w.faden(0);
            let c2 = w.faden(0);
            let sch = w.tue_recv(&mut ep, s, 0);
            b.deckt("RECV parkt (Vorbereitung)", &sch, false);
            let sch = w.tue_call(&mut ep, c, 0);
            b.deckt("CALL trifft den Empfaenger (Vorbereitung)", &sch, true);

            let (e, n) = w.tue_begin_quiesce(&mut ep);
            b.deckt_zustand("begin_quiesce schliesst das Tor", &e, &n);
            let sch = w.tue_call(&mut ep, c2, 0);
            b.deckt("CALL am stillgelegten Endpoint -> ERR_QUIESCING, und zwar WIRKUNGSLOS", &sch, true);
            let sch = w.tue_recv(&mut ep, s2, 0);
            b.deckt("RECV am stillgelegten Endpoint -> ERR_QUIESCING, und zwar WIRKUNGSLOS", &sch, false);
            b.probe("A-4.2: der abgewiesene Aufrufer wurde NICHT blockiert",
                    !w.blockiert_worden(c2) && !w.blockiert_worden(s2));

            let sch = w.tue_reply(&mut ep, s, 0, ANTWORT1);
            b.deckt_reply("REPLY wirkt trotz Stilllegung (nur NEUE Transaktionen werden abgewiesen)",
                          &sch);
            b.probe("A-4.2: der Aufrufer bekommt seine Antwort, obwohl das Tor zu ist",
                    w.wort(c) == ANTWORT1 && w.weck_zahl(c) == 1);

            let (e, n) = w.tue_end_quiesce(&mut ep);
            b.deckt_zustand("end_quiesce oeffnet das Tor wieder", &e, &n);
            let sch = w.tue_recv(&mut ep, s2, 0);
            b.deckt("RECV nach end_quiesce parkt wieder regulaer", &sch, false);
        }
        {
            let mut w = Welt::neu();
            let mut ep = Endpoint::EMPTY; // NICHT belegt
            let c = w.faden(0);
            let s = w.faden(0);
            let sch = w.tue_call(&mut ep, c, 0);
            b.deckt("CALL auf unbelegtem Endpoint -> ERR_BADCAP, wirkungslos", &sch, true);
            let sch = w.tue_recv(&mut ep, s, 0);
            b.deckt("RECV auf unbelegtem Endpoint -> ERR_BADCAP, wirkungslos", &sch, false);
            let sch = w.tue_reply(&mut ep, s, 0, ANTWORT1);
            b.deckt_reply("REPLY auf unbelegtem Endpoint -> ERR_BADCAP", &sch);
        }

        // -- Die Kapazitaetsschranke ----------------------------------------------------------
        {
            let mut w = Welt::neu();
            let mut ep = Endpoint::EMPTY;
            ep.mark_used();
            let mut alle: Vec<ThreadId> = Vec::new();
            let mut kette = true;
            for i in 0..QUEUE_CAP - 1 {
                let c = w.faden(0);
                alle.push(c);
                let sch = w.tue_call(&mut ep, c, 0);
                kette &= b.deckt_leise(&sch, true);
                let _ = i;
            }
            b.probe(&format!("CALL 1..{} decken (die Warteschlange fuellt sich)", QUEUE_CAP - 1),
                    kette);
            let vorletzter = w.faden(0);
            alle.push(vorletzter);
            let sch = w.tue_call(&mut ep, vorletzter, 0);
            b.deckt(&format!("CALL {} nimmt den LETZTEN Platz", QUEUE_CAP), &sch, true);

            let letzter = w.faden(0);
            alle.push(letzter);
            let sch = w.tue_call(&mut ep, letzter, 0);
            b.deckt(&format!("CALL {} -- der Sender wird STILL verworfen (das Modell sagt es \
                              voraus, statt es zu uebergehen)", QUEUE_CAP + 1), &sch, true);
            b.befund(&format!("B2 der {}. Sender: block_current gerufen, in KEINER Warteschlange, \
                               kein Token, KEIN Ergebniscode -- und quiescence_of meldet ihn als \
                               ruhig", QUEUE_CAP + 1),
                     w.blockiert_worden(letzter)
                         && !ep.senders.contains(letzter)
                         && ep.caller() != Some(letzter)
                         && sch.code == 0
                         && ep.quiescence_of(letzter).is_quiescent());
            b.probe(&format!("die Buchhaltung sieht den Verlust: dropped_senders == 1 bei \
                              {} Aufrufern", QUEUE_CAP + 1),
                    w.alpha(&ep).dropped_senders == 1);

            // Alle abarbeiten: wie viele werden je geweckt?
            let mut kette = true;
            for _ in 0..=QUEUE_CAP {
                let srv = w.faden(0);
                let sch = w.tue_recv(&mut ep, srv, 0);
                kette &= b.deckt_leise(&sch, false);
                let sch = w.tue_reply(&mut ep, srv, 0, ANTWORT1);
                kette &= b.deckt_reply_leise(&sch);
            }
            b.probe(&format!("{} RECV+REPLY danach decken ebenfalls", QUEUE_CAP + 1), kette);
            let geweckt = alle.iter().filter(|c| w.weck_zahl(**c) > 0).count();
            b.befund(&format!("B2 Folge: von {} Aufrufern werden {} geweckt -- der letzte NIE. \
                               Er bleibt blockiert, ohne dass ihn jemand wecken koennte",
                              QUEUE_CAP + 1, geweckt),
                     geweckt == QUEUE_CAP && w.weck_zahl(letzter) == 0);
            b.befund("B2 Folge: auch purge_thread findet ihn nicht -- er steht in keiner Struktur \
                      des Endpoints, und audit meldet weder Leiche noch Duplikat",
                     !ep.purge_thread(letzter) && ep.audit(&mut |_t: ThreadId| true) == (false, false));
        }
        {
            // Dieselbe Schranke auf der EMPFAENGER-Seite.
            let mut w = Welt::neu();
            let mut ep = Endpoint::EMPTY;
            ep.mark_used();
            let mut kette = true;
            for _ in 0..QUEUE_CAP {
                let s = w.faden(0);
                let sch = w.tue_recv(&mut ep, s, 0);
                kette &= b.deckt_leise(&sch, false);
            }
            b.probe(&format!("RECV 1..{} decken (die Empfaenger-Warteschlange fuellt sich)",
                             QUEUE_CAP), kette);
            let letzter = w.faden(0);
            let sch = w.tue_recv(&mut ep, letzter, 0);
            b.deckt(&format!("RECV {} -- der Empfaenger wird STILL verworfen", QUEUE_CAP + 1),
                    &sch, false);
            b.befund(&format!("B2b dieselbe Schranke, andere Seite: der {}. RECV blockiert, steht \
                               in keiner Warteschlange und gilt als ruhig", QUEUE_CAP + 1),
                     w.blockiert_worden(letzter)
                         && !ep.receivers.contains(letzter)
                         && ep.quiescence_of(letzter).is_quiescent());

            // Und `bind_receiver` an derselben vollen Warteschlange.
            let v = w.faden(0);
            let (e, n, ok) = w.tue_bind(&mut ep, v);
            b.deckt_zustand("bind_receiver bei VOLLER Empfaenger-Warteschlange", &e, &n);
            b.befund("B2c bind_receiver meldet ERFOLG, obwohl der Empfaenger verworfen wurde -- \
                      ein Aufrufer, der darauf baut, haelt einen Endpoint fuer gebunden, der es \
                      nicht ist",
                     ok && !ep.receivers.contains(v));
        }

        // -- A-4.1/A-4.3: bind_receiver und migrate_owner --------------------------------------
        {
            let mut w = Welt::neu();
            let mut ep = Endpoint::EMPTY;
            ep.mark_used();
            let c = w.faden(0);
            let v2 = w.faden(0);
            let sch = w.tue_call(&mut ep, c, 0);
            b.deckt("CALL parkt (kein Empfaenger da)", &sch, true);
            let (e, n, ok) = w.tue_bind(&mut ep, v2);
            b.deckt_zustand("bind_receiver bei wartendem Sender", &e, &n);
            b.befund("B1a bind_receiver sieht die Sender-Warteschlange nicht an -- \
                      ep_inv_strong ist danach verletzt",
                     ok && !n.senders.is_empty() && !n.receivers.is_empty()
                         && !modell::ep_inv_strong(&n) && !modell::ep_inv(&n));
            b.probe("die Aufrufdisziplin traegt: unter Stilllegung ist derselbe Zustand von \
                     ep_inv zugelassen",
                    modell::ep_inv(&modell::begin_quiesce(&n)));
        }
        {
            let mut w = Welt::neu();
            let mut ep = Endpoint::EMPTY;
            ep.mark_used();
            let v1 = w.faden(0);
            let c = w.faden(0);
            let v2 = w.faden(0);
            let c2 = w.faden(0);
            let sch = w.tue_recv(&mut ep, v1, 0);
            b.deckt("RECV v1 parkt (Hot-Reload-Aufbau)", &sch, false);
            let sch = w.tue_call(&mut ep, c, 0);
            b.deckt("CALL c trifft v1 -- v1 schuldet c eine Antwort", &sch, true);
            let (e, n, _) = w.tue_bind(&mut ep, v2);
            b.deckt_zustand("bind_receiver bindet v2, ohne dass v2 RECV ruft (A-4.1)", &e, &n);
            let (e, n, ok) = w.tue_migrate(&mut ep, v1);
            b.deckt_zustand("migrate_owner reiht den Aufrufer wieder als Sender ein (A-4.3)", &e, &n);
            b.befund("B1b migrate_owner bei geparktem Empfaenger: Sender UND Empfaenger stehen an, \
                      ep_inv_strong ist verletzt -- ein Rendezvous ist FAELLIG",
                     ok && !n.senders.is_empty() && !n.receivers.is_empty()
                         && !modell::ep_inv_strong(&n));

            let sch = w.tue_call(&mut ep, c2, 0);
            b.deckt("ein DRITTER ruft -- und trifft v2 sofort", &sch, true);
            b.befund("B4 der migrierte Aufrufer wird UEBERHOLT: der Dritte bekommt v2, waehrend c \
                      in der Sender-Warteschlange stehenbleibt. Sein faelliges Rendezvous wird von \
                      keiner Operation nachgeholt -- beide Seiten waren blockiert",
                     w.wort(v2) == w.letzte_msg() && ep.senders.contains(c)
                         && w.weck_zahl(c) == 0);
        }
        {
            // migrate_owner an der VOLLEN Sender-Warteschlange.
            let mut w = Welt::neu();
            let mut ep = Endpoint::EMPTY;
            ep.mark_used();
            let mut kette = true;
            for _ in 0..QUEUE_CAP {
                let c = w.faden(0);
                kette &= b.deckt_leise(&w.tue_call(&mut ep, c, 0), true);
            }
            let srv = w.faden(0);
            let sch = w.tue_recv(&mut ep, srv, 0);
            kette &= b.deckt_leise(&sch, false);
            let nach = w.faden(0);
            kette &= b.deckt_leise(&w.tue_call(&mut ep, nach, 0), true);
            b.probe("Aufbau fuer die volle Sender-Warteschlange deckt", kette);
            let erster = ep.caller();
            let (e, n, ok) = w.tue_migrate(&mut ep, srv);
            b.deckt_zustand("migrate_owner bei VOLLER Sender-Warteschlange", &e, &n);
            b.befund("B2d migrate_owner meldet ERFOLG, obwohl der Aufrufer beim Wiedereinreihen \
                      verworfen wurde -- die Antwortpflicht ist geloescht und der Client ist weg",
                     ok && erster.is_some() && !ep.senders.contains(erster.unwrap())
                         && ep.caller().is_none());
        }

        // -- Das Reply-Token: die schwierige Stelle --------------------------------------------
        {
            let mut w = Welt::neu();
            let mut ep = Endpoint::EMPTY;
            ep.mark_used();
            let a = w.faden(0);
            let zwei = w.faden(0);
            let srv = w.faden(0);
            let sch = w.tue_call(&mut ep, a, 0);
            b.deckt("CALL A parkt", &sch, true);
            let sch = w.tue_call(&mut ep, zwei, 0);
            b.deckt("CALL B parkt", &sch, true);
            let sch = w.tue_recv(&mut ep, srv, 0);
            b.deckt("RECV 1 nimmt A -- der Server schuldet A eine Antwort", &sch, false);
            let sch = w.tue_recv(&mut ep, srv, 0);
            b.deckt("RECV 2 DESSELBEN Servers, ohne geantwortet zu haben, nimmt B", &sch, false);
            b.befund("B3 das Reply-Token wird beim zweiten RECV UEBERSCHRIEBEN: A steht in keiner \
                      Warteschlange, haelt kein Token, und quiescence_of meldet ihn als ruhig",
                     ep.caller() == Some(zwei) && !ep.senders.contains(a)
                         && ep.quiescence_of(a).is_quiescent());
            let sch = w.tue_reply(&mut ep, srv, 0, ANTWORT1);
            b.deckt_reply("REPLY beantwortet B", &sch);
            b.befund("B3 Folge: A wird nie geweckt -- und is_idle() meldet den Endpoint als RUHIG. \
                      Ein Austausch nach A-4.1 traefe also scheinbar niemanden",
                     w.weck_zahl(a) == 0 && w.weck_zahl(zwei) == 1 && ep.is_idle());
        }

        // -- B: Gegenproben. Ohne die Nebenbedingungen zerbricht die Entsprechung. -------------
        {
            // G1: der wartende Empfaenger ist eine Leiche.
            let mut w = Welt::neu();
            let mut ep = Endpoint::EMPTY;
            ep.mark_used();
            let s = w.faden(0);
            let c = w.faden(0);
            let _ = w.tue_recv(&mut ep, s, 0);
            w.toeten(s);
            let sch = w.tue_call(&mut ep, c, 0);
            b.weicht_ab("G1 toter Empfaenger: der Code verwirft die Leiche, das Modell stellt zu",
                        &sch.erwartet, &sch.nachher);
        }
        {
            // G2: derselbe Zweig auf der anderen Seite -- ein toter SENDER.
            let mut w = Welt::neu();
            let mut ep = Endpoint::EMPTY;
            ep.mark_used();
            let c = w.faden(0);
            let s = w.faden(0);
            let _ = w.tue_call(&mut ep, c, 0);
            w.toeten(c);
            let sch = w.tue_recv(&mut ep, s, 0);
            b.weicht_ab("G2 toter Sender: der Code verwirft die Leiche, das Modell stellt zu",
                        &sch.erwartet, &sch.nachher);
        }
        {
            // G3: `purge_thread` -- das Modell kennt keinen Thread-Tod.
            let mut w = Welt::neu();
            let mut ep = Endpoint::EMPTY;
            ep.mark_used();
            let c = w.faden(0);
            let _ = w.tue_call(&mut ep, c, 0);
            let vorher = w.alpha(&ep);
            ep.purge_thread(c);
            let nachher = w.alpha(&ep);
            b.weicht_ab("G3 purge_thread: der Code raeumt den sterbenden Faden aus, das Modell \
                         kennt keinen Tod", &vorher, &nachher);
        }
        {
            // G4: `owner_died` -- das Modell kennt keinen Serverausfall.
            let mut w = Welt::neu();
            let mut ep = Endpoint::EMPTY;
            ep.mark_used();
            let s = w.faden(0);
            let c = w.faden(0);
            let _ = w.tue_recv(&mut ep, s, 0);
            let _ = w.tue_call(&mut ep, c, 0);
            let vorher = w.alpha(&ep);
            let gerettet = ep.owner_died(s);
            let nachher = w.alpha(&ep);
            b.weicht_ab("G4 owner_died: der Code loest das Token auf (ERR_SERVER_GONE), das Modell \
                         kennt keinen Serverausfall", &vorher, &nachher);
            b.probe("G4 Positivkontrolle: owner_died gibt genau den wartenden Aufrufer zurueck",
                    gerettet == Some(c));
        }

        println!("-- {} Faelle geprueft, {} Abweichung(en) --", b.n, b.fehler);
        if b.fehler > 0 {
            1
        } else {
            0
        }
    }
}

fn main() {
    std::process::exit(treue::run());
}
RSEOF
}

# ================================================================================================
# 4. Bauen und Fahren.
# ================================================================================================
bauen_und_fahren() {   # bauen_und_fahren <lib.rs> <endpoint.rs> <abi.rs> <arbeitsverzeichnis> [--leise]
    local code="$1" modell="$2" abi="$3" W="$4" leise="${5:-}"
    local ausgabe rc

    for f in "$code" "$modell" "$abi"; do
        if [ ! -f "$f" ]; then
            [ -n "$leise" ] || echo "  FEHLER: $f ist nicht vorhanden -- der Waechter hat keinen Gegenstand." >&2
            return 2
        fi
    done

    if ! ausgabe="$(modell_uebersetzen "$modell" 2>&1)"; then
        [ -n "$leise" ] || echo "$ausgabe" >&2
        return 2
    fi
    printf '%s\n' "$ausgabe" > "$W/modell.rs"

    if ! ausgabe="$(abi_extrahieren "$abi" 2>&1)"; then
        [ -n "$leise" ] || echo "$ausgabe" >&2
        return 2
    fi
    printf '%s\n' "$ausgabe" > "$W/abi_shim.rs"

    # Der echte Quelltext, unveraendert -- bis auf die EINE Zeile, die einem Host-Binary im Weg
    # steht. Fehlt sie, bricht der Waechter ab: dann ist die Datei nicht die, fuer die er sie haelt.
    if ! ausgabe="$(python3 - "$code" "$W/harness.rs" 2>&1 <<'PY'
import io, sys
quelle, ziel = sys.argv[1], sys.argv[2]
s = io.open(quelle, encoding='utf-8').read()
if '#![no_std]\n' not in s:
    sys.exit("FEHLER: `#![no_std]` in %s nicht gefunden.\n"
             "        Der Waechter nimmt an, dies sei eine no_std-Kernel-Crate; ist sie es\n"
             "        nicht, stimmt seine Annahme ueber den Gegenstand nicht mehr." % quelle)
s = s.replace('#![no_std]\n',
              '// #![no_std] -- fuer den Host-Lauf des Modell-Treue-Waechters entfernt.\n', 1)
io.open(ziel, 'w', encoding='utf-8').write(s)
PY
    )"; then
        [ -n "$leise" ] || echo "$ausgabe" >&2
        return 2
    fi

    cat "$W/abi_shim.rs" >> "$W/harness.rs"
    anbau_schreiben "$W/anbau.rs"
    cat "$W/anbau.rs" >> "$W/harness.rs"

    rm -f "$W/waechter.bin"
    if ! ausgabe="$($RUSTC --edition 2021 -A warnings "$W/harness.rs" -o "$W/waechter.bin" 2>&1)"; then
        if [ -z "$leise" ]; then
            echo "  FEHLER: der Waechter liess sich nicht uebersetzen." >&2
            printf '%s\n' "$ausgabe" | sed 's/^/    /' >&2
            echo "    Ein Uebersetzungsfehler ist hier ein BEFUND, kein Werkzeugdefekt: der echte" >&2
            echo "    Quelltext passt nicht mehr zu dem, was der Waechter von ihm annimmt" >&2
            echo "    (umbenannte Methode, geaendertes Feld, neue SchedOps-Anforderung)." >&2
        fi
        return 2
    fi
    if [ ! -x "$W/waechter.bin" ]; then
        [ -n "$leise" ] || echo "  FEHLER: kein lauffaehiges Binary entstanden." >&2
        return 2
    fi

    ausgabe="$("$W/waechter.bin" 2>&1)"; rc=$?
    # Ein leeres Ergebnis ist KEIN Erfolg. Ohne diese Schranke koennte ein Lauf, der gar keinen
    # Fall anfasst, gruen melden -- dieselbe Form wie die leere Ereigniswarteschlange ohne `CD.R`.
    local zusammenfassung anzahl
    zusammenfassung="$(printf '%s\n' "$ausgabe" | sed -n 's/^-- \([0-9]*\) Faelle geprueft.*/\1/p')"
    anzahl="${zusammenfassung:-0}"
    if [ -z "$zusammenfassung" ] || [ "$anzahl" -lt 1 ]; then
        if [ -z "$leise" ]; then
            if [ -z "$ausgabe" ]; then
                echo "  FEHLER: kein einziger Fall gelaufen -- ein leerer Lauf ist kein Testergebnis." >&2
            else
                echo "  FEHLER: der Lauf ist abgebrochen, bevor die Ergebniszeile entstand." >&2
                echo "          Ein Teilergebnis ist kein Ergebnis: was nach dem Abbruch kaeme," >&2
                echo "          ist ungeprueft. (Haeufigster Grund: das uebersetzte Modell laeuft" >&2
                echo "          in eine eigene Vorbedingung, etwa drop_first auf einer leeren Folge.)" >&2
            fi
            printf '%s\n' "$ausgabe" | sed 's/^/    /' >&2
        fi
        return 2
    fi
    if [ -z "$leise" ]; then
        printf '%s\n' "$ausgabe" | sed 's/^/  /'
    fi
    return "$rc"
}

pruefen() {   # pruefen [--leise]
    local leise="${1:-}"
    local W; W="$(mktemp -d -p "$WURZEL")"
    bauen_und_fahren "$CODE_STD" "$MODELL_STD" "$ABI_STD" "$W" "$leise"
    local rc=$?
    rm -rf "$W"
    return "$rc"
}

# ================================================================================================
# 5. Selbsttest. Kann dieser Waechter ueberhaupt ausloesen -- auf BEIDEN Seiten -- und haelt er
#    still, wenn nur Kosmetik geaendert wird?
#
#    Gearbeitet wird ausschliesslich auf KOPIEN in einem Wegwerfverzeichnis; die Originale werden
#    nie angefasst, auch nicht bei Abbruch (`trap ... RETURN` + `mktemp -d`).
# ================================================================================================
selbsttest() {
    local W; W="$(mktemp -d -p "$WURZEL")"; trap 'rm -rf "$W"' RETURN
    local fehler=0 n=0
    # **Hat der letzte Eingriff ueberhaupt gegriffen?** Ohne diese Kopplung ginge eine Mutation,
    # deren Anker veraltet ist, als „der Waechter schweigt" durch -- und ausgerechnet die
    # Negativkontrolle wuerde dann IMMER bestehen. Genau das ist hier einmal passiert.
    MUT_OK=0

    mutieren() {   # mutieren <ziel: code|modell> <python-ersetzung>
        cp "$CODE_STD" "$W/lib.rs"
        cp "$MODELL_STD" "$W/endpoint.rs"
        local datei="$W/lib.rs"; [ "$1" = "modell" ] && datei="$W/endpoint.rs"
        local rc
        python3 - "$datei" "$2" <<'PY'
import io, sys
p, prog = sys.argv[1], sys.argv[2]
s = io.open(p, encoding='utf-8').read()
vorher = s
ns = {'s': s}
exec(prog, ns)
if ns['s'] == vorher:
    sys.exit("FEHLER: die Mutation hat NICHTS geaendert -- der Anker passt nicht mehr.\n"
             "        Ein Selbsttest, der unveraenderte Dateien prueft, prueft nichts.")
io.open(p, 'w', encoding='utf-8').write(ns['s'])
PY
        rc=$?
        if [ "$rc" -ne 0 ]; then MUT_OK=0; else MUT_OK=1; fi
    }

    erwarte() {   # erwarte <kracht|still> <name>
        n=$((n+1))
        if [ "$MUT_OK" != "1" ]; then
            echo "  HARTER FEHLER: '$2' -- der Eingriff hat nicht gegriffen." >&2
            echo "                 Der Fall hat NICHT gemessen; ein nicht angewandter Eingriff" >&2
            echo "                 darf weder als 'erkannt' noch als 'still' durchgehen." >&2
            fehler=1
            return
        fi
        local L; L="$(mktemp -d -p "$WURZEL")"
        bauen_und_fahren "$W/lib.rs" "$W/endpoint.rs" "$ABI_STD" "$L" --leise
        local rc=$?
        rm -rf "$L"
        if [ "$1" = "kracht" ]; then
            case "$rc" in
                0) echo "  FEHLER: '$2' -- der Waechter schweigt." >&2; fehler=1 ;;
                1) echo "  erkannt : $2 (Verhalten weicht ab)" ;;
                *) echo "  erkannt : $2 (bricht ab, bevor ein Urteil entsteht)" ;;
            esac
        else
            case "$rc" in
                0) echo "  still   : $2" ;;
                1) echo "  FEHLER: '$2' -- der Waechter schlaegt grundlos an." >&2; fehler=1 ;;
                *) echo "  FEHLER: '$2' -- der Waechter scheitert an seiner eigenen Mechanik." >&2
                   local L2; L2="$(mktemp -d -p "$WURZEL")"
                   bauen_und_fahren "$W/lib.rs" "$W/endpoint.rs" "$ABI_STD" "$L2" >&2
                   rm -rf "$L2"; fehler=1 ;;
            esac
        fi
    }

    # -- Mutationen am ECHTEN Code: Rendezvous und Abbildung --------------------------------------
    mutieren code 's = s.replace("""        self.senders.enqueue(caller);
        ops.block_current(core, frame)""", """        ops.block_current(core, frame)""", 1)'
    erwarte kracht "Code: CALL reiht den Sender nicht mehr ein"

    mutieren code 's = s.replace("            transfer(frame, sframe);\n", "", 1)'
    erwarte kracht "Code: CALL uebertraegt die Nachricht nicht (Rendezvous ohne Zustellung)"

    mutieren code 's = s.replace("""        self.receivers.enqueue(server);
        ops.block_current(core, frame)""", """        self.receivers.enqueue(server);
        self.receivers.enqueue(server);
        ops.block_current(core, frame)""", 1)'
    erwarte kracht "Code: RECV reiht den Empfaenger doppelt ein"

    mutieren code 's = s.replace("    pub fn call(&mut self, ops: &mut dyn SchedOps",
                                 "    pub fn call_umbenannt(&mut self, ops: &mut dyn SchedOps", 1)'
    erwarte kracht "Code: die Methode call gibt es nicht mehr (der Waechter liest nicht ins Leere)"

    mutieren code 's = s.replace("    senders: TidQueue,", "    sender_schlange: TidQueue,", 1)'
    erwarte kracht "Code: das Feld senders heisst anders (die Abbildung haengt daran)"

    # -- Mutationen am ECHTEN Code: die Kapazitaetsschranke ---------------------------------------
    mutieren code 's = s.replace("pub const QUEUE_CAP: usize = 32;", "pub const QUEUE_CAP: usize = 16;", 1)'
    erwarte kracht "Code: QUEUE_CAP ist nicht mehr die Schranke, die das Modell abbildet"

    # -- Mutationen am ECHTEN Code: das Quiescing-Tor (A-4.2) -------------------------------------
    mutieren code 's = s.replace("""            Some(result::ERR_BADCAP)
        } else if self.quiescing {
            Some(result::ERR_QUIESCING)""", """            Some(result::ERR_QUIESCING)
        } else if self.quiescing {
            Some(result::ERR_BADCAP)""", 1)'
    erwarte kracht "Code: die beiden Abweisungsgruende sind vertauscht (ERR_BADCAP <-> ERR_QUIESCING)"

    mutieren code 's = s.replace("""        if let Some(code) = self.gate_new_transaction() {
            frame_set_reg(frame, reg::SYSNO_RESULT, code);
            return frame;
        }
        let caller = ops.current_id(core);""", """        let caller = ops.current_id(core);""", 1)'
    erwarte kracht "Code: CALL fragt das Tor gar nicht mehr (der stillgelegte Endpoint nimmt an)"

    # -- Mutationen am ECHTEN Code: das Reply-Token -----------------------------------------------
    mutieren code 's = s.replace("if let Some(caller) = self.caller.take() {",
                                 "if let Some(caller) = self.caller {", 1)'
    erwarte kracht "Code: REPLY konsumiert das Token nicht mehr (Doppel-Reply moeglich)"

    mutieren code 's = s.replace("""            ops.unblock(caller);
        }
        frame_set_reg(frame, reg::SYSNO_RESULT, result::OK);""", """        }
        frame_set_reg(frame, reg::SYSNO_RESULT, result::OK);""", 1)'
    erwarte kracht "Code: REPLY weckt den wartenden Aufrufer nicht mehr (Wirkung faellt weg)"

    mutieren code 's = s.replace("""            self.caller = Some(sender);
            self.reply_owner = Some(server);""", """            self.caller = Some(sender);""", 1)'
    erwarte kracht "Code: RECV setzt keinen Reply-Owner (halbes Token -- token_inv verletzt)"

    mutieren code 's = s.replace("        if !self.used || self.receivers.contains(tid) {",
                                 "        if !self.used || self.receivers.contains(tid) || !self.senders.is_empty() {", 1)'
    erwarte kracht "Code: bind_receiver sieht die Sender-Queue doch an (der Befund B1a faellt weg)"

    # -- Mutationen am MODELL: Rendezvous und Buchhaltung -----------------------------------------
    mutieren modell 's = s.replace("""            caller: Some(tid), reply_owner: Some(ep.receivers.first()),
            delivered: ep.delivered + 1,""", """            caller: Some(tid), reply_owner: Some(ep.receivers.first()),
            delivered: ep.delivered,""", 1)'
    erwarte kracht "Modell: send zaehlt die Zustellung nicht mehr"

    mutieren modell 's = s.replace("} else if ep.receivers.len() > 0 {", "} else if ep.receivers.len() >= 0 {", 1)'
    erwarte kracht "Modell: send nimmt immer den Rendezvous-Zweig"

    mutieren modell 's = s.replace("            senders: ep.senders.drop_first(), receivers: ep.receivers,",
                                   "            senders: ep.senders, receivers: ep.receivers,", 1)'
    erwarte kracht "Modell: recv nimmt die Nachricht nicht aus der Warteschlange"

    mutieren modell 's = s.replace("pub open spec fn send(", "pub open spec fn send_umbenannt(", 1)'
    erwarte kracht "Modell: die spec fn send gibt es nicht mehr (der Waechter liest nicht ins Leere)"

    mutieren modell 's = s.replace("pub open spec fn reply(", "pub open spec fn reply_umbenannt(", 1)'
    erwarte kracht "Modell: die spec fn reply gibt es nicht mehr"

    mutieren modell 's = s.replace("pub open spec fn msgs_total", "pub open spec fn msgs_gesamt", 1)'
    erwarte kracht "Modell: msgs_total umbenannt"

    mutieren modell 's = s.replace("    ep.delivered + ep.senders.len() + ep.dropped_senders",
                                   "    ep.delivered + ep.senders.len()", 1)'
    erwarte kracht "Modell: msgs_total zaehlt den Verlust nicht mehr mit (der Verlust wird unsichtbar)"

    # -- Mutationen am MODELL: die Invarianten (nur die Sprechproben koennen das fangen) -----------
    mutieren modell 's = s.replace("    ep.senders.len() == 0 || ep.receivers.len() == 0", "    true", 1)'
    erwarte kracht "Modell: ep_inv_strong auf true aufgeweicht (ein Pruefer, der nicht mehr urteilt)"

    mutieren modell 's = s.replace("    ep.quiescing || ep_inv_strong(ep)", "    true", 1)'
    erwarte kracht "Modell: ep_inv auf true aufgeweicht"

    mutieren modell 's = s.replace("    (ep.caller is Some) == (ep.reply_owner is Some)", "    true", 1)'
    erwarte kracht "Modell: token_inv auf true aufgeweicht"

    # -- Mutationen am MODELL: Kapazitaetsschranke, Tor und Reply ---------------------------------
    mutieren modell 's = s.replace("pub open spec fn queue_cap() -> nat { 32 }",
                                   "pub open spec fn queue_cap() -> nat { 64 }", 1)'
    erwarte kracht "Modell: die Schranke ist nicht mehr die des Codes"

    mutieren modell 's = s.replace("    } else if ep.senders.len() < queue_cap() {", "    } else if true {", 1)'
    erwarte kracht "Modell: die Kapazitaetsschranke im send-Zweig ist weg (der 33. Sender geht durch)"

    mutieren modell 's = s.replace("""    } else if ep.quiescing {
        2""", """    } else if ep.quiescing {
        1""", 1)'
    erwarte kracht "Modell: das Tor gibt fuer beide Gruende denselben Code (A-4.2 verschwiegen)"

    mutieren modell 's = s.replace("""            senders: ep.senders, receivers: ep.receivers,
            caller: None, reply_owner: None,
            delivered: ep.delivered,
            dropped_senders: ep.dropped_senders, dropped_receivers: ep.dropped_receivers,""",
                                   """            senders: ep.senders, receivers: ep.receivers,
            caller: ep.caller, reply_owner: ep.reply_owner,
            delivered: ep.delivered,
            dropped_senders: ep.dropped_senders, dropped_receivers: ep.dropped_receivers,""", 1)'
    erwarte kracht "Modell: reply konsumiert das Token nicht mehr"

    mutieren modell 's = s.replace("            caller: Some(ep.senders.first()), reply_owner: Some(tid),",
                                   "            caller: Some(ep.senders.first()), reply_owner: ep.reply_owner,", 1)'
    erwarte kracht "Modell: recv setzt keinen Reply-Owner"

    # -- Und die Gegenprobe: Kosmetik auf BEIDEN Seiten darf NICHT ausloesen ----------------------
    cp "$CODE_STD" "$W/lib.rs"; cp "$MODELL_STD" "$W/endpoint.rs"
    python3 - "$W/lib.rs" "$W/endpoint.rs" <<'PY'
import io, sys
c, m = sys.argv[1], sys.argv[2]

s = io.open(c, encoding='utf-8').read()
vor = s
# Binder umbenannt (NICHT das Feld `self.caller`), Kommentar und Leerzeile eingezogen.
s = s.replace("        let caller = ops.current_id(core);",
              "        // Kommentar des Selbsttests.\n\n        let aufrufer = ops.current_id(core);", 1)
s = s.replace("            self.caller = Some(caller);", "            self.caller = Some(aufrufer);", 1)
s = s.replace("""        self.senders.enqueue(caller);
        ops.block_current(core, frame)""",
              """        self.senders.enqueue(aufrufer);
        ops.block_current(core, frame)""", 1)
if s == vor:
    sys.exit("FEHLER: die kosmetische Mutation am Code hat nichts geaendert.")
io.open(c, 'w', encoding='utf-8').write(s)

s = io.open(m, encoding='utf-8').read()
vor = s
teile = [
    ("pub open spec fn send(ep: Endpoint, tid: nat) -> Endpoint {",
     "// Kommentar des Selbsttests.\n\npub open spec fn send(ep: Endpoint, faden: nat) -> Endpoint {"),
    ("            caller: Some(tid), reply_owner: Some(ep.receivers.first()),",
     "            caller: Some(faden), reply_owner: Some(ep.receivers.first()),"),
    ("            senders: ep.senders.push(tid), receivers: ep.receivers,",
     "            senders: ep.senders.push(faden), receivers: ep.receivers,"),
]
for alt, neu in teile:
    if alt not in s:
        sys.exit("FEHLER: der Anker der kosmetischen Mutation am Modell fehlt:\n  %r" % alt)
    s = s.replace(alt, neu, 1)
if s == vor:
    sys.exit("FEHLER: die kosmetische Mutation am Modell hat nichts geaendert.")
io.open(m, 'w', encoding='utf-8').write(s)
PY
    # Auch hier gilt die Kopplung: griffe der Eingriff nicht, waere „still" wertlos -- die
    # Negativkontrolle bestuende dann ausgerechnet deshalb, weil nichts geaendert wurde.
    if [ $? -eq 0 ]; then MUT_OK=1; else MUT_OK=0; fi
    erwarte still "Kosmetik beidseitig (Binder/Parameter umbenannt, Kommentare, Leerzeilen)"

    echo "  Selbsttest: $n Faelle"
    return "$fehler"
}

command -v "$RUSTC" >/dev/null 2>&1 || {
    echo "FEHLER: '$RUSTC' nicht gefunden (setze \$RUSTC)." >&2; exit 2; }
command -v python3 >/dev/null 2>&1 || { echo "FEHLER: python3 nicht gefunden." >&2; exit 2; }

MODUS="${1:-alles}"
case "$MODUS" in
    --nur-pruefen)
        pruefen --leise; exit $? ;;
    --selftest)
        selbsttest; exit $? ;;
    alles|"")
        echo "== Modell-Treue IPC: Verus-endpoint gegen sel4lake-ipc::Endpoint =="
        pruefen; rc=$?
        [ "$rc" -eq 0 ] || { echo "== MODELL-TREUE (IPC) VERLETZT ==" >&2; exit "$rc"; }
        echo "-- Selbsttest --"
        selbsttest || { echo "== WAECHTER (IPC) NICHT SPRECHFAEHIG ==" >&2; exit 1; }
        echo "== Modell und Code entsprechen einander unter der benannten Abbildung =="
        ;;
    *) echo "Aufruf: $0 [--nur-pruefen|--selftest]" >&2; exit 2 ;;
esac
