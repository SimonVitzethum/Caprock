//! **Zustandsübergabe über einen Hot-Reload hinweg** (A-4.3).
//!
//! # Die Festlegung
//!
//! A-4.3 liess zwei Wege offen: eine Region, die den Austausch überlebt (dann ist ihr Format eine
//! ABI und muss versioniert werden), oder ein ausdrückliches Übergabeprotokoll zwischen alter und
//! neuer Instanz. Gewählt ist die **Region mit versioniertem Kopf**, aus zwei Gründen:
//!
//! 1. Ein Übergabeprotokoll setzt voraus, dass die alte Instanz zum Zeitpunkt der Übergabe noch
//!    *läuft und mitspielt*. Genau das schliesst der Rest von A-4 aus: A-4.2 legt den Endpoint
//!    still, A-4.1 tauscht unter einem Lock. Eine kooperierende Übergabe müsste die alte Instanz
//!    nach dem Lösen am Leben halten — das ist die Nebenläufigkeit, die A-4.1 gerade beseitigt hat.
//! 2. Der häufigste Grund, eine Komponente auszutauschen, ist, dass sie **nicht mehr mitspielt**.
//!    Ein Protokoll, das nur mit einer gesunden alten Instanz funktioniert, fehlt genau dann.
//!
//! # Warum ein Kopf, und nicht bloss die Nutzlast
//!
//! Ohne Kopf ist „der Zustand überlebt den Tausch" eine Behauptung über zwei Programme, die
//! niemand prüft: die alte Fassung schreibt ein `u64` an Offset 0, die neue liest an Offset 0 —
//! und wenn die neue dort inzwischen etwas anderes versteht, liest sie **stillschweigend Unsinn**
//! weiter. Das ist schlimmer als der Datenverlust, den A-4.3 verhindern soll: Datenverlust fällt
//! auf, ein fehlinterpretierter Zustand nicht.
//!
//! Deshalb trägt die Region einen Kopf, und [`attach`] weist ab, statt zu raten:
//!
//! * **`state_version`** — die Layout-Version der Nutzlast. Sie ist bewusst **nicht** dieselbe wie
//!   die `iface_version` aus A-4.4: die eine beschreibt, was über den Endpoint geht, die andere,
//!   was im Speicher liegt. Eine Fassung kann ihr Nachrichtenformat behalten und ihr
//!   Zustandslayout ändern — dann muss genau dieser Fall auffallen.
//! * **`program_id`** — Zustand gehört einem Programm. Eine fremde `program_id` heisst: hier liegt
//!   der Zustand von jemand anderem, nicht „vermutlich passend".
//! * **`generation`** — zählt die Übernahmen. Eine frisch angelegte Region steht auf 0. Damit ist
//!   „der Zustand wurde geerbt" eine **Messung** und keine Annahme; ohne den Zähler sieht ein
//!   stillschweigend neu angelegter Zustand genauso aus wie ein geerbter, der zufällig dieselben
//!   Werte trägt.
//!
//! Jeder Fehlerfall hat einen eigenen Namen ([`StateError`]). Ein gemeinsames `false` würde „hier
//! liegt gar kein Zustand" mit „hier liegt der falsche" vermengen — für den Aufrufer sind das
//! verschiedene Lagen: das erste ist ein Kaltstart, das zweite ein Abbruchgrund.

use crate::RegionView;

/// Magie im Kopf der Zustandsregion. Ein Wert, der nicht zufällig entsteht: eine genullte oder
/// wiederverwendete Region fällt damit als „kein Zustand" auf statt als „Version 0".
pub const STATE_MAGIC: u64 = 0x5354_4154_454C_414B; // "STATELAK"

/// Länge des Kopfes in Bytes. Die Nutzlast beginnt dahinter (8-Byte-ausgerichtet).
pub const HEADER_LEN: usize = 32;

