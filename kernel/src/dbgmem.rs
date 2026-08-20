//! **Z6b: die Speicher-Sonde des Debuggers — architekturneutral, und das ist der Punkt.**
//!
//! ## Warum diese Datei existiert
//!
//! `DEBUG_READ_MEM` laeuft ueber `hal::mmu::vspace_resolve`, und das ist **die einzige Stelle in
//! v1, an der sich x86_64 und aarch64 wirklich unterscheiden**: vierstufiges Paging mit `P`/`PS`
//! gegen Deskriptorbits `0b01`/`0b11`, andere Adressmasken, andere Blockgroessen. Der ganze uebrige
//! Debugger ist arch-neutral (Scheduler, Caps, ABI) — dort traegt ein gruener x86-Lauf die andere
//! Seite mit. **Hier nicht.**
//!
//! Deshalb liegt die Sonde hier und nicht in einem der beiden Hochlaufwege, und deshalb fahren sie
//! **beide**. Das ist woertlich die Lehre aus `kernel/src/dmatests.rs`: ein Abnahmekriterium, das
//! „die vorhandenen Tests hoeren auf zu skippen" lautet, setzt voraus, dass es sie gibt — auf x86
//! gab es sie nicht, sie skippten nicht, sie **fehlten**.
//!
//! ## Was gemessen wird
//!
//! Zwei isolierte PDs mit je einem lebenden Thread:
//!
//! * **Ziel** — eine Seite mit einem Magic-Wort, in seine VSpace gemappt.
//! * **Debugger** — eine Pufferseite, und die Debug-Caps ueber das Ziel.
//!
//! Der Debugger ruft `debug_read_mem`; der Kernel prueft danach den Puffer. Die Aussage ist nicht
//! „ein Syscall gab OK zurueck", sondern **„die Bytes des Ziels stehen im Puffer des Debuggers"**.
//!
//! ## Und warum das kein Schreiber ist, der sich selbst bestaetigt
//!
//! Das Magic-Wort ist eine **Konstante im Quelltext**; der Kernel legt es hin, aber er liest es
//! nicht von dort zurueck. Gelesen wird ueber die Seitentabellen des **Ziels**, geschrieben in die
//! des **Debuggers** — zwei Aufloesungen, die der Kernel beim Hinlegen nicht benutzt hat. Die
//! Gegenprobe, die das absichert, ist `M-DBGMEM` in `tools/dbg-negativ.sh`: laeuft der Lesepfad
//! ueber die Tabellen des AUFRUFERS statt des Ziels, faellt genau diese Zeile.

use crate::system;
use caprock_hal::println;

/// Das Wort, das das Ziel traegt. Zwei verschiedene, damit ein Puffer, der zufaellig schon den
/// richtigen Wert enthielt, nicht als Erfolg durchgeht.
const MAGIE_A: u64 = 0xD3B6_C0DE_1234_5678;
const MAGIE_B: u64 = 0x0BAD_F00D_5EAD_DA7A;

/// Womit der Puffer vorbelegt wird: **nicht** null. Eine Null waere von „nichts geschrieben"
/// nicht zu unterscheiden — dieselbe Falle wie ein einseitiger Schwellenvergleich, der gruen wird,
/// sobald die Messung ausfaellt.
const PUFFER_VORBELEGUNG: u64 = 0xAAAA_AAAA_AAAA_AAAA;

/// Urteil der Sonde. `None` = nicht gefahren (dann sagt der Bericht **SKIP**, nicht PASS).
static ERGEBNIS: caprock_sync::SpinLock<Option<Ergebnis>> = caprock_sync::SpinLock::new(None);

