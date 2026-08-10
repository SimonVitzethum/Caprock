//! **Grosse, zusammenhängende DMA mit Gerätesicht** (Z26, Vorbedingung 2) — die Kernel-Seite.
//!
//! ## Was hier neu ist
//!
//! Vergeben wurde DMA bisher in einer Grösse: `DRIVER_DMA_BYTES = 16 KiB`, fest verdrahtet in
//! `assign_driver_device`. Ein Treiber mit Ringpuffern braucht mehr — eine NVMe-Queue, die
//! RX-/TX-Ringe einer Netzkarte, die Transfer-Ringe eines USB-Controllers, ein GPU-Kommandopuffer.
//! Das ist **kein GPU-Sonderfall**: die Geräteart bestimmt hier nur die Zahl, nicht den Mechanismus.
//!
//! Zwei Dinge kommen dazu:
//!
//! 1. **Eine Länge, die der Aufrufer nennt** — mehrere MiB am Stück, aus dem regulären Allokator.
//! 2. **Eine Absage mit NAMEN** ([`caprock_dma::gross::GrossDmaFehler`]) statt `None`.
//!
//! ## Die Obergrenze, als Zahl
//!
//! Eine DMA-Region muss vollständig in `[hal::mmu::USER_RAM_MIN, hal::mmu::GIB1_END)` liegen —
//! auf x86 also in `[16 MiB, 1 GiB)`, **1008 MiB**. Das ist keine Vorliebe: `vspace_map_page_at`
//! weist jede VA `>= GIB1_END` ab, eine Region darüber wäre für die PD nicht abbildbar
//! (`alloc_dma_region` gibt sie deshalb an den Allokator zurück, statt sie auszuliefern).
//!
//! Eine Grenze, die niemand kennt, ist keine — deshalb steht sie als [`zone`] im Code und wird
//! im Fehlerfall **mitgeliefert** (`GroesserAlsZone { zone }`).
//!
//! Die IOVA-Seite ist nicht die bindende: das Fenster je Kontext ist rund `(2^39 − Fensterbasis)/4`
//! und damit auf dieser Plattform zweistellig in GiB — die 1008 MiB der physischen Zone schlagen
//! immer zuerst zu.
//!
//! ## Warum die grösste noch mögliche Zahl GEMESSEN und nicht nachgerechnet wird
//!
//! Der naheliegende Weg wäre, über die Freiliste zu laufen und den grössten Block auszurechnen.
//! Das wäre eine **zweite Wirklichkeit** neben `PhysAllocator::alloc_in` — genau die
//! `iova_window_clear_of_msi`-Falle („Zuteiler und Prüfer brauchen EINE Quelle"): Best-Fit,
//! Zonenbeschneidung und die Fragment-Schranke müssten nachgebildet werden, und die Kopie ginge
//! beim nächsten Umbau auseinander.
//!
//! Stattdessen wird der Allokator **gefragt** ([`groesster_block`]): eine binäre Suche mit echten
//! `alloc`/`free`-Paaren. Sie läuft nur im Fehlerfall (rund 18 Versuche für den ganzen Bereich)
//! und liefert genau die Zahl, die der Aufrufer braucht — die, die durchgegangen **wäre**.
//!
//! ## Die zwei Achsen
//!
//! [`GrossDmaGrant`] trägt `Pa` **und** `Iova` in getrennten Typen und hat keinen öffentlichen
//! Konstruktor aus rohen Zahlen. Vor der Auslieferung läuft
//! [`caprock_dma::gross::pruefe_achsen`]: eine IOVA gleich der PA ist ein **Sicherheitsbefund**
//! und wird abgewiesen, nicht ausgeliefert. Ohne diese Zeile liefe ein Treiber richtig, solange
//! die beiden zufällig übereinstimmen — dieselbe Form wie `DmaRegion::identity`, das genau
//! deshalb absichtlich entfernt wurde.

