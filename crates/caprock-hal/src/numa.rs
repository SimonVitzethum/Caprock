//! **NUMA topology: reading it, and the placement rules built on it** (Z8 / N0–N2, 2026-08-17).
//!
//! Pure over injected bytes, like `dmar.rs`, `smt.rs` and `iommu_health.rs` — and here the reason
//! is sharper than usual. The development machine has exactly **one** node, so the interesting
//! cases cannot arise on it at all; a test against the real firmware table would be an oracle that
//! holds because its antecedent is false. Every decoder below therefore takes a byte slice.
//!
//! ## Three rules that are the content of this module
//!
//! **1. Unaffiliated is not node 0.** SRAT need not cover all of RAM, and firmware routinely
//! leaves ranges out. Assigning those to node 0 would make "nobody classified this" and
//! "classified as node 0" indistinguishable — the `Unknown`-as-`Single` failure from `smt.rs`, and
//! the `NOSEL_TEXT` failure before it. Unaffiliated memory gets [`Node::Unaffiliated`], is counted
//! in bytes, and is used **last**.
//!
//! **2. Truncated is not complete.** The tables are firmware data of unbounded size and the
//! storage here is fixed. A topology that dropped entries is **not** the machine's topology, and
//! [`Topology::trustworthy`] says so — a picture with holes must not drive placement decisions
//! that assume it is whole.
//!
//! **3. The yield order is named, not emergent.** Colour, zone and node all constrain the *same*
//! physical address, so with three axes the constraint set is regularly empty and the fallback
//! decides everything. See [`YieldOrder`].

// -------------------------------------------------------------------------------------------
// Types
// -------------------------------------------------------------------------------------------

/// Nodes tracked. Beyond this the topology is [`Topology::truncated`] rather than silently folded.
pub const MAX_NODES: usize = 8;
/// Memory ranges tracked.
pub const MAX_RANGES: usize = 32;
/// CPU→node affinities tracked.
pub const MAX_CPU_AFFINITIES: usize = 64;

/// Which node something belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Node {
    At(u8),
    /// No table entry covers it. **Not the same as node 0** — see the module doc.
    Unaffiliated,
}

/// One physical memory range with its node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemRange {
    pub base: u64,
    pub len: u64,
    pub node: u8,
}

impl MemRange {
    /// End address, saturating. `base + len` from firmware data is untrusted arithmetic — S3
    /// literally, and an unprotected addition here would fold a bogus range over the whole space.
    pub fn end(&self) -> u64 {
        self.base.saturating_add(self.len)
    }
    pub fn contains(&self, pa: u64) -> bool {
        pa >= self.base && pa < self.end()
    }
}

/// What the firmware said. Fixed storage; overflow sets [`Topology::truncated`].
#[derive(Clone, Copy)]
pub struct Topology {
    ranges: [MemRange; MAX_RANGES],
    n_ranges: usize,
    cpus: [(u32, u8); MAX_CPU_AFFINITIES],
    n_cpus: usize,
    /// Highest node id seen, plus one. `0` means nothing was decoded.
    n_nodes: usize,
    /// Entries were dropped for lack of storage, or a node id exceeded [`MAX_NODES`].
    pub truncated: bool,
    /// A topology table was found **and** parsed. `false` is a finding, not a default.
    pub readable: bool,
    /// The distance matrix, if a SLIT was present. `10` is the ACPI value for "local".
    distances: [[u8; MAX_NODES]; MAX_NODES],
    pub distances_present: bool,
}

impl Default for Topology {
    fn default() -> Self {
        Self::new()
    }
}

impl Topology {
    pub const fn new() -> Self {
        Self {
            ranges: [MemRange { base: 0, len: 0, node: 0 }; MAX_RANGES],
            n_ranges: 0,
            cpus: [(0, 0); MAX_CPU_AFFINITIES],
            n_cpus: 0,
            n_nodes: 0,
            truncated: false,
            readable: false,
            distances: [[0; MAX_NODES]; MAX_NODES],
            distances_present: false,
        }
    }

    /// **May this topology drive placement?**
    ///
    /// Readable *and* whole. A truncated picture is the more dangerous of the two failures: it
    /// looks like an answer. Placing by it would put memory on "node 0" simply because the entry
    /// naming its real node was the one that did not fit.
    pub fn trustworthy(&self) -> bool {
        self.readable && !self.truncated && self.n_nodes > 0
    }

