//! **Der Verifiziererthread** — die Krypto vom Stack des Aufrufers holen (C8).
//!
//! # Der Befund, aus dem das folgt
//!
//! Gemessen am 2026-08-10 (`kstack`-Zeile, Lade-Suite): der tiefste Kernelpfad dieses Systems ist
//! `SYS_LOAD` → `verify_image` → **Ed25519 + SHA-2 im Kernel**, und er lief auf dem
//! **16-KiB-EL1-Stack des aufrufenden EL0-Threads**. Höchststand **11 992 von 16 384 B (73,1 %)**,
//! Reserve 4392 B; die grössten Rahmen des Abbilds liegen genau dort
//! (`vartime_double_scalar_mul_basepoint` 3528 B, `NafLookupTable::from` 1928 B).
//!
//! Damit zahlt **jeder** Thread 16 KiB für **einen** Pfad. Alle anderen Syscalls bleiben unter
//! 1 KiB. Bei 10 000 Threads sind das 160 MiB — auf einer 512-MiB-Maschine 31 % nur für schlafende
//! Threads. Die Verifikation gehört deshalb vom Aufrufer-Stack herunter, nicht der Stack
//! vergrössert.
//!
//! # Warum ein THREAD und nicht die zwei bequemeren Fassungen
//!
//! | Fassung | was sie kostet |
//! |---|---|
//! | Präemption für die Dauer aus | ein Latenzloch im Millisekundenbereich — für einen Mikrokern peinlich |
//! | Ein Arbeitsstack **je Kern** + Belegt-Bit | braucht einen neuen Wartegrund UND eine Blockiernaht, die es sonst nicht gäbe |
//! | **Ein dedizierter Verifiziererthread** ✔ | serialisiert Ladevorgänge, nutzt die **vorhandene** Blockiermaschinerie |
//!
//! Der Thread ist ein gewöhnlicher Kernel-Thread mit 64-KiB-Stack. `SYS_LOAD` reicht ihm den
//! Auftrag und blockiert den Aufrufer **regulär** über die Grund-Menge aus Z24
//! ([`BlockReasons::LOAD`](caprock_sched::BlockReasons::LOAD)).
//!
//! # Die Serialisierung IST ein Kanal — deshalb hat sie eine benannte Absage
//!
//! Eine PD, die `SYS_LOAD` spammt, verzögert fremde Ladevorgänge. Die Schlange hat deshalb eine
//! Schranke ([`AUFTRAEGE_MAX`]) und der Überlauf einen **Namen**
//! ([`ERR_LOAD_BUSY`](caprock_abi::result::ERR_LOAD_BUSY)). Das ist wörtlich die Lehre aus D11:
//! wer eine Kapazität einführt und den Überlauf nicht benennt, hat keinen Schutz gebaut, sondern
//! ein Loch — der 33. Sender wurde dort **trotzdem** blockiert, stand in keiner Struktur, bekam
//! keinen Code und wurde nie geweckt, während jeder Prüfer Ordnung meldete.
//!
//! **Der Überläufer bleibt lauffähig.** Er wird gar nicht erst blockiert.
//!
//! # Die Reihenfolge, an der alles hängt
//!
//! Der Aufrufer wird **blockiert, bevor sein Auftrag sichtbar wird** — beides unter *einer*
//! Sperrung von [`SCHLANGE`]. Andersherum könnte der Verifizierer auf einem anderen Kern fertig
//! sein, bevor der Aufrufer blockiert ist; sein `load_reply` liefe ins Leere, und der Aufrufer
//! setzte danach einen Grund, den niemand mehr entfernt. Für `PARK` fängt eine Weckmarke genau
//! das ab; für `LOAD` gibt es keine — die Sperrung ist der Ersatz, und sie ist billiger als ein
//! zweiter Zustand.
//!
//! **Sperrordnung:** `SCHLANGE` (neu, Rang zwischen R1 und R2) → `SCHEDS[*]`. Der Verifizierer
//! hält nie beide gleichzeitig: er entnimmt unter `SCHLANGE`, gibt frei, lädt, und antwortet dann
//! über `SCHEDS`. Damit kann kein Pfad `SCHEDS` vor `SCHLANGE` nehmen.
//!
//! # Was der Umzug an der `kstack`-Zeile ÄNDERT
//!
//! Sie misst danach **etwas anderes**: den Restpfad. Der EL0-Höchststand fällt von 73 % auf den
//! tiefsten *verbleibenden* Syscall — die Krypto steckt ab jetzt im 64-KiB-Wasserstand des
//! Verifizierers. Wer die beiden Zahlen über diesen Bedeutungswechsel hinweg vergleicht,
//! vergleicht zwei verschiedene Grössen; genau daran ist in diesem Projekt schon eine Zahl
//! wertlos geworden. Die Umdefinition steht deshalb in der Berichtszeile selbst, nicht nur hier.

