//! **K1b: several thread stacks out of ONE `Memory` Cap — the probe.**
//!
//! Until 2026-08-26 one Cap bought exactly one thread, and the Cap stayed pinned for that thread's
//! life. "How many threads may a PD have" was therefore really "how many Cap slots are left" —
//! which is the wrong question, because threads of one PD share the address space anyway and a
//! separate Cap per stack buys **no isolation**. A driver PD spending 6 of its 8 slots on its
//! endowment could spawn at most two.
//!
//! ## What this line measures that nothing else did
//!
//! `SYS_SPAWN` was built on 2026-08-17 and, until today, **had no caller and no gate anywhere in
//! the tree** — 0 hits outside its own definition and one number anchor. `pdthrd` (Z22 P2) proves
//! that a PD *can* hold two threads, but its threads are made by the **kernel**; the ABI path an
//! ordinary program takes had never executed. So this probe carries two claims at once, and the
//! order matters: first that the syscall works at all (`ganze-region`, the `x1 == 0` form every
//! caller written before the sub-region gets for free), then that a window inside a Cap works.
//!
//! ## Why each conjunct is the one that can fall
//!
//! * **`slots==2`** is the point. Four stacks that cost four Caps would satisfy "four threads ran"
//!   just as well. *„spawn gab `Some`" zaehlt nicht* — and neither does "four threads exist".
//! * **The magic word per window** is the disjointness. Four threads on one arena would also
//!   "work" while trampling each other; each child writes a word derived from **its own base**,
//!   keeps re-reading it, and reports corruption if anybody overwrote it. Without this a mutation
//!   that ignores the offset entirely would look green.
//! * **`runden>0` per child** is the liveness. A region carrying a value proves a thread executed
//!   there; the capacity curve once counted 3040 "isolated processes" that had all faulted at their
//!   own entry address, and it was a side observation that caught it, not a check.
//! * **The two refusals** are the guard rails the sub-region *creates*: an overlapping window and
//!   a window outside the Cap. `ueberlappung` in particular could not fire before today — see
//!   `system::stack_sibling_overlaps`.
//! * **`ERR_INUSE`** is the K1a promise, now under a Cap that carries five stacks instead of one.
//!
//! ## The carriers are EL0 assembly, and that is not stylistic
//!
//! A child written in Rust builds a stack frame. Under the mutation "ignore the offset" all four
//! would share one stack top, corrupt each other's return addresses and die — several conjuncts
//! would fall at once and the counter-proof would say nothing about the property it is aimed at
//! (*eine Mutation, die zwei Dinge zugleich kaputtmacht, beweist nichts ueber das gemeinte*).
//! These children touch the stack pointer never; they only store through it. Under that mutation
//! exactly the magic word collides.

use crate::system;
use caprock_hal::println;

/// Der Ausgang dieser Sonde — dreiwertig (s. `crate::befund`). Vorgabe ist `NichtGefahren`: ein
/// Aufbau, der an einem SKIP-Ausgang abbricht, ist weder bestanden noch durchgefallen.
static ARENA_OK: crate::befund::AtomicBefund = crate::befund::AtomicBefund::neu();

/// Urteil der `arena`-Zeile, fuer beide Hochlaufwege.
pub fn urteil() -> crate::befund::Befund {
    ARENA_OK.lesen()
}

const PAGE: u64 = 4096;
/// Seiten je Kindstapel — `spawncheck::MIN_STACK_BYTES` sind 16 KiB, also genau vier.
const STACK_PAGES: u64 = 4;
/// Kinder auf der Arena. Vier, weil das die Zahl ist, die der Treiberfall braucht (Boot-,
/// Timer- und zwei Workqueue-Threads) und die heute an `CAP_BUDGET_PER_PD` scheitert.
const KINDER: usize = 4;
/// Die Arena: vier Stapel **und** eine Parameterseite dahinter.
const ARENA_PAGES: u64 = KINDER as u64 * STACK_PAGES + 1;
/// Offset der Parameterseite in der Arena.
const PARAM_OFF: u64 = KINDER as u64 * STACK_PAGES * PAGE;
/// Die zweite Cap: sie traegt EINEN Stapel und wird mit `x1 == 0` benutzt.
const GANZ_PAGES: u64 = STACK_PAGES;

