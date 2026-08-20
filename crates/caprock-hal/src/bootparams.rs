//! **What a kexec launcher hands the kernel** (T0, 2026-08-17) — a pure parser over the boot
//! command line.
//!
//! ## Why this exists
//!
//! Measured 2026-08-17, this machine cannot run Caprock observably: it is UEFI (no CSM), the kernel
//! boots Multiboot 1 (a BIOS-era protocol), the RSDP is found by scanning the legacy BIOS area
//! (empty under UEFI), and the console is COM1 port-I/O — with **no UART behind it**
//! (`/sys/class/tty/ttyS0/type = 0`). Four independent reasons for a silent run, and the last one
//! is this project's own rule turned on itself: *a checker that cannot speak has not tested
//! anything.*
//!
//! `kexec-tools` 2.0.32 loads `multiboot-x86` directly from a running Linux, which removes the
//! bootloader problem entirely. What it does **not** do is discover ACPI or the framebuffer for us.
//! But Linux already knows both — so the launcher passes them on the command line, and this module
//! is where they are read. It is the same trick Linux itself uses for kexec under EFI
//! (`acpi_rsdp=`).
//!
//! ## The rule that shapes every parse below
//!
//! **A malformed value must never become a plausible one.** `0` is the address of the real mode
//! IVT and a perfectly typable framebuffer base; a parser that turns `fb=garbage` into
//! `base = 0` would have the kernel scribble over low memory and call it a display. Every field is
//! therefore `Option`, and a single bad field discards the **whole** parameter rather than keeping
//! the fields that happened to scan — a half-parsed framebuffer is worse than none, because it
//! looks like a framebuffer.

/// Where the ACPI RSDP is, as told by the launcher.
///
/// Under UEFI the RSDP lives in the EFI configuration table; the legacy scan of
/// `0xE0000..0x100000` finds it only by luck. Linux publishes it in `/sys/firmware/efi/systab`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rsdp(pub u64);

/// A linear framebuffer the launcher already knows the geometry of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Framebuffer {
    pub base: u64,
    pub width: u32,
    pub height: u32,
    /// Bytes per scanline. **Not** `width * bytes_per_pixel` — the hardware pads. Measured on this
    /// machine: 2560 px at 32 bpp gives `10240`, which happens to be exactly `width * 4`, and on
    /// the next machine it will not be. A renderer that recomputes it instead of reading it is the
    /// `iova_window_clear_of_msi` mistake: *Zuteiler und Pruefer brauchen EINE Quelle.*
    pub pitch: u32,
    pub bpp: u32,
}

impl Framebuffer {
    /// Bytes the framebuffer occupies.
    pub fn size_bytes(&self) -> Option<u64> {
        (self.pitch as u64).checked_mul(self.height as u64)
    }

    /// **Is this geometry physically possible?**
    ///
    /// Three ways a launcher can hand over a shape that would make the kernel write outside the
    /// framebuffer, and all three are cheap to refuse:
    ///
    /// * a pitch narrower than one line of pixels — every row would run into the next,
    /// * a zero dimension — a display nobody can see, and a divisor of zero downstream,
    /// * a size that overflows, or a base that overflows when the size is added.
    ///
    /// This is checked **here**, once, rather than at every blit. A bounds check that lives in the
    /// drawing loop is a bounds check that a later fast path removes.
    pub fn plausible(&self) -> bool {
        if self.width == 0 || self.height == 0 || self.bpp == 0 {
            return false;
        }
        // Nur Formate, die ein linearer Framebuffer wirklich hat. 24 bpp ist absichtlich dabei
        // (es kommt auf aelterer Firmware vor), 15/16 nicht: die Pixelformate unterscheiden sich
        // dort (555 gegen 565), und ein Renderer, der das raet, malt Farbmuell.
        if self.bpp != 32 && self.bpp != 24 {
            return false;
        }
        let bytes_per_px = self.bpp / 8;
        let Some(line) = self.width.checked_mul(bytes_per_px) else {
            return false;
        };
        if self.pitch < line {
            return false;
        }
        let Some(size) = self.size_bytes() else {
            return false;
        };
        self.base.checked_add(size).is_some()
    }
}

/// Everything the launcher can hand over. Absent fields are `None`, never a default that looks
/// like an answer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BootParams {
    pub rsdp: Option<Rsdp>,
    pub fb: Option<Framebuffer>,
    /// A key was recognised but its value did not parse. **Counted, because silence here is the
    /// dangerous outcome**: without it, `fb=` with a typo and no `fb=` at all are the same
    /// observation, and the report line would say "no framebuffer" for a launcher bug.
    pub malformed: u32,
}