use caprock_cap::CapPtr;
use caprock_sched::ThreadId;
use caprock_sync::SpinLock;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// **Wie viele Ladeaufträge gleichzeitig warten dürfen.**
///
/// Die Zahl ist eine Politikentscheidung und keine Herleitung — deshalb steht sie hier an *einer*
/// Stelle und wird im Bericht mitgedruckt. Vier ist gewählt, weil ein Ladevorgang teuer und selten
/// ist: mehr Warteplätze verlängern nur die Kette, an deren Ende ohnehin ein einziger Thread
/// arbeitet. Wer sie erhöht, macht den DoS-Kanal länger, nicht schmaler.
pub const AUFTRAEGE_MAX: usize = 4;

/// Höchstzahl der Endowment-Caps eines Auftrags — dieselbe Schranke, die `load_by_index` für seine
/// verdichtete Liste benutzt.
pub const ENDOW_MAX: usize = 8;

/// Ein wartender Ladeauftrag.
#[derive(Clone, Copy)]
struct Auftrag {
    /// Index im Boot-Archiv.
    index: u32,
    /// PD des Aufrufers (A-5.1: der Partner eines HardwareLand-Backends).
    caller_pd: usize,
    /// Wer wartet — und wem geantwortet wird.
    caller: ThreadId,
    /// **Der Kern des AUFRUFERS**, nicht der des Verifizierers.
    ///
    /// `load_into_pd_mit` legt den neuen Thread auf `pol.core.unwrap_or(hal::cpu::core_id())`.
    /// Liefe die Vorgabe über den Verifizierer, landete jedes ohne Affinität geladene Programm auf
    /// dessen Kern — eine stille Änderung der Platzierungspolitik als Nebenwirkung einer
    /// Stack-Verschiebung. Der Auftrag trägt den Kern deshalb mit.
    heimatkern: usize,
    /// Die vom Dispatch aus dem Aufrufer-Cspace abgeleiteten Caps.
    ///
    /// `Option`, weil [`CapPtr`] bewusst keinen öffentlichen Konstruktor hat — ein fabrizierbarer
    /// Cap-Handle wäre eine Einladung. Ein Feld aus `None` braucht keinen.
    endow: [Option<(usize, CapPtr)>; ENDOW_MAX],
}

/// Die Warteschlange: ein Ring fester Grösse.
struct Schlange {
    eintraege: [Option<Auftrag>; AUFTRAEGE_MAX],
    kopf: usize,
    anzahl: usize,
}

impl Schlange {
    const LEER: Schlange = Schlange {
        eintraege: [None; AUFTRAEGE_MAX],
        kopf: 0,
        anzahl: 0,
    };

    /// **Einen Platz am Ende reservieren** — `None` heisst voll.
    ///
    /// Getrennt vom Beschreiben, und das ist der Kern der Reihenfolge oben: zwischen Reservieren
    /// und Beschreiben wird der Aufrufer blockiert. Beides passiert unter *derselben* Sperrung, ein
    /// reservierter, noch leerer Platz ist also für niemanden sichtbar.
    ///
    /// Kein `enqueue(wert) -> bool` mit stillem `if`: D11. Wer nicht reservieren kann, bekommt
    /// `None` und **muss** damit umgehen.
    #[must_use = "ohne Platz darf der Aufrufer NICHT blockiert werden -- das waere D11 noch einmal"]
    fn platz_nehmen(&mut self) -> Option<usize> {
        if self.anzahl == AUFTRAEGE_MAX {
            return None;
        }
        let i = (self.kopf + self.anzahl) % AUFTRAEGE_MAX;
        self.anzahl += 1;
        Some(i)
    }

    fn belegen(&mut self, i: usize, a: Auftrag) {
        self.eintraege[i] = Some(a);
    }

    /// Den ältesten Auftrag entnehmen. `None` = leer.
    ///
    /// Ein *reservierter, aber unbeschriebener* Platz kann hier nicht auftauchen (s. o.). Träte er
    /// doch auf, wäre das ein Kernelfehler und keine leere Schlange — deshalb zählt [`VERLOREN`]
    /// ihn mit, und die Berichtszeile gattert darauf.
    fn entnehmen(&mut self) -> Option<Auftrag> {
        if self.anzahl == 0 {
            return None;
        }
        let a = self.eintraege[self.kopf].take();
        self.kopf = (self.kopf + 1) % AUFTRAEGE_MAX;
        self.anzahl -= 1;
        if a.is_none() {
            VERLOREN.fetch_add(1, Ordering::Relaxed);
        }
        a
    }
}

/// Die Schlange. Blattlock gegenüber allem ausser `SCHEDS` (s. Sperrordnung in der Moduldoku).
static SCHLANGE: SpinLock<Schlange> = SpinLock::new(Schlange::LEER);

