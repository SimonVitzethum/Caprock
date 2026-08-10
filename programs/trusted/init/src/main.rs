//! `init` — der **Root-Task** (A-2.1).
//!
//! Der seL4-Weg: der Kernel laedt genau ein Startprogramm aus der Startmenge und uebergibt ihm die
//! Wurzel-Caps, die das System-Manifest ihm zuweist. Alles Weitere entsteht von hier aus im
//! Userland.
//!
//! Was dieses `init` tut, und warum jeder Schritt eine Aussage traegt:
//!
//! 1. **Die endowte Notification signalisieren.** Der Kernel hat sonst kein Fenster in einen
//!    isolierten Prozess; das Badge ist der Beleg, dass extern gebauter, aus dem Manifest
//!    ausgewaehlter Code tatsaechlich laeuft.
//! 2. **Die restliche Startmenge laden** -- ueber die eigene Loader-Cap, nicht ueber den Kernel.
//!    Das ist der eigentliche Punkt: der Kernel hat danach ein Programm geladen, das seinerseits
//!    Programme laedt. Jede weitere Faehigkeit ist ab hier ein Userland-Programm.
//!    Die eigene Notification wird dabei an Slot 0 der Kind-PD delegiert -- die Kinder melden sich
//!    also ueber denselben Kanal, und der Kernel sieht am akkumulierten Badge, wer gelaufen ist.
//! 3. **Sich beenden.** Kein Dauer-Park: Stack und TCB-Slot gehen zurueck, der Lebenszyklus ist
//!    vollstaendig. (Ein Root-Task eines echten Systems bliebe stattdessen als Dienst stehen; hier
//!    ist das Ende die pruefbarere Aussage.)
//!
//! **Boot-Argument** (`arg`): `(Anzahl der Programme << 32) | eigener Index`. Bewusst das Minimum
//! -- damit laesst sich "alle ausser mir" ausdruecken, ohne eine Indexverabredung mit dem
//! Testskript. Alles Weitere gehoerte hinter eine Capability, nicht in ein Register.

#![no_std]
#![no_main]
#![forbid(unsafe_code)]

/// Charakteristisches Badge ("der Root-Task lief"). Bewusst ein **hohes** Bit: die Badges der
/// Kinder liegen im unteren Wort, und ein akkumuliertes (ODER-verknuepftes) Badge soll beide
/// Aussagen getrennt lesbar lassen statt sie ineinander laufen zu lassen.
pub const ROOT_BADGE: u64 = 1 << 32;

/// A-3.1: die eigene Loader-Cap liess sich loeschen **und** die Autoritaet war danach wirklich weg
/// (ein anschliessendes `SYS_LOAD` wird abgewiesen). Der zweite Teil ist der wichtigere: dass ein
/// Slot leer ist, sagt fuer sich genommen nichts darueber, ob die Faehigkeit erloschen ist.
pub const CDELETE_GONE_BADGE: u64 = 1 << 33;

/// A-3.1: ein Cap, von dem noch Kopien abgeleitet sind, wird **nicht** geloescht -- und bleibt
/// benutzbar. Ein halb entfernter Cap waere schlimmer als gar keiner.
pub const CDELETE_CHILDREN_BADGE: u64 = 1 << 34;

/// Badge, das der Root-Task den Caps seiner Kinder mitgibt. Unteres Wort, damit es sich mit
/// [`ROOT_BADGE`] (hohes Bit) nicht vermischt. Der Kernel-Test liest daran ab, dass ein von
/// **init** geladenes Programm gelaufen ist -- nicht bloss init selbst.
pub const CHILD_BADGE: u64 = 0x4845_4C4F; // "HELO"