use crate::addr::{Iova, Pa};
use crate::system;
use caprock_cap::{CapPtr, DmaCoherence, DmaDir};
use caprock_dma::gross::{klassifiziere, pruefe_achsen, GrossDmaFehler, Lage, Zone, SEITE};
use caprock_hal as hal;
use caprock_mem::{Rights, MAX_FRAGMENTS};

/// **Eine vergebene grosse DMA-Region: beide Achsen, unzertrennlich.**
///
/// Kein öffentlicher Konstruktor aus rohen Adressen — derselbe Gedanke wie bei `addr::Va` und bei
/// `caprock_dma::DmaBuf`. Ein Grant entsteht **nur** aus [`belege`], und damit ist „diese beiden
/// Adressen bezeichnen denselben Speicher, und sie sind verschieden" eine Eigenschaft des Typs
/// statt der Sorgfalt des Aufrufers.
///
/// `#[must_use]`: ein weggeworfener Grant ist eine angehängte Übersetzung und eine Region, die
/// niemand mehr freigibt.
// **Warum hier `allow(dead_code)` steht — und was das GENAU heisst.**
//
// Der Weg bis zur Gerätesicht (`belege`/`gib_frei`/`GrossDmaGrant`/`pruefe_ende_zu_ende`) hat in
// diesem Stand **keinen Aufrufer**: er müsste hinter `dma_enforcer_init()` gerufen werden, und
// das steht in `kernel/src/arch/x86_64/bringup.rs` — eine Datei, die dieser Durchgang
// ausdrücklich nicht anfasst (ein zweiter Agent arbeitet parallel daran). Die genaue Einfügezeile
// steht im Abschlussbericht.
//
// Das ist ausdrücklich **kein** „wird schon jemand brauchen": ein Test, der nirgends läuft, ist
// kein Test, und ein Pfad, den niemand ruft, ist keine Fähigkeit. Bis die Zeile eingefügt ist,
// gilt für diese Hälfte: **gebaut und host-geprüft, nie ausgeführt.** Die Zuteilungshälfte
// (`zone`/`groesster_block`/`pruefe`) hängt dagegen in `crate::selftest::run()` und braucht keine
// IOMMU.
#[allow(dead_code)]
#[must_use = "ein verworfener Grant laesst eine Uebersetzung und eine Region stehen"]
pub struct GrossDmaGrant {
    pa: Pa,
    iova: Iova,
    len: u64,
    cap: CapPtr,
    rid: u32,
    handle: system::DmaHandle,
}