/// `ThreadId` des Verifizierers als Rohwert (`0` = keiner).
static VERIFIZIERER: AtomicU64 = AtomicU64::new(0);
/// Physische Basis seines 64-KiB-Stacks (`0` = unbekannt) — für die Wasserstandsmessung.
static STACK_BASIS: AtomicUsize = AtomicUsize::new(0);
/// Länge seines Stacks.
static STACK_LAENGE: AtomicUsize = AtomicUsize::new(0);

/// Wie viele Aufträge angenommen (= eingereiht) wurden.
static ANGENOMMEN: AtomicUsize = AtomicUsize::new(0);
/// Wie viele **benannt abgewiesen** wurden, weil die Schlange voll war.
static ABGEWIESEN: AtomicUsize = AtomicUsize::new(0);
/// Wie viele abgewiesen wurden, weil es gar keinen Verifizierer gibt (Aufbaufehler, nicht Last).
static OHNE_THREAD: AtomicUsize = AtomicUsize::new(0);
/// Wie viele der Verifizierer **bearbeitet** hat.
static BEARBEITET: AtomicUsize = AtomicUsize::new(0);
/// Wie viele davon ein Ergebnis geliefert haben, dessen Aufrufer nicht mehr auflösbar war.
static ANTWORT_INS_LEERE: AtomicUsize = AtomicUsize::new(0);
/// Höchste je gleichzeitig wartende Auftragszahl (Füllstand, nicht Durchsatz).
static HOECHSTSTAND: AtomicUsize = AtomicUsize::new(0);
/// Reservierte, aber nie beschriebene Plätze — muss **0** sein (s. [`Schlange::entnehmen`]).
static VERLOREN: AtomicUsize = AtomicUsize::new(0);

/// Ausgang einer Übergabe an den Verifizierer.
///
/// **Der Typ gehört dem Dispatch, nicht diesem Modul.** Er ist die Antwort auf einen Syscall; wer
/// ihn hier noch einmal definierte, hätte zwei Aufzählungen für dieselbe Entscheidung — und die
/// Übersetzung dazwischen wäre die Stelle, an der eines Tages zwei Lagen zusammenfallen.
pub use caprock_microkit::LadeUebergabe as Uebergabe;

/// Die `ThreadId` des Verifizierers, falls er läuft.
pub fn thread_id() -> Option<ThreadId> {
    let raw = VERIFIZIERER.load(Ordering::Acquire);
    (raw != 0).then(|| ThreadId::from_raw(raw))
}

/// **Einen `SYS_LOAD` an den Verifizierer übergeben.**
///
/// `blockieren` blockiert den *laufenden* Thread mit [`BlockReasons::LOAD`] und liefert den
/// Stackpointer des nächsten Threads. Es wird **unter der Sperrung** gerufen und **vor** dem
/// Veröffentlichen des Auftrags — s. Moduldoku.
///
/// Der Aufrufer dieser Funktion (der Syscall-Dispatch) hält keine Locks; `blockieren` nimmt
/// `SCHEDS[core]` und gibt es sofort wieder frei.
pub fn uebergeben(
    index: u32,
    caller_pd: usize,
    endow: &[(usize, CapPtr)],
    caller: ThreadId,
    heimatkern: usize,
    blockieren: impl FnOnce() -> usize,
) -> Uebergabe {
    let Some(v) = thread_id() else {
        OHNE_THREAD.fetch_add(1, Ordering::Relaxed);
        return Uebergabe::KeinVerifizierer;
    };
    let mut felder: [Option<(usize, CapPtr)>; ENDOW_MAX] = [None; ENDOW_MAX];
    for (ziel, &c) in felder.iter_mut().zip(endow.iter()) {
        *ziel = Some(c);
    }
    let sp = {
        let mut q = SCHLANGE.lock();
        let Some(platz) = q.platz_nehmen() else {
            ABGEWIESEN.fetch_add(1, Ordering::Relaxed);
            return Uebergabe::Ausgelastet; // **nicht** blockiert
        };
        // Erst blockieren, dann veröffentlichen. Beides unter dieser Sperrung.
        let sp = blockieren();
        q.belegen(
            platz,
            Auftrag {
                index,
                caller_pd,
                caller,
                heimatkern,
                endow: felder,
            },
        );
        ANGENOMMEN.fetch_add(1, Ordering::Relaxed);
        HOECHSTSTAND.fetch_max(q.anzahl, Ordering::Relaxed);
        sp
    }; // SCHLANGE freigegeben
    // Wecken **nach** dem Freigeben: `unpark` nimmt `SCHEDS`, und die Ordnung ist SCHLANGE →
    // SCHEDS. Ein Wecken, das den Verifizierer trifft, bevor er schläft, hinterlässt seine Marke
    // (Z22 P4) — genau dafür gibt es sie.
    crate::system::unpark_thread(v);
    Uebergabe::Uebergeben(sp)
}

