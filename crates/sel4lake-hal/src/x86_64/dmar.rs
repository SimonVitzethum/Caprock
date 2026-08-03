//! **DMAR-Auswertung und Gruppenbildung** (VT-d, Schritt 2).
//!
//! Was hier entsteht, ist keine Übersetzung, sondern eine **Liste**: welche Remapping-Einheit für
//! welches Gerät zuständig ist, welche Geräte eine gemeinsame Isolationsgruppe bilden, welche
//! Requester-IDs eine Gruppe unter Umständen trägt — und welche Geräte **ausgeschlossen** sind.
//!
//! ## Warum „Gruppe" und nicht „Gerät"
//!
//! Die Isolationsgranularität einer IOMMU ist nicht das Gerät, sondern die Gruppe. Fehlt einer
//! Bridge oberhalb des Geräts die ACS-Fähigkeit, können die Geräte darunter **an der IOMMU
//! vorbei** miteinander reden (Peer-to-Peer hinter einem Switch); ein Multifunktionsgerät ohne
//! ACS teilt die Sicht auf die Requester-ID unter allen Funktionen. Die Benennung steht deshalb
//! von Anfang an auf `Group` — nicht, weil das heute schon jeder Pfad ausnutzt, sondern weil
//! Tests, die erst an `dma_device` hängen, später an einer **zu starken** Aussage hängen.
//!
//! ## RMRR gilt der **Gruppe**, nicht der Funktion
//!
//! Eine RMRR ist ein Speicherbereich, den die **Firmware** für sich beansprucht (Legacy-USB,
//! BMC, integrierte Grafik). Das betroffene Gerät ist deshalb nicht zuteilbar — ein Teil seines
//! Zugriffs liegt per Konstruktion außerhalb unserer Kontrolle.
//!
//! Der Ausschluss muss aber **dieselbe Reichweite** haben wie die Gruppenbildung, und zwar aus
//! genau demselben Argument: fehlt der Bridge oberhalb ACS (oder handelt es sich um ein
//! Multifunktionsgerät ohne ACS), können die Geräte der Gruppe **an der IOMMU vorbei**
//! untereinander DMA umleiten. Wer eine Funktion der Gruppe bekommt, bekommt damit faktisch den
//! Weg des RMRR-Geräts — und zusätzlich schreibt die Zuteilung Kontexteinträge für **alle** RIDs
//! der Gruppe (`Groups::aliases`), also auch für die RID des RMRR-Geräts, in **seine** Domäne.
//!
//! Bis 2026-08-03 färbte `Rmrr` nur die einzelne Funktion, während `GroupSpansUnits` die ganze
//! Gruppe färbte. Gemessen: zwei Funktionen ohne ACS in einer Gruppe, RMRR auf 05.1 →
//! `excluded[05.0] = None`, Aliasmenge `[0x28, 0x29]`, `audit() = 0`. **Auf QEMU q35 ist das
//! unsichtbar (0 RMRRs), auf echter Hardware der Normalfall** — also genau die Sorte Lücke, die
//! eine Emulation nie zeigt und die deshalb aus Literalen geprüft werden muss
//! (`tools/host-tests.sh dmar`, Mutationen in `tools/dmar-rmrr-negativ.sh`).
//!
//! **Wirkung gleich, Grund verschieden.** Die Nachbarfunktion trägt keine RMRR; sie ist
//! ausgeschlossen, *weil sie in einer Gruppe mit einer steht*. Deshalb ein eigener Grund
//! (`GroupHasRmrr`) statt `Rmrr` über alle zu streichen: eine Meldung `AUSGESCHLOSSEN 00:05.0 --
//! Rmrr` schickte den Leser in die DMAR, wo für 05.0 nichts steht. Eine Beschriftung, die neben
//! der Sache herläuft, erzeugt Arbeit, die es nicht braucht.
//!
//! ## Isolation und Aliasing sind zwei Dinge
//!
//! * **Isolation** beantwortet: welche Geräte müssen zusammen zugeteilt werden?
//! * **Aliasing** beantwortet: unter welcher Requester-ID *sieht* die Einheit die Transaktion?
//!   Hinter einer konventionellen PCI-Bridge trägt jede Transaktion die RID der Bridge, nicht die
//!   des Geräts. Eine Gruppe hat deshalb eine **Menge** von RIDs, nicht eine.
//!
//! Das ist dieselbe 1:N-Beziehung wie Kontext→StreamID auf ARM, samt derselben Falle beim
//! Stilllegen: es müssen **alle** RIDs der Gruppe entwaffnet werden, nicht nur die auslösende.
//!
//! ## Prüfbarkeit
//!
//! Alles hier ist **reine Funktion über eingespeiste Daten**: `parse` nimmt einen Byte-Slice,
//! `build_groups` eine Topologiebeschreibung. Der reale Pfad füllt beides aus ACPI und
//! PCI-Enumeration, der Test aus Literalen. Das ist nicht Bequemlichkeit — auf dem
//! Standardaufbau (QEMU q35, flache Topologie, keine RMRR) laufen der Ausschlusspfad und die
//! interessanten Gruppenfälle **nie**. Ein Test gegen die reale Topologie wäre wieder ein
//! Oracle, das gilt, weil sein Antezedens falsch ist.

#![forbid(unsafe_code)]

pub const MAX_UNITS: usize = 4;
pub const MAX_SCOPE: usize = 16;
pub const MAX_RESERVED: usize = 16;
pub const MAX_DEVS: usize = 32;
pub const MAX_PATH: usize = 4;
pub const MAX_ALIASES: usize = 4;

// --- Device Scope -----------------------------------------------------------------------------

/// Scope-Eintragstyp 1: **Endpunkt**. Der Pfad zeigt auf genau ein Gerät.
pub const SCOPE_ENDPOINT: u8 = 0x01;
/// Scope-Eintragstyp 2: **Bridge**, also eine ganze Subhierarchie.
///
/// Wer das wie Typ 1 behandelt, ordnet alle Endpunkte *unterhalb* der Bridge dem Catch-all zu —
/// still, und auf einer Plattform mit nur einer Einheit folgenlos, bis es das nicht mehr ist.
pub const SCOPE_BRIDGE: u8 = 0x02;

