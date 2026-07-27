//! **Selbsttest der DMAR-Auswertung und Gruppenbildung** (VT-d, Schritt 2).
//!
//! Gegen eine **eingespeiste** Tabelle und eine eingespeiste Topologie, nicht gegen die reale.
//! Der Grund ist derselbe, aus dem der virtio-Negativtest existiert: auf dem Standardaufbau
//! (QEMU q35, alles auf Bus 0, keine RMRR) laufen der Ausschlusspfad und die interessanten
//! Gruppenfälle **nie**. Ein Test gegen die reale Topologie wäre grün, weil sein Antezedens
//! falsch ist — und damit dieselbe Klasse wie ein Oracle, das nichts prüft.
//!
//! Geprüft werden die drei Stellen, an denen die Auswertung still falsch wird:
//! * `INCLUDE_PCI_ALL` greift **zuletzt** (die synthetische Tabelle listet die Catch-all-Einheit
//!   absichtlich **zuerst** — mit „erster Treffer gewinnt" landete das Gerät in der falschen
//!   Einheit, und mit nur einer Einheit fiele das nie auf).
//! * Scope-Typ 2 ist eine **Subhierarchie**, kein Gerät.
//! * Firmware-Eingabe: Prüfsumme, Länge 0, unbekannte Typen.

use sel4lake_hal::dmar::{self, DevNode, DmarInfo, Exclusion, Scope};
use sel4lake_hal::println;

/// Ein Scope-Eintrag in Bytes.
fn put_scope(buf: &mut [u8], at: usize, kind: u8, start_bus: u8, path: &[(u8, u8)]) -> usize {
    let len = 6 + 2 * path.len();
    buf[at] = kind;
    buf[at + 1] = len as u8;
    buf[at + 5] = start_bus;
    for (i, &(d, f)) in path.iter().enumerate() {
        buf[at + 6 + 2 * i] = d;
        buf[at + 7 + 2 * i] = f;
    }
    len
}

fn put_u16(buf: &mut [u8], at: usize, v: u16) {
    buf[at..at + 2].copy_from_slice(&v.to_le_bytes());
}
fn put_u64(buf: &mut [u8], at: usize, v: u64) {
    buf[at..at + 8].copy_from_slice(&v.to_le_bytes());
}

/// Die synthetische DMAR bauen. Reihenfolge bewusst: **Catch-all zuerst**, explizit danach.
fn build_table(buf: &mut [u8], with_unknown: bool, zero_len_elem: bool) -> usize {
    buf.fill(0);
    buf[..4].copy_from_slice(b"DMAR");
    let mut off = 48;

    // DRHD #0: INCLUDE_PCI_ALL (Catch-all).
    put_u16(buf, off, 0);
    put_u16(buf, off + 2, 16);
    buf[off + 4] = 1; // Flags: INCLUDE_PCI_ALL
    put_u64(buf, off + 8, 0xfed9_1000);
    off += 16;

    // DRHD #1: expliziter Scope, Typ 2 (Bridge 00:01.0) -> deckt die ganze Subhierarchie.
    let drhd1 = off;
    put_u16(buf, off, 0);
    put_u64(buf, off + 8, 0xfed9_0000);
    let sl = put_scope(buf, off + 16, dmar::SCOPE_BRIDGE, 0, &[(1, 0)]);
    put_u16(buf, drhd1 + 2, (16 + sl) as u16);
    off += 16 + sl;

    if with_unknown {
        // Unbekannter Strukturtyp (SATC/SIDP-Klasse): muss per Länge übersprungen werden.
        put_u16(buf, off, 0x00ff);
        put_u16(buf, off + 2, 8);
        off += 8;
    }

    // RMRR mit Scope auf 00:05.0 -> dieses Gerät ist nicht zuteilbar.
    let rmrr = off;
    put_u16(buf, off, 1);
    let sl = put_scope(buf, off + 24, dmar::SCOPE_ENDPOINT, 0, &[(5, 0)]);
    put_u16(buf, rmrr + 2, (24 + sl) as u16);
    off += 24 + sl;

    if zero_len_elem {
        put_u16(buf, off, 0);
        put_u16(buf, off + 2, 0); // Länge 0 -> Endlosschleife, wenn nicht abgefangen
        off += 4;
    }

    put_u16(buf, 4, off as u16);
    let sum = buf[..off].iter().fold(0u8, |a, &x| a.wrapping_add(x));
    buf[9] = (0u8).wrapping_sub(sum);
    off
}

/// Die synthetische Topologie.
fn build_topo(t: &mut [DevNode; 11]) {
    let ep = |bus: u8, dev: u8, func: u8, parent: usize, acs: bool| DevNode {
        bus,
        dev,
        func,
        acs,
        parent,
        ..DevNode::EMPTY
    };
    let br = |dev: u8, sec: u8, pcie: bool, acs: bool| DevNode {
        bus: 0,
        dev,
        func: 0,
        bridge: true,
        sec_bus: sec,
        sub_bus: sec,
        pcie,
        acs,
        ..DevNode::EMPTY
    };
    t[0] = br(1, 1, true, true); //   00:01.0 Root Port MIT ACS
    t[1] = ep(1, 0, 0, 0, true); //   01:00.0 Endpunkt darunter
    t[2] = br(2, 2, true, false); //  00:02.0 Root Port OHNE ACS
    t[3] = ep(2, 0, 0, 2, true); //   02:00.0 \ muessen eine Gruppe sein
    t[4] = ep(2, 1, 0, 2, true); //   02:01.0 /
    t[5] = br(3, 3, false, false); // 00:03.0 KONVENTIONELLE Bridge -> Aliasing
    t[6] = ep(3, 1, 0, 5, true); //   03:01.0 \ tragen die RID der Bridge
    t[7] = ep(3, 2, 0, 5, true); //   03:02.0 /
    t[8] = DevNode {
        multifunction: true,
        ..ep(0, 4, 0, usize::MAX, false)
    }; // 00:04.0 \ Multifunktion ohne ACS
    t[9] = DevNode {
        multifunction: true,
        ..ep(0, 4, 1, usize::MAX, false)
    }; // 00:04.1 /
    t[10] = ep(0, 5, 0, usize::MAX, true); // 00:05.0 RMRR-behaftet
}