/// Der Rumpf des Verifizierers: entnehmen, laden, antworten, schlafen.
///
/// **Warum `PARK` und keine Warteschleife:** ein Poller kostet eine Zeitscheibe je Runde und
/// verdrängt genau die Threads, für die er arbeitet — dieselbe Form, mit der ein pollender Treiber
/// auf höherer Priorität am 2026-08-07 seinen Client verhungern liess. Die Weckmarke von `PARK`
/// deckt das verlorene Wecken zwischen „Schlange leer" und „schlafen" ab.
extern "C" fn rumpf(_arg: usize) -> ! {
    loop {
        // **`while let Some(a) = SCHLANGE.lock().entnehmen()` waere hier ein GROBER Fehler**, und
        // er hat den ganzen Entwurf still ausgehebelt: der Guard eines `while let`-Scrutinees lebt
        // bis zum ENDE DES RUMPFES (gemessen mit einem `Drop`-Zeugen, nicht erschlossen). Und
        // `SpinLock::lock` maskiert die IRQs des eigenen Kerns. Die gesamte Ed25519-/SHA-2-Prüfung
        // liefe damit **mit gesperrten Interrupts** — also genau die Fassung „Präemption für die
        // Dauer aus", die C8 ausdrücklich verworfen hat, hereingeholt durch eine
        // Temporaries-Lebensdauer statt durch eine Entscheidung.
        //
        // `naechster()` erzwingt die Freigabe: der Guard endet an der Funktionsgrenze.
        while let Some(a) = naechster() {
            BEARBEITET.fetch_add(1, Ordering::Relaxed);
            let pd = laden(&a);
            if !crate::system::lade_antwort(a.caller, pd) {
                // Der Aufrufer ist zwischen Übergabe und Antwort gestorben. Das ist kein Fehler,
                // aber es ist auch nicht nichts: eine geladene PD ohne jemanden, der ihre Id
                // erfährt, gehört gezählt statt verschwiegen.
                ANTWORT_INS_LEERE.fetch_add(1, Ordering::Relaxed);
            }
        }
        caprock_hal::syscall::invoke(caprock_abi::sys::PARK, 0, [0; 4], 0);
    }
}

/// Den nächsten Auftrag entnehmen — **und die Sperre dabei sicher wieder loslassen**.
///
/// Die Funktionsgrenze ist hier der Mechanismus, nicht die Formatierung: sie beendet die
/// Lebensdauer des Guards *garantiert* vor dem Laden. Ein `SCHLANGE.lock().entnehmen()` direkt im
/// `while let` hielte ihn über den ganzen Rumpf — mit maskierten IRQs (s. `rumpf`).
fn naechster() -> Option<Auftrag> {
    SCHLANGE.lock().entnehmen()
}

/// Den Auftrag ausführen — **auf DIESEM Stack**, und das ist der ganze Zweck.
fn laden(a: &Auftrag) -> Option<usize> {
    // Verdichten mit dem ersten echten Cap als Füllwert; `CapPtr` hat keinen öffentlichen
    // Konstruktor (derselbe Weg wie in `load_by_index`).
    let Some(first) = a.endow.iter().flatten().next().copied() else {
        return crate::loader::load_by_index(a.index, a.caller_pd, &[], a.heimatkern);
    };
    let mut dense = [first; ENDOW_MAX];
    let mut n = 0usize;
    for &c in a.endow.iter().flatten() {
        dense[n] = c;
        n += 1;
    }
    crate::loader::load_by_index(a.index, a.caller_pd, &dense[..n], a.heimatkern)
}

/// **Den Verifizierer starten.** Aus beiden Hochlaufwegen zu rufen, **vor** dem Root-Task — der
/// ist der erste, der `SYS_LOAD` benutzt.
///
/// Priorität: dieselbe wie ein gewöhnlicher Thread. Höher zu gehen wäre verlockend (er hält
/// Wartende auf), wäre aber genau der Fehler, den `ladepol` am 2026-08-07 gefunden hat — nur ist
/// er hier harmloser, weil der Verifizierer nicht pollt, sondern schläft. Niedriger zu gehen wäre
/// schlechter: dann verlängert jeder rechnende Thread die Wartezeit aller Ladenden.
pub fn starten() -> bool {
    if thread_id().is_some() {
        return true; // schon da (zweiter Hochlaufweg / Wiederholung)
    }
    let kern = 0usize; // der Bootkern -- dort läuft auch der Root-Task
    let Some((parked, basis, laenge)) = crate::system::spawn_on_core_parked_mit_stack(
        kern,
        rumpf as *const () as usize,
        0,
        crate::system::IDLE_PRIO,
    ) else {
        return false;
    };
    let Some(tid) = crate::system::admit(parked) else {
        return false;
    };
    STACK_BASIS.store(basis, Ordering::Relaxed);
    STACK_LAENGE.store(laenge, Ordering::Relaxed);
    // **Zuletzt**, und das ist Absicht: ab dem Store nimmt `uebergeben` Aufträge an. Stünde er
    // vorher, könnte ein `SYS_LOAD` einen Thread wecken, der noch nicht zugelassen ist.
    VERIFIZIERER.store(tid.to_raw(), Ordering::Release);
    true
}