/// Ein Device-Scope-Eintrag: Startbus + Pfad aus `(Device, Function)`-Paaren.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Scope {
    pub kind: u8,
    pub segment: u16,
    pub start_bus: u8,
    pub path: [(u8, u8); MAX_PATH],
    pub path_len: usize,
}

impl Scope {
    pub const EMPTY: Scope = Scope {
        kind: 0,
        segment: 0,
        start_bus: 0,
        path: [(0, 0); MAX_PATH],
        path_len: 0,
    };
}

// --- Geparste DMAR ----------------------------------------------------------------------------

/// Eine **DRHD**: eine Remapping-Einheit mit ihrem Zuständigkeitsbereich.
#[derive(Clone, Copy)]
pub struct Drhd {
    pub reg_base: u64,
    pub segment: u16,
    /// `INCLUDE_PCI_ALL` — Catch-all für alles, was keine andere DRHD ausdrücklich scoped.
    pub include_all: bool,
    pub scope: [Scope; MAX_SCOPE],
    pub n_scope: usize,
}

impl Drhd {
    pub const EMPTY: Drhd = Drhd {
        reg_base: 0,
        segment: 0,
        include_all: false,
        scope: [Scope::EMPTY; MAX_SCOPE],
        n_scope: 0,
    };
}

/// Ergebnis der DMAR-Auswertung.
pub struct DmarInfo {
    pub units: [Drhd; MAX_UNITS],
    pub n_units: usize,
    /// **RMRR**-Scopes — ausschließlich als **Ausschlussfilter**, nie als Mapping-Quelle.
    ///
    /// Eine RMRR-Region müsste identitätsgemappt werden, und Identität heißt IOVA = PA. Das ist
    /// nicht bloß „eine Lücke im Zugriff", sondern die Aufhebung genau der Eigenschaft, die die
    /// Achsentrennung hergestellt hat — und zwar **innerhalb** desselben Kontexts, in dem sonst
    /// das Fenster oberhalb `RAM_TOP` gilt. Ein solcher Kontext hätte zwei Achsenregime
    /// nebeneinander, und jede Bounds-Prüfung müsste beide kennen. Deshalb werden betroffene
    /// Geräte abgewiesen; die Regionsadressen braucht der Kernel dafür gar nicht, nur den Scope.
    pub rmrr: [Scope; MAX_RESERVED],
    pub n_rmrr: usize,
    /// **ATSR**-Scopes — nur Protokoll. Dass ein Gerät hier auftaucht, ist kein Grund, ATS zu
    /// aktivieren; der Vermerk dokumentiert, dass die Entscheidung dagegen bewusst gegen eine
    /// vorhandene Möglichkeit steht (`docs/invariants.md` §2b).
    pub n_atsr: usize,
    /// Kapazität reichte nicht — laut, nicht still gekürzt.
    pub truncated: bool,
    /// Längen/Prüfsumme unplausibel; die Auswertung wurde abgebrochen.
    pub malformed: bool,
    /// Es kam ein Segment != 0 vor (geprüfte Annahme, kein weggelassenes Feld).
    pub nonzero_segment: bool,
}

impl DmarInfo {
    pub const EMPTY: DmarInfo = DmarInfo {
        units: [Drhd::EMPTY; MAX_UNITS],
        n_units: 0,
        rmrr: [Scope::EMPTY; MAX_RESERVED],
        n_rmrr: 0,
        n_atsr: 0,
        truncated: false,
        malformed: false,
        nonzero_segment: false,
    };
}

fn rd16(b: &[u8], off: usize) -> Option<u16> {
    let s = b.get(off..off + 2)?;
    Some(u16::from_le_bytes([s[0], s[1]]))
}
fn rd64(b: &[u8], off: usize) -> Option<u64> {
    let s = b.get(off..off + 8)?;
    Some(u64::from_le_bytes([
        s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7],
    ]))
}

/// Scope-Einträge aus `[off, end)` lesen.
fn parse_scopes(
    b: &[u8],
    mut off: usize,
    end: usize,
    segment: u16,
    out: &mut [Scope],
    n: &mut usize,
) -> bool {
    let mut truncated = false;
    while off + 6 <= end {
        let kind = b[off];
        let elen = b[off + 1] as usize;
        // Länge 0 wäre eine Endlosschleife — Firmware-Eingabe, also abbrechen statt vertrauen.
        if elen < 6 || off + elen > end {
            break;
        }
        let start_bus = b[off + 5];
        let mut sc = Scope {
            kind,
            segment,
            start_bus,
            path: [(0, 0); MAX_PATH],
            path_len: 0,
        };
        let mut p = off + 6;
        while p + 2 <= off + elen && sc.path_len < MAX_PATH {
            sc.path[sc.path_len] = (b[p], b[p + 1]);
            sc.path_len += 1;
            p += 2;
        }
        if sc.path_len > 0 {
            if *n < out.len() {
                out[*n] = sc;
                *n += 1;
            } else {
                truncated = true;
            }
        }
        off += elen;
    }
    truncated
}