/// Parse `u64` in hex (`0x…`) or decimal. `None` on anything else — including an empty string,
/// which `from_str_radix`-style code routinely turns into a silent `0`.
fn num(s: &str) -> Option<u64> {
    let (digits, radix) = match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(d) => (d, 16u64),
        None => (s, 10u64),
    };
    if digits.is_empty() {
        return None;
    }
    let mut v: u64 = 0;
    for c in digits.bytes() {
        let d = match c {
            b'0'..=b'9' => (c - b'0') as u64,
            b'a'..=b'f' => (c - b'a' + 10) as u64,
            b'A'..=b'F' => (c - b'A' + 10) as u64,
            _ => return None,
        };
        if d >= radix {
            return None;
        }
        // Ueberlauf ist ein Fehlschlag, kein Umlauf. S3 woertlich.
        v = v.checked_mul(radix)?.checked_add(d)?;
    }
    Some(v)
}

/// Parse the whole command line.
///
/// Unknown words are ignored on purpose — the launcher may pass things this kernel does not know,
/// and refusing the boot over an unknown word would make the launcher and the kernel a matched
/// pair that must be updated together.
pub fn parse(cmdline: &str) -> BootParams {
    let mut p = BootParams::default();
    for word in cmdline.split_ascii_whitespace() {
        if let Some(v) = word.strip_prefix("acpi_rsdp=") {
            match num(v) {
                // Adresse 0 ist keine RSDP, sondern ein nicht gesetzter Wert, der wie einer
                // aussieht. Genau die Verwechslung, gegen die `NOSEL_TEXT` steht.
                Some(a) if a != 0 => p.rsdp = Some(Rsdp(a)),
                _ => p.malformed += 1,
            }
        } else if let Some(v) = word.strip_prefix("fb=") {
            match parse_fb(v) {
                Some(f) => p.fb = Some(f),
                None => p.malformed += 1,
            }
        }
    }
    p
}