#[derive(Clone, Copy, Default)]
struct Ergebnis {
    aufgebaut: bool,
    /// Hat der Aufruf ueberhaupt Bytes gemeldet?
    gelesen: u64,
    /// Steht Wort A im Puffer?
    wort_a: bool,
    /// Und Wort B, an einem anderen Versatz? (Ein Puffer, der nur das erste Wort traegt, hat einen
    /// Laengenfehler, und der saehe mit einem einzigen Wort wie ein Erfolg aus.)
    wort_b: bool,
    /// Der Puffer war **vorher** nachweislich anders belegt — sonst belegt „steht drin" nichts.
    vorher_anders: bool,
    /// Eine Adresse, die das Ziel NICHT gemappt hat, liefert 0 Bytes statt geratener.
    luecke_abgewiesen: bool,
    /// **Der ZWEITE Zweig von `vspace_resolve`:** das User-Fenster (`ISO_USER_VA`), gemappt als
    /// 2-MiB-BLOCK statt ueber eine Seitentabelle. Auf x86 `PS`, auf aarch64 `BLOCK_DESC` — und
    /// die Rechtepruefung sitzt dort an einer anderen Stelle als beim Blatt.
    fenster_gelesen: bool,
    /// Und die Gegenrichtung im selben Zweig: eine Fenster-Adresse hinter der Region loest nicht
    /// auf. Ohne sie belegte „gelesen" nur, dass IRGENDETWAS zurueckkam.
    fenster_luecke_abgewiesen: bool,
    /// Ein Aufrufer OHNE Leserecht wird abgewiesen (nur `DebugControl`-Wurzelrecht reicht nicht).
    ohne_recht_abgewiesen: bool,
}