/// Die **DMAR** auswerten. `tbl` ist die vollständige Tabelle einschließlich Header.
///
/// Firmware-Eingabe, also durchgehend defensiv: Prüfsumme, jede Strukturlänge gegen das
/// Tabellenende, Länge 0 als Abbruch statt Endlosschleife, **unbekannte Typen per Länge
/// überspringen** statt zu scheitern (SATC/SIDP und Nachfolger tauchen auf neuerer Firmware auf
/// und dürfen die Auswertung der bekannten nicht verhindern).
pub fn parse(tbl: &[u8]) -> DmarInfo {
    let mut info = DmarInfo::EMPTY;
    if tbl.len() < 48 || &tbl[..4] != b"DMAR" {
        info.malformed = true;
        return info;
    }
    let len = match rd16(tbl, 4) {
        // Länge steht als u32; die oberen Bytes müssen 0 sein, sonst ist die Tabelle unplausibel.
        Some(lo) if tbl[6] == 0 && tbl[7] == 0 => lo as usize,
        _ => {
            info.malformed = true;
            return info;
        }
    };
    if len > tbl.len() || len < 48 {
        info.malformed = true;
        return info;
    }
    let tbl = &tbl[..len];
    if tbl.iter().fold(0u8, |a, &x| a.wrapping_add(x)) != 0 {
        info.malformed = true;
        return info;
    }
    let mut off = 48;
    while off + 4 <= len {
        let etype = match rd16(tbl, off) {
            Some(v) => v,
            None => break,
        };
        let elen = match rd16(tbl, off + 2) {
            Some(v) => v as usize,
            None => break,
        };
        if elen < 4 || off + elen > len {
            info.malformed = true;
            break;
        }
        match etype {
            0 => {
                // DRHD: Flags(1) reserviert(1) Segment(2) RegBase(8), dann Scopes.
                if elen >= 16 {
                    let flags = tbl[off + 4];
                    let segment = rd16(tbl, off + 6).unwrap_or(0);
                    let base = rd64(tbl, off + 8).unwrap_or(0);
                    if segment != 0 {
                        info.nonzero_segment = true;
                    }
                    if info.n_units < MAX_UNITS {
                        let u = &mut info.units[info.n_units];
                        u.reg_base = base;
                        u.segment = segment;
                        u.include_all = flags & 1 != 0;
                        let mut n = 0;
                        if parse_scopes(tbl, off + 16, off + elen, segment, &mut u.scope, &mut n) {
                            info.truncated = true;
                        }
                        u.n_scope = n;
                        info.n_units += 1;
                    } else {
                        info.truncated = true;
                    }
                }
            }
            1 => {
                // RMRR: reserviert(2) Segment(2) Base(8) Limit(8), dann Scopes. Adressen werden
                // bewusst nicht gelesen — nur der Scope, weil er ausschließt.
                if elen >= 24 {
                    let segment = rd16(tbl, off + 6).unwrap_or(0);
                    if segment != 0 {
                        info.nonzero_segment = true;
                    }
                    if parse_scopes(
                        tbl,
                        off + 24,
                        off + elen,
                        segment,
                        &mut info.rmrr,
                        &mut info.n_rmrr,
                    ) {
                        info.truncated = true;
                    }
                }
            }
            2 => {
                // ATSR: nur zählen (Protokoll).
                let mut tmp = [Scope::EMPTY; MAX_RESERVED];
                let mut n = 0;
                if elen >= 8 {
                    let segment = rd16(tbl, off + 6).unwrap_or(0);
                    parse_scopes(tbl, off + 8, off + elen, segment, &mut tmp, &mut n);
                }
                info.n_atsr += n;
            }
            // Unbekannt (SATC, SIDP, künftige): per Länge überspringen.
            _ => {}
        }
        off += elen;
    }
    info
}

// --- Topologie --------------------------------------------------------------------------------

/// Ein Knoten der PCI-Topologie. Reine Daten — der reale Pfad füllt sie aus der Enumeration,
/// der Test aus Literalen.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DevNode {
    pub segment: u16,
    pub bus: u8,
    pub dev: u8,
    pub func: u8,
    /// Bridge? Dann gelten `sec_bus`/`sub_bus`.
    pub bridge: bool,
    pub sec_bus: u8,
    pub sub_bus: u8,
    /// PCIe (hat die PCI-Express-Capability)? Eine Bridge **ohne** sie ist eine konventionelle
    /// Bridge und erzeugt Aliasing.
    pub pcie: bool,
    /// Trägt die Bridge/das Gerät ACS mit den vier relevanten Fähigkeiten aktiviert
    /// (Source Validation, Translation Blocking, P2P Request Redirect, Upstream Forwarding)?
    pub acs: bool,
    /// Mehrere Funktionen vorhanden?
    pub multifunction: bool,
    /// Index des übergeordneten Bridge-Knotens, `usize::MAX` = direkt am Root Complex.
    pub parent: usize,
}

impl DevNode {
    pub const EMPTY: DevNode = DevNode {
        segment: 0,
        bus: 0,
        dev: 0,
        func: 0,
        bridge: false,
        sec_bus: 0,
        sub_bus: 0,
        pcie: true,
        acs: true,
        multifunction: false,
        parent: usize::MAX,
    };
    pub fn rid(&self) -> u32 {
        ((self.bus as u32) << 8) | ((self.dev as u32) << 3) | (self.func as u32)
    }
}

/// Deckt der Scope-Eintrag das Gerät `i` ab?
///
/// Typ 1 trifft genau das Gerät am Ende des Pfads; Typ 2 trifft die Bridge **und alles
/// darunter**. Der Pfad wird vollständig durchlaufen — nur das erste Paar zu lesen ordnet
/// tiefer liegende Endpunkte dem Catch-all zu.
fn scope_covers(sc: &Scope, topo: &[DevNode], i: usize) -> bool {
    let Some(target) = resolve_scope(sc, topo) else {
        return false;
    };
    if sc.segment != topo[i].segment {
        return false;
    }
    if target == i {
        return true;
    }
    if sc.kind != SCOPE_BRIDGE {
        return false;
    }
    // Subhierarchie: von `i` aufwärts, bis die Bridge gefunden ist.
    let mut cur = topo[i].parent;
    let mut guard = 0;
    while cur != usize::MAX && guard < MAX_DEVS {
        if cur == target {
            return true;
        }
        cur = topo[cur].parent;
        guard += 1;
    }
    false
}

/// Den Pfad eines Scope-Eintrags gegen die Topologie auflösen: Index des Zielknotens.
fn resolve_scope(sc: &Scope, topo: &[DevNode]) -> Option<usize> {
    let mut bus = sc.start_bus;
    let mut idx = None;
    for h in 0..sc.path_len {
        let (d, f) = sc.path[h];
        let found = topo.iter().position(|n| {
            n.segment == sc.segment && n.bus == bus && n.dev == d && n.func == f
        })?;
        idx = Some(found);
        if h + 1 < sc.path_len {
            if !topo[found].bridge {
                return None; // Pfad führt durch etwas, das keine Bridge ist
            }
            bus = topo[found].sec_bus;
        }
    }
    idx
}

// --- Einheiten-Zuordnung ----------------------------------------------------------------------