const OFF_MAGIC: usize = 0;
const OFF_PROGRAM: usize = 8;
const OFF_VERSION: usize = 12;
const OFF_PAYLOAD_LEN: usize = 16;
const OFF_GENERATION: usize = 20;
// 24..32: reserviert (Nullen). Bewusst freigehalten, damit ein späteres Feld den Kopf nicht
// verlängert und damit jedes bestehende Nutzlast-Offset verschiebt.

/// Warum eine Übernahme nicht stattfindet. Jeder Ausgang benannt — siehe Modul-Doku.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StateError {
    /// Kein Kopf (Magie fehlt): hier liegt **kein** übergebener Zustand. Kaltstart, kein Fehler.
    NoState,
    /// Der Zustand gehört einem anderen Programm.
    WrongProgram { found: u32 },
    /// Das Layout der Nutzlast ist ein anderes als das, was der Aufrufer versteht.
    VersionMismatch { found: u32 },
    /// Die Region ist kürzer als Kopf + angeforderte Nutzlast.
    TooSmall,
    /// Der Kopf behauptet mehr Nutzlast, als die Region trägt — der Kopf lügt oder ist beschädigt.
    Corrupt { payload_len: u32 },
    /// Der Zähler der Übernahmen ist am Anschlag. Weiterzählen hiesse, ihn zurückzusetzen, und
    /// damit sähe eine spätere Übernahme wie eine frische Region aus.
    GenerationExhausted,
}

/// Ergebnis einer geglückten Übernahme.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Handover {
    /// Wievielte Übernahme das ist. `1` = die erste nach [`init`].
    pub generation: u32,
    /// Länge der Nutzlast in Bytes (aus dem Kopf, gegen die Regionsgrösse geprüft).
    pub payload_len: u32,
}

/// Eine Region als Zustandsregion **anlegen**: Kopf schreiben, Nutzlast nullen, `generation = 0`.
///
/// Nullt ausdrücklich: der Allokator gibt nicht garantiert genullten Speicher, und ein Zustand,
/// der aus Resten einer fremden Allokation besteht, ist von einem echten nicht zu unterscheiden.
pub fn init(
    view: &mut RegionView<'_>,
    program_id: u32,
    state_version: u32,
    payload_len: u32,
) -> Result<(), StateError> {
    let need = HEADER_LEN
        .checked_add(payload_len as usize)
        .ok_or(StateError::TooSmall)?;
    if view.len() < need {
        return Err(StateError::TooSmall);
    }
    // Erst nullen, dann den Kopf schreiben: fällt etwas dazwischen aus, steht keine gültige Magie
    // über halbem Müll.
    let mut payload = view.subview(HEADER_LEN, payload_len as usize).ok_or(StateError::TooSmall)?;
    payload.fill(0);
    view.set::<u32>(OFF_PROGRAM, program_id);
    view.set::<u32>(OFF_VERSION, state_version);
    view.set::<u32>(OFF_PAYLOAD_LEN, payload_len);
    view.set::<u32>(OFF_GENERATION, 0);
    view.set::<u64>(24, 0); // reservierter Bereich definiert genullt
    view.set::<u64>(OFF_MAGIC, STATE_MAGIC); // zuletzt: macht den Kopf gültig
    Ok(())
}