/// **Die Sonde.** Von beiden Hochlaufwegen gerufen, nach dem Aufbau der uebrigen PDs.
///
/// Die Sonde bringt ihren EL0-Einsprung **selbst** mit ([`traeger`]) und haengt damit an keiner
/// Sonde eines der beiden Zweige. Sie braucht lebende Threads — eine PD ohne Thread hat keine ASID
/// und damit keine Seitentabellen —, aber es ist ihr gleich, was sie tun.
pub fn messen(prio: u8) {
    let entry = traeger as *const () as usize;
    let mut e = Ergebnis::default();

    // --- Aufbau: zwei isolierte PDs, je ein Thread ---------------------------------------------
    let Some(ziel_seite) = system::alloc(4096, 4096).map(|c| c.region().base) else {
        return;
    };
    let Some(puffer_seite) = system::alloc(4096, 4096).map(|c| c.region().base) else {
        return;
    };
    // Das Ziel traegt zwei Woerter an **verschiedenen** Versaetzen.
    poke(ziel_seite, MAGIE_A);
    poke(ziel_seite + 64, MAGIE_B);
    poke(puffer_seite, PUFFER_VORBELEGUNG);
    poke(puffer_seite + 64, PUFFER_VORBELEGUNG);

    let (Some(ziel_pd), Some(dbg_pd)) = (system::create_pd(), system::create_pd()) else {
        return;
    };
    let Some((zp, ziel_region)) = system::spawn_isolated_parked(entry, 0, prio) else {
        return;
    };
    system::bind_pd_parked(&zp, ziel_pd);
    system::map_into_parked(&zp, ziel_seite, 4096, 0); // RO -- gelesen wird nur
    let Some(_zt) = system::admit(zp) else { return };

    let Some((dp, _)) = system::spawn_isolated_parked(entry, 0, prio) else {
        return;
    };
    system::bind_pd_parked(&dp, dbg_pd);
    system::map_into_parked(&dp, puffer_seite, 4096, 1); // RW -- hier landet das Ergebnis
    let Some(_dt) = system::admit(dp) else { return };

    // --- Autoritaet: praegen, ableiten ---------------------------------------------------------
    let Some(wurzel) = system::mint_debuggable(ziel_pd) else {
        return;
    };
    let Some(lese_slot) = system::debug_attach_read_to(0, wurzel, dbg_pd) else {
        return;
    };
    e.aufgebaut = true;

    // --- Die Messung ---------------------------------------------------------------------------
    //
    // **Vorher pruefen, dass der Puffer anders aussieht.** Ohne diese Zeile belegt „das Magic-Wort
    // steht im Puffer" nichts: es koennte immer dort gestanden haben.
    e.vorher_anders = peek(puffer_seite) != MAGIE_A && peek(puffer_seite + 64) != MAGIE_B;

    e.gelesen = system::debug_read_mem(dbg_pd, lese_slot, ziel_seite, 128, puffer_seite)
        .unwrap_or(0);
    e.wort_a = peek(puffer_seite) == MAGIE_A;
    e.wort_b = peek(puffer_seite + 64) == MAGIE_B;

    // Eine Adresse, die das Ziel nicht gemappt hat: **0 Bytes**, nicht geratene. Ein Debugger soll
    // eine Luecke SEHEN.
    e.luecke_abgewiesen =
        system::debug_read_mem(dbg_pd, lese_slot, ziel_seite + (16 << 20), 8, puffer_seite)
            == Ok(0);

    // Und die Wurzel selbst darf **nicht** lesen -- sie ist das Recht abzuleiten, sonst nichts.
    e.ohne_recht_abgewiesen =
        system::debug_read_mem(0, wurzel, ziel_seite, 8, puffer_seite).is_err();

    // --- Der ZWEITE Zweig: das User-Fenster, als 2-MiB-Block ------------------------------------
    //
    // Die private Region der isolierten PD liegt **nicht** identisch, sondern unter
    // `ISO_USER_VA + SLOT_DATA * 2 MiB` -- und sie ist ein Blockdeskriptor. In `vspace_resolve`
    // ist das der andere Zweig; wer nur den Seitenzweig misst, hat die Haelfte ungeprueft.
    //
    // Das Magic-Wort wird ueber die **Physadresse** hingelegt (der Kernel erreicht sie identisch)
    // und ueber die **Fenster-VA** gelesen. Zwei verschiedene Wege auf dieselben Bytes -- genau
    // die Trennung, die belegt, dass die Aufloesung wirklich laeuft und nicht identisch raet.
    poke(ziel_region, MAGIE_A);
    poke(ziel_region + 64, MAGIE_B);
    poke(puffer_seite, PUFFER_VORBELEGUNG);
    poke(puffer_seite + 64, PUFFER_VORBELEGUNG);
    let fenster_va = system::iso_user_data_va();
    let n = system::debug_read_mem(dbg_pd, lese_slot, fenster_va, 128, puffer_seite).unwrap_or(0);
    e.fenster_gelesen =
        n == 128 && peek(puffer_seite) == MAGIE_A && peek(puffer_seite + 64) == MAGIE_B;
    // Hinter der 2-MiB-Region ist nichts gemappt -- und „nichts" muss 0 Bytes heissen, nicht
    // geratene. Ein Blockdeskriptor deckt genau 2 MiB; die Adresse dahinter faellt in einen
    // Nachbarplatz des Fensters, den diese PD nie belegt hat.
    e.fenster_luecke_abgewiesen =
        system::debug_read_mem(dbg_pd, lese_slot, fenster_va + (4 << 20), 8, puffer_seite) == Ok(0);

    *ERGEBNIS.lock() = Some(e);
}

/// **Der Traeger-Thread: ein EL0-Rundlauf, der nichts tut ausser zu leben.**
///
/// `#[link_section = ".user_text"]` ist hier keine Formalie. Ein Ring-3-Einsprung, der in `.text`
/// landet, ist aus Ring 3 nicht ausfuehrbar und faultet **an seiner eigenen Einsprungadresse** —
/// im Protokoll steht dann ein Fault und eine Pruefzeile aus lauter Nullen, was wie ein kaputter
/// Mechanismus aussieht und eine fehlende Zeile ist. Dieses Projekt hat das bezahlt (die
/// Kapazitaetskurve, die die Geschwindigkeit des Sterbens mass).
///
/// Kein Helferaufruf im Rumpf, aus demselben Grund: der Helfer laege in `.text`, und der Fehler
/// wanderte nur eine Ebene tiefer.
#[link_section = ".user_text"]
extern "C" fn traeger(_arg: usize) -> ! {
    loop {
        core::hint::spin_loop();
    }
}