/// Welcher Einheit gehört das Gerät `i`?
///
/// **Explizite Scopes zuerst, `INCLUDE_PCI_ALL` als Rückfall.** Höchstens eine DRHD je Segment
/// trägt das Catch-all-Flag, und sie deckt alles, was keine andere ausdrücklich scoped. Eine
/// Schleife mit „erster Treffer gewinnt" ordnet Geräte sonst der falschen Einheit zu — was auf
/// einer Plattform mit einer einzigen Einheit folgenlos bleibt und deshalb unentdeckt.
pub fn unit_of(info: &DmarInfo, topo: &[DevNode], i: usize) -> Option<usize> {
    for u in 0..info.n_units {
        let d = &info.units[u];
        for s in 0..d.n_scope {
            if d.scope[s].kind == SCOPE_ENDPOINT || d.scope[s].kind == SCOPE_BRIDGE {
                if scope_covers(&d.scope[s], topo, i) {
                    return Some(u);
                }
            }
        }
    }
    for u in 0..info.n_units {
        if info.units[u].include_all && info.units[u].segment == topo[i].segment {
            return Some(u);
        }
    }
    None
}

/// Trägt das Gerät `i` **selbst** eine RMRR?
///
/// Bewusst geräteweise: die Ausweitung auf die Isolationsgruppe passiert in `build_groups`, weil
/// sie die Gruppen braucht. Wer diese Funktion als Zuteilungsschranke benutzt, prüft die
/// **falsche** Reichweite — die Schranke ist `Groups::excluded`.
pub fn has_rmrr(info: &DmarInfo, topo: &[DevNode], i: usize) -> bool {
    (0..info.n_rmrr).any(|r| scope_covers(&info.rmrr[r], topo, i))
}

// --- Gruppenbildung ---------------------------------------------------------------------------

/// Warum ein Gerät **nicht** zuteilbar ist.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Exclusion {
    /// Keine Remapping-Einheit zuständig. Ein solches Gerät könnte nie übersetzt werden — bekäme
    /// es später trotzdem ein `attach`, hinge eine Übersetzung an einer Einheit, die es nicht
    /// sieht.
    NoUnit,
    /// **Dieses Gerät** ist RMRR-behaftet (s. `DmarInfo::rmrr`).
    Rmrr,
    /// Ein **anderes Gerät derselben Isolationsgruppe** ist RMRR-behaftet.
    ///
    /// Die Wirkung ist dieselbe wie bei `Rmrr` — nicht zuteilbar —, die Tatsache ist eine andere:
    /// über dieses Gerät steht in der DMAR nichts. Wer beides `Rmrr` nennte, schickte jeden
    /// Leser der Ausschlussliste in eine Tabelle, in der er den Grund nicht findet.
    ///
    /// Warum überhaupt ausgeschlossen: ohne ACS reden die Geräte einer Gruppe an der IOMMU vorbei
    /// miteinander, und die Zuteilung schreibt Kontexteinträge für **alle** RIDs der Gruppe —
    /// also auch für die des RMRR-Geräts, in die Domäne des Zuteilungsempfängers.
    GroupHasRmrr,
    /// Die Gruppe streut über mehrere Einheiten — ein Firmware-Zustand, der abgewiesen und nicht
    /// behandelt wird.
    GroupSpansUnits,
}

pub struct Groups {
    /// Gruppenindex je Gerät (`usize::MAX` = nicht zugeordnet).
    pub group_of: [usize; MAX_DEVS],
    /// Einheit je Gerät.
    pub unit_of: [usize; MAX_DEVS],
    /// Ausschlussgrund je Gerät (`None` = zuteilbar).
    pub excluded: [Option<Exclusion>; MAX_DEVS],
    /// RID-Aliase je **Gruppe** — die Menge der Requester-IDs, unter denen die Einheit
    /// Transaktionen dieser Gruppe sieht. Eine Zuteilung muss Kontexteinträge für **alle**
    /// schreiben und der Teardown alle räumen.
    pub aliases: [[u32; MAX_ALIASES]; MAX_DEVS],
    pub n_aliases: [usize; MAX_DEVS],
    pub n_devs: usize,
    pub n_groups: usize,
    /// **RMRR-Scopes, die auf kein Gerät der Topologie zeigen.**
    ///
    /// Ein solcher Scope kann niemanden ausschließen — und das heißt nicht „kein Ausschluss
    /// nötig", sondern „die Ausschlussliste ist nachweislich unvollständig". Genau die Sorte
    /// Schweigen, die sonst als Erfolg durchgeht: `0 Ausschlüsse` sähe identisch aus, ob die
    /// Firmware nichts beansprucht oder ob wir das beanspruchte Gerät nur nicht gefunden haben.
    /// `audit()` unterscheidet die beiden Fälle (Code 5).
    pub rmrr_unresolved: usize,
}

impl Groups {
    pub const EMPTY: Groups = Groups {
        group_of: [usize::MAX; MAX_DEVS],
        unit_of: [usize::MAX; MAX_DEVS],
        excluded: [None; MAX_DEVS],
        aliases: [[0; MAX_ALIASES]; MAX_DEVS],
        n_aliases: [0; MAX_DEVS],
        n_devs: 0,
        n_groups: 0,
        rmrr_unresolved: 0,
    };
}

fn find(parent: &mut [usize; MAX_DEVS], mut x: usize) -> usize {
    while parent[x] != x {
        parent[x] = parent[parent[x]];
        x = parent[x];
    }
    x
}
fn union(parent: &mut [usize; MAX_DEVS], a: usize, b: usize) {
    let (ra, rb) = (find(parent, a), find(parent, b));
    if ra != rb {
        parent[ra.max(rb)] = ra.min(rb);
    }
}

/// Die **RID, unter der die Einheit** eine Transaktion von `i` sieht.
///
/// Hinter einer konventionellen (nicht-PCIe) Bridge trägt jede Transaktion die RID der Bridge.
/// Maßgeblich ist die **wurzelnächste** solche Bridge — sie ist die letzte, die die RID ersetzt,
/// bevor die Transaktion die Einheit erreicht.
fn alias_rid(topo: &[DevNode], i: usize) -> Option<u32> {
    let mut cur = topo[i].parent;
    let mut alias = None;
    let mut guard = 0;
    while cur != usize::MAX && guard < MAX_DEVS {
        if topo[cur].bridge && !topo[cur].pcie {
            alias = Some(topo[cur].rid());
        }
        cur = topo[cur].parent;
        guard += 1;
    }
    alias
}