/// `base,width,height,pitch,bpp` — **all five, or nothing.**
///
/// Four of five fields parsing is not four fifths of a framebuffer; it is a wrong one. The same
/// reason `Image::build` refuses a checkpoint rather than writing a partial one.
fn parse_fb(v: &str) -> Option<Framebuffer> {
    let mut it = v.split(',');
    let base = num(it.next()?)?;
    let width = num(it.next()?)? as u32;
    let height = num(it.next()?)? as u32;
    let pitch = num(it.next()?)? as u32;
    let bpp = num(it.next()?)? as u32;
    if it.next().is_some() {
        return None; // ein sechstes Feld heisst: der Aufrufer meint etwas anderes als wir
    }
    let fb = Framebuffer { base, width, height, pitch, bpp };
    fb.plausible().then_some(fb)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fb(v: &str) -> Option<Framebuffer> {
        parse(&alloc_line(v)).fb
    }
    fn alloc_line(v: &str) -> String {
        format!("fb={v}")
    }

    #[test]
    fn the_measured_geometry_of_this_machine_parses() {
        // 2560x1600, stride 10240, 32 bpp -- gemessen am 2026-08-17 aus /sys/class/graphics/fb0.
        let p = parse("acpi_rsdp=0x7f9ea014 fb=0xd0000000,2560,1600,10240,32");
        assert_eq!(p.rsdp, Some(Rsdp(0x7f9e_a014)));
        assert_eq!(
            p.fb,
            Some(Framebuffer { base: 0xd000_0000, width: 2560, height: 1600, pitch: 10240, bpp: 32 })
        );
        assert_eq!(p.malformed, 0);
        assert_eq!(p.fb.unwrap().size_bytes(), Some(10240 * 1600));
    }

    #[test]
    fn decimal_and_hex_both_work_and_junk_does_not() {
        assert_eq!(num("4096"), Some(4096));
        assert_eq!(num("0x1000"), Some(4096));
        assert_eq!(num("0X1000"), Some(4096));
        assert_eq!(num(""), None, "leer darf NICHT 0 werden");
        assert_eq!(num("0x"), None);
        assert_eq!(num("12g"), None);
        assert_eq!(num("0x1g"), None);
        assert_eq!(num("9a"), None, "Dezimal mit Hexziffer");
    }

    /// **Die tragende Regel.** Ein kaputter Wert darf nicht zu einem plausiblen werden.
    #[test]
    fn a_malformed_value_is_counted_and_never_becomes_zero() {
        let p = parse("acpi_rsdp=nonsense fb=0x1000,,,,");
        assert_eq!(p.rsdp, None);
        assert_eq!(p.fb, None);
        assert_eq!(p.malformed, 2, "beide Fehler muessen gezaehlt sein");
    }

    /// Adresse 0 ist kein Ergebnis. Ohne diesen Zweig saehe ein nicht gesetzter Wert wie eine
    /// gefundene RSDP aus.
    #[test]
    fn rsdp_zero_is_refused() {
        let p = parse("acpi_rsdp=0");
        assert_eq!(p.rsdp, None);
        assert_eq!(p.malformed, 1);
    }

    #[test]
    fn a_missing_key_is_not_a_malformed_one() {
        let p = parse("quiet splash");
        assert_eq!((p.rsdp, p.fb, p.malformed), (None, None, 0));
    }

    /// **Vier von fuenf Feldern sind kein Framebuffer.**
    #[test]
    fn a_partial_framebuffer_is_refused_whole() {
        assert_eq!(fb("0xd0000000,2560,1600,10240"), None);
        assert_eq!(fb("0xd0000000,2560,1600"), None);
        assert_eq!(fb("0xd0000000,2560,1600,10240,32,7"), None, "ein sechstes Feld");
    }

    /// Ein Pitch schmaler als eine Pixelzeile laesst jede Zeile in die naechste laufen.
    #[test]
    fn a_pitch_narrower_than_one_line_is_impossible() {
        assert_eq!(fb("0xd0000000,2560,1600,8192,32"), None, "2560*4 = 10240 > 8192");
        assert!(fb("0xd0000000,2560,1600,10240,32").is_some());
        assert!(fb("0xd0000000,2560,1600,12288,32").is_some(), "Polsterung ist erlaubt");
    }

    #[test]
    fn zero_dimensions_are_refused() {
        assert_eq!(fb("0xd0000000,0,1600,10240,32"), None);
        assert_eq!(fb("0xd0000000,2560,0,10240,32"), None);
        assert_eq!(fb("0xd0000000,2560,1600,10240,0"), None);
    }

    /// 15/16 bpp sind ABSICHTLICH nicht erlaubt: 555 und 565 sind zwei Formate, und ein Renderer,
    /// der raet, malt Farbmuell statt Text.
    #[test]
    fn only_unambiguous_pixel_formats_are_accepted() {
        assert!(fb("0xd0000000,2560,1600,10240,32").is_some());
        assert!(fb("0xd0000000,2560,1600,7680,24").is_some());
        assert_eq!(fb("0xd0000000,2560,1600,5120,16"), None);
        assert_eq!(fb("0xd0000000,2560,1600,5120,15"), None);
    }

    /// S3: keine ungeschuetzte Arithmetik ueber Werten, die von aussen kommen.
    ///
    /// **Die erste Fassung dieses Tests war falsch, nicht der Code**: sie behauptete, ein
    /// Framebuffer mit `width=1, pitch=u32::MAX` sei unmoeglich („die Zeile ist schmaler als das
    /// Pixel") -- tatsaechlich ist der Pitch dort riesig und die Zeile winzig, also durchaus
    /// zulaessig. Absurd ist nicht dasselbe wie unmoeglich, und ein Pruefer darf nur das Zweite
    /// abweisen. Geprueft wird jetzt, was wirklich nicht geht: eine Groesse, die den Adressraum
    /// verlaesst.
    #[test]
    fn a_geometry_that_would_overflow_is_refused() {
        assert_eq!(fb("0xffffffffffffff00,2560,1600,10240,32"), None, "base+size laeuft ueber");
        // Direkt am Typ, ohne den Parser: dieselbe Aussage, andere Tuer.
        let am_rand = Framebuffer {
            base: u64::MAX - 4096,
            width: 640,
            height: 480,
            pitch: 2560,
            bpp: 32,
        };
        assert!(am_rand.size_bytes().is_some());
        assert!(!am_rand.plausible(), "Basis + Groesse verlaesst den Adressraum");
        // Und die Gegenprobe: dieselbe Geometrie tief im Adressraum ist in Ordnung.
        assert!(Framebuffer { base: 0x1000_0000, ..am_rand }.plausible());
    }

    /// **Sprechprobe in beide Richtungen** — ein Parser, der nicht durchfallen kann, ist keiner.
    #[test]
    fn the_parser_can_both_accept_and_reject() {
        assert!(parse("fb=0xd0000000,2560,1600,10240,32").fb.is_some());
        assert!(parse("fb=0xd0000000,2560,1600,1,32").fb.is_none());
    }
}