/// **Den Stack des Verifizierers messen** — mit demselben Wasserzeichen wie jeder andere.
///
/// Muss ausdrücklich gerufen werden: [`crate::system::kstack_marke_fegen`] fegt über die
/// **EL0**-Kstacks, und der Verifizierer ist ein Kernel-Thread. Er stirbt zudem nie, wird also auch
/// vom Reap-Pfad nie gemessen. Ohne diese Zeile wäre seine Stackgrösse **gewählt statt gemessen** —
/// und genau das verlangt C8 (b) nicht.
///
/// Gibt `(benutzt, frei, groesse)`; `(0, 0, 0)` heisst „nicht messbar", nicht „viel Luft".
pub fn stack_messen() -> (usize, usize, usize) {
    let basis = STACK_BASIS.load(Ordering::Relaxed);
    let laenge = STACK_LAENGE.load(Ordering::Relaxed);
    if basis == 0 || laenge == 0 {
        return (0, 0, 0);
    }
    // SAFETY: identity-gemappter, dem Verifizierer gehörender Stack; gelesen wird nur. Dass er
    // gerade darauf rechnet, macht den Messwert höchstens zu klein, nie zu gross.
    let (benutzt, frei) = unsafe {
        crate::kstackmark::messen(
            crate::kstackmark::KL_KERN,
            basis,
            laenge,
            crate::kstackmark::Anlass::Fegen(usize::MAX),
        )
    };
    (benutzt, frei, laenge)
}

/// Zählerstand für Bericht und Urteil.
#[derive(Clone, Copy)]
pub struct Stand {
    pub angenommen: usize,
    pub abgewiesen: usize,
    pub ohne_thread: usize,
    pub bearbeitet: usize,
    pub antwort_ins_leere: usize,
    pub hoechststand: usize,
    pub verloren: usize,
    pub wartend: usize,
    pub laeuft: bool,
}

// ================================================================================================
// DIE SONDE — die Absage wird GEFAHREN, nicht behauptet
// ================================================================================================
//
// **Warum das nicht als Kommentar reicht.** Eine Schranke, die nie erreicht wurde, ist von einer
// fehlenden nicht zu unterscheiden — das ist D11 in einem Satz. Der Überlauf muss also in einem
// gewöhnlichen Lauf **stattfinden**, mit seinem benannten Code, und der Überläufer muss danach
// nachweislich **weiterlaufen**.
//
// **Wie er deterministisch herbeigeführt wird:** der Verifizierer wird pausiert (`PAUSE` — ein
// anderer Grund als sein `PARK`, beide stehen nebeneinander in der Menge und stören einander
// nicht). Dann melden sich [`SONDEN`] Aufrufer, also einer mehr als [`AUFTRAEGE_MAX`]. Vier finden
// Platz und blockieren; der fünfte findet keinen und muss **sofort** zurückkommen.
//
// **Warum ein ungültiger Archivindex.** Gemessen wird die SCHLANGE, nicht das Laden. Ein gültiger
// Index hätte Nebenwirkungen (PDs, Hot-Reload-Gate, Gerätezuteilung) und liefe in der Hauptsuite
// gar nicht, weil es dort kein Archiv gibt. Mit einem ungültigen läuft die Sonde in **beiden**
// Suiten, und die beiden Ausgänge sind trennscharf: `ERR_BADCAP` heisst „der Auftrag war beim
// Verifizierer und ist dort gescheitert", `ERR_LOAD_BUSY` heisst „er kam nie hin".

/// Wie viele Sonden — einer mehr, als die Schlange fasst.
#[cfg(feature = "selftest")]
pub const SONDEN: usize = AUFTRAEGE_MAX + 1;

/// Ein Archivindex, den kein Archiv hat. Der Auftrag läuft damit bis zum Verifizierer und fällt
/// dort sauber durch — ohne eine einzige Ressource zu belegen.
#[cfg(feature = "selftest")]
const SONDEN_INDEX: u64 = 0xffff_0000;