/// Gruppen bilden — **rein** über die eingespeiste Topologie.
pub fn build_groups(info: &DmarInfo, topo: &[DevNode]) -> Groups {
    let mut g = Groups::EMPTY;
    let n = topo.len().min(MAX_DEVS);
    g.n_devs = n;
    let mut parent = [0usize; MAX_DEVS];
    for (i, p) in parent.iter_mut().enumerate() {
        *p = i;
    }

    for i in 0..n {
        // (1) Multifunktionsgerät ohne ACS: alle Funktionen desselben Geräts in eine Gruppe.
        if !topo[i].acs {
            for j in 0..n {
                if j != i
                    && topo[j].segment == topo[i].segment
                    && topo[j].bus == topo[i].bus
                    && topo[j].dev == topo[i].dev
                {
                    union(&mut parent, i, j);
                }
            }
        }
        // (2) Fehlt einer Bridge oberhalb ACS, erstreckt sich die Gruppe über die Bridge hinaus:
        //     alles unterhalb derselben Bridge kann untereinander an der IOMMU vorbei reden.
        let mut cur = topo[i].parent;
        let mut guard = 0;
        while cur != usize::MAX && guard < MAX_DEVS {
            if !topo[cur].acs {
                // Die Bridge selbst gehört dazu: sie ist der Grund, warum die Gruppe größer ist
                // als das Gerät, und sie kann selbst DMA absetzen.
                union(&mut parent, i, cur);
                for j in 0..n {
                    if j != i && is_below(topo, j, cur) {
                        union(&mut parent, i, j);
                    }
                }
            }
            cur = topo[cur].parent;
            guard += 1;
        }
        // (3) Geräte, die dieselbe Alias-RID tragen, sind für die Einheit ununterscheidbar.
        if let Some(a) = alias_rid(topo, i) {
            for j in 0..n {
                if j != i && alias_rid(topo, j) == Some(a) {
                    union(&mut parent, i, j);
                }
            }
        }
    }

    // Gruppenindizes vergeben (kompakt).
    let mut map = [usize::MAX; MAX_DEVS];
    for i in 0..n {
        let r = find(&mut parent, i);
        if map[r] == usize::MAX {
            map[r] = g.n_groups;
            g.n_groups += 1;
        }
        g.group_of[i] = map[r];
    }

    // Einheiten + geräteeigene Ausschlüsse.
    for i in 0..n {
        match unit_of(info, topo, i) {
            Some(u) => g.unit_of[i] = u,
            None => g.excluded[i] = Some(Exclusion::NoUnit),
        }
        if has_rmrr(info, topo, i) {
            g.excluded[i] = Some(Exclusion::Rmrr);
        }
    }
    // **Eine RMRR schließt die GRUPPE aus, nicht die Funktion** (s. Modulkopf).
    //
    // Dasselbe Argument wie bei `GroupSpansUnits`: die Gruppe ist die Isolationsgranularität. Wer
    // eine Funktion einer Gruppe bekommt, in der ein RMRR-Gerät sitzt, bekommt ohne ACS dessen
    // Weg — und die Zuteilung schreibt ohnehin Kontexteinträge für **alle** RIDs der Gruppe, also
    // auch für die RID des RMRR-Geräts, in die Domäne des Empfängers.
    //
    // Der Grund bleibt unterscheidbar (`GroupHasRmrr`), und ein bereits vorhandener,
    // **geräteeigener** Grund wird nicht überschrieben: `NoUnit` sagt mehr über dieses Gerät als
    // „steht neben einem RMRR-Gerät".
    for gi in 0..g.n_groups {
        let hat_rmrr = (0..n).any(|i| g.group_of[i] == gi && g.excluded[i] == Some(Exclusion::Rmrr));
        if hat_rmrr {
            for i in 0..n {
                if g.group_of[i] == gi && g.excluded[i].is_none() {
                    g.excluded[i] = Some(Exclusion::GroupHasRmrr);
                }
            }
        }
    }
    // Ein RMRR-Scope, den die Topologie nicht auflöst, schließt **niemanden** aus. Das ist kein
    // „nichts zu tun", sondern eine Lücke im Wissen — sie wird gezählt statt verschluckt, damit
    // `audit()` sie melden kann (Code 5). Fail-closed heißt hier: der Zustand ist benannt, nicht
    // stillschweigend als „keine RMRR" gelesen.
    for r in 0..info.n_rmrr {
        if resolve_scope(&info.rmrr[r], &topo[..n]).is_none() {
            g.rmrr_unresolved += 1;
        }
    }
    // Eine Gruppe, die über Einheiten streut, wird **vollständig** ausgeschlossen — teilweise
    // zuzuteilen hieße, die Gruppe als Isolationseinheit aufzugeben.
    for gi in 0..g.n_groups {
        let mut unit = usize::MAX;
        let mut spans = false;
        for i in 0..n {
            if g.group_of[i] == gi && g.unit_of[i] != usize::MAX {
                if unit == usize::MAX {
                    unit = g.unit_of[i];
                } else if unit != g.unit_of[i] {
                    spans = true;
                }
            }
        }
        if spans {
            for i in 0..n {
                if g.group_of[i] == gi {
                    g.excluded[i] = Some(Exclusion::GroupSpansUnits);
                }
            }
        }
    }

    // Alias-Mengen je Gruppe: eigene RID jedes Mitglieds + dessen Alias.
    for i in 0..n {
        let gi = g.group_of[i];
        for r in [Some(topo[i].rid()), alias_rid(topo, i)].into_iter().flatten() {
            if !g.aliases[gi][..g.n_aliases[gi]].contains(&r) && g.n_aliases[gi] < MAX_ALIASES {
                g.aliases[gi][g.n_aliases[gi]] = r;
                g.n_aliases[gi] += 1;
            }
        }
    }
    g
}

fn is_below(topo: &[DevNode], i: usize, bridge: usize) -> bool {
    let mut cur = topo[i].parent;
    let mut guard = 0;
    while cur != usize::MAX && guard < MAX_DEVS {
        if cur == bridge {
            return true;
        }
        cur = topo[cur].parent;
        guard += 1;
    }
    false
}