    pub fn node_count(&self) -> usize {
        self.n_nodes
    }
    pub fn range_count(&self) -> usize {
        self.n_ranges
    }
    pub fn cpu_count(&self) -> usize {
        self.n_cpus
    }

    /// Total bytes covered by *some* node.
    pub fn covered_bytes(&self) -> u64 {
        self.ranges[..self.n_ranges].iter().fold(0u64, |a, r| a.saturating_add(r.len))
    }

    /// Which node holds this physical address?
    pub fn node_of_pa(&self, pa: u64) -> Node {
        for r in &self.ranges[..self.n_ranges] {
            if r.contains(pa) {
                return Node::At(r.node);
            }
        }
        Node::Unaffiliated
    }

    /// Which node does this logical CPU sit on? (x86: LAPIC/x2APIC id, aarch64: processor uid.)
    pub fn node_of_cpu(&self, id: u32) -> Node {
        for &(cid, n) in &self.cpus[..self.n_cpus] {
            if cid == id {
                return Node::At(n);
            }
        }
        Node::Unaffiliated
    }

    /// The `i`-th memory range of `node`, as an allocation window `[base, end)`.
    ///
    /// A node is a **set** of ranges, not one — the usual layout gives node 0 both a low chunk and
    /// one above 4 GiB. The allocator already takes a window (`alloc_colored_in`), so a node is
    /// expressed by iterating its windows rather than by a second allocator.
    pub fn window_of(&self, node: u8, i: usize) -> Option<(u64, u64)> {
        self.ranges[..self.n_ranges]
            .iter()
            .filter(|r| r.node == node && r.len > 0)
            .nth(i)
            .map(|r| (r.base, r.end()))
    }

    /// Distance from `a` to `b` (ACPI: 10 = local). `None` without a SLIT.
    pub fn distance(&self, a: u8, b: u8) -> Option<u8> {
        if !self.distances_present || a as usize >= MAX_NODES || b as usize >= MAX_NODES {
            return None;
        }
        Some(self.distances[a as usize][b as usize])
    }

    /// Feed one memory range from a source that is not ACPI (aarch64 device tree, Z8/N0).
    ///
    /// Public so a second source can fill the **same** type with the **same** truncation rules.
    /// Two sources with two notions of "node" would be two designs — the reason `caprock-dtb`
    /// reports through callbacks instead of building a structure of its own.
    pub fn add_range(&mut self, base: u64, len: u64, node: u32) {
        if len > 0 {
            self.push_range(base, len, node);
        }
    }

    /// Feed one CPU affinity from a non-ACPI source.
    pub fn add_cpu(&mut self, id: u32, node: u32) {
        self.push_cpu(id, node);
    }

    /// Mark that a topology source was found and produced entries.
    ///
    /// Separate from [`add_range`](Self::add_range) on purpose: a source may exist and describe
    /// nothing, and "found a table" is a different fact from "learned something from it".
    pub fn mark_readable(&mut self) {
        self.readable = true;
    }

    fn push_range(&mut self, base: u64, len: u64, node: u32) {
        if node as usize >= MAX_NODES || self.n_ranges >= MAX_RANGES {
            self.truncated = true;
            return;
        }
        self.ranges[self.n_ranges] = MemRange { base, len, node: node as u8 };
        self.n_ranges += 1;
        self.n_nodes = self.n_nodes.max(node as usize + 1);
    }

    fn push_cpu(&mut self, id: u32, node: u32) {
        if node as usize >= MAX_NODES || self.n_cpus >= MAX_CPU_AFFINITIES {
            self.truncated = true;
            return;
        }
        self.cpus[self.n_cpus] = (id, node as u8);
        self.n_cpus += 1;
        self.n_nodes = self.n_nodes.max(node as usize + 1);
    }
}

// -------------------------------------------------------------------------------------------
// ACPI SRAT / SLIT
// -------------------------------------------------------------------------------------------

fn rd_u16(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes([*b.get(o)?, *b.get(o + 1)?]))
}
fn rd_u32(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes([*b.get(o)?, *b.get(o + 1)?, *b.get(o + 2)?, *b.get(o + 3)?]))
}
fn rd_u64(b: &[u8], o: usize) -> Option<u64> {
    Some(u64::from(rd_u32(b, o)?) | (u64::from(rd_u32(b, o + 4)?) << 32))
}