/// Ergebnis des Selbsttests (für den Bericht).
pub struct SelfTest {
    pub parse_ok: bool,
    pub catch_all_last: bool,
    pub bridge_scope_subtree: bool,
    pub groups_ok: bool,
    pub alias_ok: bool,
    pub rmrr_excluded: bool,
    pub malformed_caught: bool,
    pub audit: u32,
}

impl SelfTest {
    pub fn ok(&self) -> bool {
        self.parse_ok
            && self.catch_all_last
            && self.bridge_scope_subtree
            && self.groups_ok
            && self.alias_ok
            && self.rmrr_excluded
            && self.malformed_caught
            && self.audit == 0
    }
}

pub fn run() -> SelfTest {
    let mut buf = [0u8; 512];
    let n = build_table(&mut buf, true, false);
    let info: DmarInfo = dmar::parse(&buf[..n]);
    let parse_ok = !info.malformed
        && !info.truncated
        && info.n_units == 2
        && info.n_rmrr == 1
        && info.units[0].include_all
        && !info.units[1].include_all;

    let mut topo = [DevNode::EMPTY; 11];
    build_topo(&mut topo);
    let g = dmar::build_groups(&info, &topo);

    // Der explizite Bridge-Scope (Einheit 1) muss den Catch-all (Einheit 0) schlagen — obwohl
    // der Catch-all in der Tabelle ZUERST steht.
    let catch_all_last = g.unit_of[0] == 1 && g.unit_of[10] == 0;
    // ... und er deckt die ganze Subhierarchie, nicht nur die Bridge selbst.
    let bridge_scope_subtree = g.unit_of[1] == 1;

    let same = |a: usize, b: usize| g.group_of[a] == g.group_of[b];
    let groups_ok = same(3, 4) //          Root Port ohne ACS: alles darunter eine Gruppe
        && same(2, 3) //                   ... inklusive der Bridge selbst
        && same(5, 6) && same(6, 7) //     konventionelle Bridge + Geraete darunter
        && same(8, 9) //                   Multifunktion ohne ACS
        && !same(1, 3) //                  Root Port MIT ACS trennt
        && !same(10, 8); //                unabhaengige Geraete bleiben getrennt

    // Die Gruppe hinter der konventionellen Bridge traegt deren RID als Alias — eine Zuteilung
    // muesste Kontexteintraege fuer ALLE RIDs der Gruppe schreiben.
    let gi = g.group_of[6];
    let bridge_rid = topo[5].rid();
    let alias_ok = g.aliases[gi][..g.n_aliases[gi]].contains(&bridge_rid)
        && g.n_aliases[gi] >= 3
        && g.n_aliases[g.group_of[1]] == 1;

    let rmrr_excluded = g.excluded[10] == Some(Exclusion::Rmrr) && g.excluded[1].is_none();

    // Firmware-Eingabe: kaputte Pruefsumme, Laenge 0, zu kurze Tabelle.
    let mut bad = buf;
    bad[9] = bad[9].wrapping_add(1);
    let cs = dmar::parse(&bad[..n]).malformed;
    let n2 = build_table(&mut bad, false, true);
    let zl = dmar::parse(&bad[..n2]); // darf nicht haengen
    let short = dmar::parse(&buf[..20]).malformed;
    let malformed_caught = cs && zl.malformed && short && zl.n_units == 2;

    SelfTest {
        parse_ok,
        catch_all_last,
        bridge_scope_subtree,
        groups_ok,
        alias_ok,
        rmrr_excluded,
        malformed_caught,
        audit: dmar::audit(&g),
    }
}

/// Die **reale** Topologie auswerten und berichten — inklusive der Liste der Ausschlüsse.
///
/// Die gehört ins Log wie die undeklarierten Adressbreiten: geführt statt weggelassen. Was hier
/// als „0 Ausschlüsse" erscheint, ist eine Aussage über *diese* Plattform, keine über den Code —
/// dafür ist der Selbsttest oben zuständig.
pub fn report_real() -> (usize, usize, usize) {
    let Some(tbl) = sel4lake_hal::acpi::dmar_table() else {
        return (0, 0, 0);
    };
    let info = dmar::parse(tbl);
    let mut topo = [DevNode::EMPTY; dmar::MAX_DEVS];
    let n = sel4lake_hal::pcie::read_topology(&mut topo);
    let g = dmar::build_groups(&info, &topo[..n]);
    let mut excluded = 0;
    for i in 0..g.n_devs {
        if let Some(why) = g.excluded[i] {
            excluded += 1;
            println!(
                "vtdgrp  : AUSGESCHLOSSEN {:02x}:{:02x}.{} -- {:?}",
                topo[i].bus,
                topo[i].dev,
                topo[i].func,
                why
            );
        }
    }
    println!(
        "vtdgrp  : real: {} Geraet(e), {} Einheit(en), {} Gruppe(n), {} ausgeschlossen, ATSR-Eintraege {}, Segment!=0 {}, Oracle {}",
        g.n_devs,
        info.n_units,
        g.n_groups,
        excluded,
        info.n_atsr,
        info.nonzero_segment,
        dmar::audit(&g)
    );
    let _ = Scope::EMPTY;
    (g.n_devs, g.n_groups, excluded)
}
