//! **Text into a linear framebuffer** (T0, 2026-08-17) — the arithmetic, without the hardware.
//!
//! On the target machine there is no UART (measured: `/sys/class/tty/ttyS0/type = 0`), so a metal
//! run can only speak through the display. This module is the part that can be wrong in a way no
//! amount of staring finds, and it takes a `&mut [u8]` so a host test can hand it a `Vec`.
//!
//! ## The bug this module exists to prevent
//!
//! **`pitch` is not `width * bytes_per_pixel`.** The hardware pads scanlines, and on the machine
//! measured on 2026-08-17 the two happen to be equal (2560 px × 4 B = 10240 = pitch) — which is
//! exactly the condition under which the mistake is invisible. It is the same shape as
//! *„unten zuerst war ein Zufall der Groessenrelation"*: a property that holds until the next
//! machine, where every row would land a few pixels further left than the last and the text would
//! shear across the screen.
//!
//! So: the pitch is **read**, never recomputed, and every offset goes through one function that
//! is tested against a framebuffer whose pitch is deliberately *wider* than its width.

/// The framebuffer geometry this module works on — **numbers, not a borrowed type**.
///
/// `bootparams::Framebuffer` carries the same four fields, and importing it would have been the
/// obvious move. It is the wrong one: the host-test harness compiles each of these modules as a
/// **single file**, so a module that imports a sibling is a module nobody can test alone. Passing
/// numbers is the established shape here (`cache_decode::colors_from(sets, line, PAGE)`,
/// `caprock_mem::stripe` taking the colour count) and it keeps the two questions apart:
/// `Framebuffer::plausible` asks *is this a describable framebuffer*, [`Grid::new`] asks *can I lay
/// a text grid on it*.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Geom {
    pub width: u32,
    pub height: u32,
    /// Bytes per scanline — **read, never recomputed**. See the module doc.
    pub pitch: u32,
    pub bpp: u32,
}

impl Geom {
    pub fn size_bytes(&self) -> Option<u64> {
        (self.pitch as u64).checked_mul(self.height as u64)
    }
    /// Can a grid be laid on this at all?
    fn usable(&self) -> bool {
        if self.width == 0 || self.height == 0 {
            return false;
        }
        if self.bpp != 32 && self.bpp != 24 {
            return false;
        }
        self.width.checked_mul(self.bpp / 8).is_some_and(|line| self.pitch >= line)
            && self.size_bytes().is_some()
    }
}

/// Glyph cell size. Independent of any particular font — the font is handed in as a bitmap, so
/// this module has no opinion about glyph shapes and can be tested without one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    pub w: u32,
    pub h: u32,
}

/// A text grid over a framebuffer.
#[derive(Clone, Copy, Debug)]
pub struct Grid {
    pub fb: Geom,
    pub cell: Cell,
    pub cols: u32,
    pub rows: u32,
}

impl Grid {
    /// Build a grid, or `None` if the geometry cannot carry even one cell.
    ///
    /// **`None` rather than a zero-sized grid**: a grid with 0 columns accepts every write and
    /// draws nothing, which is precisely a console that cannot speak while looking like one.
    pub fn new(fb: Geom, cell: Cell) -> Option<Self> {
        if !fb.usable() || cell.w == 0 || cell.h == 0 {
            return None;
        }
        let cols = fb.width / cell.w;
        let rows = fb.height / cell.h;
        if cols == 0 || rows == 0 {
            return None;
        }
        Some(Self { fb, cell, cols, rows })
    }

    fn bytes_per_px(&self) -> u32 {
        self.fb.bpp / 8
    }

    /// Byte offset of pixel `(x, y)` — **the one place the pitch is used**.
    ///
    /// `None` outside the visible area. Bounds live here rather than in the drawing loops, because
    /// a bounds check inside a loop is one a later fast path removes.
    pub fn pixel_offset(&self, x: u32, y: u32) -> Option<usize> {
        if x >= self.fb.width || y >= self.fb.height {
            return None;
        }
        let off = (y as u64)
            .checked_mul(self.fb.pitch as u64)?
            .checked_add((x as u64).checked_mul(self.bytes_per_px() as u64)?)?;
        // Die letzte Pixelbreite muss noch in den Puffer passen, nicht nur ihr Anfang.
        let end = off.checked_add(self.bytes_per_px() as u64)?;
        if end > self.size() as u64 {
            return None;
        }
        Some(off as usize)
    }

    /// Bytes the grid's framebuffer spans.
    pub fn size(&self) -> usize {
        self.fb.size_bytes().unwrap_or(0) as usize
    }

    /// Write one pixel. Silently does nothing outside the area — the bounds decision was already
    /// made in [`pixel_offset`](Self::pixel_offset), and duplicating it here would be two truths.
    pub fn put_pixel(&self, buf: &mut [u8], x: u32, y: u32, rgb: u32) {
        let Some(o) = self.pixel_offset(x, y) else { return };
        let n = self.bytes_per_px() as usize;
        if o + n > buf.len() {
            return;
        }
        // Little-endian BGR(X) -- das Format eines linearen 24/32-bpp-Framebuffers auf x86.
        buf[o] = (rgb & 0xFF) as u8;
        buf[o + 1] = ((rgb >> 8) & 0xFF) as u8;
        buf[o + 2] = ((rgb >> 16) & 0xFF) as u8;
        if n == 4 {
            buf[o + 3] = 0;
        }
    }