#[cfg(feature = "selftest")]
static SONDE_TID: [AtomicU64; SONDEN] = [const { AtomicU64::new(0) }; SONDEN];
/// Ergebniscode je Sonde; `u64::MAX` = noch nicht zurück.
#[cfg(feature = "selftest")]
static SONDE_CODE: [AtomicU64; SONDEN] = [const { AtomicU64::new(u64::MAX) }; SONDEN];
/// Runden, die eine Sonde **nach** ihrem Syscall gedreht hat — die Größe, an der „läuft weiter"
/// hängt. Ein Zustandsbit sagt das nicht; ein Fortschritt schon (dieselbe Unterscheidung wie bei
/// der FP- und der Park-Sonde).
#[cfg(feature = "selftest")]
static SONDE_RUNDEN: [AtomicU64; SONDEN] = [const { AtomicU64::new(0) }; SONDEN];
/// Ergebnis der EINMALIGEN Messung. Bit 0 „bestanden", **Bit 11 „gemessen"**, Bit 8..10 die
/// Einzelaussagen, ab Bit 16 die Zahlen zu je 8 Bit.
///
/// **Die Marke steht bewusst NICHT auf Bit 63.** Dort lag sie im ersten Entwurf, und Bit 63 gehört
/// zum obersten Zahlenfeld (`verloren`): die Marke las sich beim Auspacken als `verloren = 128`,
/// das Urteil fiel durch, und der Lauf ging in den Watchdog. Ein Bild, das genau so aussieht wie
/// ein echter Befund — nur dass jedes Einzelfeld grün war.
#[cfg(feature = "selftest")]
static SONDE_MESS: AtomicU64 = AtomicU64::new(0);

/// Bit 11: „diese Messung hat stattgefunden" — s. [`SONDE_MESS`].
#[cfg(feature = "selftest")]
const MESS_GELAUFEN: u64 = 1 << 11;

#[cfg(feature = "selftest")]
extern "C" fn sonde(arg: usize) -> ! {
    let i = arg;
    let r = caprock_hal::syscall::invoke(
        caprock_abi::sys::LOAD,
        0, // Slot 0 = Loader-Cap
        [SONDEN_INDEX, u64::MAX, 0, 0],
        0,
    );
    if i < SONDEN {
        SONDE_CODE[i].store(r.result, Ordering::Release);
    }
    loop {
        if i < SONDEN {
            SONDE_RUNDEN[i].fetch_add(1, Ordering::Relaxed);
        }
        caprock_hal::syscall::invoke(caprock_abi::sys::YIELD, 0, [0; 4], 0);
    }
}

/// Die Einzelaussagen der Messung (Bit 8 aufwärts in [`SONDE_MESS`]).
#[cfg(feature = "selftest")]
pub struct Sondenbild {
    /// Es gibt überhaupt einen Verifizierer (Sprechprobe).
    pub laeuft: bool,
    /// Wie viele Sonden gestartet wurden.
    pub gestartet: usize,
    /// Wie viele die **benannte** Absage bekamen.
    pub abgewiesen: usize,
    /// Wie viele bedient wurden (Auftrag lief durch den Verifizierer → `ERR_BADCAP`).
    pub bedient: usize,
    /// Wie viele Wartende zum Beobachtungszeitpunkt **wegen LOAD** blockiert waren.
    pub blockiert: usize,
    /// Lief **jeder** Abgewiesene danach weiter (Rundenzähler gewachsen)?
    pub ueberlaeufer_laeuft: bool,
    /// War **kein** Abgewiesener blockiert? (Die Kernaussage von C8 (a).)
    pub ueberlaeufer_frei: bool,
    /// Höchster erreichter Füllstand — muss die Schranke erreicht haben, sonst war sie nicht im
    /// Spiel und die ganze Messung sagt nichts.
    pub hoechststand: usize,
    /// Reservierte, aber nie beschriebene Plätze — muss 0 sein.
    pub verloren: usize,
}

#[cfg(feature = "selftest")]
impl Sondenbild {
    /// **Das Urteil.** Acht Konjunkte, jedes einzeln falsifizierbar.
    pub fn ok(&self) -> bool {
        self.laeuft
            && self.gestartet == SONDEN
            // Die Schranke wurde WIRKLICH erreicht -- ohne das ist alles Weitere eine Aussage
            // ueber einen Fall, der nie eingetreten ist.
            && self.hoechststand == AUFTRAEGE_MAX
            && self.abgewiesen >= 1
            // Jeder Abgewiesene lief weiter und war NIE blockiert -- D11 in beide Richtungen.
            && self.ueberlaeufer_frei
            && self.ueberlaeufer_laeuft
            // Positivkontrolle: die Bedienten haben wirklich wegen LOAD gewartet. Ohne sie
            // bestuende die Zeile auch ein System, in dem gar niemand blockiert -- dann waere
            // „der Ueberlaeufer ist nicht blockiert" trivial wahr.
            && self.blockiert >= 1
            && self.abgewiesen + self.bedient == SONDEN
            && self.verloren == 0
    }
}

