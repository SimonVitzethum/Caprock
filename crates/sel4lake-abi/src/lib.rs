#![no_std]
//! Geteilte Kernel<->Thread-ABI (ADR 0004).
//!
//! Syscalls werden über `SVC #0` ausgelöst; Argumente und Rückgaben liegen in
//! General-Purpose-Registern (register-basierte IPC für niedrige Latenz). Diese
//! Crate definiert die Nummern, das Register-Layout und die Ergebniscodes — die
//! einzige gemeinsame Schnittstelle zwischen Aufrufer-Thread und Kernel.

/// Syscall-Nummern (Register `x0` beim Eintritt).
pub mod sys {
    pub const YIELD: u64 = 0;
    /// Synchroner Aufruf: Nachricht senden + auf Antwort warten.
    pub const CALL: u64 = 1;
    /// Auf einen Aufrufer warten (Server).
    pub const RECV: u64 = 2;
    /// Den zuletzt empfangenen Aufrufer beantworten.
    pub const REPLY: u64 = 3;
    /// Den aufrufenden Thread dauerhaft blockieren (Selbst-Park; kein Cap nötig).
    pub const PARK: u64 = 5;
    /// Den aufrufenden Thread beenden (Stack/TCB werden zurückgewonnen; kein Cap).
    pub const EXIT: u64 = 6;
    /// Den über eine Tcb-Cap bezeichneten Thread beenden (cap-kontrolliert).
    pub const KILL: u64 = 7;
    /// Über eine Notification-Cap signalisieren (asynchron, nicht blockierend).
    pub const SIGNAL: u64 = 8;
    /// Auf eine Notification warten (blockiert; liefert den Badge in `x1`).
    pub const WAIT: u64 = 9;
    /// Einen über eine Memory-Cap bezeichneten Frame in die **eigene VSpace** mappen
    /// (identity, EL0-RW; cap-gated). Nur in einer isolierten VSpace sinnvoll.
    pub const MAP: u64 = 10;
    /// Einen zuvor gemappten Frame wieder aus der eigenen VSpace entfernen.
    pub const UNMAP: u64 = 11;
    /// **PD-Management** (ext-22): Lifecycle einer Ziel-PD steuern, gated auf eine
    /// `PdControl`-Cap (WRITE). Die Sub-Operation steht in `x2` (s. [`pdctl`]), Argumente
    /// in `x3`..`x5`. Nur eine TrustedSas-PD darf eine UserLand-PD so steuern.
    pub const PDCTL: u64 = 12;
    /// **Binary-Loader laden** (ext-26): ein Programm aus dem Boot-Archiv zur Laufzeit laden +
    /// starten, gated auf eine `Loader`-Cap (WRITE). `x1` = Loader-Cap-Index, `x2` = Archiv-
    /// Programm-Index, `x3` = lokaler Cap-Index, der in **Slot 0** der neuen PD delegiert wird
    /// (`u64::MAX` = keiner). Rückgabe: `x0` = result, `x1` = neue PD-Id (bei OK). Der geladene
    /// Prozess erhält NUR die so explizit delegierten Caps (keine Sonderrechte über die Cap).
    pub const LOAD: u64 = 13;
    /// **Einen Cap im eigenen Cspace löschen** (A-3.1). `x1` = lokaler Slot. Kein zusätzliches
    /// Cap nötig — die Autorität ist der eigene Cspace: Autorität *abzugeben* darf nie an einer
    /// Erlaubnis hängen.
    ///
    /// Ohne das kann ein langlebiger Dienst, der Caps per IPC empfängt, seine Slots nicht
    /// freigeben und läuft gegen `CAP_BUDGET_PER_PD`. Mit dem Root-Task (A-2) wird aus dieser
    /// ABI-Lücke ein Betriebsproblem: er ist genau so ein Dienst.
    pub const CDELETE: u64 = 14;
    /// **Einen Cap im eigenen Cspace kopieren** (A-3.2). `x1` = Quell-Slot, `x2` = Ziel-Slot
    /// (muss frei sein), `x3` = gewünschte Rechte-Bitmaske (1=R, 2=W, 4=X), `x4` = Badge für die
    /// Kopie (`0` = Badge des Originals erben). Die Kopie bekommt **höchstens** die Rechte des
    /// Originals — eine Kopie darf nie mehr können als die Vorlage.
    ///
    /// Das Badge ist hier kein Beiwerk: bei Notifications und Endpoints steckt die Absender-
    /// Kennung in der **Cap**, nicht in der Nachricht. Wer zwei unterscheidbare Kanäle auf
    /// dasselbe Objekt braucht, badgt zwei Kopien — genau dafür ist die Operation da.
    pub const CCOPY: u64 = 15;
    /// **Einen Cap im eigenen Cspace verschieben** (A-3.2). `x1` = Quell-Slot, `x2` = Ziel-Slot
    /// (muss frei sein). Erzeugt **keine** Ableitung: derselbe Cap, ein anderer Slot.
    pub const CMOVE: u64 = 16;
    /// **Den Empfangs-Slot für per IPC übertragene Caps festlegen** (A-3.2). `x1` = Slot.
    ///
    /// Der **Empfänger** bestimmt, wo eine gegrantete Cap landet — nicht der Sender. Ohne das
    /// landete jeder Grant im festen [`GRANT_RECV_SLOT`], und ein Server konnte damit den Cap
    /// verdrängen, den sein Client dort gerade hielt.
    pub const SETRECV: u64 = 17;
}

