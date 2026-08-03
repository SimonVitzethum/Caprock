#!/usr/bin/env bash
# **Haelt das Verus-IPC-Modell an den echten Endpoint.** (B-7.3, IPC-Strang)
#
# ================================================================================================
# WAS HIER GEPRUEFT WIRD -- und was ausdruecklich NICHT
# ================================================================================================
#
# `Verification/ipc/proofs/endpoint.rs` beweist etwas ueber ein Modell mit DREI Feldern
# (`senders`, `receivers`, `delivered`) und ZWEI Operationen (`send`, `recv`).
# `crates/sel4lake-ipc/src/lib.rs` hat SECHS Felder (`used`, `quiescing`, `senders`, `receivers`,
# `caller`, `reply_owner`) und rund fuenfzehn Operationen. Zwischen beiden klafft eine
# Abstraktionsluecke -- anders als bei `unlink` (tools/verus-modelltreue.sh), wo Modell und Code
# fast deckungsgleich sind und ein struktureller 1:1-Vergleich ehrlich war.
#
# Ein struktureller Vergleich waere hier eine LUEGE. Er schlaege entweder immer an (nutzlos) oder
# muesste so weit aufgeweicht werden, dass er nicht mehr anschlagen KANN -- und ein Pruefer, der
# nicht fehlschlagen kann, laesst eine Aussage wahr *aussehen*. Deshalb wird die Abstraktion hier
# **hingeschrieben und ausgefuehrt** statt behauptet:
#
#   1. Das ausfuehrbare Modell wird aus der Verus-Datei **uebersetzt**, nicht abgeschrieben.
#      Eine handgeschriebene Zweitfassung koennte still auseinanderlaufen -- genau die Falle,
#      die B-7.2 bezahlt hat ("die Kopie war nicht der Grund, aber sie war eine Kopie").
#      Der Uebersetzer ist fail-closed: was er nicht kennt, ist ein Fehler, kein Ueberspringen.
#   2. Der ECHTE Quelltext von `sel4lake-ipc` wird **unveraendert** uebernommen (eine einzige,
#      geprueft vorhandene Zeile `#![no_std]` faellt weg, damit ein Host-Binary entsteht) und
#      gegen Stellvertreter fuer HAL/Scheduler/ABI gelinkt.
#   3. Die Abbildung `echter Endpoint -> Modell-Endpoint` (`alpha`) steht an EINER Stelle:
#         senders    := die Nachricht, die jeder blockierte Sender abgesetzt hat (aus seinem Frame)
#         receivers  := die Thread-IDs der geparkten Empfaenger, in FIFO-Reihenfolge
#         delivered  := **effektbasiert** gezaehlt -- eine Nachricht gilt als zugestellt, sobald
#                       ihr Wort erstmals im Frame eines ANDEREN Fadens auftaucht.
#      `delivered` hat im echten Endpoint kein Gegenstueck. Es aus dem Code abzulesen waere die
#      Falle aus CLAUDE.md ("`rx_used` sagt, dass das Geraet gehandelt hat, nicht dass Daten
#      ankamen") -- deshalb die Wirkung, nicht die Absicht.
#   4. Dann wird gefahren: jede echte `call`/`recv`-Operation muss unter `alpha` dasselbe tun wie
#      der Modellschritt `send`/`recv`, und `ep_inv`/`msgs_total` (ebenfalls aus der Verus-Datei
#      uebersetzt) muessen am abgebildeten Zustand gelten.
#
# **GEPRUEFT wird damit:**
#   * Rendezvous-Semantik von `call`/`recv` (beide Zweige, beide Kern-Pfade: `switch_to` und
#     `unblock`), FIFO-Reihenfolge beider Warteschlangen, Zustellzaehlung.
#   * dass `ep_inv` und `msgs_total` unter der Abbildung an echten Zustaenden gelten -- und an
#     welchen NICHT (s. u.).
#   * dass die Nebenbedingungen der Abbildung **tragen**: fuer jede wird gemessen, dass die
#     Entsprechung ohne sie zerbricht (Gegenproben G1..G4). Eine Nebenbedingung, deren Verletzung
#     folgenlos bliebe, waere keine.
#
# **NICHT geprueft wird:**
#   * `caller`/`reply_owner`, also der ganze REPLY-Pfad, `owner_died`, `abort_call` -- das Modell
#     hat dafuer kein Feld. `reply` wird gefahren und ist unter `alpha` **unsichtbar**; das ist
#     hier ein gemessener Befund (Zeile "REPLY ist unter alpha unsichtbar"), keine
#     stillschweigende Auslassung.
#   * `used`/`quiescing` (A-4.2), `rebind_server`, `bind_receiver`, `retire_receiver`,
#     `migrate_owner`, `purge_thread`, `audit` -- keine Modell-Entsprechung.
#   * Nebenlaeufigkeit. Das Modell ist sequentiell, dieser Waechter ebenso; die Locks des Kernels
#     bleiben Concurrency-TCB (so steht es auch im Kopf der Beweisdatei).
#   * die HAL (Frame-Register) und der Scheduler. Beide sind hier **Stellvertreter**. Was ein
#     echter `switch_to` tut, sagt dieser Lauf nicht.
#   * `Notification` (SIGNAL/WAIT) -- im Modell kommt sie nicht vor.
#
# **Die drei Befunde, die dieser Waechter FESTHAELT** (er prueft sie als Tatsachen, damit sie
# nicht stillschweigend verschwinden -- und damit ein aufgeweichtes `ep_inv` auffliegt):
#   B1  `ep_inv` gilt am echten Endpoint NICHT. Ueber die oeffentliche Schnittstelle sind
#       Zustaende mit wartenden Sendern UND geparkten Empfaengern erreichbar
#       (`bind_receiver` prueft die Sender-Queue nicht; `migrate_owner` reiht einen Aufrufer als
#       Sender ein, waehrend ein Empfaenger geparkt sein kann).
#   B2  `send_no_loss` gilt am echten Endpoint NICHT. `TidQueue::enqueue` verwirft ab
#       `QUEUE_CAP` still -- der 33. Sender geht verloren, `msgs_total` steigt nicht.
#       (Im Quelltext selbst benannt, aber nirgends gemessen.)
#   B3  `call`/`recv` haben Abweisungs- und Leichen-Zweige (`ERR_BADCAP`, `ERR_QUIESCING`,
#       toter Partner), die im Modell schlicht fehlen.
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
SPEC_ERWARTET  = {'ep_inv', 'msgs_total', 'send', 'recv'}
PROOF_ERWARTET = {'send_preserves_inv', 'recv_preserves_inv', 'send_no_loss',
                  'recv_delivers_once', 'rendezvous_progress'}