/// Das Ergebnis der Messung (`None` = noch nicht gemessen).
#[cfg(feature = "selftest")]
pub fn sondenbild() -> Option<Sondenbild> {
    let m = SONDE_MESS.load(Ordering::Acquire);
    if m & MESS_GELAUFEN == 0 {
        return None;
    }
    Some(Sondenbild {
        laeuft: m & (1 << 8) != 0,
        gestartet: ((m >> 16) & 0xff) as usize,
        abgewiesen: ((m >> 24) & 0xff) as usize,
        bedient: ((m >> 32) & 0xff) as usize,
        blockiert: ((m >> 40) & 0xff) as usize,
        ueberlaeufer_laeuft: m & (1 << 9) != 0,
        ueberlaeufer_frei: m & (1 << 10) != 0,
        hoechststand: ((m >> 48) & 0xff) as usize,
        verloren: ((m >> 56) & 0xff) as usize,
    })
}

/// **Das Urteil der `verif`-Zeile** — an EINER Stelle, damit `all_done` und der Bericht dieselbe
/// Wirklichkeit lesen. `false`, solange nicht gemessen wurde: eine nie gefahrene Absage darf nicht
/// wie eine bestandene aussehen.
#[cfg(feature = "selftest")]
pub fn urteil() -> bool {
    sondenbild().is_some_and(|b| b.ok())
}

/// **Die Messung — läuft genau einmal**, aus der Hauptschleife des Hochlaufs.
///
/// Wandzeit: sie pausiert den Verifizierer für die Dauer. Solange sie läuft, wartet jeder fremde
/// `SYS_LOAD` mit — das ist beabsichtigt und harmlos, weil sie erst läuft, wenn der Ladepfad der
/// Suite durch ist.
#[cfg(feature = "selftest")]
pub fn messen() {
    if SONDE_MESS.load(Ordering::Acquire) & MESS_GELAUFEN != 0 {
        return;
    }
    let bild = messen_inner();
    let m = MESS_GELAUFEN
        | u64::from(bild.ok())
        | (u64::from(bild.laeuft) << 8)
        | (u64::from(bild.ueberlaeufer_laeuft) << 9)
        | (u64::from(bild.ueberlaeufer_frei) << 10)
        | ((bild.gestartet as u64 & 0xff) << 16)
        | ((bild.abgewiesen as u64 & 0xff) << 24)
        | ((bild.bedient as u64 & 0xff) << 32)
        | ((bild.blockiert as u64 & 0xff) << 40)
        | ((bild.hoechststand as u64 & 0xff) << 48)
        | ((bild.verloren as u64 & 0xff) << 56);
    SONDE_MESS.store(m, Ordering::Release);
}