    /// Draw one glyph at text position `(col, row)`.
    ///
    /// `glyph` is `cell.h` rows of `ceil(cell.w / 8)` bytes, MSB first — the layout of every
    /// bitmap console font. A glyph of the wrong length draws **nothing**: a partially drawn
    /// character is a wrong character, and a console that renders half a report is worse than one
    /// that renders none, because the half looks complete.
    pub fn draw_glyph(
        &self,
        buf: &mut [u8],
        col: u32,
        row: u32,
        glyph: &[u8],
        fg: u32,
        bg: u32,
    ) -> bool {
        if col >= self.cols || row >= self.rows {
            return false;
        }
        let stride = ((self.cell.w + 7) / 8) as usize;
        if glyph.len() != stride * self.cell.h as usize {
            return false;
        }
        let x0 = col * self.cell.w;
        let y0 = row * self.cell.h;
        for gy in 0..self.cell.h {
            for gx in 0..self.cell.w {
                let byte = glyph[gy as usize * stride + (gx / 8) as usize];
                let on = byte & (0x80 >> (gx % 8)) != 0;
                self.put_pixel(buf, x0 + gx, y0 + gy, if on { fg } else { bg });
            }
        }
        true
    }

    /// Scroll up by one text row and clear the last one.
    ///
    /// Moves **`cols * cell.w` pixels per line, not `pitch` bytes** — the padding beyond the last
    /// visible column may belong to nobody, and copying it is how a scroll ends up dragging
    /// whatever the firmware left there across the screen.
    pub fn scroll_up(&self, buf: &mut [u8], bg: u32) {
        let line = self.cell.h;
        let visible_h = self.rows * line;
        let pitch = self.fb.pitch as usize;
        let visible_bytes = (self.cols * self.cell.w * self.bytes_per_px()) as usize;
        for y in line..visible_h {
            let (src, dst) = ((y as usize) * pitch, ((y - line) as usize) * pitch);
            if src + visible_bytes > buf.len() || dst + visible_bytes > buf.len() {
                return;
            }
            buf.copy_within(src..src + visible_bytes, dst);
        }
        for y in (visible_h - line)..visible_h {
            for x in 0..self.cols * self.cell.w {
                self.put_pixel(buf, x, y, bg);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **Pitch ABSICHTLICH breiter als die Zeile.** Auf der gemessenen Maschine sind beide gleich
    /// (2560*4 == 10240) -- genau die Bedingung, unter der ein Pitch-Fehler unsichtbar bleibt.
    fn fb_padded() -> Geom {
        Geom { width: 16, height: 8, pitch: 16 * 4 + 32, bpp: 32 }
    }

    fn buf_for(fb: &Geom) -> Vec<u8> {
        vec![0u8; fb.size_bytes().unwrap() as usize]
    }

    #[test]
    fn the_offset_uses_the_pitch_and_not_the_width() {
        let g = Grid::new(fb_padded(), Cell { w: 8, h: 8 }).unwrap();
        assert_eq!(g.pixel_offset(0, 0), Some(0));
        // Zeile 1 beginnt beim PITCH, nicht bei width*4 (= 64).
        assert_eq!(g.pixel_offset(0, 1), Some(96));
        assert_ne!(g.pixel_offset(0, 1), Some(64), "das waere der Breitenfehler");
        assert_eq!(g.pixel_offset(3, 2), Some(2 * 96 + 3 * 4));
    }

    #[test]
    fn outside_the_visible_area_there_is_no_offset() {
        let g = Grid::new(fb_padded(), Cell { w: 8, h: 8 }).unwrap();
        assert_eq!(g.pixel_offset(16, 0), None, "x == width");
        assert_eq!(g.pixel_offset(0, 8), None, "y == height");
        assert!(g.pixel_offset(15, 7).is_some(), "die letzte sichtbare Stelle gibt es");
    }

    /// Ein Raster mit 0 Spalten nimmt jede Ausgabe an und zeigt nichts -- eine Konsole, die nicht
    /// sprechen kann und dabei aussieht wie eine.
    #[test]
    fn a_grid_that_cannot_hold_one_cell_is_refused() {
        let fb = fb_padded();
        assert!(Grid::new(fb, Cell { w: 32, h: 8 }).is_none(), "breiter als der Schirm");
        assert!(Grid::new(fb, Cell { w: 8, h: 32 }).is_none(), "hoeher als der Schirm");
        assert!(Grid::new(fb, Cell { w: 0, h: 8 }).is_none());
        let kaputt = Geom { pitch: 4, ..fb };
        assert!(Grid::new(kaputt, Cell { w: 8, h: 8 }).is_none(), "unmoegliche Geometrie");
    }

    #[test]
    fn the_grid_dimensions_come_from_the_geometry() {
        let g = Grid::new(fb_padded(), Cell { w: 8, h: 8 }).unwrap();
        assert_eq!((g.cols, g.rows), (2, 1));
    }

    /// Ein Glyph falscher Laenge zeichnet GAR NICHTS -- ein halb gezeichnetes Zeichen ist ein
    /// falsches Zeichen.
    #[test]
    fn a_glyph_of_the_wrong_length_draws_nothing() {
        let g = Grid::new(fb_padded(), Cell { w: 8, h: 8 }).unwrap();
        let mut b = buf_for(&g.fb);
        assert!(!g.draw_glyph(&mut b, 0, 0, &[0xFF; 7], 0xFFFFFF, 0));
        assert!(!g.draw_glyph(&mut b, 0, 0, &[0xFF; 9], 0xFFFFFF, 0));
        assert!(b.iter().all(|&x| x == 0), "nichts darf geschrieben worden sein");
        assert!(g.draw_glyph(&mut b, 0, 0, &[0xFF; 8], 0xFFFFFF, 0), "8 Zeilen a 1 Byte");
    }

    #[test]
    fn a_glyph_outside_the_grid_is_refused() {
        let g = Grid::new(fb_padded(), Cell { w: 8, h: 8 }).unwrap();
        let mut b = buf_for(&g.fb);
        assert!(!g.draw_glyph(&mut b, 2, 0, &[0xFF; 8], 0xFFFFFF, 0), "cols == 2");
        assert!(!g.draw_glyph(&mut b, 0, 1, &[0xFF; 8], 0xFFFFFF, 0), "rows == 1");
    }

    /// Der Glyph landet an der richtigen STELLE, und zwar in der zweiten Zelle.
    #[test]
    fn the_second_cell_starts_one_cell_width_further_right() {
        let g = Grid::new(fb_padded(), Cell { w: 8, h: 8 }).unwrap();
        let mut b = buf_for(&g.fb);
        // Nur das oberste linke Pixel des Glyphs setzen.
        let mut glyph = [0u8; 8];
        glyph[0] = 0x80;
        assert!(g.draw_glyph(&mut b, 1, 0, &glyph, 0x00FF_FFFF, 0));
        let erwartet = g.pixel_offset(8, 0).unwrap();
        assert_eq!(&b[erwartet..erwartet + 3], &[0xFF, 0xFF, 0xFF]);
        assert_eq!(&b[0..3], &[0, 0, 0], "Zelle 0 bleibt unberuehrt");
    }

    /// **Sprechprobe in beide Richtungen** -- ein Renderer, der nichts zeichnet, faellt sonst nicht auf.
    #[test]
    fn drawing_actually_changes_the_buffer_and_the_background_is_written_too() {
        let g = Grid::new(fb_padded(), Cell { w: 8, h: 8 }).unwrap();
        let mut b = buf_for(&g.fb);
        assert!(g.draw_glyph(&mut b, 0, 0, &[0x00; 8], 0xFFFFFF, 0x0000_2040));
        // Ein LEERER Glyph muss den Hintergrund malen, sonst bleibt alter Inhalt stehen.
        let o = g.pixel_offset(0, 0).unwrap();
        assert_eq!(&b[o..o + 3], &[0x40, 0x20, 0x00], "BGR little-endian");
    }

    #[test]
    fn scrolling_moves_the_rows_up_and_clears_the_last() {
        let fb = Geom { width: 8, height: 16, pitch: 8 * 4 + 16, bpp: 32 };
        let g = Grid::new(fb, Cell { w: 8, h: 8 }).unwrap();
        assert_eq!(g.rows, 2);
        let mut b = buf_for(&fb);
        // Zeile 8 (Anfang der zweiten Textzeile) markieren ...
        g.put_pixel(&mut b, 0, 8, 0x00AB_CDEF);
        g.scroll_up(&mut b, 0);
        // ... danach muss sie ganz oben stehen.
        let o0 = g.pixel_offset(0, 0).unwrap();
        assert_eq!(&b[o0..o0 + 3], &[0xEF, 0xCD, 0xAB]);
        // und die letzte Textzeile ist geloescht.
        let o8 = g.pixel_offset(0, 8).unwrap();
        assert_eq!(&b[o8..o8 + 3], &[0, 0, 0]);
    }

    /// Der Bildlauf darf die POLSTERUNG nicht mitkopieren -- was hinter der letzten sichtbaren
    /// Spalte liegt, gehoert niemandem.
    #[test]
    fn scrolling_does_not_drag_the_padding_along() {
        let fb = Geom { width: 8, height: 16, pitch: 8 * 4 + 16, bpp: 32 };
        let g = Grid::new(fb, Cell { w: 8, h: 8 }).unwrap();
        let mut b = buf_for(&fb);
        // Eine Marke IN DER POLSTERUNG von Zeile 8 (hinter dem letzten Pixel).
        let pad = 8 * (8 * 4 + 16) + 8 * 4 + 4;
        b[pad] = 0x5A;
        g.scroll_up(&mut b, 0);
        let ziel = 0 * (8 * 4 + 16) + 8 * 4 + 4;
        assert_ne!(b[ziel], 0x5A, "die Polsterung darf nicht nach oben wandern");
    }
}
