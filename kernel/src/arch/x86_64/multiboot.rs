//! **Multiboot1-Info lesen** (A-1.1) — Speicherplan *und* Modulliste.
//!
//! Bis hierher las der Bring-up nur die `mmap` und tat das mit hand-ausgerechneten Offsets
//! mitten im Ablauf. Die Modulliste kommt aus derselben Struktur, mit denselben Fallen (ein
//! Feld ist nur gültig, wenn sein Flag gesetzt ist), also liegt beides jetzt an einer Stelle
//! und hinter einer Schnittstelle, die die Flags **erzwingt**: wer `modules()` ruft, bekommt
//! einen leeren Iterator, wenn Bit 3 fehlt — nicht die Bytes, die zufällig dort liegen.
//!
//! Die Struktur kommt vom Bootloader und ist damit **fremde Eingabe**. Sie wird nicht
//! kopiert (sie liegt im identity-gemappten Low-Memory und wird nur gelesen), aber jeder
//! Zugriff ist gegen die von ihr selbst gemeldeten Längen geprüft, und alle Grenzfälle
//! (Zähler ohne Adresse, Adresse 0, Ende vor Anfang, Kette mit `size == 0`) enden in
//! „nichts gefunden" statt in einem Fehlzugriff.
//!
//! ## Layout (Multiboot 1.6, Abschnitt 3.3)
//! ```text
//!  0  flags:u32
//!  4  mem_lower:u32   8  mem_upper:u32
//! 12  boot_device:u32
//! 16  cmdline:u32                      (Flag Bit 2)
//! 20  mods_count:u32  24  mods_addr:u32 (Flag Bit 3)
//! 28..44 syms                          (Flag Bit 4/5)
//! 44  mmap_length:u32 48  mmap_addr:u32 (Flag Bit 6)
//! ```
//! Ein Modul-Eintrag (16 B): `mod_start:u32  mod_end:u32  string:u32  reserved:u32`.
//! Ein `mmap`-Eintrag: `size:u32  base:u64  len:u64  type:u32` — `size` zählt sich selbst
//! **nicht** mit, deshalb der `+4` beim Weiterschalten.

/// Flag-Bit 3: `mods_count`/`mods_addr` sind gültig.
const FLAG_MODS: u32 = 1 << 3;
/// Flag-Bit 6: `mmap_length`/`mmap_addr` sind gültig.
const FLAG_MMAP: u32 = 1 << 6;

/// Wie viele Module höchstens ausgewertet werden. Ein Bootloader, der mehr mitgibt, meint
/// etwas anderes als dieser Kernel — die Grenze ist bewusst klein und sichtbar, statt eine
/// unbegrenzte Schleife über fremde Daten zu laufen.
pub const MAX_MODULES: usize = 8;

/// Ein Multiboot-Modul, wie der Bootloader es abgelegt hat: `[start, end)` physisch.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Module {
    pub start: u64,
    pub end: u64,
}

impl Module {
    pub fn len(&self) -> u64 {
        self.end - self.start
    }
}

/// Ein 32-Bit-Wort aus der (identity-gemappten) Info-Struktur lesen.
///
/// SAFETY-Begründung an einer Stelle statt an sechs: `addr` liegt im ersten GiB, das vom
/// Boot-Trampolin identity-gemappt wurde, und wird ausschließlich gelesen. Der Aufrufer hält
/// sich an die Flags; ein falsch gesetztes Flag liefert Müll, aber keinen Fehlzugriff.
fn rd32(addr: u64) -> u32 {
    // SAFETY: siehe Absatz oben — lesender Zugriff auf identity-gemapptes Low-Memory.
    unsafe { core::ptr::read_volatile(addr as *const u32) }
}

fn rd64(addr: u64) -> u64 {
    // SAFETY: wie `rd32`.
    unsafe { core::ptr::read_volatile(addr as *const u64) }
}

/// Die Multiboot-Info-Struktur des Bootloaders.
#[derive(Clone, Copy)]
pub struct MultibootInfo {
    addr: u64,
    flags: u32,
}

impl MultibootInfo {
    /// Die Struktur an `addr` übernehmen. `None`, wenn kein Bootloader-Zeiger vorliegt.
    pub fn new(addr: u64) -> Option<Self> {
        if addr == 0 {
            return None;
        }
        Some(MultibootInfo { addr, flags: rd32(addr) })
    }

