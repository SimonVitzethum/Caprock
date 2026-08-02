//! Minimal-Userland-SDK für extern geladene SEL4Lake-EL0-Programme (ext-26).
//!
//! Bietet die **Syscall-Stubs** (SVC-`#0`-ABI: `x0`=nr, `x1`=cap-Index, `x2..x5`=msg, `x6`=tag;
//! Rückgabe `x0`=result, `x1`=badge, `x2..x5`=reply, `x6`=tag) + einen **Panik-Handler**. Bewusst
//! ohne Abhängigkeit vom Kernel-Workspace (die ABI-Konstanten sind dupliziert) — ein extern
//! gebautes Programm hängt nur von diesem SDK ab.

#![no_std]

use core::arch::asm;

/// Syscall-Nummern (Spiegel von `sel4lake_abi::sys`).
pub mod sys {
    pub const YIELD: u64 = 0;
    pub const CALL: u64 = 1;
    pub const RECV: u64 = 2;
    pub const REPLY: u64 = 3;
    pub const PARK: u64 = 5;
    pub const EXIT: u64 = 6;
    pub const KILL: u64 = 7;
    pub const SIGNAL: u64 = 8;
    pub const WAIT: u64 = 9;
    pub const MAP: u64 = 10;
    pub const UNMAP: u64 = 11;
    pub const PDCTL: u64 = 12;
    pub const LOAD: u64 = 13;
    pub const CDELETE: u64 = 14;
    pub const CCOPY: u64 = 15;
    pub const CMOVE: u64 = 16;
    pub const SETRECV: u64 = 17;
}

/// Ergebniscodes (Register `x0` beim Austritt; Spiegel von `sel4lake_abi::result`).
pub mod result {
    pub const OK: u64 = 0;
    /// Kein gültiger Capability an der Stelle / falscher Objekttyp.
    pub const ERR_BADCAP: u64 = 1;
    /// Unbekannte Syscall-Nummer.
    pub const ERR_BADSYS: u64 = 2;
    /// Capability hat nicht die nötigen Rechte.
    pub const ERR_RIGHTS: u64 = 3;
    /// Aufrufer gehört zu keiner Protection Domain.
    pub const ERR_NOPD: u64 = 4;
    /// Antwort-seitiger Liveness-Fehler (Reply-Owner verschwunden).
    pub const ERR_SERVER_GONE: u64 = 5;
    /// Am Cap hängen noch abgeleitete Kopien/Mints — er bleibt unverändert im Slot.
    pub const ERR_HASCHILDREN: u64 = 6;
    /// Kein Platz: Ziel-Slot belegt oder Cap-Budget der PD erschöpft.
    pub const ERR_NOSPACE: u64 = 7;
}

/// Sub-Operationen für [`sys::PDCTL`] (Register `x2`; Spiegel von `sel4lake_abi::pdctl`).
pub mod pdctl {
    pub const START: u64 = 0;
    pub const STOP: u64 = 1;
    pub const PAUSE: u64 = 2;
    pub const RESUME: u64 = 3;
    pub const ASSIGN_BUDGET: u64 = 4;
}

/// Syscall-Ergebnis (Register `x0..x6` nach `eret`).
#[derive(Clone, Copy)]
pub struct Ret {
    pub result: u64,
    pub badge: u64,
    pub msg: [u64; 4],
    pub tag: u64,
}