/// SRAT subtable types (ACPI 6.5, table 5.62).
const SRAT_CPU_APIC: u8 = 0;
const SRAT_MEMORY: u8 = 1;
const SRAT_CPU_X2APIC: u8 = 2;
const SRAT_GICC: u8 = 3;

/// Decode an ACPI **SRAT** into `t`. Returns `false` if the table is unusable.
///
/// The `flags` bit 0 ("enabled") is honoured on every entry type: a disabled entry describes
/// hardware that is not there, and folding it in would claim memory the machine does not have.
///
/// **A broken chain aborts rather than guessing on** — the same rule as `dmar_unit_base` and
/// `acpi::cpus`. Continuing past a bad length re-synchronises on arbitrary bytes, and firmware
/// garbage that decodes as a plausible range is worse than no range at all.
pub fn decode_srat(b: &[u8], t: &mut Topology) -> bool {
    // Header 36 + reserved u32 (=1) + reserved u64 -> entries at 48.
    if b.len() < 48 || &b[0..4] != b"SRAT" {
        return false;
    }
    let mut off = 48usize;
    let mut seen = 0usize;
    while off + 2 <= b.len() {
        let etype = b[off];
        let elen = b[off + 1] as usize;
        if elen < 2 || off + elen > b.len() {
            break; // defekte Kette -> abbrechen statt weiterzuraten
        }
        let e = &b[off..off + elen];
        match etype {
            SRAT_MEMORY if elen >= 40 => {
                let flags = rd_u32(e, 28).unwrap_or(0);
                if flags & 1 != 0 {
                    let dom = rd_u32(e, 2).unwrap_or(0);
                    let base = rd_u64(e, 8).unwrap_or(0);
                    let len = rd_u64(e, 16).unwrap_or(0);
                    if len > 0 {
                        t.push_range(base, len, dom);
                        seen += 1;
                    }
                }
            }
            SRAT_CPU_APIC if elen >= 16 => {
                let flags = rd_u32(e, 4).unwrap_or(0);
                if flags & 1 != 0 {
                    // Die Proximity-Domain steht in VIER Bytes an ZWEI Stellen: das niedrigste
                    // in Byte 2, die oberen drei in 9..12. Ein Dekoder, der nur Byte 2 liest,
                    // ist auf jeder Maschine mit <256 Domains zufaellig richtig.
                    let lo = e[2] as u32;
                    let hi = (e[9] as u32) | ((e[10] as u32) << 8) | ((e[11] as u32) << 16);
                    t.push_cpu(e[3] as u32, lo | (hi << 8));
                    seen += 1;
                }
            }
            SRAT_CPU_X2APIC if elen >= 24 => {
                let flags = rd_u32(e, 12).unwrap_or(0);
                if flags & 1 != 0 {
                    let dom = rd_u32(e, 4).unwrap_or(0);
                    let id = rd_u32(e, 8).unwrap_or(0);
                    t.push_cpu(id, dom);
                    seen += 1;
                }
            }
            SRAT_GICC if elen >= 18 => {
                let flags = rd_u32(e, 10).unwrap_or(0);
                if flags & 1 != 0 {
                    let dom = rd_u32(e, 2).unwrap_or(0);
                    let uid = rd_u32(e, 6).unwrap_or(0);
                    t.push_cpu(uid, dom);
                    seen += 1;
                }
            }
            _ => {}
        }
        off += elen;
    }
    let _ = rd_u16(b, 0); // Hilfsfunktion bleibt benutzt, auch wenn kein Feld sie heute braucht
    if seen > 0 {
        t.readable = true;
    }
    seen > 0
}

/// Decode an ACPI **SLIT** (distance matrix) into `t`.
///
/// A matrix larger than [`MAX_NODES`] marks the topology truncated instead of taking a corner of
/// it: a corner of a distance matrix is a *different* matrix, and it would read as complete.
pub fn decode_slit(b: &[u8], t: &mut Topology) -> bool {
    if b.len() < 44 || &b[0..4] != b"SLIT" {
        return false;
    }
    let Some(n) = rd_u64(b, 36) else { return false };
    if n == 0 || b.len() < 44 + (n as usize) * (n as usize) {
        return false;
    }
    if n as usize > MAX_NODES {
        t.truncated = true;
        return false;
    }
    for i in 0..n as usize {
        for j in 0..n as usize {
            t.distances[i][j] = b[44 + i * n as usize + j];
        }
    }
    t.distances_present = true;
    true
}