/// **Bitmaske der Einheiten, die mindestens eine zuteilbare Gruppe scopen** (B-3.3).
///
/// Der Sinn: die Fähigkeitsmittelung (`vtd::caps_for_units`) soll das Minimum über *genau diese*
/// Einheiten nehmen — nicht über alle. Eine Einheit, hinter der ausschliesslich ausgeschlossene
/// Geräte hängen (RMRR, gruppenübergreifend, ohne Einheit), kann die Zusicherung für niemanden
/// tragen; ihre schwächeren Fähigkeiten würden die Zuteilung anderer Gruppen grundlos
/// beschneiden. Umgekehrt darf keine Einheit fehlen, an der ein zuteilbares Gerät hängt.
///
/// Bit `u` gesetzt heisst: Einheit `u` trägt mindestens ein **nicht ausgeschlossenes** Gerät.
/// `0` heisst: es gibt nichts zuzuteilen — und das ist ein anderer Zustand als „alle Einheiten",
/// weshalb der Aufrufer ihn unterscheiden muss statt auf „alle" zurückzufallen.
pub fn units_scoping_allocatable(g: &Groups) -> u32 {
    let mut mask = 0u32;
    for i in 0..g.n_devs {
        if g.excluded[i].is_none() && g.unit_of[i] != usize::MAX && g.unit_of[i] < 32 {
            mask |= 1u32 << g.unit_of[i];
        }
    }
    mask
}

// --- Oracle -----------------------------------------------------------------------------------