/// Roh-Syscall. `cap` = lokaler Cap-Index der eigenen PD.
///
/// Die ABI ist auf beiden Architekturen **dieselbe** (`x0`=Nummer, `x1`=Cap, `x2..x5`=Nachricht,
/// `x6`=Tag); nur die Träger unterscheiden sich: `svc #0` mit `x0..x6` auf aarch64, `int 0x80` mit
/// `rax/rdi/rsi/rdx/r10/r8/r9` auf x86_64 (Abbildung: `sel4lake_hal::x86_64::exception::ABI_TO_GPR`).
/// Deshalb sieht ein Programm oberhalb dieser Funktion keinen Unterschied.
#[cfg(target_arch = "aarch64")]
#[inline]
pub fn invoke(nr: u64, cap: u64, msg: [u64; 4], tag: u64) -> Ret {
    let (r0, r1, r2, r3, r4, r5, r6);
    // SAFETY: reiner Supervisor-Call; der Kernel restauriert beim `eret` alle Register aus dem
    // Frame (nur x0..x6 tragen das Ergebnis). Kein Speicher-/Stack-Effekt im User-Kontext.
    unsafe {
        asm!(
            "svc #0",
            inout("x0") nr => r0,
            inout("x1") cap => r1,
            inout("x2") msg[0] => r2,
            inout("x3") msg[1] => r3,
            inout("x4") msg[2] => r4,
            inout("x5") msg[3] => r5,
            inout("x6") tag => r6,
            options(nostack),
        );
    }
    Ret { result: r0, badge: r1, msg: [r2, r3, r4, r5], tag: r6 }
}

/// Roh-Syscall (`int 0x80`) — x86_64. Siehe die aarch64-Fassung für die ABI.
#[cfg(target_arch = "x86_64")]
#[inline]
pub fn invoke(nr: u64, cap: u64, msg: [u64; 4], tag: u64) -> Ret {
    let (r0, r1, r2, r3, r4, r5);
    // SAFETY: `int 0x80` ist der für Ring 3 freigegebene Syscall-Vektor (IDT-Gate DPL 3). Der
    // Kernel liest/schreibt ausschließlich die ABI-Register dieses Frames. `r9` (Tag) wird nur
    // gelesen — der Kernel gibt den Antwort-Tag über denselben Weg zurück, aber `asm!` darf `r9`
    // hier nicht als `inout` führen, weil `clobber_abi` es sonst doppelt beansprucht.
    unsafe {
        asm!(
            "int 0x80",
            inout("rax") nr => r0,
            inout("rdi") cap => r1,
            inout("rsi") msg[0] => r2,
            inout("rdx") msg[1] => r3,
            inout("r10") msg[2] => r4,
            inout("r8")  msg[3] => r5,
            in("r9") tag,
            clobber_abi("sysv64"),
        );
    }
    Ret { result: r0, badge: r1, msg: [r2, r3, r4, r5], tag: 0 }
}

/// Notification (Cap `cap`) signalisieren (asynchron, nicht blockierend).
///
/// **Achtung, das ist die haeufigste Fehlannahme an dieser ABI:** das Badge, das beim Empfaenger
/// ankommt, ist eine Eigenschaft der **Capability**, nicht dieses Aufrufs. Der Kernel verodert das
/// Badge der benutzten Cap in `pending`; `badge` hier wird **nicht** ausgewertet und bleibt nur als
/// Dokumentation der Absicht stehen. Wer unterscheidbar signalisieren will, muss beim *Vergeben*
/// unterschiedlich badgen (`SYS_LOAD` mit `badge != 0`, s. [`load`]).
pub fn signal(cap: u64, badge: u64) {
    let _ = invoke(sys::SIGNAL, cap, [badge, 0, 0, 0], 0);
}
/// Auf eine Notification (Cap `cap`) warten; gibt das akkumulierte Badge zurück.
pub fn wait(cap: u64) -> u64 {
    invoke(sys::WAIT, cap, [0; 4], 0).badge
}
/// Synchroner Aufruf (Endpoint-Cap `cap`): senden + auf Antwort warten.
pub fn call(cap: u64, msg: [u64; 4]) -> Ret {
    invoke(sys::CALL, cap, msg, 0)
}
/// Auf einen Aufruf warten (Server, Endpoint-Cap `cap`).
pub fn recv(cap: u64) -> Ret {
    invoke(sys::RECV, cap, [0; 4], 0)
}
/// Den letzten Aufrufer beantworten (Endpoint-Cap `cap`).
pub fn reply(cap: u64, msg: [u64; 4]) {
    let _ = invoke(sys::REPLY, cap, msg, 0);
}
/// Frame (Memory-Cap `cap`) in die eigene VSpace mappen; gibt den Ergebniscode zurück.
pub fn map(cap: u64) -> u64 {
    invoke(sys::MAP, cap, [0; 4], 0).result
}