spec_gefunden  = set(re.findall(r'\bspec\s+fn\s+(\w+)\s*\(', text))
proof_gefunden = set(re.findall(r'\bproof\s+fn\s+(\w+)\s*\(', text))
if spec_gefunden != SPEC_ERWARTET:
    fehler("die Menge der `spec fn` weicht ab.",
           "erwartet : %s" % ", ".join(sorted(SPEC_ERWARTET)),
           "gefunden : %s" % (", ".join(sorted(spec_gefunden)) or "(keine)"),
           "Fehlt eine, laese der Waechter ins Leere. Kam eine dazu, beruehrt sie kein Testfall.")
if proof_gefunden != PROOF_ERWARTET:
    fehler("die Menge der `proof fn` weicht ab.",
           "erwartet : %s" % ", ".join(sorted(PROOF_ERWARTET)),
           "gefunden : %s" % (", ".join(sorted(proof_gefunden)) or "(keine)"),
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
    if t == 'nat':      return 'u64'
    if t == 'bool':     return 'bool'
    if t == 'Seq<nat>': return 'Vec<u64>'
    if t == 'Endpoint': return '&Endpoint' if als_param else 'Endpoint'
    fehler("unbekannter Typ %r -- bekannt sind nat, bool, Seq<nat>, Endpoint." % t)

def ist_seq(f):
    return FELDTYP.get(f, '').startswith('Seq<')

TOK = re.compile(r'\s+|(\d+)|([A-Za-z_][A-Za-z0-9_]*)|(==|>=|<=|\|\||&&|[{}(),:.+><])')

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
    def __init__(self, toks, params):
        self.t, self.i, self.params = toks, 0, params
    def sieh(self):  return self.t[self.i]
    def nimm(self):
        t = self.t[self.i]; self.i += 1; return t
    def erwarte(self, art, wert=None):
        t = self.nimm()
        if t[0] != art or (wert is not None and t[1] != wert):
            fehler("erwartet %r, bekommen %r." % (wert or art, t[1]))
        return t
    def ist(self, art, wert):
        return self.sieh()[0] == art and self.sieh()[1] == wert
    def expr(self):
        if self.ist('id', 'if'):
            self.nimm()
            bed = self.expr()
            self.erwarte('op', '{'); dann  = self.expr(); self.erwarte('op', '}')
            self.erwarte('id', 'else')
            self.erwarte('op', '{'); sonst = self.expr(); self.erwarte('op', '}')
            return "if %s { %s } else { %s }" % (bed, dann, sonst)
        return self.oder()
    def oder(self):
        l = self.und()
        while self.ist('op', '||'):
            self.nimm(); l = "%s || %s" % (l, self.und())
        return l
    def und(self):
        l = self.vergleich()
        while self.ist('op', '&&'):
            self.nimm(); l = "%s && %s" % (l, self.vergleich())
        return l
    def vergleich(self):
        l = self.summe()
        if self.sieh()[0] == 'op' and self.sieh()[1] in ('==', '>', '<', '>=', '<='):
            op = self.nimm()[1]; l = "%s %s %s" % (l, op, self.summe())
        return l
    def summe(self):
        l = self.primaer()
        while self.ist('op', '+'):
            self.nimm(); l = "%s + %s" % (l, self.primaer())
        return l
    def primaer(self):
        t = self.sieh()
        if t[0] == 'num':
            self.nimm(); return t[1]
        if t[0] == 'op' and t[1] == '(':
            self.nimm(); e = self.expr(); self.erwarte('op', ')'); return "(%s)" % e
        if t[0] != 'id':
            fehler("unerwartetes Token %r in einem spec-Rumpf." % (t[1],))
        name = self.nimm()[1]
        if name in ('true', 'false'):
            return name
        if name == 'Endpoint' and self.ist('op', '{'):
            self.nimm()
            gesetzt, teile = [], []
            while not self.ist('op', '}'):
                f = self.erwarte('id')[1]
                if f not in FELDTYP:
                    fehler("Struktur-Literal setzt unbekanntes Feld %r." % f)
                self.erwarte('op', ':')
                teile.append("%s: %s" % (f, self.expr())); gesetzt.append(f)
                if self.ist('op', ','): self.nimm()
            self.erwarte('op', '}')
            if set(gesetzt) != set(FELDTYP):
                fehler("Struktur-Literal setzt %s, der struct hat %s." % (gesetzt, list(FELDTYP)),
                       "Ein weggelassenes Feld waere eine stille Uebernahme.")
            return "Endpoint { %s }" % ", ".join(teile)
        if name not in self.params:
            fehler("Bezeichner %r ist kein Parameter dieser spec fn (%s)."
                   % (name, ", ".join(self.params) or "keine"))
        if not self.ist('op', '.'):
            return name
        self.nimm()
        feld = self.erwarte('id')[1]
        if feld not in FELDTYP:
            fehler("Feldzugriff %s.%s -- `struct Endpoint` hat kein Feld %r." % (name, feld, feld))
        basis = "%s.%s" % (name, feld)
        if not self.ist('op', '.'):
            return "%s.clone()" % basis if ist_seq(feld) else basis
        self.nimm()
        meth = self.erwarte('id')[1]
        self.erwarte('op', '(')
        args = []
        while not self.ist('op', ')'):
            args.append(self.expr())
            if self.ist('op', ','): self.nimm()
        self.erwarte('op', ')')
        if not ist_seq(feld):
            fehler("Methodenaufruf .%s() auf dem Nicht-Seq-Feld %r." % (meth, feld))
        if meth == 'len' and not args:        return "(%s.len() as u64)" % basis
        if meth == 'drop_first' and not args: return "mseq_drop_first(&%s)" % basis
        if meth == 'push' and len(args) == 1: return "mseq_push(&%s, %s)" % (basis, args[0])
        fehler("Seq-Methode %r mit %d Argument(en) -- bekannt: len(), drop_first(), push(x)."
               % (meth, len(args)))

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

aus = []
for name in ('ep_inv', 'msgs_total', 'send', 'recv'):
    params, rtyp, rumpf_txt = spec_rumpf(name)
    p = P(zerlegen(rumpf_txt), [n for n, _ in params])
    koerper = p.expr()
    if p.sieh()[0] != 'eof':
        fehler("im Rumpf von %s bleibt unuebersetzter Text ab %r stehen." % (name, p.sieh()[1]))
    sig = ", ".join("%s: %s" % (n, rust_typ(t, als_param=True)) for n, t in params)
    aus.append("pub fn %s(%s) -> %s {\n    %s\n}" % (name, sig, rust_typ(rtyp), koerper))

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
    use crate::sel4lake_abi::reg;
    use crate::sel4lake_hal::exception::{frame_neu, frame_reg, frame_set_reg};
    use crate::sel4lake_sched::{owner_core, setze_kern, SchedOps, ThreadId};
    use crate::{Endpoint, QUEUE_CAP};
    use std::collections::{HashMap, HashSet};

    /// Die Welt um den Endpoint: Faeden mit Frames und Heimatkernen, dazu die
    /// **effektbasierte** Buchhaltung ueber tatsaechlich angekommene Nachrichten.
    pub struct Welt {
        laufend: HashMap<usize, ThreadId>,
        frames: HashMap<u64, usize>,
        tot: HashSet<u64>,
        leerlauf: usize,
        /// Nachricht -> Absender. Solange sie nur dort liegt, ist sie nicht zugestellt.
        offen: HashMap<u64, u64>,
        angekommen: HashSet<u64>,
        zugestellt: u64,
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
        fn block_current(&mut self, _core: usize, _frame: usize) -> usize {
            self.leerlauf
        }
        fn switch_to(&mut self, _core: usize, _frame: usize, target: ThreadId) -> usize {
            let f = self.frame_von(target);
            f.unwrap_or(self.leerlauf)
        }
        fn unblock(&mut self, _tid: ThreadId) {}
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

        /// **Die Abbildung.** Sie steht an genau dieser Stelle, damit sie ueberprueft und
        /// veraendert werden kann, statt in den Faellen verstreut zu sein.
        fn alpha(&self, ep: &Endpoint) -> modell::Endpoint {
            let mut senders = Vec::new();
            ep.senders.for_each(|t| {
                // Die Nachricht eines blockierten Senders liegt in SEINEM Frame -- die Bruecke
                // zwischen „Warteschlange aus Thread-IDs" (Code) und „Folge von Nachrichten"
                // (Modell). Eine Leiche hat keinen Frame; ihr Beitrag ist unbestimmbar.
                senders.push(match self.frame_von(t) {
                    Some(f) => frame_reg(f, reg::MSG0),
                    None => u64::MAX,
                });
            });
            let mut receivers = Vec::new();
            ep.receivers.for_each(|t| receivers.push(t.to_raw()));
            modell::Endpoint { senders, receivers, delivered: self.zugestellt }
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

        /// Ein echter `call` -- mit dem Modellschritt, den er unter `alpha` bewirken muss.
        fn tue_call(&mut self, ep: &mut Endpoint, c: ThreadId, kern: usize)
            -> (modell::Endpoint, modell::Endpoint, modell::Endpoint) {
            let msg = self.naechste_msg;
            self.naechste_msg += 1;
            let vorher = self.alpha(ep);
            let erwartet = modell::send(&vorher, msg);
            let f = self.frame_von(c).expect("Aufrufer ohne Frame");
            frame_set_reg(f, reg::MSG0, msg);
            self.offen.insert(msg, c.to_raw());
            self.laufend.insert(kern, c);
            ep.call(&mut *self, kern, f);
            self.nachzaehlen();
            let nachher = self.alpha(ep);
            (vorher, erwartet, nachher)
        }

        /// Ein echter `recv` -- mit seinem Modellschritt.
        fn tue_recv(&mut self, ep: &mut Endpoint, s: ThreadId, kern: usize)
            -> (modell::Endpoint, modell::Endpoint, modell::Endpoint) {
            let vorher = self.alpha(ep);
            let erwartet = modell::recv(&vorher, s.to_raw());
            let f = self.frame_von(s).expect("Empfaenger ohne Frame");
            self.laufend.insert(kern, s);
            ep.recv(&mut *self, kern, f);
            self.nachzaehlen();
            let nachher = self.alpha(ep);
            (vorher, erwartet, nachher)
        }

        fn tue_reply(&mut self, ep: &mut Endpoint, s: ThreadId, kern: usize) {
            let f = self.frame_von(s).expect("Server ohne Frame");
            self.laufend.insert(kern, s);
            ep.reply(&mut *self, kern, f);
            self.nachzaehlen();
        }
    }

    struct Bericht {
        n: u32,
        fehler: u32,
    }

    impl Bericht {
        /// Entsprechung: der echte Schritt tut unter `alpha` dasselbe wie der Modellschritt --
        /// und `ep_inv`/`msgs_total` gelten am Ergebnis.
        ///
        /// `sendend` traegt die bewiesene Buchhaltung mit: **jedes** `send` erhoeht `msgs_total`
        /// um genau 1 (zugestellt ODER eingereiht, nie beides und nie keines -- `send_no_loss`),
        /// **jedes** `recv` laesst sie unveraendert (`recv_delivers_once`). Das ist die eine
        /// Zahl, an der ein Nachrichtenverlust auffiele.
        fn deckt(&mut self, name: &str, vorher: &modell::Endpoint, erwartet: &modell::Endpoint,
                 nachher: &modell::Endpoint, sendend: bool) {
            self.n += 1;
            let gleich = erwartet == nachher;
            let inv = modell::ep_inv(nachher);
            let erwartete_summe = modell::msgs_total(vorher) + u64::from(sendend);
            let summe = modell::msgs_total(nachher) == erwartete_summe;
            if gleich && inv && summe {
                println!("  deckt   : {}", name);
            } else {
                self.fehler += 1;
                println!("  ABWEICHUNG: {}", name);
                if !gleich {
                    println!("              Modell {:?}", erwartet);
                    println!("              Code   {:?}", nachher);
                }
                if !inv {
                    println!("              ep_inv verletzt: {:?}", nachher);
                }
                if !summe {
                    println!("              msgs_total {} statt {}",
                             modell::msgs_total(nachher), erwartete_summe);
                }
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

        /// Befund: eine Aussage des Modells gilt am echten Endpoint NICHT. Wird als Tatsache
        /// geprueft, damit sie nicht stillschweigend verschwindet -- und damit ein
        /// aufgeweichtes `ep_inv` (etwa `true`) auffliegt.
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

    pub fn run() -> i32 {
        let mut b = Bericht { n: 0, fehler: 0 };

        // -- A: die Entsprechung unter den Nebenbedingungen ------------------------------------
        //    (belegt, quer, kein toter Partner, Warteschlange nicht voll)
        {
            let mut w = Welt::neu();
            let mut ep = Endpoint::EMPTY;
            ep.mark_used();
            let s = w.faden(0);
            let c = w.faden(0);

            let (v, e, n) = w.tue_recv(&mut ep, s, 0);
            b.deckt("RECV auf leeren Endpoint -> Empfaenger parkt", &v, &e, &n, false);

            let (v, e, n) = w.tue_call(&mut ep, c, 0);
            b.deckt("CALL trifft wartenden Empfaenger (gleicher Kern, switch_to)", &v, &e, &n, true);

            w.tue_reply(&mut ep, s, 0);
            let nach_reply = w.alpha(&ep);
            b.befund("REPLY ist unter alpha unsichtbar (das Modell kennt keine Antwort)",
                     nach_reply == n);
        }
        {
            let mut w = Welt::neu();
            let mut ep = Endpoint::EMPTY;
            ep.mark_used();
            let c = w.faden(0);
            let s = w.faden(0);

            let (v, e, n) = w.tue_call(&mut ep, c, 0);
            b.deckt("CALL auf leeren Endpoint -> Sender parkt", &v, &e, &n, true);

            let (v, e, n) = w.tue_recv(&mut ep, s, 0);
            b.deckt("RECV holt wartenden Sender", &v, &e, &n, false);
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
                let (v, e, n) = w.tue_call(&mut ep, *c, 0);
                b.deckt(&format!("CALL {} von drei parkt in Reihenfolge", i + 1), &v, &e, &n, true);
            }
            for (i, s) in [s1, s2].iter().enumerate() {
                let (v, e, n) = w.tue_recv(&mut ep, *s, 0);
                b.deckt(&format!("RECV {} nimmt den AELTESTEN Sender (FIFO)", i + 1),
                        &v, &e, &n, false);
            }
        }
        {
            // Der zweite Kern-Pfad: Empfaenger auf einem FREMDEN Kern -> unblock statt switch_to.
            // Dass beide Pfade dasselbe Modellverhalten haben, ist eine Aussage; sie muss laufen.
            let mut w = Welt::neu();
            let mut ep = Endpoint::EMPTY;
            ep.mark_used();
            let s = w.faden(3);
            let c = w.faden(0);
            let (v, e, n) = w.tue_recv(&mut ep, s, 3);
            b.deckt("RECV auf Kern 3 parkt", &v, &e, &n, false);
            let (v, e, n) = w.tue_call(&mut ep, c, 0);
            b.deckt("CALL trifft Empfaenger auf FREMDEM Kern (unblock+IPI-Pfad)", &v, &e, &n, true);
        }
        {
            // Zwei geparkte Empfaenger, zwei Aufrufer -- Rendezvous in Reihenfolge.
            let mut w = Welt::neu();
            let mut ep = Endpoint::EMPTY;
            ep.mark_used();
            let s1 = w.faden(0);
            let s2 = w.faden(0);
            let c1 = w.faden(0);
            let c2 = w.faden(0);
            for (i, s) in [s1, s2].iter().enumerate() {
                let (v, e, n) = w.tue_recv(&mut ep, *s, 0);
                b.deckt(&format!("RECV {} von zwei parkt", i + 1), &v, &e, &n, false);
            }
            for (i, c) in [c1, c2].iter().enumerate() {
                let (v, e, n) = w.tue_call(&mut ep, *c, 0);
                b.deckt(&format!("CALL {} nimmt den AELTESTEN Empfaenger (FIFO)", i + 1),
                        &v, &e, &n, true);
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
                let (v, e, n) = if sendend {
                    w.tue_call(&mut ep, *t, *kern)
                } else {
                    w.tue_recv(&mut ep, *t, *kern)
                };
                b.deckt(&format!("gemischte Kette, Schritt {} ({})", k + 1,
                                 if sendend { "CALL" } else { "RECV" }), &v, &e, &n, sendend);
            }
        }

        // -- B: Gegenproben. Ohne die Nebenbedingungen zerbricht die Entsprechung. -------------
        {
            // G1: stillgelegt (A-4.2). `gate_new_transaction` weist ab; das Modell kennt kein Tor.
            let mut w = Welt::neu();
            let mut ep = Endpoint::EMPTY;
            ep.mark_used();
            ep.begin_quiesce();
            let c = w.faden(0);
            let (_, e, n) = w.tue_call(&mut ep, c, 0);
            b.weicht_ab("G1 stillgelegter Endpoint: CALL wird abgewiesen, das Modell reiht ein",
                        &e, &n);
        }
        {
            // G2: unbelegter Endpoint -> ERR_BADCAP. Im Modell gibt es kein `used`.
            let mut w = Welt::neu();
            let mut ep = Endpoint::EMPTY;
            let c = w.faden(0);
            let (_, e, n) = w.tue_call(&mut ep, c, 0);
            b.weicht_ab("G2 unbelegter Endpoint: CALL wird abgewiesen, das Modell reiht ein",
                        &e, &n);
        }
        {
            // G3: der wartende Empfaenger ist eine Leiche. Der Code verwirft sie und blockiert
            //     den Sender; das Modell sieht eine Empfaengerschlange der Laenge 1 und stellt zu.
            let mut w = Welt::neu();
            let mut ep = Endpoint::EMPTY;
            ep.mark_used();
            let s = w.faden(0);
            let c = w.faden(0);
            let (_, _, _) = w.tue_recv(&mut ep, s, 0);
            w.toeten(s);
            let (_, e, n) = w.tue_call(&mut ep, c, 0);
            b.weicht_ab("G3 toter Empfaenger: der Code verwirft die Leiche, das Modell stellt zu",
                        &e, &n);
        }
        {
            // G4: die Warteschlange laeuft ueber. `TidQueue::enqueue` verwirft STILL --
            //     `send_no_loss` (kein Nachrichtenverlust) gilt hier nicht.
            let mut w = Welt::neu();
            let mut ep = Endpoint::EMPTY;
            ep.mark_used();
            let mut letzte = (modell::Endpoint::default(), modell::Endpoint::default());
            let mut summe_vorher = 0u64;
            for i in 0..=QUEUE_CAP {
                let c = w.faden(0);
                let (v, e, n) = w.tue_call(&mut ep, c, 0);
                if i == QUEUE_CAP {
                    summe_vorher = modell::msgs_total(&v);
                    letzte = (e, n);
                }
            }
            b.weicht_ab(&format!("G4 der {}. Sender an EINEM Endpoint (QUEUE_CAP={})",
                                 QUEUE_CAP + 1, QUEUE_CAP), &letzte.0, &letzte.1);
            b.befund(&format!("B2 send_no_loss gilt am echten Endpoint NICHT \
                               (msgs_total {} statt {})",
                              modell::msgs_total(&letzte.1), summe_vorher + 1),
                     modell::msgs_total(&letzte.1) != summe_vorher + 1);
        }

        // -- C: die Befunde am `ep_inv` -- ueber die OEFFENTLICHE Schnittstelle erreichbar. -----
        {
            // B1a: `bind_receiver` prueft die Sender-Warteschlange nicht. Ein Empfaenger parkt,
            //      waehrend eine Nachricht ansteht -- genau der Zustand, den `ep_inv` ausschliesst.
            let mut w = Welt::neu();
            let mut ep = Endpoint::EMPTY;
            ep.mark_used();
            let c = w.faden(0);
            let v2 = w.faden(0);
            let _ = w.tue_call(&mut ep, c, 0);
            let gebunden = ep.bind_receiver(v2);
            let a = w.alpha(&ep);
            b.befund("B1a bind_receiver bei wartendem Sender verletzt ep_inv",
                     gebunden && !a.senders.is_empty() && !a.receivers.is_empty()
                         && !modell::ep_inv(&a));
        }
        {
            // B1b: derselbe Riss auf dem Hot-Reload-Weg: `migrate_owner` reiht den Aufrufer
            //      wieder als Sender ein, waehrend v2 schon als Empfaenger geparkt ist.
            let mut w = Welt::neu();
            let mut ep = Endpoint::EMPTY;
            ep.mark_used();
            let v1 = w.faden(0);
            let c = w.faden(0);
            let v2 = w.faden(0);
            let _ = w.tue_recv(&mut ep, v1, 0);
            let _ = w.tue_call(&mut ep, c, 0);
            let gebunden = ep.bind_receiver(v2);
            let migriert = ep.migrate_owner(v1);
            let a = w.alpha(&ep);
            b.befund("B1b migrate_owner bei geparktem Empfaenger verletzt ep_inv",
                     gebunden && migriert && !a.senders.is_empty() && !a.receivers.is_empty()
                         && !modell::ep_inv(&a));
        }
        {
            // Positivkontrolle zu B1: derselbe Ausdruck muss an einem gesunden Zustand WAHR sein.
            // Ohne sie koennte `!ep_inv(...)` auch deshalb halten, weil `ep_inv` immer falsch ist.
            let mut w = Welt::neu();
            let mut ep = Endpoint::EMPTY;
            ep.mark_used();
            let c = w.faden(0);
            let _ = w.tue_call(&mut ep, c, 0);
            let a = w.alpha(&ep);
            b.n += 1;
            if modell::ep_inv(&a) && !a.senders.is_empty() {
                println!("  deckt   : Positivkontrolle -- ep_inv gilt am gesunden Zustand");
            } else {
                b.fehler += 1;
                println!("  FEHLER  : ep_inv gilt nicht einmal am gesunden Zustand -- die");
                println!("              Befunde B1a/B1b sagen dann nichts aus.");
            }
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

    mutieren() {   # mutieren <ziel: code|modell> <python-ersetzung>
        cp "$CODE_STD" "$W/lib.rs"
        cp "$MODELL_STD" "$W/endpoint.rs"
        local datei="$W/lib.rs"; [ "$1" = "modell" ] && datei="$W/endpoint.rs"
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
    }

    erwarte() {   # erwarte <kracht|still> <name>
        n=$((n+1))
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

    # -- Mutationen am ECHTEN Code ---------------------------------------------------------------
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

    # -- Mutationen am MODELL --------------------------------------------------------------------
    mutieren modell 's = s.replace("Endpoint { senders: ep.senders, receivers: ep.receivers.drop_first(), delivered: ep.delivered + 1 }",
                                   "Endpoint { senders: ep.senders, receivers: ep.receivers.drop_first(), delivered: ep.delivered }", 1)'
    erwarte kracht "Modell: send zaehlt die Zustellung nicht mehr"

    mutieren modell 's = s.replace("    if ep.receivers.len() > 0 {", "    if ep.receivers.len() >= 0 {", 1)'
    erwarte kracht "Modell: send nimmt immer den Rendezvous-Zweig"

    mutieren modell 's = s.replace("        Endpoint { senders: ep.senders.drop_first(), receivers: ep.receivers, delivered: ep.delivered + 1 }",
                                   "        Endpoint { senders: ep.senders, receivers: ep.receivers, delivered: ep.delivered + 1 }", 1)'
    erwarte kracht "Modell: recv nimmt die Nachricht nicht aus der Warteschlange"

    mutieren modell 's = s.replace("pub open spec fn send(", "pub open spec fn send_umbenannt(", 1)'
    erwarte kracht "Modell: die spec fn send gibt es nicht mehr (der Waechter liest nicht ins Leere)"

    mutieren modell 's = s.replace("    ep.senders.len() == 0 || ep.receivers.len() == 0", "    true", 1)'
    erwarte kracht "Modell: ep_inv auf true aufgeweicht (ein Pruefer, der nicht mehr urteilt)"

    mutieren modell 's = s.replace("pub open spec fn msgs_total", "pub open spec fn msgs_gesamt", 1)'
    erwarte kracht "Modell: msgs_total umbenannt"

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
s = s.replace("pub open spec fn send(ep: Endpoint, msg: nat) -> Endpoint {",
              "// Kommentar des Selbsttests.\n\npub open spec fn send(ep: Endpoint, nachricht: nat) -> Endpoint {", 1)
s = s.replace("Endpoint { senders: ep.senders.push(msg),", "Endpoint { senders: ep.senders.push(nachricht),", 1)
if s == vor:
    sys.exit("FEHLER: die kosmetische Mutation am Modell hat nichts geaendert.")
io.open(m, 'w', encoding='utf-8').write(s)
PY
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