/// Cap-Slot der Loader-Cap (Slot-Konvention des Kernel-Glue, `kernel::loader::endow_from_manifest`).
const LOADER_SLOT: u64 = 0;
/// Cap-Slot der endowten Notification. Ihr Badge ist [`ROOT_BADGE`] -- gesetzt vom Kernel beim
/// Endowment, nicht von uns.
const NTFN_SLOT: u64 = 1;
/// Freie Slots fuer eigene, ANDERS GEBADGTE Kopien derselben Notification (A-3.2, `CCOPY`).
/// Ohne die waeren alle Signale dieses Programms ununterscheidbar: das Badge steckt in der Cap.
const NTFN_CHILDREN_SLOT: u64 = 3;
const NTFN_GONE_SLOT: u64 = 4;
/// Rechte-Bitmaske fuer die Kopien (R+W+X; wird ohnehin mit den Rechten des Originals geschnitten).
const RWX: u64 = 7;

libcaprock::entry!(run);

fn run(arg: usize) -> ! {
    let index = (arg & 0xffff_ffff) as u64;
    let count = (arg >> 32) as u64;

    // 1. "Ich laufe."
    libcaprock::signal(NTFN_SLOT, ROOT_BADGE);

    // 2. Die uebrige Startmenge laden. Fehler werden NICHT stillschweigend uebergangen: was nicht
    //    laedt, meldet sich als gesetztes Bit im Badge -- sonst saehe ein leerer Lauf aus wie ein
    //    erfolgreicher.
    let mut failed = 0u64;
    let mut i = 0u64;
    while i < count {
        if i != index {
            // Die eigene Notification (Slot 1) an Slot 0 der Kind-PD delegieren.
            // Die eigene Notification delegieren, aber mit EIGENEM Badge fuer das Kind -- sonst
            // signalisierten alle Kinder unter dem Badge des Root-Tasks und waeren nicht
            // auseinanderzuhalten (das Badge steckt in der Cap, nicht in der Nachricht).
            if libcaprock::load(LOADER_SLOT, i, NTFN_SLOT, CHILD_BADGE) != libcaprock::result::OK {
                failed |= 1 << (i.min(30) + 1);
            }
        }
        i += 1;
    }
    if failed != 0 {
        libcaprock::signal(NTFN_SLOT, failed);
    }

    // 3. Zwei eigene, unterschiedlich gebadgte Kopien der Notification anlegen (A-3.2, `CCOPY`).
    //    Ohne sie koennte dieses Programm dem Kernel nur EINE Tatsache melden -- naemlich die, die
    //    im Badge seiner endowten Cap steht. Mit ihnen wird jedes Teilergebnis unterscheidbar.
    let ok_children = libcaprock::ccopy(NTFN_SLOT, NTFN_CHILDREN_SLOT, RWX, CDELETE_CHILDREN_BADGE);
    let ok_gone = libcaprock::ccopy(NTFN_SLOT, NTFN_GONE_SLOT, RWX, CDELETE_GONE_BADGE);

    // 4. A-3.1 aus Ring 3 pruefen -- hier und nicht im Kernel, weil genau der Weg geprueft werden
    //    soll, den ein echter Dienst nimmt: ueber die ABI, aus einer isolierten PD heraus.
    //
    //    4a. Die eigene Notification (Slot 1) hat jetzt abgeleitete Kopien. Sie zu loeschen MUSS
    //        scheitern, und sie MUSS danach weiter funktionieren -- das Signal ueber die KOPIE ist
    //        der Beweis fuer beides in einem (die Kopie zeigt auf dasselbe Objekt).
    if ok_children == libcaprock::result::OK
        && libcaprock::cdelete(NTFN_SLOT) == libcaprock::result::ERR_HASCHILDREN
    {
        libcaprock::signal(NTFN_CHILDREN_SLOT, 0);
    }
    //    4b. Die Loader-Cap dagegen hat keine Ableitungen -- sie laesst sich loeschen. Und danach
    //        ist die Faehigkeit weg: ein weiteres `SYS_LOAD` wird abgewiesen. Erst das zusammen ist
    //        die Aussage; ein geraeumter Slot allein waere Buchhaltung.
    if ok_gone == libcaprock::result::OK
        && libcaprock::cdelete(LOADER_SLOT) == libcaprock::result::OK
        && libcaprock::load(LOADER_SLOT, 0, u64::MAX, 0) != libcaprock::result::OK
    {
        libcaprock::signal(NTFN_GONE_SLOT, 0);
    }

    // 5. Fertig.
    libcaprock::exit();
}
