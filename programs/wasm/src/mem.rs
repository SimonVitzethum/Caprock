//! Grant-gebundener Linearspeicher der Stufe 2a (Strang 7/WASM-MEM).
//!
//! Norm: `programs/mem-server/SPEZIFIKATION.md`, Anhang Stufe 2a. Der Speicher ist genau
//! EIN Grant: Basis + Laenge liefert der Aufrufer als Slice (`Speicher::neu`), es gibt
//! kein Alloc und kein Wachstum — `wachsen` delegiert an [`wachstum`][super::wachstum]
//! und sagt in 2a immer ab (ausser `delta = 0`, reine Groessenanfrage).
//!
//! Regeln, die hier gelten (alle aus dem Anhang / `lib.rs`):
//! - Jede Adresse + Breite laeuft durch [`zugriff_pruefen`][super::zugriff_pruefen] —
//!   kein direkter Index ohne Pruefung.
//! - Bound-Check-Fehler sind [`Trap::OobZugriff`] (PD-Fault), niemals Panic: Nach der
//!   Pruefung greift der Code nur noch ueber `get`/`get_mut` zu und meldet `None`
//!   ebenfalls als OOB (kein `[]`, kein `unwrap`).
//! - `laden_*`/`speichern_*` sind Little-Endian (WASM-Byteordnung).
//! - Kein `unsafe` (die Crate verbietet es ohnehin per `forbid`).

use super::{Trap, WASM_SEITE, wachstum, zugriff_pruefen};

/// Linearspeicher ueber genau einem Grant. Der Aufrufer gibt Basis + Laenge als
/// Slice (`zellen`); die Engine allokiert nichts und haengt nichts an.
#[derive(Debug)]
pub struct Speicher<'a> {
    zellen: &'a mut [u8],
}

impl<'a> Speicher<'a> {
    /// Grant-Slice uebernehmen (Basis + Laenge vom Aufrufer, kein Alloc).
    pub fn neu(zellen: &'a mut [u8]) -> Self {
        Speicher { zellen }
    }

    /// Grant-Laenge in Bytes.
    pub fn len(&self) -> u64 {
        self.zellen.len() as u64
    }

    /// Ob der Grant leer ist (`min = 0`-Form duerfte es nie geben, s. Validator).
    pub fn ist_leer(&self) -> bool {
        self.zellen.is_empty()
    }

    /// Aktuelle Groesse in WASM-Seiten (abrundend; Anfragewert fuer `delta = 0`).
    pub fn seiten(&self) -> u32 {
        (self.len() / WASM_SEITE) as u32
    }

    /// Ein Zugriffsfenster gegen die Grant-Grenze pruefen (OOB = PD-Fault).
    fn fenster(&self, adresse: u32, breite: u64) -> Result<(), Trap> {
        zugriff_pruefen(adresse as u64, breite, self.len()).map_err(|_| Trap::OobZugriff)
    }

    /// Ein Byte laden.
    pub fn laden_u8(&self, adresse: u32) -> Result<u8, Trap> {
        self.fenster(adresse, 1)?;
        self.zellen.get(adresse as usize).copied().ok_or(Trap::OobZugriff)
    }