/// Vollständigkeits-Oracle für Schritt 2. `0` = konsistent.
///
/// Der Schritt liefert eine **Liste**, also braucht er eine Vollständigkeitsaussage statt eines
/// Verhaltenstests:
/// * `1` — ein Gerät ohne Gruppe.
/// * `2` — ein **zuteilbares** Gerät ohne Einheit (das könnte nie übersetzt werden; bekäme es
///   später ein `attach`, hinge eine Übersetzung an einer Einheit, die es nicht sieht).
/// * `3` — die Alias-Mengen zweier Gruppen überlappen. Dann sind zwei „Gruppen" in Wahrheit eine
///   — genau der Fehler, den die Gruppenbildung verhindern soll.
/// * `4` — eine Gruppe enthält ein RMRR-behaftetes **und** ein zuteilbares Gerät. Dann ist die
///   Gruppe als Isolationseinheit aufgegeben: der Zuteilungsempfänger erbt ohne ACS den Weg des
///   RMRR-Geräts, und seine Kontexteinträge decken dessen RID mit ab. **Genau der Zustand, den
///   dieser Code bis 2026-08-03 nicht melden konnte** — er war weder ein Ausschluss noch eine
///   Alias-Überlappung, also für die Codes 1–3 unsichtbar, und `audit()` gab `0` zurück.
/// * `5` — ein RMRR-Scope zeigt auf kein Gerät der Topologie (`Groups::rmrr_unresolved`). Dann
///   ist die Ausschlussliste nachweislich unvollständig, und „0 Ausschlüsse" bedeutet nichts.
///
/// Die Prüfungen 4 und 5 sind **unabhängige Nachzählungen** über das fertige Ergebnis: sie rufen
/// nichts aus der Gruppenbildung auf, sondern lesen nur, was dort herauskam. Ein Prüfer, der die
/// Schleife des Geprüften wiederverwendet, bestätigt dessen Fehler mit.
pub fn audit(g: &Groups) -> u32 {
    for i in 0..g.n_devs {
        if g.group_of[i] == usize::MAX {
            return 1;
        }
        if g.excluded[i].is_none() && g.unit_of[i] == usize::MAX {
            return 2;
        }
    }
    for a in 0..g.n_groups {
        for b in (a + 1)..g.n_groups {
            for x in 0..g.n_aliases[a] {
                if g.aliases[b][..g.n_aliases[b]].contains(&g.aliases[a][x]) {
                    return 3;
                }
            }
        }
    }
    for gi in 0..g.n_groups {
        let mut mit_rmrr = false;
        let mut zuteilbar = false;
        for i in 0..g.n_devs {
            if g.group_of[i] != gi {
                continue;
            }
            if g.excluded[i] == Some(Exclusion::Rmrr) {
                mit_rmrr = true;
            }
            if g.excluded[i].is_none() {
                zuteilbar = true;
            }
        }
        if mit_rmrr && zuteilbar {
            return 4;
        }
    }
    if g.rmrr_unresolved > 0 {
        return 5;
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Zwei Einheiten, zwei Endpunkte -- Geraet 0 an Einheit 0, Geraet 1 an Einheit 1.
    fn zwei_einheiten() -> (DmarInfo, [DevNode; 2]) {
        let mut info = DmarInfo::EMPTY;
        info.n_units = 2;
        for (u, dev) in [(0usize, 2u8), (1usize, 3u8)] {
            info.units[u].reg_base = 0xfed9_0000 + (u as u64) * 0x1000;
            info.units[u].n_scope = 1;
            info.units[u].scope[0] = Scope {
                kind: SCOPE_ENDPOINT,
                segment: 0,
                start_bus: 0,
                path: [(dev, 0), (0, 0), (0, 0), (0, 0)],
                path_len: 1,
            };
        }
        let mk = |dev: u8| DevNode { bus: 0, dev, func: 0, ..DevNode::EMPTY };
        (info, [mk(2), mk(3)])
    }

    /// Beide Geraete zuteilbar -> beide Einheiten stehen in der Maske.
    #[test]
    fn maske_enthaelt_jede_einheit_mit_zuteilbarem_geraet() {
        let (info, topo) = zwei_einheiten();
        let g = build_groups(&info, &topo);
        assert_eq!(g.unit_of[0], 0);
        assert_eq!(g.unit_of[1], 1);
        assert_eq!(units_scoping_allocatable(&g), 0b11);
    }

    /// **Der eigentliche Zweck**: eine Einheit, hinter der nur AUSGESCHLOSSENE Geraete haengen,
    /// darf die Faehigkeitsmittelung nicht beschneiden -- ihr Bit faellt aus der Maske.
    ///
    /// Ohne diesen Unterschied waere `caps_for_units` nur ein umstaendliches `caps_common`.
    #[test]
    fn ausgeschlossene_einheit_faellt_aus_der_maske() {
        let (mut info, topo) = zwei_einheiten();
        // Geraet 1 (Bus 0, Dev 3) mit einer RMRR belegen -> ausgeschlossen.
        info.n_rmrr = 1;
        info.rmrr[0] = Scope {
            kind: SCOPE_ENDPOINT,
            segment: 0,
            start_bus: 0,
            path: [(3, 0), (0, 0), (0, 0), (0, 0)],
            path_len: 1,
        };
        let g = build_groups(&info, &topo);
        assert_eq!(g.excluded[1], Some(Exclusion::Rmrr));
        assert_eq!(
            units_scoping_allocatable(&g),
            0b01,
            "Einheit 1 traegt nur ein ausgeschlossenes Geraet und gehoert nicht in die Mittelung"
        );
    }

    /// Nichts zuteilbar heisst **leere Maske** -- und das ist ein anderer Zustand als „alle".
    /// Ein Aufrufer, der hier auf `u32::MAX` zurueckfiele, mittelte ueber Einheiten, die
    /// niemanden tragen, und bekaeme eine Zusage ohne Subjekt.
    #[test]
    fn ohne_zuteilbares_geraet_ist_die_maske_leer() {
        let (mut info, topo) = zwei_einheiten();
        info.n_rmrr = 2;
        for (k, dev) in [(0usize, 2u8), (1usize, 3u8)] {
            info.rmrr[k] = Scope {
                kind: SCOPE_ENDPOINT,
                segment: 0,
                start_bus: 0,
                path: [(dev, 0), (0, 0), (0, 0), (0, 0)],
                path_len: 1,
            };
        }
        info.n_rmrr = 2;
        let g = build_groups(&info, &topo);
        assert_eq!(units_scoping_allocatable(&g), 0);
    }

    // --- RMRR faerbt die GRUPPE, nicht die Funktion (E-Rest 2, 2026-08-03) ----------------------
    //
    // Diese Faelle laufen auf QEMU q35 NIE: die Plattform hat 0 RMRRs. Auf echter Hardware ist
    // eine RMRR der Normalfall (Legacy-USB, BMC, Grafik). Deshalb aus Literalen -- eine Messung
    // gegen die reale Topologie waere gruen, weil ihr Antezedens falsch ist.

    /// Ein RMRR-Scope auf `00:05.<func>`.
    fn rmrr_auf(func: u8) -> Scope {
        Scope {
            kind: SCOPE_ENDPOINT,
            segment: 0,
            start_bus: 0,
            path: [(5, func), (0, 0), (0, 0), (0, 0)],
            path_len: 1,
        }
    }

    /// Zwei Funktionen eines Multifunktionsgeraets (00:05.0 / 00:05.1), RMRR auf **05.1**, dazu
    /// ein unbeteiligtes Geraet 00:06.0. `acs` steuert den einzigen Unterschied zwischen dem
    /// gesunden und dem gefaehrlichen Fall.
    fn rmrr_szenario(acs: bool) -> (DmarInfo, [DevNode; 3]) {
        let mut info = DmarInfo::EMPTY;
        info.n_units = 1;
        info.units[0].reg_base = 0xfed9_0000;
        info.units[0].include_all = true;
        info.n_rmrr = 1;
        info.rmrr[0] = rmrr_auf(1);
        let f = |func: u8| DevNode {
            bus: 0,
            dev: 5,
            func,
            acs,
            multifunction: true,
            ..DevNode::EMPTY
        };
        let fremd = DevNode { bus: 0, dev: 6, func: 0, ..DevNode::EMPTY };
        (info, [f(0), f(1), fremd])
    }

    /// **Positivkontrolle.** Mit ACS trennt die Hardware die Funktionen -- dann darf der
    /// Ausschluss NICHT ueber das RMRR-Geraet hinausgehen. Ohne diesen Fall waere „alles
    /// ausschliessen" eine bestandene Loesung, und die Maschine haette am Ende gar keine
    /// zuteilbaren Geraete mehr.
    #[test]
    fn rmrr_mit_acs_faerbt_nur_das_geraet() {
        let (info, topo) = rmrr_szenario(true);
        let g = build_groups(&info, &topo);
        assert_eq!(g.n_groups, 3, "mit ACS sind es drei getrennte Gruppen");
        assert_eq!(g.excluded[1], Some(Exclusion::Rmrr), "05.1 traegt die RMRR");
        assert_eq!(g.excluded[0], None, "05.0 ist durch ACS getrennt und bleibt zuteilbar");
        assert_eq!(g.excluded[2], None, "06.0 war nie beteiligt");
        assert_eq!(&g.aliases[g.group_of[0]][..g.n_aliases[g.group_of[0]]], &[0x28]);
        assert_eq!(audit(&g), 0);
    }

    /// **Der Fehlerfall.** Ohne ACS bilden 05.0 und 05.1 EINE Gruppe -- sie koennen DMA
    /// untereinander umleiten, ohne dass die IOMMU es sieht, und eine Zuteilung von 05.0 schriebe
    /// Kontexteintraege fuer die Aliasmenge `[0x28, 0x29]`, also auch fuer die RID des
    /// RMRR-Geraets. Vor dem 2026-08-03 war `excluded[0] == None`.
    #[test]
    fn rmrr_ohne_acs_faerbt_die_ganze_gruppe() {
        let (info, topo) = rmrr_szenario(false);
        let g = build_groups(&info, &topo);
        assert_eq!(g.group_of[0], g.group_of[1], "ohne ACS eine Gruppe");
        assert_eq!(g.excluded[1], Some(Exclusion::Rmrr));
        assert_eq!(
            g.excluded[0],
            Some(Exclusion::GroupHasRmrr),
            "05.0 steht in der Gruppe eines RMRR-Geraets und ist damit nicht zuteilbar"
        );
        assert_eq!(g.excluded[2], None, "der Ausschluss endet an der Gruppengrenze");
        // Die Aliasmenge bleibt unveraendert -- Aliasing und Isolation sind zwei Achsen. Der
        // Unterschied ist, dass diese Menge jetzt niemandem mehr zugeteilt wird.
        let gi = g.group_of[0];
        assert_eq!(&g.aliases[gi][..g.n_aliases[gi]], &[0x28, 0x29]);
        assert_eq!(audit(&g), 0);
        // Einheit 0 traegt weiterhin 06.0 -- ein Gruppenausschluss darf die Faehigkeits-
        // mittelung nicht leerraeumen.
        assert_eq!(units_scoping_allocatable(&g), 0b1);
    }

    /// Dasselbe eine Ebene hoeher: hinter einer **konventionellen** Bridge tragen alle Geraete
    /// die RID der Bridge. Eine RMRR auf einem von ihnen faerbt die Bridge und das
    /// Nachbargeraet mit -- sie sind fuer die Einheit ununterscheidbar.
    #[test]
    fn rmrr_hinter_konventioneller_bruecke_faerbt_alles_darunter() {
        let mut info = DmarInfo::EMPTY;
        info.n_units = 1;
        info.units[0].include_all = true;
        info.n_rmrr = 1;
        info.rmrr[0] = Scope {
            kind: SCOPE_ENDPOINT,
            segment: 0,
            start_bus: 3,
            path: [(1, 0), (0, 0), (0, 0), (0, 0)],
            path_len: 1,
        };
        // `acs: false` ist hier keine Zutat, sondern die Wirklichkeit: ACS ist eine
        // **PCI-Express**-Extended-Capability, eine konventionelle Bridge kann sie nicht tragen.
        // `pcie::acs_enabled` liefert fuer sie folglich `false`. (`DevNode::EMPTY` steht auf
        // `acs: true` -- der optimistische Default fuer handgebaute PCIe-Endpunkte.)
        let topo = [
            DevNode { bus: 0, dev: 3, func: 0, bridge: true, sec_bus: 3, sub_bus: 3, pcie: false, acs: false, ..DevNode::EMPTY },
            DevNode { bus: 3, dev: 1, func: 0, parent: 0, ..DevNode::EMPTY }, // RMRR
            DevNode { bus: 3, dev: 2, func: 0, parent: 0, ..DevNode::EMPTY },
        ];
        let g = build_groups(&info, &topo);
        assert_eq!(g.excluded[1], Some(Exclusion::Rmrr));
        assert_eq!(g.excluded[2], Some(Exclusion::GroupHasRmrr), "gleiche Alias-RID");
        assert_eq!(g.excluded[0], Some(Exclusion::GroupHasRmrr), "die Bruecke setzt selbst DMA ab");
        assert_eq!(audit(&g), 0);
        assert_eq!(units_scoping_allocatable(&g), 0, "hier ist wirklich nichts zuteilbar");
    }

    /// Ein **geraeteeigener** Grund wird nicht von dem abgeleiteten ueberschrieben: `NoUnit` sagt
    /// mehr ueber dieses Geraet als „steht neben einem RMRR-Geraet".
    #[test]
    fn geraeteeigener_grund_schlaegt_den_abgeleiteten() {
        let (mut info, topo) = rmrr_szenario(false);
        info.units[0].include_all = false; // niemand ist mehr zustaendig
        let g = build_groups(&info, &topo);
        assert_eq!(g.excluded[0], Some(Exclusion::NoUnit));
        assert_eq!(g.excluded[1], Some(Exclusion::Rmrr), "die eigene RMRR bleibt der Grund");
    }

    /// **Kann `audit()` diesen Zustand ueberhaupt melden?** Der alte Zustand wird nachgebaut,
    /// indem genau die neue Faerbung wieder entfernt wird -- mehr aendert sich nicht. Vorher
    /// meldete `audit()` dazu `0`; ohne Code 4 waere der Fehler auch nach dem Einbau eines
    /// Pruefers unsichtbar geblieben.
    #[test]
    fn audit_meldet_die_ungefaerbte_gruppe() {
        let (info, topo) = rmrr_szenario(false);
        let g = build_groups(&info, &topo);
        assert_eq!(audit(&g), 0, "der behobene Zustand ist konsistent");

        let mut alt = build_groups(&info, &topo);
        alt.excluded[0] = None; // <- der Stand vor dem 2026-08-03
        assert_eq!(
            audit(&alt),
            4,
            "eine Gruppe mit RMRR-Geraet UND zuteilbarem Geraet ist keine Isolationseinheit"
        );
    }

    /// Ein RMRR-Scope, der auf kein Geraet der Topologie zeigt, schliesst niemanden aus. „0
    /// Ausschluesse" darf dann nicht wie „keine RMRR" aussehen -- Code 5.
    #[test]
    fn unaufloesbare_rmrr_wird_gemeldet_statt_verschluckt() {
        let (mut info, topo) = rmrr_szenario(true);
        info.rmrr[0] = rmrr_auf(7); // 00:05.7 gibt es in dieser Topologie nicht
        let g = build_groups(&info, &topo);
        assert_eq!(g.rmrr_unresolved, 1);
        assert!(
            (0..g.n_devs).all(|i| g.excluded[i].is_none()),
            "der Scope kann niemanden treffen -- genau deshalb braucht es die Meldung"
        );
        assert_eq!(audit(&g), 5);

        // Gegenprobe: derselbe Aufbau mit aufloesbarem Scope schweigt nicht aus Zufall.
        let (info2, topo2) = rmrr_szenario(true);
        let g2 = build_groups(&info2, &topo2);
        assert_eq!(g2.rmrr_unresolved, 0);
        assert_eq!(audit(&g2), 0);
    }

    /// Ein Geraet ohne zustaendige Einheit ist ausgeschlossen (`NoUnit`) und darf kein Bit
    /// setzen -- `usize::MAX` als Index waere sonst ein Schiebefehler.
    #[test]
    fn geraet_ohne_einheit_setzt_kein_bit() {
        let mut info = DmarInfo::EMPTY;
        info.n_units = 1;
        info.units[0].reg_base = 0xfed9_0000;
        // Kein Scope, kein INCLUDE_PCI_ALL -> niemand ist zustaendig.
        let topo = [DevNode { bus: 0, dev: 2, func: 0, ..DevNode::EMPTY }];
        let g = build_groups(&info, &topo);
        assert_eq!(g.excluded[0], Some(Exclusion::NoUnit));
        assert_eq!(units_scoping_allocatable(&g), 0);
    }
}