/// Sub-Operationen für [`sys::PDCTL`] (Register `x2`). Jede ist auf den Besitz der
/// `PdControl`-Cap für die Ziel-PD beschränkt.
pub mod pdctl {
    /// Ziel-PD starten (initial blockierten Haupt-Thread wecken).
    pub const START: u64 = 0;
    /// Ziel-PD stoppen (Thread beenden + isolierte Ressourcen abbauen).
    pub const STOP: u64 = 1;
    /// Ziel-PD pausieren (Thread blockieren, ohne ihn zu beenden).
    pub const PAUSE: u64 = 2;
    /// Pausierte Ziel-PD fortsetzen (Thread entblocken).
    pub const RESUME: u64 = 3;
    /// Der Ziel-PD ein CPU-Budget zuweisen (Arg: SchedContext-Cap-Slot in `x3`).
    pub const ASSIGN_BUDGET: u64 = 4;
}

/// Anzahl der Nachrichten-Datenwörter (Register `x2`..`x5`).
pub const MSG_WORDS: usize = 4;

/// Capability-Transfer: Ist dieses Bit im Tag (`x6`) gesetzt, überträgt ein
/// `REPLY` zusätzlich die Capability am lokalen Slot `tag & 0xff` des Servers an
/// den Aufrufer (in dessen [`GRANT_RECV_SLOT`]).
pub const GRANT_FLAG: u64 = 1 << 63;
/// Slot im Empfänger-Cspace, in dem eine per IPC übertragene Cap landet.
pub const GRANT_RECV_SLOT: usize = 1;

/// Register-Indizes im TrapFrame (`gpr[i]` == `xi`).
pub mod reg {
    /// Eintritt: Syscall-Nummer · Austritt: Ergebniscode.
    pub const SYSNO_RESULT: usize = 0;
    /// Eintritt: Endpoint-ID · Austritt: Badge des Senders.
    pub const EP_BADGE: usize = 1;
    /// Erstes Nachrichten-Datenwort (`x2`); weitere folgen aufsteigend.
    pub const MSG0: usize = 2;
    /// Nachrichten-Tag/Label (`x6`).
    pub const TAG: usize = 6;
}

/// Ergebniscodes (Register `x0` beim Austritt).
pub mod result {
    pub const OK: u64 = 0;
    /// Kein gültiger Capability an der angegebenen Stelle / falscher Objekttyp.
    pub const ERR_BADCAP: u64 = 1;
    /// Unbekannte Syscall-Nummer.
    pub const ERR_BADSYS: u64 = 2;
    /// Capability hat nicht die nötigen Rechte für diese Operation.
    pub const ERR_RIGHTS: u64 = 3;
    /// Aufrufer gehört zu keiner Protection Domain.
    pub const ERR_NOPD: u64 = 4;
    /// Operation auf einem Cap, von dem noch Kopien/Mints abgeleitet sind (CDT-Kinder). Der Cap
    /// bleibt unverändert im Slot — ein halb entfernter Cap wäre schlimmer als gar keiner.
    pub const ERR_HASCHILDREN: u64 = 6;
    /// Kein Platz: der Ziel-Slot ist belegt, oder das Cap-Budget der PD ist erschöpft.
    pub const ERR_NOSPACE: u64 = 7;
    /// **Antwort-seitiger Liveness-Fehler:** der Server, der eine Antwort schuldete
    /// (Reply-Owner), ist verschwunden (KILL/EXIT/Fault/Reload), bevor er antworten
    /// konnte. Der blockierte `CALL`-Aufrufer wird damit entblockt, statt dauerhaft zu
    /// hängen — der Client kann den Fehler behandeln (Retry/Abbruch).
    pub const ERR_SERVER_GONE: u64 = 5;
}
