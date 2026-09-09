#![no_std]
#![forbid(unsafe_code)]
//! Minimaler Parser für einen Flattened Device Tree (FDT/DTB).
//!
//! Liest die Plattformbeschreibung (hier: die RAM-Region aus dem `/memory`-
//! Knoten) statt sie fest zu verdrahten. Reines, **sicheres** Parsen über einen
//! `&[u8]`-Slice (alle Zugriffe bounds-checked, **kein `unsafe`**).
//!
//! In einem echten System übergibt der Bootloader den DTB-Zeiger (aarch64: `x0`).
//! Da QEMU für ein rohes `-kernel`-ELF (ohne Linux-Image-Header) keinen DTB
//! ablegt, betten wir den von QEMU erzeugten DTB ein und parsen ihn — der Parser
//! arbeitet auf einem echten Device Tree.

const MAGIC: u32 = 0xd00d_feed;
const FDT_BEGIN_NODE: u32 = 1;
const FDT_END_NODE: u32 = 2;
const FDT_PROP: u32 = 3;
const FDT_NOP: u32 = 4;
const FDT_END: u32 = 9;

/// Geparster Device Tree.
pub struct Dtb<'a> {
    data: &'a [u8],
    off_struct: usize,
    off_strings: usize,
}

fn be32(data: &[u8], off: usize) -> Option<u32> {
    let b = data.get(off..off + 4)?;
    Some(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

fn be64(data: &[u8], off: usize) -> Option<u64> {
    let hi = be32(data, off)? as u64;
    let lo = be32(data, off + 4)? as u64;
    Some((hi << 32) | lo)
}

/// Null-terminierten String ab `off` zurückgeben (ohne das `\0`).
fn cstr(data: &[u8], off: usize) -> &[u8] {
    let rest = match data.get(off..) {
        Some(r) => r,
        None => return &[],
    };
    let end = rest.iter().position(|&c| c == 0).unwrap_or(rest.len());
    &rest[..end]
}

const fn align4(n: usize) -> usize {
    (n + 3) & !3
}

impl<'a> Dtb<'a> {
    /// Einen DTB-Slice parsen (prüft Magic + Header).
    pub fn parse(data: &'a [u8]) -> Option<Dtb<'a>> {
        if be32(data, 0)? != MAGIC {
            return None;
        }
        let off_struct = be32(data, 8)? as usize;
        let off_strings = be32(data, 12)? as usize;
        Some(Dtb {
            data,
            off_struct,
            off_strings,
        })
    }

    /// **Anzahl der CPUs** aus den `cpu@…`-Knoten unterhalb von `/cpus` (ext-30).
    ///
    /// Die Kernzahl wird damit von der Plattform *gelesen* statt fest verdrahtet — Grundlage
    /// für die zur Boot-Zeit dimensionierten Scheduler-Tabellen. Gezählt werden Knoten, deren
    /// Name mit `cpu@` beginnt **und** die direkt unter `/cpus` liegen (Tiefe 2); `cpu-map`-
    /// Untereinträge (Cluster-Topologie) heißen anders und werden nicht mitgezählt.
    /// `None`, wenn der Baum unlesbar ist; `Some(0)`, wenn es keine `cpu@`-Knoten gibt.
    pub fn cpu_count(&self) -> Option<usize> {
        let mut pos = self.off_struct;
        let mut depth = 0usize;
        let mut in_cpus_at = usize::MAX; // Tiefe des `/cpus`-Knotens
        let mut n = 0usize;
        loop {
            let tok = be32(self.data, pos)?;
            pos += 4;
            match tok {
                FDT_BEGIN_NODE => {
                    let name = cstr(self.data, pos);
                    pos += align4(name.len() + 1);
                    depth += 1;
                    if depth == 2 && name == b"cpus" {
                        in_cpus_at = depth;
                    } else if in_cpus_at != usize::MAX
                        && depth == in_cpus_at + 1
                        && name.starts_with(b"cpu@")
                    {
                        n += 1;
                    }
                }
                FDT_END_NODE => {
                    if depth == in_cpus_at {
                        in_cpus_at = usize::MAX; // `/cpus` verlassen
                    }
                    depth = depth.saturating_sub(1);
                }
                FDT_PROP => {
                    let len = be32(self.data, pos)? as usize;
                    pos = pos + 8 + align4(len);
                }
                FDT_NOP => {}
                FDT_END => return Some(n),
                _ => return None,
            }
        }
    }

    /// **NUMA affinities from the device tree** (Z8/N0) — the aarch64 counterpart to ACPI SRAT.
    ///
    /// Calls `mem(base, size, node)` for every `memory@…` node that carries a `numa-node-id`, and
    /// `cpu(index, node)` for every `cpu@…` node under `/cpus` that does. Returns the number of
    /// affinities reported, or `None` if the tree is unreadable.
    ///
    /// **Callbacks rather than a returned structure, on purpose.** This crate is dependency-free
    /// and stays that way; the classification (what an unaffiliated range means, what happens when
    /// storage runs out) belongs in one place for both architectures, and that place is
    /// `caprock_hal::numa`. Two crates deciding independently what a node is would be two designs.
    ///
    /// The CPU index is the **ordinal** of the `cpu@` node under `/cpus`, which is what
    /// `MPIDR_EL1.Aff0` reports on QEMU `virt` — the same identity `hal::cpu::core_id` uses. On a
    /// machine where those differ this needs the `reg` property instead, and it would be wrong
    /// silently; that is why the report line prints how many CPU affinities were matched.
    pub fn numa(
        &self,
        mut mem: impl FnMut(u64, u64, u32),
        mut cpu: impl FnMut(u32, u32),
    ) -> Option<usize> {
        let mut pos = self.off_struct;
        let mut depth = 0usize;
        let mut in_cpus_at = usize::MAX;
        // Zustand des GERADE offenen Knotens: `reg` und `numa-node-id` koennen in beliebiger
        // Reihenfolge kommen, also erst am `FDT_END_NODE` auswerten.
        let mut is_mem = false;
        let mut is_cpu = false;
        let mut cpu_ord = 0u32;
        let mut this_cpu_ord = 0u32;
        let mut reg: Option<(u64, u64)> = None;
        let mut node_id: Option<u32> = None;
        let mut n = 0usize;
        loop {
            let tok = be32(self.data, pos)?;
            pos += 4;
            match tok {
                FDT_BEGIN_NODE => {
                    let name = cstr(self.data, pos);
                    pos += align4(name.len() + 1);
                    depth += 1;
                    if depth == 2 && name == b"cpus" {
                        in_cpus_at = depth;
                    }
                    is_mem = name.starts_with(b"memory");
                    is_cpu = in_cpus_at != usize::MAX
                        && depth == in_cpus_at + 1
                        && name.starts_with(b"cpu@");
                    if is_cpu {
                        this_cpu_ord = cpu_ord;
                        cpu_ord += 1;
                    }
                    reg = None;
                    node_id = None;
                }
                FDT_END_NODE => {
                    if let Some(nd) = node_id {
                        if is_mem {
                            if let Some((b, l)) = reg {
                                mem(b, l, nd);
                                n += 1;
                            }
                        } else if is_cpu {
                            cpu(this_cpu_ord, nd);
                            n += 1;
                        }
                    }
                    if depth == in_cpus_at {
                        in_cpus_at = usize::MAX;
                    }
                    depth = depth.saturating_sub(1);
                    is_mem = false;
                    is_cpu = false;
                    reg = None;
                    node_id = None;
                }
                FDT_PROP => {
                    let len = be32(self.data, pos)? as usize;
                    let nameoff = be32(self.data, pos + 4)? as usize;
                    let val = pos + 8;
                    pos = val + align4(len);
                    let pname = cstr(self.data, self.off_strings + nameoff);
                    if pname == b"numa-node-id" && len >= 4 {
                        node_id = Some(be32(self.data, val)?);
                    } else if pname == b"reg" && is_mem && len >= 16 {
                        reg = Some((be64(self.data, val)?, be64(self.data, val + 8)?));
                    }
                }
                FDT_NOP => {}
                FDT_END => return Some(n),
                _ => return None,
            }
        }
    }

    /// Die erste RAM-Region aus dem `/memory`-Knoten: `(base, size)`.
    /// Annahme: `#address-cells = #size-cells = 2` (Standard für QEMU `virt`).
    pub fn memory(&self) -> Option<(u64, u64)> {
        let mut pos = self.off_struct;
        let mut in_memory = false;
        loop {
            let tok = be32(self.data, pos)?;
            pos += 4;
            match tok {
                FDT_BEGIN_NODE => {
                    let name = cstr(self.data, pos);
                    in_memory = name.starts_with(b"memory");
                    pos += align4(name.len() + 1);
                }
                FDT_END_NODE => in_memory = false,
                FDT_PROP => {
                    let len = be32(self.data, pos)? as usize;
                    let nameoff = be32(self.data, pos + 4)? as usize;
                    let val = pos + 8;
                    pos = val + align4(len);
                    if in_memory && cstr(self.data, self.off_strings + nameoff) == b"reg" {
                        let base = be64(self.data, val)?;
                        let size = be64(self.data, val + 8)?;
                        return Some((base, size));
                    }
                }
                FDT_NOP => {}
                FDT_END => return None,
                _ => return None,
            }
        }
    }
}
