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
    /// Den aufrufenden Thread schlafen legen — **es sei denn, eine Weckmarke liegt vor**
    /// (Selbst-Park; kein Cap nötig, weil er nur auf sich selbst wirkt).
    ///
    /// Zusammen mit [`UNPARK`] ist das der Baustein, auf dem eine PD `wait_event`, Completions
    /// und Mutex-Warteschlangen baut, **ohne je ein Kernelobjekt anzulegen** (Z22, P4): die
    /// Warteschlange ist eine Liste im Speicher der PD. Ohne Weckmarke ist der unbelastete Weg
    /// „Bedingung ist schon wahr" **null Syscalls**.
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
    /// **Einen Thread der EIGENEN PD wecken** (Z22, P4). `x1` = rohe `ThreadId` des Ziels.
    ///
    /// Gegenstück zu [`PARK`]. Die Weckmarke wird **immer** hinterlegt, auch wenn das Ziel noch
    /// gar nicht schläft — genau dann ginge das Wecken sonst verloren. Geweckt wird nur, wer
    /// **wegen `PARK`** blockiert ist; wer in IPC wartet oder pausiert wurde, bleibt liegen.
    ///
    /// ## Warum das ohne Cap geht, und warum das keine Lücke ist
    ///
    /// Das Ziel muss in **derselben PD** liegen wie der Aufrufer (fail-closed: keine PD, fremde
    /// PD oder unbekannter Thread → [`result::ERR_BADCAP`]). Innerhalb einer PD teilen sich die
    /// Threads ohnehin den Adressraum — wer einen Nachbarthread wecken kann, konnte vorher schon
    /// seinen Stack beschreiben. Es kommt also **keine** Autorität hinzu, und eine Tcb-Cap je
    /// Thread wäre ein Slot je Warteschlangeneintrag, ohne etwas zu schützen.
    ///
    /// PD-übergreifend wecken bleibt, was es war: eine Notification.
    pub const UNPARK: u64 = 18;
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
    /// Zweites Nachrichten-Datenwort (`x3`).
    pub const MSG1: usize = 3;
    /// Drittes Nachrichten-Datenwort (`x4`).
    pub const MSG2: usize = 4;
    /// Viertes Nachrichten-Datenwort (`x5`).
    pub const MSG3: usize = 5;
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
    /// **Der Endpoint wird gerade stillgelegt** (A-4.2, ruhender Punkt): ein Austausch der
    /// Server-Instanz läuft, deshalb wird keine *neue* Transaktion mehr eröffnet. Laufende
    /// Transaktionen dürfen abschliessen (`REPLY` bleibt erlaubt) — abgewiesen werden nur
    /// `CALL` und `RECV`.
    ///
    /// Der Unterschied zu [`ERR_SERVER_GONE`] ist der, auf den es ankommt: dort ist eine
    /// **begonnene** Transaktion verloren, hier ist eine **nicht begonnene** abgewiesen. Ein
    /// Client darf hierauf gefahrlos wiederholen, sobald der Austausch durch ist; dort muss er
    /// wissen, ob der Server die Wirkung schon hatte. Denselben Code für beides zu nehmen
    /// hiesse, dem Client diesen Unterschied zu verschweigen.
    pub const ERR_QUIESCING: u64 = 8;
    /// **Die Warteschlange dieses Endpoints ist voll** (D11): mehr als `QUEUE_CAP` Threads
    /// können an *einem* Endpoint nicht zugleich blockieren.
    ///
    /// Vor diesem Code gab es die Lage nicht als Antwort, sondern als Loch: der 33. Sender
    /// wurde **trotzdem** blockiert, landete in keiner Struktur des Endpoints, bekam keinen
    /// Ergebniscode und wurde nie geweckt — und `is_quiescent()` meldete ihn als *ruhig*.
    /// Ein Thread ging verloren, und jeder Prüfer meldete Ordnung.
    ///
    /// **Warum ein dritter Code und nicht [`ERR_QUIESCING`]** (die Frage, die D11 offenließ):
    /// die drei Lagen verlangen vom Client verschiedene Antworten. [`ERR_BADCAP`] heisst „gibt
    /// es nicht" — nie wieder versuchen. `ERR_QUIESCING` heisst „kommt gleich wieder" — warten,
    /// bis der Austausch durch ist; die Wartezeit ist durch den Austausch begrenzt und hängt
    /// nicht am Verhalten anderer Clients. `ERR_EP_FULL` heisst „gerade kein Platz" — das ist
    /// eine **Lastaussage**: sie hängt an den anderen 32 Wartenden, kann sofort wieder gelten,
    /// und ein Client, der stumpf wiederholt, verschärft sie. Wer die beiden zusammenwürfe,
    /// nähme dem Client genau die Unterscheidung, die A-4.2 zwischen `ERR_QUIESCING` und
    /// `ERR_BADCAP` gerade eingeführt hat.
    pub const ERR_EP_FULL: u64 = 9;
}