const SLOT_ARENA: u64 = 0;
const SLOT_GANZ: u64 = 1;

// --- Die Parameterseite: ausgeschrieben, nicht abgezaehlt -------------------------------------
//
// In C8 kollidierte eine Marke auf Bit 63 mit dem obersten Zahlenfeld eines Ergebniswortes; das
// Urteil fiel durch, der Lauf ging in den Watchdog, und jedes gedruckte Feld war gruen. Wer Zahlen
// und Marken in einen Bereich packt, schreibt die Belegung hin.
//
//   Eingaben (der Kernel schreibt, der Elternthread liest)
const P_ARENA: u64 = 0x00; // Basis der Arena (im SAS gleich der Physadresse)
const P_GANZ: u64 = 0x08; // Basis der Ein-Stapel-Cap
const P_ENTRY: u64 = 0x10; // Einsprung der Kinder
//   Ausgaben (der Elternthread schreibt, der Kernel liest)
const P_ELTER_LIEF: u64 = 0x18; // != 0, sobald der Elternthread ueberhaupt lief
const P_MASKE: u64 = 0x20; // Bit i: der i-te Arena-Spawn meldete OK
const P_GANZ_CODE: u64 = 0x28; // Ergebnis des `x1 == 0`-Spawns
const P_UEBERLAPP: u64 = 0x30; // Ergebnis des ueberlappenden Fensters
const P_AUSSERHALB: u64 = 0x38; // Ergebnis des Fensters ausserhalb der Cap
const P_CDELETE: u64 = 0x40; // Ergebnis des `CDELETE` auf die Stack-Cap
const P_FERTIG: u64 = 0x48; // != 0, wenn der Elternthread alles abgearbeitet hat

// --- Das Fenster eines Kindes -------------------------------------------------------------
const K_MAGIE: u64 = 0x00;
const K_RUNDEN: u64 = 0x08;
const K_KORRUPT: u64 = 0x10;

/// Die Magie, die ein Kind in **sein** Fenster legt. Aus der Basis abgeleitet, also je Kind
/// verschieden und vom Kernel nachrechenbar — ein konstanter Wert liesse „das richtige Kind hat
/// hier geschrieben" von „irgendjemand hat hier geschrieben" nicht unterscheiden.
fn magie_von(base: u64) -> u64 {
    base.wrapping_add(0x777)
}

/// Runden, nach denen ein Kind parkt. Es soll die Korruption noch sehen koennen, aber danach die
/// CPU freigeben — ein dauerhaft drehender EL0-Thread verschoebe die Baseline jeder spaeteren
/// Messung des Laufs.
const KIND_RUNDEN: u64 = 1 << 20;