    /// Zwei Bytes laden (Little-Endian).
    pub fn laden_u16(&self, adresse: u32) -> Result<u16, Trap> {
        self.fenster(adresse, 2)?;
        let a = adresse as usize;
        let e = a.checked_add(2).ok_or(Trap::OobZugriff)?;
        let b = self.zellen.get(a..e).ok_or(Trap::OobZugriff)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    /// Vier Bytes laden (Little-Endian).
    pub fn laden_u32(&self, adresse: u32) -> Result<u32, Trap> {
        self.fenster(adresse, 4)?;
        let a = adresse as usize;
        let e = a.checked_add(4).ok_or(Trap::OobZugriff)?;
        let b = self.zellen.get(a..e).ok_or(Trap::OobZugriff)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// Acht Bytes laden (Little-Endian).
    pub fn laden_u64(&self, adresse: u32) -> Result<u64, Trap> {
        self.fenster(adresse, 8)?;
        let a = adresse as usize;
        let e = a.checked_add(8).ok_or(Trap::OobZugriff)?;
        let b = self.zellen.get(a..e).ok_or(Trap::OobZugriff)?;
        Ok(u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
    }

    /// Ein Byte speichern.
    pub fn speichern_u8(&mut self, adresse: u32, wert: u8) -> Result<(), Trap> {
        self.fenster(adresse, 1)?;
        *self.zellen.get_mut(adresse as usize).ok_or(Trap::OobZugriff)? = wert;
        Ok(())
    }

    /// Zwei Bytes speichern (Little-Endian).
    pub fn speichern_u16(&mut self, adresse: u32, wert: u16) -> Result<(), Trap> {
        self.fenster(adresse, 2)?;
        let a = adresse as usize;
        let e = a.checked_add(2).ok_or(Trap::OobZugriff)?;
        let f = self.zellen.get_mut(a..e).ok_or(Trap::OobZugriff)?;
        f.copy_from_slice(&wert.to_le_bytes());
        Ok(())
    }

    /// Vier Bytes speichern (Little-Endian).
    pub fn speichern_u32(&mut self, adresse: u32, wert: u32) -> Result<(), Trap> {
        self.fenster(adresse, 4)?;
        let a = adresse as usize;
        let e = a.checked_add(4).ok_or(Trap::OobZugriff)?;
        let f = self.zellen.get_mut(a..e).ok_or(Trap::OobZugriff)?;
        f.copy_from_slice(&wert.to_le_bytes());
        Ok(())
    }

    /// Acht Bytes speichern (Little-Endian).
    pub fn speichern_u64(&mut self, adresse: u32, wert: u64) -> Result<(), Trap> {
        self.fenster(adresse, 8)?;
        let a = adresse as usize;
        let e = a.checked_add(8).ok_or(Trap::OobZugriff)?;
        let f = self.zellen.get_mut(a..e).ok_or(Trap::OobZugriff)?;
        f.copy_from_slice(&wert.to_le_bytes());
        Ok(())
    }

    /// Fremde Bytes in den Speicher kopieren (Ueberlauf-sicher: `ziel + len`
    /// laeuft ueber `zugriff_pruefen`, das Wrappen als OOB meldet).
    pub fn kopieren_in(&mut self, ziel: u32, quelle: &[u8]) -> Result<(), Trap> {
        zugriff_pruefen(ziel as u64, quelle.len() as u64, self.len())
            .map_err(|_| Trap::OobZugriff)?;
        let a = ziel as usize;
        let e = a.checked_add(quelle.len()).ok_or(Trap::OobZugriff)?;
        let f = self.zellen.get_mut(a..e).ok_or(Trap::OobZugriff)?;
        f.copy_from_slice(quelle);
        Ok(())
    }

    /// Speicherbytes in einen fremden Puffer kopieren (Ueberlauf-sicher wie oben).
    pub fn kopieren_aus(&self, quelle: u32, ziel: &mut [u8]) -> Result<(), Trap> {
        zugriff_pruefen(quelle as u64, ziel.len() as u64, self.len())
            .map_err(|_| Trap::OobZugriff)?;
        let a = quelle as usize;
        let e = a.checked_add(ziel.len()).ok_or(Trap::OobZugriff)?;
        let f = self.zellen.get(a..e).ok_or(Trap::OobZugriff)?;
        ziel.copy_from_slice(f);
        Ok(())
    }

    /// Bereich mit einem Byte fuellen (Ueberlauf-sicher wie oben).
    pub fn fuellen(&mut self, ziel: u32, wert: u8, laenge: u32) -> Result<(), Trap> {
        zugriff_pruefen(ziel as u64, laenge as u64, self.len()).map_err(|_| Trap::OobZugriff)?;
        let a = ziel as usize;
        let e = a.checked_add(laenge as usize).ok_or(Trap::OobZugriff)?;
        let f = self.zellen.get_mut(a..e).ok_or(Trap::OobZugriff)?;
        f.fill(wert);
        Ok(())
    }

    /// `memory.grow` der Stufe 2a: delegiert an `lib.rs::wachstum` — `delta = 0`
    /// fragt die Seitenzahl an, jedes echte Wachstum trappt benannt. Die Engine
    /// spricht nie mit dem Server (Anhang Punkt 2).
    pub fn wachsen(&self, delta: u32) -> Result<u32, Trap> {
        wachstum(delta, self.seiten()).map_err(|_| Trap::WachstumAbgelehnt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Kleiner Grant-Anker (16 B — kein 64-KiB-Block noetig, Grenzen zaehlen).
    fn grant() -> [u8; 16] {
        [0u8; 16]
    }

    #[test]
    fn u32_rundweg_le() {
        let mut g = grant();
        let mut s = Speicher::neu(&mut g);
        s.speichern_u32(0, 0x01020304).expect("drinnen");
        // Little-Endian: niederwertigstes Byte zuerst.
        assert_eq!(s.laden_u8(0), Ok(0x04));
        assert_eq!(s.laden_u8(3), Ok(0x01));
        assert_eq!(s.laden_u16(0), Ok(0x0304));
        assert_eq!(s.laden_u32(0), Ok(0x01020304));
        s.speichern_u64(8, 0x0102030405060708).expect("drinnen");
        assert_eq!(s.laden_u64(8), Ok(0x0102030405060708));
        assert_eq!(s.laden_u32(8), Ok(0x05060708));
    }

    #[test]
    fn oob_pro_breite() {
        let mut g = grant();
        let mut s = Speicher::neu(&mut g);
        // Letzte gueltige Startadressen bei 16 B: 15/14/12/8.
        assert!(s.laden_u8(15).is_ok());
        assert!(s.laden_u16(14).is_ok());
        assert!(s.laden_u32(12).is_ok());
        assert!(s.laden_u64(8).is_ok());
        // Jeweils ein Byte weiter: benannter PD-Fault, kein Panic.
        assert_eq!(s.laden_u8(16).err(), Some(Trap::OobZugriff));
        assert_eq!(s.laden_u16(15).err(), Some(Trap::OobZugriff));
        assert_eq!(s.laden_u32(13).err(), Some(Trap::OobZugriff));
        assert_eq!(s.laden_u64(9).err(), Some(Trap::OobZugriff));
        assert_eq!(s.speichern_u8(16, 1).err(), Some(Trap::OobZugriff));
        assert_eq!(s.speichern_u16(15, 1).err(), Some(Trap::OobZugriff));
        assert_eq!(s.speichern_u32(13, 1).err(), Some(Trap::OobZugriff));
        assert_eq!(s.speichern_u64(9, 1).err(), Some(Trap::OobZugriff));
    }

    #[test]
    fn ueberlauf_addr_len() {
        let mut g = grant();
        let s = Speicher::neu(&mut g);
        // `u32::MAX + Breite` darf nicht wrappen — alles OOB, nichts panickt.
        assert_eq!(s.laden_u8(u32::MAX).err(), Some(Trap::OobZugriff));
        assert_eq!(s.laden_u16(u32::MAX).err(), Some(Trap::OobZugriff));
        assert_eq!(s.laden_u32(u32::MAX).err(), Some(Trap::OobZugriff));
        assert_eq!(s.laden_u64(u32::MAX).err(), Some(Trap::OobZugriff));
        let mut h = grant();
        let mut t = Speicher::neu(&mut h);
        assert_eq!(t.fuellen(u32::MAX, 0xAA, 1).err(), Some(Trap::OobZugriff));
        assert_eq!(t.fuellen(15, 0xAA, u32::MAX).err(), Some(Trap::OobZugriff));
        assert_eq!(t.kopieren_in(u32::MAX, &[1, 2]).err(), Some(Trap::OobZugriff));
        assert_eq!(t.kopieren_aus(u32::MAX, &mut [0u8; 2]).err(), Some(Trap::OobZugriff));
    }

    #[test]
    fn kopieren_hin_zurueck() {
        let mut g = grant();
        let mut s = Speicher::neu(&mut g);
        let daten = [0xDE, 0xAD, 0xBE, 0xEF];
        s.kopieren_in(4, &daten).expect("drinnen");
        let mut zurueck = [0u8; 4];
        s.kopieren_aus(4, &mut zurueck).expect("drinnen");
        assert_eq!(zurueck, daten);
        assert_eq!(s.laden_u32(4), Ok(0xEFBEADDE));
        // Leerer Zugriff am Ende geht durch (Nullkopie, kein OOB).
        s.kopieren_in(16, &[]).expect("Nullkopie am Ende");
        let mut leer = [0u8; 0];
        s.kopieren_aus(16, &mut leer).expect("Nullkopie am Ende");
        // Ein Byte zu weit ist OOB — hin wie zurueck.
        assert_eq!(s.kopieren_in(13, &daten).err(), Some(Trap::OobZugriff));
        let mut zuviel = [0u8; 4];
        assert_eq!(s.kopieren_aus(13, &mut zuviel).err(), Some(Trap::OobZugriff));
    }

    #[test]
    fn fuellen_grenzen() {
        let mut g = grant();
        let mut s = Speicher::neu(&mut g);
        s.fuellen(0, 0xAA, 16).expect("Vollfuellung");
        assert_eq!(s.laden_u32(0), Ok(0xAAAAAAAA));
        assert_eq!(s.laden_u32(12), Ok(0xAAAAAAAA));
        s.fuellen(4, 0x00, 4).expect("Teilfuellung");
        assert_eq!(s.laden_u32(4), Ok(0x00000000));
        assert_eq!(s.laden_u8(3), Ok(0xAA));
        assert_eq!(s.laden_u8(8), Ok(0xAA));
        // Nullfuellung am Ende geht durch, ein Byte darueber nicht.
        s.fuellen(16, 0xFF, 0).expect("Nullfuellung am Ende");
        assert_eq!(s.fuellen(16, 0xFF, 1).err(), Some(Trap::OobZugriff));
        assert_eq!(s.fuellen(17, 0xFF, 0).err(), Some(Trap::OobZugriff));
    }

    #[test]
    fn wachsen_abgelehnt() {
        let mut g = grant();
        let s = Speicher::neu(&mut g);
        // 16 B < eine Seite: Anfrage meldet 0 Seiten, Wachstum trappt benannt.
        assert_eq!(s.seiten(), 0);
        assert_eq!(s.wachsen(0), Ok(0));
        assert_eq!(s.wachsen(1).err(), Some(Trap::WachstumAbgelehnt));
        assert_eq!(s.wachsen(u32::MAX).err(), Some(Trap::WachstumAbgelehnt));
    }

    #[test]
    fn oob_aendert_nichts() {
        let mut g = grant();
        let mut s = Speicher::neu(&mut g);
        s.speichern_u32(0, 0x11223344).expect("drinnen");
        // Abgewiesene Schreibungen lassen den Bestand unberuehrt (Trap ohne Wirkung).
        let _ = s.speichern_u32(13, 0xFFFFFFFF);
        let _ = s.kopieren_in(14, &[9, 9, 9]);
        let _ = s.fuellen(0, 0x00, u32::MAX);
        assert_eq!(s.laden_u32(0), Ok(0x11223344));
        assert_eq!(s.laden_u8(15), Ok(0x00));
    }
}