/// Den Zustand **übernehmen**: Kopf prüfen und, nur wenn er passt, den Übernahmezähler erhöhen.
///
/// Der Zähler wird hier und nur hier erhöht — eine geglückte Übernahme ist damit im Speicher
/// sichtbar und nicht bloss im Kontrollfluss des Aufrufers.
pub fn attach(
    view: &mut RegionView<'_>,
    program_id: u32,
    state_version: u32,
) -> Result<Handover, StateError> {
    if view.len() < HEADER_LEN {
        return Err(StateError::TooSmall);
    }
    if view.get::<u64>(OFF_MAGIC) != Some(STATE_MAGIC) {
        return Err(StateError::NoState);
    }
    let found_program = view.get::<u32>(OFF_PROGRAM).ok_or(StateError::TooSmall)?;
    if found_program != program_id {
        return Err(StateError::WrongProgram {
            found: found_program,
        });
    }
    let found_version = view.get::<u32>(OFF_VERSION).ok_or(StateError::TooSmall)?;
    if found_version != state_version {
        return Err(StateError::VersionMismatch {
            found: found_version,
        });
    }
    let payload_len = view.get::<u32>(OFF_PAYLOAD_LEN).ok_or(StateError::TooSmall)?;
    // Der Kopf ist eine Behauptung über die Region, keine Tatsache: er kann aus einer grösseren
    // Region stammen. Ungeprüft übernommen wäre jeder spätere Nutzlastzugriff bloss so weit
    // begrenzt, wie der Kopf es zufällig zulässt.
    if HEADER_LEN.saturating_add(payload_len as usize) > view.len() {
        return Err(StateError::Corrupt { payload_len });
    }
    let gen = view.get::<u32>(OFF_GENERATION).ok_or(StateError::TooSmall)?;
    let next = gen.checked_add(1).ok_or(StateError::GenerationExhausted)?;
    view.set::<u32>(OFF_GENERATION, next);
    Ok(Handover {
        generation: next,
        payload_len,
    })
}

/// Die Nutzlast einer **bereits übernommenen** Region als eigene Sicht leihen. Der Kopf liegt
/// ausserhalb — ein Schreibfehler in der Nutzlast kann die Versionsangabe nicht überschreiben.
pub fn payload<'v>(view: &'v mut RegionView<'_>) -> Option<RegionView<'v>> {
    let payload_len = view.get::<u32>(OFF_PAYLOAD_LEN)? as usize;
    view.subview(HEADER_LEN, payload_len)
}

/// Die Übernahmezahl lesen, ohne sie zu erhöhen (Telemetrie/Selbsttest).
pub fn generation(view: &RegionView<'_>) -> Option<u32> {
    if view.get::<u64>(OFF_MAGIC) != Some(STATE_MAGIC) {
        return None;
    }
    view.get::<u32>(OFF_GENERATION)
}

#[cfg(kani)]
mod kani_proofs {
    use super::*;

    /// **BEWEIS:** eine frisch angelegte Region wird von [`attach`] genau dann übernommen, wenn
    /// `program_id` **und** `state_version` übereinstimmen — und liefert dann `generation == 1`.
    /// Bei Abweichung gibt es nie ein `Ok`, also nie einen stillschweigend fehlinterpretierten
    /// Zustand.
    #[kani::proof]
    fn attach_only_on_exact_match() {
        const N: usize = 64;
        let mut buf = [0u8; N];
        let base = buf.as_mut_ptr() as u64;
        let mut rv = RegionView {
            base,
            len: N,
            _p: core::marker::PhantomData,
        };
        let prog: u32 = kani::any();
        let ver: u32 = kani::any();
        assert!(init(&mut rv, prog, ver, 8).is_ok());

        let want_prog: u32 = kani::any();
        let want_ver: u32 = kani::any();
        match attach(&mut rv, want_prog, want_ver) {
            Ok(h) => {
                assert!(want_prog == prog && want_ver == ver);
                assert!(h.generation == 1);
                assert!(h.payload_len == 8);
            }
            Err(_) => assert!(want_prog != prog || want_ver != ver),
        }
        let _ = buf;
    }

    /// **BEWEIS:** ohne gültige Magie gibt es kein `Ok` — eine genullte oder fremd beschriebene
    /// Region wird nie als Zustand gelesen (insbesondere nicht als „Version 0").
    #[kani::proof]
    fn no_magic_never_attaches() {
        const N: usize = 64;
        let mut buf = [0u8; N];
        let base = buf.as_mut_ptr() as u64;
        let mut rv = RegionView {
            base,
            len: N,
            _p: core::marker::PhantomData,
        };
        let magic: u64 = kani::any();
        kani::assume(magic != STATE_MAGIC);
        rv.set::<u64>(OFF_MAGIC, magic);
        assert!(attach(&mut rv, kani::any(), kani::any()).is_err());
        let _ = buf;
    }
}
