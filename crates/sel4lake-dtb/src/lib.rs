#![no_std]
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