    /// Ende des nutzbaren RAM aus der `mmap`: die höchste Obergrenze einer als „available"
    /// (Typ 1) gemeldeten Region **oberhalb von 1 MiB** (darunter liegen BIOS-/Legacy-Bereiche).
    /// `None`, wenn kein Speicherplan vorliegt oder er unplausibel ist.
    pub fn ram_end(&self) -> Option<u64> {
        if self.flags & FLAG_MMAP == 0 {
            return None;
        }
        let mmap_len = rd32(self.addr + 44) as u64;
        let mmap_addr = rd32(self.addr + 48) as u64;
        if mmap_len == 0 || mmap_addr == 0 {
            return None;
        }
        let mut best = 0u64;
        let mut off = 0u64;
        while off + 24 <= mmap_len {
            let e = mmap_addr + off;
            let size = rd32(e) as u64;
            let base = rd64(e + 4);
            let len = rd64(e + 12);
            let kind = rd32(e + 20);
            if kind == 1 && base >= 0x10_0000 {
                if let Some(end) = base.checked_add(len) {
                    best = best.max(end);
                }
            }
            if size == 0 {
                break; // defekte Kette -> abbrechen statt weiterzuraten
            }
            off += size + 4; // `size` zählt sich selbst nicht mit
        }
        (best > 0x10_0000).then_some(best)
    }

    /// Die als **verfügbar** gemeldeten RAM-Bereiche (Typ 1, oberhalb 1 MiB) in `out` ablegen;
    /// gibt die Anzahl zurück. `0`, wenn kein Speicherplan vorliegt.
    ///
    /// **Warum das nicht dasselbe ist wie [`ram_end`].** Bis 2026-08-03 leitete der Bring-up
    /// seinen freien Speicher als **ein** `[free_base, ram_end)` ab. Auf einer Maschine, deren
    /// RAM ganz unter 4 GiB liegt, ist das richtig; sobald QEMU Speicher oberhalb 4 GiB anlegt,
    /// ist es falsch — und zwar teuer: zwischen dem RAM unter 4 GiB und dem ab 4 GiB klafft das
    /// PCI-Loch (bei `-m 3G` gemessen: `0x8000_0000..0x1_0000_0000`, zwei ganze GiB), und der
    /// Kernel meldete es dem Allokator als freies RAM. Eine Allokation dort trifft
    /// Geräteregister oder gar nichts.
    ///
    /// Der Unterschied ist eine Klasse von Fehler, die dieses Projekt kennt: **zwei Zahlen aus
    /// derselben Hand sind keine zwei Quellen.** `ram_end` beantwortet „wo hört der Speicher
    /// auf" (daran hängt die Wahl des IOVA-Fensters), diese Funktion „wo ist welcher"; die
    /// zweite Frage aus der ersten zu erraten geht genau so lange gut, wie es keine Löcher gibt.
    pub fn ram_regions(&self, out: &mut [(u64, u64)]) -> usize {
        if self.flags & FLAG_MMAP == 0 {
            return 0;
        }
        let mmap_len = rd32(self.addr + 44) as u64;
        let mmap_addr = rd32(self.addr + 48) as u64;
        if mmap_len == 0 || mmap_addr == 0 {
            return 0;
        }
        let mut n = 0usize;
        let mut off = 0u64;
        while off + 24 <= mmap_len && n < out.len() {
            let e = mmap_addr + off;
            let size = rd32(e) as u64;
            let base = rd64(e + 4);
            let len = rd64(e + 12);
            let kind = rd32(e + 20);
            if kind == 1 && base >= 0x10_0000 && len != 0 && base.checked_add(len).is_some() {
                out[n] = (base, len);
                n += 1;
            }
            if size == 0 {
                break; // defekte Kette -> abbrechen statt weiterzuraten
            }
            off += size + 4;
        }
        n
    }

    /// Die vom Bootloader gemeldeten Module, **bounds- und plausibilitätsgeprüft**, in `out`
    /// ablegen. Gibt die Zahl der übernommenen Module. Verworfen wird jeder Eintrag mit
    /// `end <= start` (leer/verdreht) — er beschriebe keinen Bereich, den man reservieren
    /// könnte.
    pub fn modules(&self, out: &mut [Module; MAX_MODULES]) -> usize {
        if self.flags & FLAG_MODS == 0 {
            return 0;
        }
        let count = rd32(self.addr + 20) as usize;
        let addr = rd32(self.addr + 24) as u64;
        if count == 0 || addr == 0 {
            return 0;
        }
        let mut n = 0usize;
        for i in 0..count.min(MAX_MODULES) {
            let e = addr + (i as u64) * 16;
            let start = rd32(e) as u64;
            let end = rd32(e + 4) as u64;
            if end <= start {
                continue;
            }
            out[n] = Module { start, end };
            n += 1;
        }
        n
    }

    /// Wie viele Module der Bootloader **behauptet** — kann größer als die übernommene Zahl
    /// sein (Grenze [`MAX_MODULES`], verworfene Einträge). Wird ausgegeben, damit eine
    /// stillschweigende Kürzung nicht wie Vollständigkeit aussieht.
    pub fn mods_claimed(&self) -> usize {
        if self.flags & FLAG_MODS == 0 {
            0
        } else {
            rd32(self.addr + 20) as usize
        }
    }
}