/// Ein **Fenster** (Memory-, MMIO- oder DMA-Cap) mappen und erfahren, **was** gemappt wurde:
/// `(Basis, Länge, Gerätesicht)`. `None` bei Fehlschlag.
///
/// Die Gerätesicht (IOVA) ist bei DMA-Caps die Adresse, unter der **das Gerät** die Region sieht;
/// bei MMIO ist sie `0`. Sie ist **nicht** die Basis: die IOVA stammt aus dem Fenster, das der
/// Kernel beim Zuteilen gewählt hat, und liegt oberhalb des RAM. Ein Treiber, der die beiden
/// vermischt, programmiert dem Gerät eine Adresse, die es nicht auflösen kann — oder beschreibt
/// eine, unter der nichts liegt. Deshalb kommen sie hier **getrennt** zurück, auch wenn das
/// umständlicher aussieht als ein Wert.
///
/// **Warum es das gibt** (A-5.1): ein Treiber im Userland hält Caps auf seine Fenster, aber die
/// Adressen kennt er nicht — `map` legt sie an ihre physische Lage, und die steht nirgends im
/// Programm. Der bequeme Weg wäre ein Boot-Info-Block gewesen; der wäre eine Autoritätsquelle
/// neben dem Manifest, die jedes Programm ungefragt lesen kann. Hier beschreibt der Kernel
/// stattdessen genau die Cap, die der Aufrufer ohnehin schon hält — neue Autorität entsteht
/// dabei keine.
pub fn map_window(cap: u64) -> Option<Window> {
    let r = invoke(sys::MAP, cap, [0; 4], 0);
    if r.result != result::OK {
        return None;
    }
    Some(Window { base: r.msg[0], len: r.msg[1], iova: r.msg[2] })
}

/// Ein **gemapptes Fenster** — und die Grenze, innerhalb derer darauf zugegriffen werden darf.
///
/// ## Warum es diesen Typ gibt (A-6.3)
///
/// Ein Programm, das `map_window` bekommt, hält eine Adresse und eine Länge. Um sie zu benutzen,
/// braucht es rohe Zeigerzugriffe — also `unsafe`. Für eine **TrustedSAS**-PD ist das ein
/// Ausschlusskriterium: das Zertifikats-Gate (ADR 0014) verlangt `forbid(unsafe_code)`, und zwar
/// zu Recht — eine Komponente, die ihre Vertrauensstufe behält, muss auditierbar sein.
///
/// Der Ausweg ist **nicht**, die Regel aufzuweichen, sondern das `unsafe` dorthin zu legen, wo es
/// hingehört: in das auditierte SDK, das ohnehin auf der Allowlist steht. Dieser Typ ist die
/// Kapsel dafür. Er entsteht **nur** aus [`map_window`] — also aus Basis und Länge, die der
/// Kernel gerade selbst gemappt hat — und **jeder** Zugriff wird gegen die Länge geprüft. Ein
/// Programm kann ihn nicht fälschen: die Felder sind privat und es gibt keinen Konstruktor.
///
/// Das ist derselbe Gedanke wie bei `Verified` im Manifest-Parser: die Bedingung trägt der Typ,
/// nicht die Disziplin des Aufrufers.
#[derive(Clone, Copy)]
pub struct Window {
    base: u64,
    len: u64,
    iova: u64,
}