// --- Das Kind: reines EL0-Assembler --------------------------------------------------------
//
// `.user_text` ist keine Formalie: ein Ring-3-Einsprung in `.text` ist aus User-Modus nicht
// ausfuehrbar und faultet an seiner EIGENEN Einsprungadresse -- im Log sieht das aus wie ein
// kaputter Mechanismus und ist eine fehlende Sektionsangabe.
#[cfg(target_arch = "aarch64")]
core::arch::global_asm!(
    r#"
.section .user_text,"ax"
.globl caprock_arena_kind
caprock_arena_kind:
    mov  x9, x0                  // x9 = Basis des eigenen Fensters
    add  x10, x9, #0x777         // x10 = Magie
    str  x10, [x9]               // ablegen
    mov  x11, xzr                // Rundenzaehler
    ldr  x13, ={runden}
1:  ldr  x12, [x9]
    cmp  x12, x10
    b.ne 2f
    add  x11, x11, #1
    str  x11, [x9, #8]
    cmp  x11, x13
    b.lo 1b
    b    3f
2:  mov  x12, #1
    str  x12, [x9, #16]          // jemand hat mein Fenster ueberschrieben
3:  mov  x0, #5                  // sys::PARK
    svc  #0
    b    3b
"#,
    runden = const KIND_RUNDEN,
);

#[cfg(target_arch = "x86_64")]
core::arch::global_asm!(
    r#"
.section .user_text,"ax"
.globl caprock_arena_kind
caprock_arena_kind:
    mov  r9, rdi                 // r9 = Basis des eigenen Fensters
    lea  r10, [r9 + 0x777]       // r10 = Magie
    mov  [r9], r10
    xor  r11, r11                // Rundenzaehler
    mov  r13, {runden}
1:  mov  r12, [r9]
    cmp  r12, r10
    jne  2f
    inc  r11
    mov  [r9 + 8], r11
    cmp  r11, r13
    jb   1b
    jmp  3f
2:  mov  qword ptr [r9 + 16], 1  // jemand hat mein Fenster ueberschrieben
3:  mov  rax, 5                  // sys::PARK
    int  0x80
    jmp  3b
"#,
    runden = const KIND_RUNDEN,
);

extern "C" {
    /// Einsprungsymbol des EL0-Kindes (s. `global_asm!` oben).
    static caprock_arena_kind: u8;
}

/// **`SYS_SPAWN` aus EL0/Ring 3**, mit allen sechs Argumenten.
///
/// # Safety
/// Nur aus User-Kontext zu rufen.
///
/// `#[inline(always)]` ist keine Formalie: ein nicht eingebetteter Helfer laege in `.text`, waere
/// aus User-Modus nicht ausfuehrbar, und der Fehler wanderte nur eine Ebene tiefer.
#[inline(always)]
unsafe fn user_spawn(sub: u64, slot: u64, entry: u64, arg: u64, prio: u64) -> (u64, u64) {
    let code: u64;
    let tid: u64;
    #[cfg(target_arch = "x86_64")]
    // SAFETY: siehe Funktionsdoku. Die Abbildung ABI -> GPR ist
    // `caprock_hal::x86_64::exception::ABI_TO_GPR` (x0=rax, x1=rdi, x2=rsi, x3=rdx, x4=r10, x5=r8).
    unsafe {
        core::arch::asm!("int 0x80",
                         inlateout("rax") caprock_abi::sys::SPAWN => code,
                         inlateout("rdi") sub => _,
                         inlateout("rsi") slot => tid,
                         in("rdx") entry, in("r10") arg, in("r8") prio,
                         clobber_abi("sysv64"));
    }
    #[cfg(target_arch = "aarch64")]
    // SAFETY: siehe Funktionsdoku.
    unsafe {
        core::arch::asm!("svc #0",
                         inlateout("x0") caprock_abi::sys::SPAWN => code,
                         inlateout("x1") sub => _,
                         inlateout("x2") slot => tid,
                         in("x3") entry, in("x4") arg, in("x5") prio,
                         clobber_abi("C"));
    }
    (code, tid)
}

/// Ein Syscall mit einem Argument in **x1** (`CDELETE` nimmt den Slot dort, nicht in `MSG0`).
///
/// # Safety
/// Nur aus User-Kontext zu rufen.
///
/// Die erste Fassung legte das Argument nach `MSG0`; `CDELETE` las daraufhin einen fremden
/// Registerinhalt als Slot und meldete `ERR_BADCAP` -- eine Absage, die wie ein Befund ueber die
/// Stack-Cap aussah und ein Registerfehler war.
#[inline(always)]
unsafe fn user_syscall1(nr: u64, a1: u64) -> u64 {
    let out: u64;
    #[cfg(target_arch = "x86_64")]
    // SAFETY: siehe Funktionsdoku.
    unsafe {
        core::arch::asm!("int 0x80", inlateout("rax") nr => out, in("rdi") a1,
                         clobber_abi("sysv64"));
    }
    #[cfg(target_arch = "aarch64")]
    // SAFETY: siehe Funktionsdoku.
    unsafe {
        core::arch::asm!("svc #0", inlateout("x0") nr => out, in("x1") a1,
                         clobber_abi("C"));
    }
    out
}

/// **Der Elternthread** (EL0, SAS): er liest seine Parameterseite, erzeugt die Kinder ueber die
/// ABI und schreibt jedes Ergebnis zurueck.
///
/// Er hat seinen **eigenen** Stapel aus dem Allokator und faellt damit nicht unter die
/// Disjunktheitsaussage — das ist Absicht: der Messgegenstand sind die Fenster der Kinder, nicht
/// seiner.
///
/// Er ist in Rust geschrieben (anders als die Kinder), weil er einen echten Stapel hat und weil
/// sieben Syscalls mit sechs Argumenten in Assembler eine Fehlerquelle ohne Gegenwert waeren.
///
/// **`.user_text` ist keine Formalie, und das hat dieser Eintrag beim ersten Lauf selbst bezahlt.**
/// Ohne die Sektionsangabe lag er in `.text`, war aus Ring 3 nicht ausfuehrbar und faultete an
/// seiner EIGENEN Einsprungadresse: `el0-trap ... FAR=0x160f00`, und `nm` sagte
/// `0000000000160f00 t ...spawnarena5elter`. Im Protokoll sah das aus wie ein kaputter
/// Mechanismus und war eine fehlende Zeile. Jeder Helfer, den er ruft, ist deshalb
/// `#[inline(always)]` -- sonst laege JENER in `.text` und der Fehler wanderte eine Ebene tiefer.
#[link_section = ".user_text"]
extern "C" fn elter(arg: usize) -> ! {
    let p = arg as u64;
    // SAFETY: identitaetsgemappte, EL0-zugaengliche RAM-Seite; der Kernel hat sie vor dem Spawn
    // geschrieben und liest sie erst nach `P_FERTIG` wieder.
    let lies = |off: u64| unsafe { core::ptr::read_volatile((p + off) as *const u64) };
    // SAFETY: dieselbe Seite.
    let schreib = |off: u64, v: u64| unsafe {
        core::ptr::write_volatile((p + off) as *mut u64, v);
    };
    schreib(P_ELTER_LIEF, 1);

    let arena = lies(P_ARENA);
    let ganz = lies(P_GANZ);
    let entry = lies(P_ENTRY);
    let prio = crate::system::IDLE_PRIO as u64;

    // 1. Der `x1 == 0`-Fall ZUERST: die Form, die jeder vor der Teilregion geschriebene Aufruf
    //    kodiert. Faellt sie, ist alles darunter eine Aussage ueber eine kaputte Grundlage.
    // SAFETY: User-Kontext.
    let (c_ganz, _) = unsafe { user_spawn(0, SLOT_GANZ, entry, ganz, prio) };
    schreib(P_GANZ_CODE, c_ganz);

    // 2. Vier Fenster in EINER Cap.
    let mut maske = 0u64;
    let mut i = 0u64;
    while i < KINDER as u64 {
        let off = i * STACK_PAGES;
        let sub = (off << 32) | STACK_PAGES;
        let basis = arena + off * PAGE;
        // SAFETY: User-Kontext.
        let (code, _) = unsafe { user_spawn(sub, SLOT_ARENA, entry, basis, prio) };
        if code == caprock_abi::result::OK {
            maske |= 1 << i;
        }
        i += 1;
    }
    schreib(P_MASKE, maske);

    // 3. Die zwei Absagen, die es ohne die Teilregion gar nicht geben koennte.
    //    (a) ein Fenster, das die Stapel 0 und 1 ueberlappt
    // SAFETY: User-Kontext.
    let (c_ueber, _) = unsafe {
        user_spawn((2u64 << 32) | STACK_PAGES, SLOT_ARENA, entry, arena, prio)
    };
    schreib(P_UEBERLAPP, c_ueber);
    //    (b) ein Fenster, das ueber das Ende der Cap hinausragt
    // SAFETY: User-Kontext.
    let (c_aus, _) = unsafe {
        user_spawn(((ARENA_PAGES - 1) << 32) | STACK_PAGES, SLOT_ARENA, entry, arena, prio)
    };
    schreib(P_AUSSERHALB, c_aus);

    // 4. Und die K1a-Zusage unter einer Cap, die jetzt VIER Stapel traegt: sie laesst sich nicht
    //    loeschen. Zuletzt, damit ein Fehlschlag der Wiedereinsetzung nichts darueber kippt.
    // SAFETY: User-Kontext.
    let c_del = unsafe { user_syscall1(caprock_abi::sys::CDELETE, SLOT_ARENA) };
    schreib(P_CDELETE, c_del);

    schreib(P_FERTIG, 1);
    loop {
        // SAFETY: User-Kontext.
        unsafe { user_syscall1(caprock_abi::sys::PARK, 0) };
    }
}

/// Ticks verstreichen lassen — **in Ticks und nicht in Runden**. Eine Zaehlschleife misst die
/// Geschwindigkeit des Wartenden, nicht den Fortschritt der anderen.
fn warte(ticks: u64) {
    let t0 = caprock_hal::timer::ticks(0);
    let mut wache = 0u64;
    while caprock_hal::timer::ticks(0).wrapping_sub(t0) < ticks && wache < 400_000_000 {
        core::hint::spin_loop();
        wache += 1;
    }
}

/// Eine Region nullen. **Der Kernel tut das, nicht der gemessene Pfad** — sonst waere „hier steht
/// ein Wert" von „hier stand schon einer" nicht zu unterscheiden, und ein Restwert aus einer
/// frueheren Belegung liesse die Zeile gruen melden, ohne dass ein Kind gelaufen ist.
///
/// # Safety
/// `base`/`len` muessen eine frisch allozierte, identitaetsgemappte Region sein.
unsafe fn nullen(base: u64, len: u64) {
    let mut o = 0u64;
    while o + 8 <= len {
        // SAFETY: siehe Funktionsdoku.
        unsafe { core::ptr::write_volatile((base + o) as *mut u64, 0) };
        o += 8;
    }
}

/// **Die Sonde.** Von beiden Hochlaufwegen gerufen.
///
/// Aufbau und Ablauf sind getrennt, weil jeder Abbruch im Aufbau ein **SKIP mit Grund** ist und
/// kein Urteil: „nicht messbar" ist weder bestanden noch durchgefallen, und ein Aufbau, der still
/// auf die Erfolgszeile durchfaellt, waere die schlimmere Haelfte davon.
pub fn messen() {
    let Some(pd) = system::create_pd() else {
        println!("arena  : SKIP (keine PD frei)");
        ARENA_OK.uebersprungen();
        return;
    };
    let Some(arena) = system::alloc(ARENA_PAGES * PAGE, PAGE) else {
        println!("arena  : SKIP (kein Speicher fuer die Arena)");
        ARENA_OK.uebersprungen();
        let _ = system::free_pd_slot(pd);
        return;
    };
    let (a_base, a_len) = (arena.base(), arena.len());
    let Some(ganz) = system::alloc(GANZ_PAGES * PAGE, PAGE) else {
        println!("arena  : SKIP (kein Speicher fuer die Ein-Stapel-Cap)");
        ARENA_OK.uebersprungen();
        let _ = system::free_pd_slot(pd);
        return;
    };
    let g_base = ganz.base();
    // SAFETY: beide Regionen sind frisch alloziert und identitaetsgemappt.
    unsafe {
        nullen(a_base, a_len);
        nullen(g_base, ganz.len());
    }
    let (Ok(cap_a), Ok(cap_g)) = (system::cap_install(arena), system::cap_install(ganz)) else {
        println!("arena  : SKIP (Memory-Cap nicht installierbar)");
        ARENA_OK.uebersprungen();
        let _ = system::free_pd_slot(pd);
        return;
    };
    if !system::install_pd_cap(pd, SLOT_ARENA as usize, cap_a)
        || !system::install_pd_cap(pd, SLOT_GANZ as usize, cap_g)
    {
        println!("arena  : SKIP (Cap-Slot der PD nicht belegbar)");
        ARENA_OK.uebersprungen();
        let _ = system::free_pd_slot(pd);
        return;
    }

    let param = a_base + PARAM_OFF;
    let entry = core::ptr::addr_of!(caprock_arena_kind) as u64;
    // SAFETY: identitaetsgemappte Seite der eben allozierten Arena.
    unsafe {
        core::ptr::write_volatile((param + P_ARENA) as *mut u64, a_base);
        core::ptr::write_volatile((param + P_GANZ) as *mut u64, g_base);
        core::ptr::write_volatile((param + P_ENTRY) as *mut u64, entry);
    }

    let Some(p) = system::spawn_user_parked(elter as *const () as usize, param as usize,
                                            system::IDLE_PRIO)
    else {
        println!("arena  : SKIP (Elternthread nicht startbar)");
        ARENA_OK.uebersprungen();
        let _ = system::free_pd_slot(pd);
        return;
    };
    system::bind_pd_parked(&p, pd);
    let Some(_t_elter) = system::admit(p) else {
        println!("arena  : SKIP (Elternthread nicht zulassbar)");
        ARENA_OK.uebersprungen();
        let _ = system::free_pd_slot(pd);
        return;
    };
    ablauf(pd, a_base, g_base, param);
}

/// Hat jedes Kind seine Magie abgelegt? **Nur ein Wartekriterium, kein Urteil** — das Urteil
/// unten liest jeden Wert einzeln und nennt ihn.
fn alle_haben_geschrieben(a_base: u64, g_base: u64) -> bool {
    let mut i = 0u64;
    while i < KINDER as u64 {
        let b = a_base + i * STACK_PAGES * PAGE;
        // SAFETY: identitaetsgemappte Arena-Seite.
        if unsafe { core::ptr::read_volatile((b + K_MAGIE) as *const u64) } != magie_von(b) {
            return false;
        }
        i += 1;
    }
    // SAFETY: identitaetsgemappte Seite der Ein-Stapel-Cap.
    unsafe { core::ptr::read_volatile((g_base + K_MAGIE) as *const u64) == magie_von(g_base) }
}

/// Der Ablauf, nachdem der Aufbau steht.
fn ablauf(pd: usize, a_base: u64, g_base: u64, param: u64) {
    // SAFETY: identitaetsgemappte Seite; der Elternthread schreibt sie, hier wird nur gelesen.
    let lies = |off: u64| unsafe { core::ptr::read_volatile((param + off) as *const u64) };

    // **Auf den Zustand warten, statt ihn vorauszusetzen** -- mit Frist, und beobachtet wird die
    // Groesse selbst.
    let mut runden = 0;
    while runden < 40 && lies(P_FERTIG) == 0 {
        warte(1);
        runden += 1;
    }
    let elter_lief = lies(P_ELTER_LIEF) != 0;
    let fertig = lies(P_FERTIG) != 0;
    if !elter_lief {
        // **Ein Elternthread, der nie lief, ist kein Befund ueber die Teilregion.** Jedes Feld
        // darunter waere 0, und lauter Nullen sind von „alles abgewiesen" nicht zu unterscheiden.
        println!(
            "arena  : SKIP (der Elternthread hat nie gelaufen -- ohne ihn ist NICHTS gemessen; \
             ein FAILURES hier waere ein Schluss von Schweigen auf Abwesenheit)"
        );
        ARENA_OK.uebersprungen();
        return;
    }

    // **Auf die WIRKUNG warten, nicht auf die Meldung.** `P_FERTIG` sagt, dass der Elternthread
    // seine sieben Syscalls hinter sich hat -- ueber die Kinder sagt es nichts: sie sind zugelassen
    // und noch nicht eingeplant gewesen. Der erste Lauf las genau hier lauter Nullen und haette
    // ohne diese Schleife „die Fenster tragen nicht" gemeldet, wo „noch nicht gelaufen" stand.
    // Gewartet wird auf die Groesse selbst, mit Frist.
    let mut runden = 0;
    while runden < 40 && !alle_haben_geschrieben(a_base, g_base) {
        warte(1);
        runden += 1;
    }

    let maske = lies(P_MASKE);
    let c_ganz = lies(P_GANZ_CODE);
    let c_ueber = lies(P_UEBERLAPP);
    let c_aus = lies(P_AUSSERHALB);
    let c_del = lies(P_CDELETE);

    // --- Die Wirkung in den Fenstern: was nur ein LAUFENDER Traeger hinterlassen kann ---------
    //
    // Gezaehlt wird nicht „spawn gab OK", sondern was in der Region steht. Die Arena war vor dem
    // Start genullt (vom Kernel, nicht vom gemessenen Pfad), also ist jeder Wert hier ein Beleg.
    let mut magisch = 0usize;
    let mut lebendig = 0usize;
    let mut korrupt = 0usize;
    let mut i = 0u64;
    while i < KINDER as u64 {
        let b = a_base + i * STACK_PAGES * PAGE;
        // SAFETY: identitaetsgemappte Arena-Seite.
        let (m, r, k) = unsafe {
            (
                core::ptr::read_volatile((b + K_MAGIE) as *const u64),
                core::ptr::read_volatile((b + K_RUNDEN) as *const u64),
                core::ptr::read_volatile((b + K_KORRUPT) as *const u64),
            )
        };
        if m == magie_von(b) {
            magisch += 1;
        }
        if r > 0 {
            lebendig += 1;
        }
        if k != 0 {
            korrupt += 1;
        }
        i += 1;
    }
    // SAFETY: identitaetsgemappte Seite der Ein-Stapel-Cap.
    let (g_m, g_r) = unsafe {
        (
            core::ptr::read_volatile((g_base + K_MAGIE) as *const u64),
            core::ptr::read_volatile((g_base + K_RUNDEN) as *const u64),
        )
    };

    let threads = system::pd_thread_count(pd);
    let slots = system::pd_cap_count(pd);

    let ok_ganz = c_ganz == caprock_abi::result::OK && g_m == magie_von(g_base) && g_r > 0;
    let alle_vier = maske == (1u64 << KINDER) - 1;
    let disjunkt = magisch == KINDER && korrupt == 0;
    let alle_leben = lebendig == KINDER;
    // **`threads` ist die Zahl mit Namen**: Elternthread + vier Arena-Kinder + das Kind auf der
    // ganzen Cap. `slots` ist der Punkt: zwei Caps fuer fuenf Stapel.
    let zahlen = threads == KINDER as u32 + 2 && slots == 2;
    let ueberlapp_abgewiesen = c_ueber == caprock_abi::result::ERR_NOSPACE;
    let ausserhalb_abgewiesen = c_aus == caprock_abi::result::ERR_SUBREGION;
    let cap_gesperrt = c_del == caprock_abi::result::ERR_INUSE;

    let ok = fertig
        && ok_ganz
        && alle_vier
        && disjunkt
        && alle_leben
        && zahlen
        && ueberlapp_abgewiesen
        && ausserhalb_abgewiesen
        && cap_gesperrt;

    println!(
        "arena  : {} (fertig={fertig} ganze-region={ok_ganz} vier-fenster={alle_vier} \
         disjunkt={disjunkt} lebendig={alle_leben} threads={threads} slots={slots} \
         ueberlappung-abgewiesen={ueberlapp_abgewiesen} ausserhalb-abgewiesen={ausserhalb_abgewiesen} \
         cap-gesperrt={cap_gesperrt} | maske={maske:#x} magisch={magisch}/{KINDER} \
         korrupt={korrupt} codes: ganz={c_ganz} ueber={c_ueber} aus={c_aus} del={c_del})",
        if ok { "ALL PASS" } else { "FAILURES" }
    );
    ARENA_OK.gemessen(ok);
}