/// Die Bereiche `holes` aus `[base, base+len)` **ausschneiden** und die Reststücke an `emit`
/// geben (aufsteigend, Nullängen unterdrückt).
///
/// Das ist der Kern von A-1.1: die Modulbereiche müssen dem Allokator als belegt gelten,
/// **bevor** irgendetwas alloziert wird. Der billigste Weg dahin ist, sie gar nicht erst als
/// frei zu melden. Die Funktion ist absichtlich rein (kein Allokator, keine Statics), damit
/// sie hostseitig testbar ist — sie steht unten unter `#[cfg(test)]` genau so unter Test.
pub fn subtract_holes(base: u64, len: u64, holes: &[Module], emit: &mut dyn FnMut(u64, u64)) {
    let end = base.saturating_add(len);
    let mut cur = base;
    // Die Löcher der Reihe nach abarbeiten. `holes` ist klein (<= MAX_MODULES); ein
    // Auswahl-Durchlauf je Schritt ist billiger als eine Sortierung mit Zwischenpuffer.
    loop {
        // Das nächste Loch, das rechts von `cur` noch etwas abschneidet.
        let mut next: Option<Module> = None;
        for h in holes {
            if h.end <= cur || h.start >= end {
                continue; // liegt komplett hinter uns bzw. außerhalb
            }
            let cand = Module { start: h.start.max(cur), end: h.end.min(end) };
            if cand.end <= cand.start {
                continue;
            }
            next = match next {
                Some(n) if n.start <= cand.start => Some(n),
                _ => Some(cand),
            };
        }
        match next {
            Some(h) => {
                if h.start > cur {
                    emit(cur, h.start - cur);
                }
                cur = h.end;
                if cur >= end {
                    return;
                }
            }
            None => {
                if end > cur {
                    emit(cur, end - cur);
                }
                return;
            }
        }
    }
}

/// **In-Kernel-Oracle für [`subtract_holes`]** (Feature `selftest`).
///
/// Die Funktion trägt eine Sicherheitsaussage — ein übersehenes Loch heißt, dass der Allokator
/// das Boot-Archiv überschreibt, das er gleich lesen soll. Der reale Lauf sieht davon genau
/// **einen** Fall (ein Modul, mittendrin); die Grenzfälle (verdrehte Reihenfolge, Überlappung,
/// Loch am Rand, Loch über alles) sähe er nie. Deshalb werden sie hier eingespeist, statt sich
/// auf den Regelfall zu verlassen — dieselbe Begründung wie beim eingespeisten DMAR
/// (`dmar_selftest`).
///
/// Der Kernel-Crate ist nicht host-testbar (`no_std`, arch-gebunden), also läuft der Test dort,
/// wo der Code auch wirklich läuft.
#[cfg(feature = "selftest")]
pub fn selftest() -> bool {
    /// Ein Fall: `[base, len)` minus `holes` muss genau `want` ergeben.
    fn case(base: u64, len: u64, holes: &[Module], want: &[(u64, u64)]) -> bool {
        let mut got = [(0u64, 0u64); 8];
        let mut n = 0usize;
        let mut overflow = false;
        subtract_holes(base, len, holes, &mut |b, l| {
            if n < got.len() {
                got[n] = (b, l);
                n += 1;
            } else {
                overflow = true;
            }
        });
        !overflow && n == want.len() && got[..n] == *want
    }
    fn m(start: u64, end: u64) -> Module {
        Module { start, end }
    }

    // Kein Loch -> unverändert.
    case(100, 50, &[], &[(100, 50)])
        // Loch mittendrin -> zwei Stücke (der reale Fall).
        && case(0, 100, &[m(40, 60)], &[(0, 40), (60, 40)])
        // Loch am Anfang / am Ende -> beschnitten, kein Nullstück.
        && case(0, 100, &[m(0, 10)], &[(10, 90)])
        && case(0, 100, &[m(90, 100)], &[(0, 90)])
        // Loch über alles -> gar nichts frei (NICHT: alles frei).
        && case(10, 20, &[m(0, 1000)], &[])
        // Löcher außerhalb -> ignoriert.
        && case(100, 50, &[m(0, 100), m(150, 900)], &[(100, 50)])
        // Unsortiert + überlappend: der Bootloader sagt über die Reihenfolge nichts zu.
        && case(0, 100, &[m(70, 80), m(10, 20), m(15, 30)], &[(0, 10), (30, 40), (80, 20)])
        // Aneinanderstoßende Löcher erzeugen kein leeres Zwischenstück.
        && case(0, 100, &[m(10, 20), m(20, 30)], &[(0, 10), (30, 70)])
}