#[allow(dead_code)]
impl GrossDmaGrant {
    /// Die **physische** Adresse (CPU-Sicht). Cache-Wartung und Allokator-Buchhaltung laufen
    /// darüber.
    pub fn pa(&self) -> Pa {
        self.pa
    }
    /// Die **Gerätesicht**. Das ist die Zahl, die in einen Deskriptor gehört — und nie die
    /// [`Self::pa`].
    pub fn iova(&self) -> Iova {
        self.iova
    }
    pub fn len(&self) -> u64 {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    /// Die DMA-Cap, die an eine Treiber-PD gehen kann.
    pub fn cap(&self) -> CapPtr {
        self.cap
    }
}

/// Die Zone, aus der DMA-Regionen kommen dürfen. **Eine Quelle** — dieselben Konstanten, die
/// `system::gib0_zone` benutzt.
pub fn zone() -> Zone {
    Zone {
        lo: hal::mmu::USER_RAM_MIN,
        hi: hal::mmu::GIB1_END,
    }
}

/// **Den grössten zusammenhängenden Block messen, den die Zone jetzt hergibt.**
///
/// Binäre Suche mit echten `alloc`/`free`-Paaren, also über **denselben** Weg, den `belege` geht.
/// Das ist der ganze Punkt: eine Nachrechnung über die Freiliste wäre eine zweite Wirklichkeit,
/// und die zweite Wirklichkeit hat in diesem Projekt schon einmal einen Prüfer grün gehalten, der
/// den Sperrbereich enthielt.
///
/// Kosten: `log2(zone / SEITE)` Versuche, auf x86 also **18**. Läuft nur im Fehlerfall.
///
/// **Was diese Zahl NICHT ist:** eine Zusage. Zwischen Messung und nächster Anforderung kann ein
/// anderer Kern belegen. Sie ist eine Diagnose („so viel wäre gerade gegangen"), und sie wird
/// auch so benannt.
pub fn groesster_block() -> u64 {
    caprock_dma::gross::groesster_block_suche(zone().groesse(), |len| {
        match system::alloc_dma_region(len) {
            Some(r) => {
                // **Sofort zurückgeben.** Eine Sonde, die ihren Speicher behält, verfälscht genau
                // die Grösse, die sie misst — und die nächste Iteration misst dann sich selbst.
                system::free_unattached_dma_region(r.base, r.len);
                true
            }
            None => false,
        }
    })
}

/// Die gemessene Lage des Allokators (für die Klassifikation).
#[allow(dead_code)]
fn lage() -> Lage {
    Lage {
        groesster_block: groesster_block(),
        fragmente: system::fragments(),
        kapazitaet: MAX_FRAGMENTS,
    }
}

/// **Eine grosse, zusammenhängende DMA-Region belegen und an `rid` anhängen.**
///
/// `len` in Bytes, seitenausgerichtet. Der Rückgabewert trägt beide Achsen; im Fehlerfall steht
/// ein **Name** und keine Leere.
///
/// Reihenfolge und Rücknahme, und beides ist nicht beliebig:
/// 1. Form prüfen, **bevor** irgendetwas belegt wird (eine Absage nach der Belegung müsste
///    zurückbauen, und der Rückbaupfad ist der, den niemand testet).
/// 2. Region schneiden. Scheitert das, wird die **Lage gemessen** und der Grund benannt.
/// 3. Cap prägen. Scheitert das, geht die Region zurück.
/// 4. Anhängen — **bevor** ein Treiber existiert. Danach ist die Region für genau diese RID
///    übersetzbar und für jede andere nicht.
/// 5. Achsen prüfen. Fällt das durch, wird **alles** zurückgebaut: eine ausgelieferte Identität
///    wäre schlimmer als ein Fehlschlag, weil sie funktioniert, bis jemand die Trennung
///    durchsetzt.
#[allow(dead_code)]
pub fn belege(rid: u32, len: u64) -> Result<GrossDmaGrant, GrossDmaFehler> {
    // Schritt 1 ohne Messung: Formfehler hängen nicht am Zustand der Maschine, und die Messung
    // kostet 18 Allokationen. `Lage::groesster_block` bleibt dafür bewusst ungemessen — die
    // Klassifikation braucht ihn in diesem Zweig nicht.
    klassifiziere(
        zone(),
        len,
        Lage {
            groesster_block: u64::MAX,
            fragmente: 0,
            kapazitaet: MAX_FRAGMENTS,
        },
    )?;

    let Some(region) = system::alloc_dma_region(len) else {
        // Erst jetzt messen — und die Klassifikation entscheidet, welcher der drei Gründe es ist.
        // `klassifiziere` gibt hier zwingend ein `Err`, weil die Form schon oben durchging;
        // ein `Ok` wäre ein Widerspruch zwischen Messung und Allokator und darf nicht als
        // Erfolg durchgehen.
        return Err(match klassifiziere(zone(), len, lage()) {
            Err(e) => e,
            Ok(()) => GrossDmaFehler::ZoneErschoepft {
                angefordert: len,
                groesster_block: 0,
            },
        });
    };

    // Non-coherent (Normal-NC), wie bei `assign_driver_device`: der Treiber liest Ringe, die das
    // Gerät geschrieben hat. Cacheable wäre auf x86 richtig und auf aarch64 falsch, solange kein
    // Treiber Cache-Wartung fährt — eine Zuteilung, die nur auf einer Architektur trägt, ist eine
    // Falle mit Verfallsdatum.
    let cap = match system::install_dma_cap_ex(
        region.base,
        region.len,
        DmaDir::Bidirectional,
        DmaCoherence::NonCoherent,
        Rights::RW,
    ) {
        Ok(c) => c,
        Err(_) => {
            system::free_unattached_dma_region(region.base, region.len);
            return Err(GrossDmaFehler::CapAbgewiesen);
        }
    };

    let Some(handle) = system::dma_attach(rid, cap) else {
        let _ = system::cap_delete(cap);
        return Err(GrossDmaFehler::KeineUebersetzung);
    };

    let (pa, iova) = (Pa::new(region.base), handle.iova);
    if let Err(e) = pruefe_achsen(pa.raw(), iova.raw()) {
        // Vollständiger Rückbau: lösen, dann Cap löschen (die Freigabe der Region hängt an der
        // Cap). Eine ausgelieferte Identität wäre der schlechtere Ausgang.
        system::dma_detach(rid, handle);
        let _ = system::cap_delete(cap);
        return Err(e);
    }

    Ok(GrossDmaGrant {
        pa,
        iova,
        len: region.len,
        cap,
        rid,
        handle,
    })
}

/// Einen Grant wieder einziehen: Übersetzung lösen, dann die Cap löschen (die Freigabe der Region
/// hängt an ihr — s. `alloc_dma_region`, Besitzmodell (phys,len)).
///
/// **In dieser Reihenfolge.** Umgekehrt gäbe es ein Fenster, in dem die Region schon wieder
/// vergeben werden darf, während die Übersetzung noch steht — und ein Gerät, dessen in-flight
/// Write ankommt, träfe fremden Speicher. Dieselbe Abwägung wie beim Einzug einer IRTE.
#[allow(dead_code)]
pub fn gib_frei(g: GrossDmaGrant) {
    system::dma_detach(g.rid, g.handle);
    let _ = system::cap_delete(g.cap);
}

// ================================================================================================
// Die Prüfzeile — `grossdma`
// ================================================================================================

/// Was die Prüfzeile misst. **Ein Feld je Aussage**, damit „rot" eine Diagnose ist.
#[cfg(feature = "selftest")]
#[derive(Clone, Copy, Default)]
pub struct GrossDmaBericht {
    /// **Sprechprobe:** ist überhaupt ein Block da, der gross genug für die geprüfte Anforderung
    /// ist? Ohne diese Zeile hiesse „die Absage kam" auf einer vollen Maschine dasselbe wie auf
    /// einer heilen — und alle Aussagen darunter wären vakuum.
    pub sprechfaehig: bool,
    /// Der grösste jetzt belegbare Block (Messwert).
    pub groesster_block: u64,
    /// Die Zonengrösse — die Obergrenze, als Zahl.
    pub zone: u64,
    /// Eine mehrere MiB grosse Region liess sich **am Stück** schneiden.
    pub gross_geht: bool,
    /// … und sie liegt vollständig in der Zone (sonst wäre sie für die PD nicht abbildbar).
    pub in_der_zone: bool,
    /// Eine Anforderung über die Zone hinaus wird als **`GroesserAlsZone`** abgewiesen — nicht als
    /// Erschöpfung.
    pub zu_gross_benannt: bool,
    /// Eine Anforderung eine Seite über dem gemessenen Block wird als **`ZoneErschoepft`**
    /// abgewiesen, **und die genannte Zahl ist der Messwert**.
    ///
    /// **Am ECHTEN Allokatorstand — und der ist nicht auf jeder Maschine konstruierbar.** Ist der
    /// grösste Block so gross wie die ganze Zone (gemessen bei `-m 3G`: beide 1 056 964 608 B),
    /// dann liegt `groesster_block + SEITE` bereits **über** der Zone, und die Klassifikation
    /// antwortet — richtig — mit `GroesserAlsZone`. Es gibt dort **keine** Länge, die zugleich in
    /// die Zone passt und den grössten Block übersteigt. Genau das hat die erste Fassung dieser
    /// Zeile bei 3 GiB rot gemeldet: nicht weil die Sache kaputt war, sondern weil das Kriterium
    /// **unerfüllbar** war. Dieselbe Falle wie die FP-Sonde, die „alle 64 Abgaben" verlangte.
    pub erschoepft_benannt: bool,
    /// **War der echte Fall auf DIESER Maschine überhaupt konstruierbar?** Ist er es nicht, sagt
    /// `erschoepft_benannt` nichts — und dann darf es weder als Erfolg noch als Fehlschlag
    /// zählen. Die Zahl steht in der Zeile, damit „nicht entscheidbar" von „bestanden"
    /// unterscheidbar bleibt.
    pub erschoepft_entscheidbar: bool,
    /// Derselbe Fall an einer **gestellten** Lage — und der läuft **immer**.
    ///
    /// `klassifiziere` ist eine reine Funktion: die Lage ist ihr Argument, keine Eigenschaft der
    /// Maschine. Also wird die Erschöpfung zusätzlich mit einem halb so grossen „grössten Block"
    /// vorgelegt. Ohne diesen Konjunkt wäre die Aussage auf einer 3-GiB-Maschine **vakuum** —
    /// und ein Konjunkt, das dort stillschweigend `true` wird, weil der Fall nicht eintritt, ist
    /// die Bauform von „Schweigen als Erfolg".
    pub erschoepft_gestellt: bool,
    /// Eine krumme Länge wird abgewiesen statt aufgerundet.
    pub krumm_benannt: bool,
    /// Der Allokatorstand ist nach allen Proben **unverändert** — eine Prüfzeile, die Speicher
    /// verliert, kippt baseline-empfindliche Tests (die Falle ist im Projekt schon bezahlt).
    pub kein_verlust: bool,
}

#[cfg(feature = "selftest")]
impl GrossDmaBericht {
    pub fn ok(&self) -> bool {
        self.sprechfaehig
            && self.gross_geht
            && self.in_der_zone
            && self.zu_gross_benannt
            // Der echte Fall zählt nur, wo er konstruierbar ist -- der gestellte immer.
            && (self.erschoepft_benannt || !self.erschoepft_entscheidbar)
            && self.erschoepft_gestellt
            && self.krumm_benannt
            && self.kein_verlust
    }
}

/// Wie gross die Probe ist. 8 MiB ist die Grössenordnung eines echten Ringpuffer-Satzes und
/// zugleich **500-mal** `DRIVER_DMA_BYTES` — genug, um „der Ladepfad ist auf kleine Stücke
/// ausgelegt" zu widerlegen, und klein genug, um auf einer 512-MiB-Maschine zu passen.
#[cfg(feature = "selftest")]
pub const PROBE_BYTES: u64 = 8 * 1024 * 1024;

/// **Die Prüfzeile.** Sie prüft die ZUTEILUNGS-Hälfte (Allokator + Klassifikation) und braucht
/// dafür **keine** IOMMU — sie läuft deshalb schon in `crate::selftest::run()`, also vor
/// `dma_enforcer_init()`.
///
/// Was sie ausdrücklich **nicht** prüft: das Anhängen (`dma_attach`) und damit die Gerätesicht.
/// Dafür braucht es einen aufgesetzten Übersetzungskontext; die Aussage gehört hinter
/// `dma_enforcer_init` und steht als eigene Funktion ([`pruefe_ende_zu_ende`]) bereit.
/// Beides in einer Zeile zu mischen hiesse, eine Aussage zu drucken, die zur Hälfte gar nicht
/// gemessen werden konnte.
#[cfg(feature = "selftest")]
pub fn pruefe() -> GrossDmaBericht {
    use caprock_dma::gross::GrossDmaFehler;
    let mut b = GrossDmaBericht {
        zone: zone().groesse(),
        ..Default::default()
    };
    let frei_vorher = system::total_free();
    let frag_vorher = system::fragments();

    b.groesster_block = groesster_block();
    b.sprechfaehig = b.groesster_block >= PROBE_BYTES;
    if !b.sprechfaehig {
        return b; // jede Aussage darunter wäre vakuum
    }

    // 1. Eine mehrere MiB grosse Region **am Stück** — und sie muss in der Zone liegen.
    if let Some(r) = system::alloc_dma_region(PROBE_BYTES) {
        b.gross_geht = r.len >= PROBE_BYTES;
        b.in_der_zone = r.base >= zone().lo && r.base + r.len <= zone().hi;
        system::free_unattached_dma_region(r.base, r.len);
    }

    // 2./3./4. Die drei benannten Absagen. Geprüft wird **der Name**, nicht „irgendein Fehler" —
    // sonst wäre ein Tippfehler in der Reihenfolge der Prüfungen schon ein Beleg.
    let l = Lage {
        groesster_block: b.groesster_block,
        fragmente: system::fragments(),
        kapazitaet: MAX_FRAGMENTS,
    };
    b.zu_gross_benannt = matches!(
        klassifiziere(zone(), zone().groesse() + SEITE, l),
        Err(GrossDmaFehler::GroesserAlsZone { .. })
    );
    // **Der echte Fall, sofern er auf dieser Maschine existiert.** Fuellt der groesste Block die
    // ganze Zone aus, gibt es keine Laenge, die zugleich hineinpasst und ihn uebersteigt.
    b.erschoepft_entscheidbar = b.groesster_block + SEITE <= b.zone;
    b.erschoepft_benannt = b.erschoepft_entscheidbar
        && matches!(
            klassifiziere(zone(), b.groesster_block + SEITE, l),
            Err(GrossDmaFehler::ZoneErschoepft { groesster_block, .. })
                if groesster_block == b.groesster_block
        );
    // **Und derselbe Fall an einer gestellten Lage -- der laeuft ueberall.** `klassifiziere` ist
    // rein; die Lage ist ein Argument und keine Eigenschaft der Maschine. Damit haengt die
    // Aussage nicht mehr daran, wie der Speicher dieser Maschine gerade geschnitten ist.
    let halb = (b.zone / 2) & !(SEITE - 1);
    let l_gestellt = Lage {
        groesster_block: halb,
        ..l
    };
    b.erschoepft_gestellt = halb >= SEITE
        && matches!(
            klassifiziere(zone(), halb + SEITE, l_gestellt),
            Err(GrossDmaFehler::ZoneErschoepft { groesster_block, .. }) if groesster_block == halb
        );
    b.krumm_benannt = matches!(
        klassifiziere(zone(), PROBE_BYTES + 1, l),
        Err(GrossDmaFehler::NichtSeitenausgerichtet { len }) if len == PROBE_BYTES + 1
    );

    // 5. Nichts verloren. **Beide** Grössen: der Allokator kann denselben Betrag frei melden und
    // dabei ein Fragment mehr führen (ein Loch in der Mitte) — und Fragmentierung ist genau das,
    // was 18 alloc/free-Paare anrichten könnten.
    b.kein_verlust = system::total_free() == frei_vorher && system::fragments() == frag_vorher;
    b
}

/// Die zweite Hälfte: **mit** Gerätesicht. Braucht einen aufgesetzten Übersetzungskontext, läuft
/// also erst nach `dma_enforcer_init()`.
///
/// Gibt `(ok, iova_ungleich_pa)`. Die zweite Zahl ist die eigentliche Aussage — eine Gerätesicht,
/// die mit der CPU-Sicht zusammenfällt, ist der Zustand, gegen den `docs/invariants.md` §2a–2e
/// steht. Sie wird hier gemeldet und nicht bloss vorausgesetzt, weil [`belege`] sie **abweist**:
/// ohne diese Zeile wäre nicht unterscheidbar, ob die Trennung gilt oder ob der Fall nie eintrat.
#[cfg(feature = "selftest")]
#[allow(dead_code)]
pub fn pruefe_ende_zu_ende(rid: u32) -> (bool, bool) {
    match belege(rid, PROBE_BYTES) {
        Ok(g) => {
            let getrennt = g.iova().raw() != g.pa().raw();
            let laenge_stimmt = g.len() >= PROBE_BYTES;
            gib_frei(g);
            (laenge_stimmt && getrennt, getrennt)
        }
        Err(_) => (false, false),
    }
}