fn poke(pa: u64, v: u64) {
    // SAFETY: frisch allozierte, identisch abgebildete RAM-Seite, exklusiv in dieser Hand.
    unsafe { core::ptr::write_volatile(pa as *mut u64, v) };
}
fn peek(pa: u64) -> u64 {
    // SAFETY: wie `poke`.
    unsafe { core::ptr::read_volatile(pa as *const u64) }
}

/// Das Urteil. `false` heisst rot; **nicht gefahren ist NICHT bestanden** (s. `bericht`).
pub fn urteil() -> bool {
    match *ERGEBNIS.lock() {
        Some(e) => {
            e.aufgebaut
                && e.gelesen == 128
                && e.wort_a
                && e.wort_b
                && e.vorher_anders
                && e.luecke_abgewiesen
                && e.ohne_recht_abgewiesen
                && e.fenster_gelesen
                && e.fenster_luecke_abgewiesen
        }
        None => false,
    }
}

/// Die Berichtszeile. Von beiden Hochlaufwegen gedruckt.
pub fn bericht() {
    let Some(e) = *ERGEBNIS.lock() else {
        println!(
            "dbgmem  : SKIP -- die Sonde ist nicht gelaufen (Aufbau fehlgeschlagen). **Das ist \
             kein Bestanden**: ein leerer Lauf ist kein Testergebnis"
        );
        return;
    };
    println!(
        "dbgmem  : GiB-0-Zweig (Seitentabelle): aufgebaut={} gelesen={}/128 B wort_a={} \
         wort_b={} vorher-anders={} luecke-abgewiesen={} ohne-Leserecht-abgewiesen={}",
        e.aufgebaut, e.gelesen, e.wort_a, e.wort_b, e.vorher_anders, e.luecke_abgewiesen,
        e.ohne_recht_abgewiesen
    );
    println!(
        "dbgmem  : Fenster-Zweig (2-MiB-BLOCK, ISO_USER_VA): gelesen={} luecke-abgewiesen={} -- \
         **der andere Zweig von vspace_resolve**: auf x86 `PS`, auf aarch64 `BLOCK_DESC`, und die \
         Rechtepruefung sitzt dort an einer anderen Stelle als beim Blatt. Das Magic-Wort wird \
         ueber die PHYSadresse hingelegt und ueber die FENSTER-VA gelesen -- zwei verschiedene \
         Wege auf dieselben Bytes, damit 'aufgeloest' nicht 'identisch geraten' heissen kann",
        e.fenster_gelesen, e.fenster_luecke_abgewiesen
    );
    println!(
        "dbgmem  : {} (Z6b: DEBUG_READ_MEM laeuft ueber die Seitentabellen des ZIELS, nicht ueber \
         die des Aufrufers -- und das ist die EINZIGE Stelle in v1, an der sich x86_64 und \
         aarch64 wirklich unterscheiden. Deshalb faehrt diese Sonde auf BEIDEN Zweigen: der Rest \
         von v1 ist arch-neutral, dort traegt ein gruener x86-Lauf die andere Seite mit, hier \
         nicht. Zwei Woerter an verschiedenen Versaetzen, weil ein Laengenfehler mit einem \
         einzigen Wort wie ein Erfolg aussaehe. GEMESSEN ist der Zweig fuer GiB 0; das \
         User-Fenster wird seit dem 2026-08-20 MITGEFAHREN -- beide Zweige, beide Architekturen)",
        if urteil() { "ALL PASS" } else { "FAILURES" }
    );
}