// -------------------------------------------------------------------------------------------
// The placement ladder
// -------------------------------------------------------------------------------------------

/// **Which constraint gives way when the three cannot be satisfied together.**
///
/// Colour (cache sets), zone (below/above 4 GiB) and node all restrict the *same* physical
/// address. With three axes the satisfiable set is regularly empty, and then the fallback is the
/// policy — so it is named here rather than emerging from whichever loop happens to run first.
///
/// * **Zone never yields.** It is correctness, not preference: `vspace_map_page_at` refuses
///   `va >= GIB1_END`, so a page from the wrong zone cannot be mapped at all.
/// * **Node yields first.** It is performance. Off-node memory is slower, not wrong.
/// * **Colour yields last.** It is the A1 isolation claim. A silent colour yield would be the
///   `MASK_BITS` failure again — green because nothing was separated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Everything held: requested node, requested colour.
    Exact,
    /// The node gave way — right colour, wrong node.
    OffNode,
    /// The colour gave way too. **Reportable**, because it weakens A1.
    OffNodeUncolored,
}

/// The order attempts are made in. `Exact` first, and the caller stops at the first success.
pub const YIELD_ORDER: [Step; 3] = [Step::Exact, Step::OffNode, Step::OffNodeUncolored];

/// Counters for what actually happened — **by effect, not by attempt**.
///
/// The E-Rest 3b fallback counter reported `1x` on a 512 MiB machine that had no high memory at
/// all: it had counted an oversized request that fitted nowhere. "There was no room on the node"
/// and "it was taken from another node" are two statements, and only the second one is a fact
/// about placement.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PlacementStats {
    /// Allocations that got exactly what they asked for.
    pub exact: u64,
    /// Allocations served from a different node than requested.
    pub off_node: u64,
    /// Allocations that additionally lost their colour.
    pub off_node_uncolored: u64,
    /// Requests that could not be served at all.
    pub refused: u64,
}