impl Window {
    /// Die Adresse, unter der **dieses Programm** das Fenster sieht.
    pub fn base(&self) -> u64 {
        self.base
    }
    /// Die Länge in Bytes.
    pub fn len(&self) -> u64 {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    /// Die **Gerätesicht** (IOVA) — `0`, wenn es keine gibt. Sie ist **nicht** die Basis; s.
    /// [`map_window`].
    pub fn iova(&self) -> u64 {
        self.iova
    }

    /// Ob `[off, off+n)` noch im Fenster liegt. Mit geprüfter Addition — eine Bereichsprüfung
    /// hinter einem übergelaufenen Produkt ist eine Attrappe.
    fn passt(&self, off: u64, n: u64) -> bool {
        off.checked_add(n).is_some_and(|e| e <= self.len)
    }

    /// `n` Bytes ab `off` lesen. `None`, wenn das nicht mehr ins Fenster passt — **kein**
    /// gekürzter Slice: ein halber Sektor sieht aus wie ein ganzer und wird als solcher gelesen.
    pub fn bytes(&self, off: u64, n: u64) -> Option<&[u8]> {
        if !self.passt(off, n) {
            return None;
        }
        // SAFETY: `base` und `len` stammen aus `SYS_MAP` — der Kernel hat genau diesen Bereich
        // eben in diese VSpace gemappt. `off + n <= len` ist geprüft. Nur lesend.
        Some(unsafe { core::slice::from_raw_parts((self.base + off) as *const u8, n as usize) })
    }

    /// Ein 64-Bit-Wort ab `off` lesen (little-endian, ungeprüfte Ausrichtung nicht nötig:
    /// `read_unaligned`).
    pub fn read_u64(&self, off: u64) -> Option<u64> {
        if !self.passt(off, 8) {
            return None;
        }
        // SAFETY: wie `bytes`.
        Some(unsafe { core::ptr::read_volatile((self.base + off) as *const u64) })
    }

    /// Ein einzelnes Byte ab `off` schreiben.
    pub fn write_u8(&self, off: u64, v: u8) -> Option<()> {
        if !self.passt(off, 1) {
            return None;
        }
        // SAFETY: wie `bytes`; `off < len` ist geprueft.
        unsafe { core::ptr::write_volatile((self.base + off) as *mut u8, v) };
        Some(())
    }

    /// Ein 16-Bit-Wort ab `off` schreiben (little-endian, byteweise — der Aufrufer darf nicht
    /// annehmen, dass `off` ausgerichtet ist).
    pub fn write_u16(&self, off: u64, v: u16) -> Option<()> {
        self.write_u8(off, v as u8)?;
        self.write_u8(off + 1, (v >> 8) as u8)
    }

    /// Ein 32-Bit-Wort ab `off` schreiben (little-endian, byteweise).
    pub fn write_u32(&self, off: u64, v: u32) -> Option<()> {
        for i in 0..4 {
            self.write_u8(off + i, (v >> (8 * i)) as u8)?;
        }
        Some(())
    }

    /// Ein 64-Bit-Wort ab `off` schreiben. `None`, wenn es nicht mehr ins Fenster passt.
    pub fn write_u64(&self, off: u64, v: u64) -> Option<()> {
        if !self.passt(off, 8) {
            return None;
        }
        // SAFETY: wie `bytes`; das Fenster wurde mit Schreibrecht gemappt, sonst haette
        // `SYS_MAP` es nicht herausgegeben.
        unsafe { core::ptr::write_volatile((self.base + off) as *mut u64, v) };
        Some(())
    }
}
/// Frame (Memory-Cap `cap`) wieder entfernen; gibt den Ergebniscode zurück.
pub fn unmap(cap: u64) -> u64 {
    invoke(sys::UNMAP, cap, [0; 4], 0).result
}
/// Ziel-PD steuern (PdControl-Cap `cap`, Sub-Op `subop`); gibt den Ergebniscode zurück.
pub fn pdctl(cap: u64, subop: u64) -> u64 {
    invoke(sys::PDCTL, cap, [subop, 0, 0, 0], 0).result
}
/// Programm `index` laden (Loader-Cap `cap`, delegierter Cap-Slot `delegate`, `u64::MAX`=keiner);
/// gibt den Ergebniscode zurück.
///
/// `badge` versieht die **delegierte Kopie** mit einem eigenen Etikett (`0` = Badge des Originals
/// erben). Das ist der Weg, die Signale mehrerer Kinder auseinanderzuhalten — s. [`signal`].
pub fn load(cap: u64, index: u64, delegate: u64, badge: u64) -> u64 {
    invoke(sys::LOAD, cap, [index, delegate, badge, 0], 0).result
}
/// **Einen Cap im eigenen Cspace löschen** (Slot `slot`); gibt den Ergebniscode zurück.
///
/// Braucht **kein** Cap: der eigene Cspace ist die Autorität. Ein langlebiger Dienst, der Caps per
/// IPC empfängt, muss sie loswerden können — sonst läuft er gegen sein Cap-Budget, ohne dass ihm
/// jemand etwas entzogen hätte. [`result::ERR_HASCHILDREN`] heißt: es hängen noch abgeleitete Caps
/// daran, der Slot ist **unverändert**.
pub fn cdelete(slot: u64) -> u64 {
    invoke(sys::CDELETE, slot, [0; 4], 0).result
}
/// **Einen Cap im eigenen Cspace kopieren** (A-3.2): `src` → `dst` (muss frei sein), Rechte
/// `rights` (1=R, 2=W, 4=X; wird mit den Rechten des Originals geschnitten), `badge` = Etikett der
/// Kopie (`0` = Badge des Originals erben).
///
/// Bei Notifications/Endpoints ist das Badge der Weg, zwei **unterscheidbare** Kanäle auf dasselbe
/// Objekt zu bekommen — s. [`signal`].
pub fn ccopy(src: u64, dst: u64, rights: u64, badge: u64) -> u64 {
    invoke(sys::CCOPY, src, [dst, rights, badge, 0], 0).result
}
/// **Einen Cap im eigenen Cspace verschieben** (A-3.2): `src` → `dst` (muss frei sein). Keine
/// Ableitung — derselbe Cap, ein anderer Slot.
pub fn cmove(src: u64, dst: u64) -> u64 {
    invoke(sys::CMOVE, src, [dst, 0, 0, 0], 0).result
}
/// **Empfangs-Slot festlegen** (A-3.2): wo per IPC übertragene Caps landen. Der Empfänger
/// entscheidet das, nicht der Sender.
pub fn setrecv(slot: u64) -> u64 {
    invoke(sys::SETRECV, slot, [0; 4], 0).result
}
/// Thread (Tcb-Cap `cap`) beenden; gibt den Ergebniscode zurück.
pub fn kill(cap: u64) -> u64 {
    invoke(sys::KILL, cap, [0; 4], 0).result
}
/// Freiwilliger Zeitscheibenabtritt.
pub fn yield_now() {
    let _ = invoke(sys::YIELD, 0, [0; 4], 0);
}
/// Sich selbst dauerhaft blockieren (kein Cap nötig).
pub fn park() -> ! {
    loop {
        let _ = invoke(sys::PARK, 0, [0; 4], 0);
    }
}
/// Sich selbst beenden (Stack/TCB werden zurückgewonnen).
pub fn exit() -> ! {
    let _ = invoke(sys::EXIT, 0, [0; 4], 0);
    loop {
        core::hint::spin_loop();
    }
}

/// Definiert den ELF-Entry-Point `_start` eines extern geladenen Programms.
///
/// Die `#[no_mangle]`-Glue (in aktuellem Rust ein *unsafe* Attribut) gehoert zur auditierten
/// SDK-Schicht (= Allowlist), damit das eigentliche Programm `#![forbid(unsafe_code)]` bleiben und
/// damit zertifiziert werden kann. `$main` ist eine **sichere** `fn(usize) -> !` des Programms (x0 =
/// Boot-Arg). Verwendung:  `libsel4lake::entry!(run);`  mit  `fn run(_arg: usize) -> ! { ... }`.
#[macro_export]
macro_rules! entry {
    ($main:path) => {
        #[no_mangle]
        pub extern "C" fn _start(arg: usize) -> ! {
            $main(arg)
        }
    };
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    // Kein Heap/Konsole im EL0-Programm verfügbar -> still parken; der Kernel beobachtet, dass
    // kein erwartetes Signal kam (bzw. ein Fault terminiert den Thread regulär).
    park()
}
