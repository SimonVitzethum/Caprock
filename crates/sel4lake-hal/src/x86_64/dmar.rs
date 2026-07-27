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

/// Ist das Gerät `i` von einer RMRR betroffen?
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
    /// RMRR-behaftet (s. `DmarInfo::rmrr`).
    Rmrr,
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

    // Einheiten + Ausschlüsse.
    for i in 0..n {
        match unit_of(info, topo, i) {
            Some(u) => g.unit_of[i] = u,
            None => g.excluded[i] = Some(Exclusion::NoUnit),
        }
        if has_rmrr(info, topo, i) {
            g.excluded[i] = Some(Exclusion::Rmrr);
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
    0
}