impl PlacementStats {
    pub fn record(&mut self, step: Option<Step>) {
        match step {
            Some(Step::Exact) => self.exact += 1,
            Some(Step::OffNode) => self.off_node += 1,
            Some(Step::OffNodeUncolored) => self.off_node_uncolored += 1,
            None => self.refused += 1,
        }
    }
    pub fn total(&self) -> u64 {
        self.exact + self.off_node + self.off_node_uncolored + self.refused
    }
    /// **Did placement do anything at all?** A run in which nothing was ever placed on a node
    /// cannot say whether placement works — the same reason `pdbind` asks "was a PD bound at all"
    /// rather than "did the rare failure happen".
    pub fn speaking(&self) -> bool {
        self.total() > 0
    }
    /// Nothing lost its colour. A colour yield is not an error, but it must be *visible*.
    pub fn colour_intact(&self) -> bool {
        self.off_node_uncolored == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- table builders (so the tests inject bytes, never a machine) -------------------------

    fn srat_header(entries: &[Vec<u8>]) -> Vec<u8> {
        let mut v = vec![0u8; 48];
        v[0..4].copy_from_slice(b"SRAT");
        for e in entries {
            v.extend_from_slice(e);
        }
        let len = v.len() as u32;
        v[4..8].copy_from_slice(&len.to_le_bytes());
        v
    }

    fn mem_entry(base: u64, len: u64, dom: u32, enabled: bool) -> Vec<u8> {
        let mut e = vec![0u8; 40];
        e[0] = SRAT_MEMORY;
        e[1] = 40;
        e[2..6].copy_from_slice(&dom.to_le_bytes());
        e[8..16].copy_from_slice(&base.to_le_bytes());
        e[16..24].copy_from_slice(&len.to_le_bytes());
        e[28..32].copy_from_slice(&(enabled as u32).to_le_bytes());
        e
    }

    fn x2apic_entry(id: u32, dom: u32, enabled: bool) -> Vec<u8> {
        let mut e = vec![0u8; 24];
        e[0] = SRAT_CPU_X2APIC;
        e[1] = 24;
        e[4..8].copy_from_slice(&dom.to_le_bytes());
        e[8..12].copy_from_slice(&id.to_le_bytes());
        e[12..16].copy_from_slice(&(enabled as u32).to_le_bytes());
        e
    }

    fn apic_entry(id: u8, dom: u32, enabled: bool) -> Vec<u8> {
        let mut e = vec![0u8; 16];
        e[0] = SRAT_CPU_APIC;
        e[1] = 16;
        e[2] = (dom & 0xFF) as u8;
        e[3] = id;
        e[4..8].copy_from_slice(&(enabled as u32).to_le_bytes());
        e[9] = ((dom >> 8) & 0xFF) as u8;
        e[10] = ((dom >> 16) & 0xFF) as u8;
        e[11] = ((dom >> 24) & 0xFF) as u8;
        e
    }

    fn slit(n: usize, vals: &[u8]) -> Vec<u8> {
        let mut v = vec![0u8; 44];
        v[0..4].copy_from_slice(b"SLIT");
        v[36..44].copy_from_slice(&(n as u64).to_le_bytes());
        v.extend_from_slice(vals);
        v
    }

    // --- decoding ---------------------------------------------------------------------------

    #[test]
    fn two_nodes_with_memory_and_cpus_are_read() {
        let t0 = srat_header(&[
            mem_entry(0, 1 << 30, 0, true),
            mem_entry(1 << 30, 1 << 30, 1, true),
            x2apic_entry(0, 0, true),
            x2apic_entry(2, 1, true),
        ]);
        let mut t = Topology::new();
        assert!(decode_srat(&t0, &mut t));
        assert!(t.trustworthy());
        assert_eq!(t.node_count(), 2);
        assert_eq!(t.node_of_pa(0), Node::At(0));
        assert_eq!(t.node_of_pa((1 << 30) + 4096), Node::At(1));
        assert_eq!(t.node_of_cpu(2), Node::At(1));
    }

    /// **Rule 1.** An address no entry covers is `Unaffiliated`, never node 0.
    #[test]
    fn an_uncovered_address_is_unaffiliated_and_not_node_zero() {
        let t0 = srat_header(&[mem_entry(0, 1 << 30, 0, true)]);
        let mut t = Topology::new();
        assert!(decode_srat(&t0, &mut t));
        assert_eq!(t.node_of_pa(4 << 30), Node::Unaffiliated);
        assert_ne!(t.node_of_pa(4 << 30), Node::At(0));
    }

    #[test]
    fn a_disabled_entry_describes_absent_hardware_and_is_skipped() {
        let t0 = srat_header(&[mem_entry(0, 1 << 30, 0, true), mem_entry(1 << 30, 1 << 30, 1, false)]);
        let mut t = Topology::new();
        assert!(decode_srat(&t0, &mut t));
        assert_eq!(t.range_count(), 1);
        assert_eq!(t.node_of_pa(1 << 30), Node::Unaffiliated);
    }

    /// Die Proximity-Domain des APIC-Eintrags steht in ZWEI Stuecken. Ein Dekoder, der nur Byte 2
    /// liest, ist auf jeder Maschine mit weniger als 256 Domains zufaellig richtig.
    #[test]
    fn the_split_proximity_domain_of_an_apic_entry_is_reassembled() {
        let t0 = srat_header(&[apic_entry(7, 0x0000_0003, true)]);
        let mut t = Topology::new();
        assert!(decode_srat(&t0, &mut t));
        assert_eq!(t.node_of_cpu(7), Node::At(3));
    }

    #[test]
    fn a_missing_or_foreign_table_is_not_readable() {
        let mut t = Topology::new();
        assert!(!decode_srat(&[], &mut t));
        assert!(!decode_srat(b"XXXX", &mut t));
        assert!(!t.readable);
        assert!(!t.trustworthy(), "unlesbar darf nicht platzieren duerfen");
    }

    /// **Rule 2.** A picture with holes must not look complete.
    #[test]
    fn overflowing_the_range_storage_marks_the_topology_untrustworthy() {
        let mut entries = Vec::new();
        for i in 0..(MAX_RANGES + 4) {
            entries.push(mem_entry((i as u64) << 20, 1 << 20, 0, true));
        }
        let mut t = Topology::new();
        assert!(decode_srat(&srat_header(&entries), &mut t));
        assert!(t.readable, "gelesen wurde ja etwas");
        assert!(t.truncated);
        assert!(!t.trustworthy(), "abgeschnitten darf nicht platzieren duerfen");
    }

    #[test]
    fn a_node_id_beyond_the_tracked_range_truncates_rather_than_folds() {
        let t0 = srat_header(&[mem_entry(0, 1 << 20, MAX_NODES as u32, true)]);
        let mut t = Topology::new();
        decode_srat(&t0, &mut t);
        assert!(t.truncated);
        assert_ne!(t.node_of_pa(0), Node::At(0), "darf nicht auf Knoten 0 zusammenfallen");
    }

    #[test]
    fn a_broken_chain_aborts_instead_of_resyncing_on_garbage() {
        let mut v = srat_header(&[mem_entry(0, 1 << 30, 0, true)]);
        v.push(SRAT_MEMORY);
        v.push(1); // Laenge 1 -> unmoeglich
        v.extend_from_slice(&[0xAA; 64]);
        let mut t = Topology::new();
        assert!(decode_srat(&v, &mut t));
        assert_eq!(t.range_count(), 1, "nach der kaputten Kette darf nichts mehr dazukommen");
    }

    #[test]
    fn a_range_whose_length_would_overflow_does_not_wrap() {
        let t0 = srat_header(&[mem_entry(u64::MAX - 0xFFF, 1 << 30, 0, true)]);
        let mut t = Topology::new();
        decode_srat(&t0, &mut t);
        assert_eq!(t.node_of_pa(0), Node::Unaffiliated, "kein Umlauf ueber 0");
    }

    // --- windows ----------------------------------------------------------------------------

    #[test]
    fn a_node_yields_all_its_windows_in_order() {
        // Der uebliche Aufbau: Knoten 0 hat einen niedrigen UND einen hohen Bereich.
        let t0 = srat_header(&[
            mem_entry(0, 2 << 30, 0, true),
            mem_entry(4 << 30, 2 << 30, 0, true),
            mem_entry(6 << 30, 2 << 30, 1, true),
        ]);
        let mut t = Topology::new();
        assert!(decode_srat(&t0, &mut t));
        assert_eq!(t.window_of(0, 0), Some((0, 2 << 30)));
        assert_eq!(t.window_of(0, 1), Some((4 << 30, 6 << 30)));
        assert_eq!(t.window_of(0, 2), None);
        assert_eq!(t.window_of(1, 0), Some((6 << 30, 8 << 30)));
    }

    // --- SLIT -------------------------------------------------------------------------------

    #[test]
    fn the_distance_matrix_is_read_and_local_is_ten() {
        let mut t = Topology::new();
        assert!(decode_slit(&slit(2, &[10, 21, 21, 10]), &mut t));
        assert_eq!(t.distance(0, 0), Some(10));
        assert_eq!(t.distance(0, 1), Some(21));
    }

    #[test]
    fn without_a_slit_there_is_no_distance_rather_than_a_guessed_one() {
        let t = Topology::new();
        assert_eq!(t.distance(0, 1), None);
    }

    #[test]
    fn a_matrix_larger_than_we_track_truncates_instead_of_taking_a_corner() {
        let n = MAX_NODES + 1;
        let mut t = Topology::new();
        assert!(!decode_slit(&slit(n, &vec![10u8; n * n]), &mut t));
        assert!(t.truncated);
    }

    // --- the ladder -------------------------------------------------------------------------

    #[test]
    fn the_yield_order_puts_zone_nowhere_node_first_and_colour_last() {
        assert_eq!(YIELD_ORDER, [Step::Exact, Step::OffNode, Step::OffNodeUncolored]);
    }

    /// **Nach WIRKUNG gezaehlt.** Ein Zaehler, der Versuche zaehlt, beantwortet die Frage nach der
    /// Platzierung nicht (E-Rest 3b).
    #[test]
    fn the_stats_count_effects_and_can_speak() {
        let mut s = PlacementStats::default();
        assert!(!s.speaking(), "ohne Platzierung darf die Zeile nichts behaupten");
        s.record(Some(Step::Exact));
        s.record(Some(Step::OffNode));
        s.record(None);
        assert!(s.speaking());
        assert_eq!((s.exact, s.off_node, s.refused), (1, 1, 1));
        assert!(s.colour_intact());
        s.record(Some(Step::OffNodeUncolored));
        assert!(!s.colour_intact(), "ein Farbverzicht muss SICHTBAR sein");
    }
}