#[cfg(feature = "selftest")]
fn messen_inner() -> Sondenbild {
    use caprock_mem::Rights;
    let leer = Sondenbild {
        laeuft: false,
        gestartet: 0,
        abgewiesen: 0,
        bedient: 0,
        blockiert: 0,
        ueberlaeufer_laeuft: false,
        ueberlaeufer_frei: false,
        hoechststand: 0,
        verloren: VERLOREN.load(Ordering::Relaxed),
    };
    let Some(v) = thread_id() else {
        return leer;
    };
    let warten = || {
        let t0 = caprock_hal::timer::ticks(0);
        let mut wache = 0u64;
        while caprock_hal::timer::ticks(0) < t0 + 2 && wache < 200_000_000 {
            core::hint::spin_loop();
            wache += 1;
        }
    };

    // (1) Den Verifizierer anhalten. `PAUSE` steht **neben** seinem `PARK` in der Grund-Menge; ein
    //     eintreffender Auftrag entfernt `PARK`, die Menge bleibt nicht leer, er läuft nicht los.
    //     Genau die Eigenschaft, die Z24 hergestellt hat.
    if !crate::system::pause_thread(v) {
        return leer;
    }

    // (2) Die Sonden aufsetzen. Jede bekommt ihre eigene TrustedSAS-PD mit Loader-Cap in Slot 0 —
    //     Autorität **vor** der Zulassung (D0), sonst syscallt sie mit leerem Cspace.
    let mut gestartet = 0usize;
    for i in 0..SONDEN {
        let aufbau = (|| {
            let lcap = crate::system::install_loader_cap(0, Rights::RW).ok()?;
            let pd = crate::system::create_pd_in_domain(caprock_microkit::Domain::TrustedSas)?;
            if !crate::system::install_pd_cap(pd, 0, lcap) {
                return None;
            }
            let p = crate::system::spawn_parked(
                sonde as *const () as usize,
                i,
                crate::system::IDLE_PRIO,
            )?;
            crate::system::admit_in_pd(pd, p)
        })();
        match aufbau {
            Some(tid) => {
                SONDE_TID[i].store(tid.to_raw(), Ordering::Release);
                gestartet += 1;
            }
            None => break,
        }
    }

    // (3) Warten, bis die Schranke greift. Nur der Überläufer kann zurückkommen, solange der
    //     Verifizierer steht — jede Rückkehr ist deshalb schon die Aussage.
    //
    //     **Begrenzt**, und das ist Absicht: greift die Schranke NICHT (die Gegenprobe), soll die
    //     Zeile durchfallen und der Lauf weitergehen. Ein unbegrenztes Warten machte aus einem
    //     benannten Fehlschlag einen Watchdog, und ein Watchdog erklärt nichts.
    let abgewiesen_vorher = ABGEWIESEN.load(Ordering::Relaxed);
    let mut trafen_ein = false;
    for _ in 0..96 {
        if ABGEWIESEN.load(Ordering::Relaxed) > abgewiesen_vorher {
            trafen_ein = true;
            break;
        }
        warten();
    }

    // (4) Der Beobachtungszeitpunkt: WÄHREND der Verifizierer steht. Jetzt sind die Aussagen
    //     „bedient wartet" und „Überläufer wartet nicht" gleichzeitig prüfbar.
    let mut blockiert = 0usize;
    let mut ueberlaeufer_frei = true;
    let mut runden_vor = [0u64; SONDEN];
    for i in 0..gestartet {
        let tid = caprock_sched::ThreadId::from_raw(SONDE_TID[i].load(Ordering::Acquire));
        let code = SONDE_CODE[i].load(Ordering::Acquire);
        let wartet = crate::system::is_load_blocked(tid);
        if code == u64::MAX {
            if wartet {
                blockiert += 1;
            }
        } else {
            // Zurückgekommen: er darf NICHT auf den Verifizierer warten, und sein Code muss der
            // benannte sein. Ein Überläufer mit `ERR_BADCAP` wäre schlimmer als einer mit
            // `ERR_LOAD_BUSY` — er sähe wie ein bedienter aus.
            if wartet || code != caprock_abi::result::ERR_LOAD_BUSY {
                ueberlaeufer_frei = false;
            }
        }
        runden_vor[i] = SONDE_RUNDEN[i].load(Ordering::Relaxed);
    }
    let hoechststand = HOECHSTSTAND.load(Ordering::Relaxed);

    // (5) Der Überläufer muss **weiterlaufen**, nicht bloss „nicht blockiert" sein. Gemessen an
    //     seinem Rundenzähler, noch bevor der Verifizierer wieder anläuft.
    let mut ueberlaeufer_laeuft = trafen_ein;
    for _ in 0..64 {
        let mut alle = true;
        for i in 0..gestartet {
            if SONDE_CODE[i].load(Ordering::Acquire) != u64::MAX
                && SONDE_RUNDEN[i].load(Ordering::Relaxed) <= runden_vor[i]
            {
                alle = false;
            }
        }
        if alle {
            break;
        }
        warten();
    }
    for i in 0..gestartet {
        if SONDE_CODE[i].load(Ordering::Acquire) != u64::MAX
            && SONDE_RUNDEN[i].load(Ordering::Relaxed) <= runden_vor[i]
        {
            ueberlaeufer_laeuft = false;
        }
    }

    // (6) Wieder anlaufen lassen und die Bedienten einsammeln.
    crate::system::resume_thread(v);
    for _ in 0..128 {
        if (0..gestartet).all(|i| SONDE_CODE[i].load(Ordering::Acquire) != u64::MAX) {
            break;
        }
        warten();
    }
    let mut abgewiesen = 0usize;
    let mut bedient = 0usize;
    for i in 0..gestartet {
        match SONDE_CODE[i].load(Ordering::Acquire) {
            c if c == caprock_abi::result::ERR_LOAD_BUSY => abgewiesen += 1,
            c if c == caprock_abi::result::ERR_BADCAP => bedient += 1,
            _ => {}
        }
    }
    Sondenbild {
        laeuft: true,
        gestartet,
        abgewiesen,
        bedient,
        blockiert,
        ueberlaeufer_laeuft,
        ueberlaeufer_frei,
        hoechststand,
        verloren: VERLOREN.load(Ordering::Relaxed),
    }
}

/// Den Zählerstand lesen.
pub fn stand() -> Stand {
    // **Erst die Sperre, dann alles andere** — und nicht mitten im Struct-Literal. Ein Guard, der
    // dort entsteht, lebt bis zum Ende des Literals; er hielte die Schlange (und die IRQs dieses
    // Kerns) über die übrigen Felder. Heute sind das nur Atomics, morgen vielleicht nicht.
    let wartend = SCHLANGE.lock().anzahl;
    Stand {
        angenommen: ANGENOMMEN.load(Ordering::Relaxed),
        abgewiesen: ABGEWIESEN.load(Ordering::Relaxed),
        ohne_thread: OHNE_THREAD.load(Ordering::Relaxed),
        bearbeitet: BEARBEITET.load(Ordering::Relaxed),
        antwort_ins_leere: ANTWORT_INS_LEERE.load(Ordering::Relaxed),
        hoechststand: HOECHSTSTAND.load(Ordering::Relaxed),
        verloren: VERLOREN.load(Ordering::Relaxed),
        wartend,
        laeuft: thread_id().is_some(),
    }
}
