//! **TLS, Stufe T1+T2: das Register haelt, und es haelt JE THREAD** — arch-neutral.
//!
//! ## Warum das ohne `#[thread_local]` gemessen wird
//!
//! Der naheliegende Weg waere eine `#[thread_local]`-Variable im Treiber. Der braucht
//! `has-thread-local` in der Ziel-Beschreibung und `.tdata`/`.tbss` im Linkerskript — beides fehlt
//! heute (T3). Er braucht ausserdem, dass man dem Uebersetzer glaubt.
//!
//! Was T1+T2 zusagen, ist **kleiner und direkt pruefbar**: der Kernel haelt eine Zahl je Thread und
//! spiegelt sie ins Thread-Pointer-Register. Also wird genau das gemessen — **das Register
//! zurueckgelesen**, aus EL0/Ring 3, von zwei Threads derselben PD mit verschiedenen Werten.
//!
//! Das ist ausdruecklich **keine** Aussage ueber das TLS-Layout (Variante 1 gegen 2, Selbstzeiger,
//! Offsetrichtung). Die kommt mit T4, und sie lebt im Userspace — der Kernel kennt sie nicht.
//!
//! ## Warum ZWEI Threads, und warum sie derselben PD gehoeren muessen
//!
//! Mit einem Thread ist ein Thread-Pointer von einer globalen Variablen **nicht zu
//! unterscheiden**. Und in zwei verschiedenen PDs waere die Trennung auch ohne TLS gegeben — die
//! Adressraeume sind ohnehin getrennt. Die Aussage entsteht erst bei **zwei Threads in EINEM
//! Adressraum**, und die gibt es in diesem Kernel erst seit `SYS_SPAWN` (K1a/K1b).
//!
//! ## Die vier Aussagen
//!
//! | Konjunkt | trennt |
//! |---|---|
//! | `beide-liefen` | Sprechprobe. Ohne sie sagen die uebrigen nichts, weil die Schleife nicht lief |
//! | `tp-gesetzt` | „der Kernel hat geschrieben" — aus dem **Register** gelesen, nicht aus dem TCB |
//! | `getrennt` | **die eigentliche Aussage**: jeder Thread liest SEINEN Wert, und die beiden sind verschieden |
//! | `ueberlebt-wechsel` | „gesetzt" von **„gehalten"**. Nur das misst, ob `sync_tls` restauriert |
//! | `ueberlebt-syscall` | die Luecke, die „MSR-Schreib nur bei Aenderung" aufmacht: ohne Wechsel wird **nichts** geschrieben, und fasst ein Kernelpfad `FS` an, ist der Wert weg, **ohne dass die Restaurierung je greift** |

use crate::system;
use caprock_hal::println;

/// Urteil der `tls`-Zeile.
static TLS_OK: crate::befund::AtomicBefund = crate::befund::AtomicBefund::neu();

/// Fuer den Bericht.
pub fn urteil() -> crate::befund::Befund {
    TLS_OK.lesen()
}

const PAGE: u64 = 4096;
/// Stapelseiten je Kind. Der Kern des Fensters ist der Stapel; die untersten Worte sind Ablage.
const STACK_PAGES: u64 = 4;
/// **Zwei** Kinder — mehr braucht die Aussage nicht, und jedes weitere kostet nur Speicher.
const KINDER: usize = 2;
/// Arena: je Kind ein Stapelfenster, dazu eine Parameterseite.
const ARENA_PAGES: u64 = KINDER as u64 * STACK_PAGES + 1;
/// Offset der Parameterseite in der Arena.
const PARAM_OFF: u64 = KINDER as u64 * STACK_PAGES * PAGE;

/// Cap-Slot der Arena im Cspace der Sonden-PD.
const SLOT_ARENA: u64 = 0;

// --- Parameterseite: was der Kernel hineinlegt und was der Elternthread zurueckschreibt --------
const P_ARENA: u64 = 0x00; // Basis der Arena
const P_ENTRY: u64 = 0x08; // Einsprung der Kinder
const P_ELTER_LIEF: u64 = 0x10; // != 0, sobald der Elternthread lief
const P_MASKE: u64 = 0x18; // Bit i: der i-te Spawn meldete OK
const P_FERTIG: u64 = 0x20; // != 0, wenn der Elternthread fertig ist
/// Ergebnis von `SETTLS` mit einer Adresse aus der **oberen** Haelfte.
const P_SCHRANKE: u64 = 0x28;

/// **Der Negativfall der Schranke** — kanonisch, aber ausserhalb des Benutzerbereichs.
///
/// Kanonisch mit Absicht: ein nicht-kanonischer Wert wuerde, faende er den Weg bis zum `WRMSR`,
/// den **Kernel** faulten lassen (Ring 0). Genau davor schuetzt die Schranke — und eine Gegenprobe,
/// die die Maschine umlegt, statt ein Konjunkt fallen zu lassen, ist keine Messung, sondern ein
/// Absturz. Dieser Wert laesst sich gefahrlos schreiben und ist trotzdem verboten.
const SCHRANKE_PROBE: u64 = 0xFFFF_8000_0000_0000;

// --- Ablage je Kind, am Fuss seines Fensters ---------------------------------------------------
/// Der Wert, den das Kind als Thread-Pointer gesetzt hat (== seine Fensterbasis).
const K_GESETZT: u64 = 0x00;
/// Was es unmittelbar danach aus dem Register zurueckgelesen hat.
const K_SOFORT: u64 = 0x08;
/// Was es nach einem `YIELD` (Threadwechsel) zurueckgelesen hat.
const K_NACH_WECHSEL: u64 = 0x10;
/// Was es nach einem Syscall **ohne** Wechsel zurueckgelesen hat.
const K_NACH_SYSCALL: u64 = 0x18;
/// Rundenzaehler — die Sprechprobe.
const K_RUNDEN: u64 = 0x20;

/// Wie oft das Kind seine Schleife dreht, bevor es **parkt**.
///
/// **Klein, und das ist gemessen.** Die erste Fassung stand auf `1 << 14`: die Kinder drehten nach
/// der Messung noch sechzehntausend `YIELD`-Runden und blieben damit lauffaehig. Auf aarch64 ist
/// daran die **Stopp-Latenz-Zusage des Debuggers** (`dbg`) gefallen — eine Zeile ohne jeden Bezug
/// zu TLS, gemessen in Ticks. *Ein Test, der Last erzeugt, kippt baseline-empfindliche Tests*, und
/// dieser Baum hat dafuer schon einmal bezahlt (der Farbtest in `spawn_demo`).
///
/// Gebraucht werden **wenige** Runden: `ueberlebt-wechsel` misst nach dem ersten `YIELD`, und der
/// Rest belegt nur, dass der Wert ueber mehrere Wechsel haelt. Danach parken die Kinder und
/// kosten nichts mehr.
const KIND_RUNDEN: u64 = 16;

// --- T5: was ein TREIBER meldet, wenn er TLS wirklich benutzt ----------------------------------
//
// **Spiegel von `programs/hardware/virtio-blk`.** Kernel und Treiber werden getrennt gebaut und
// haben kein gemeinsames Modul — dieselbe Lage wie bei den Badges und den B4-Offsets. Laufen sie
// auseinander, fehlt die Marke, das Konjunkt ist **nicht erfuellt statt still wahr**, und die
// Zeile sagt es.
const D_TLS_MAGIC_OFF: u64 = 0x638;
const D_TLS_WERT_OFF: u64 = 0x640;
const D_TLS_TP_OFF: u64 = 0x648;
const D_TLS_FS0_OFF: u64 = 0x650;
const D_TLS_MAGIC: u64 = 0x544C_5344_5256_2121;
const D_TLS_WERT: u64 = 0xC0FFEE_4711;

// ================================================================================================
// Das Kind (EL0/Ring 3)
// ================================================================================================
//
// **`.user_text` ist keine Formalie** — ein Ring-3-Einsprung in `.text` faultet an seiner EIGENEN
// Einsprungadresse, und im Protokoll sieht das aus wie ein kaputter Mechanismus.
//
// Der Ablauf, und jede Zeile trennt etwas:
//   1. Fensterbasis nach `[base + K_GESETZT]` — was ich gleich setzen werde.
//   2. `SETTLS(base)`.
//   3. Register zurueckleseN -> `K_SOFORT`.        („hat der Kernel geschrieben?")
//   4. `YIELD`, dann zurueckleseN -> `K_NACH_WECHSEL`. („haelt er es ueber den Wechsel?")
//   5. Ein Syscall OHNE Wechsel, dann zurueckleseN -> `K_NACH_SYSCALL`.
//   6. Schleife mit Rundenzaehler, dann `PARK`.
//
// **Der Selbstzeiger bei `[base]` ist auf x86 noetig**, weil Ring 3 dort `FS_BASE` nicht direkt
// lesen kann (`RDFSBASE` braucht `CR4.FSGSBASE`, s. E-T1). Gelesen wird deshalb `fs:[0]` — und
// dort steht, was das Kind selbst hingeschrieben hat. Auf aarch64 ist `TPIDR_EL0` aus EL0 direkt
// lesbar, der Umweg entfaellt.
//
// Das misst das **Register**, nicht das TLS-Layout: `tp = base` ist hier bewusst nicht die
// Variante-2-Anordnung (die legte `tp` ans Ende). Layout ist T4 und lebt im Userspace.

#[cfg(target_arch = "x86_64")]
core::arch::global_asm!(
    r#"
.section .user_text,"ax"
.globl caprock_tls_kind
caprock_tls_kind:
    mov  r9, rdi                     // r9 = eigene Fensterbasis
    mov  [r9 + {k_gesetzt}], r9      // "das setze ich gleich"
    mov  [r9], r9                    // Selbstzeiger: fs:[0] soll die Basis liefern
    mov  rax, {settls}
    mov  rdi, r9
    int  0x80
    mov  rax, fs:[0]                 // 3. sofort zurueckleseN
    mov  [r9 + {k_sofort}], rax
    mov  rax, {yield}                // 4. Wechsel erzwingen
    int  0x80
    mov  rax, fs:[0]
    mov  [r9 + {k_wechsel}], rax
    mov  rax, {cdelete}              // 5. Syscall OHNE Wechsel (schlaegt fehl, das genuegt)
    mov  rdi, 15
    int  0x80
    mov  rax, fs:[0]
    mov  [r9 + {k_syscall}], rax
    xor  r11, r11                    // 6. Rundenzaehler
    mov  r13, {runden}
1:  inc  r11
    mov  [r9 + {k_runden}], r11
    mov  rax, {yield}
    int  0x80
    cmp  r11, r13
    jb   1b
2:  mov  rax, {park}
    int  0x80
    jmp  2b
"#,
    settls = const caprock_abi::sys::SETTLS,
    yield = const caprock_abi::sys::YIELD,
    cdelete = const caprock_abi::sys::CDELETE,
    park = const caprock_abi::sys::PARK,
    k_gesetzt = const K_GESETZT,
    k_sofort = const K_SOFORT,
    k_wechsel = const K_NACH_WECHSEL,
    k_syscall = const K_NACH_SYSCALL,
    k_runden = const K_RUNDEN,
    runden = const KIND_RUNDEN,
);

#[cfg(target_arch = "aarch64")]
core::arch::global_asm!(
    r#"
.section .user_text,"ax"
.globl caprock_tls_kind
caprock_tls_kind:
    mov  x9, x0
    str  x9, [x9, #{k_gesetzt}]
    str  x9, [x9]
    mov  x0, #{settls}
    mov  x1, x9
    svc  #0
    mrs  x10, tpidr_el0              // aus EL0 direkt lesbar -- kein Selbstzeiger noetig
    str  x10, [x9, #{k_sofort}]
    mov  x0, #{yield}
    svc  #0
    mrs  x10, tpidr_el0
    str  x10, [x9, #{k_wechsel}]
    mov  x0, #{cdelete}
    mov  x1, #15
    svc  #0
    mrs  x10, tpidr_el0
    str  x10, [x9, #{k_syscall}]
    mov  x11, xzr
    ldr  x13, ={runden}
1:  add  x11, x11, #1
    str  x11, [x9, #{k_runden}]
    mov  x0, #{yield}
    svc  #0
    cmp  x11, x13
    b.lo 1b
2:  mov  x0, #{park}
    svc  #0
    b    2b
"#,
    settls = const caprock_abi::sys::SETTLS,
    yield = const caprock_abi::sys::YIELD,
    cdelete = const caprock_abi::sys::CDELETE,
    park = const caprock_abi::sys::PARK,
    k_gesetzt = const K_GESETZT,
    k_sofort = const K_SOFORT,
    k_wechsel = const K_NACH_WECHSEL,
    k_syscall = const K_NACH_SYSCALL,
    k_runden = const K_RUNDEN,
    runden = const KIND_RUNDEN,
);

extern "C" {
    static caprock_tls_kind: u8;
}

/// **`SYS_SPAWN` aus EL0/Ring 3.**
///
/// # Safety
/// Nur aus User-Kontext zu rufen.
///
/// `#[inline(always)]` ist keine Formalie: ein nicht eingebetteter Helfer laege in `.text`, waere
/// aus Ring 3 nicht ausfuehrbar, und der Fehler wanderte nur eine Ebene tiefer.
#[inline(always)]
unsafe fn user_spawn(sub: u64, slot: u64, entry: u64, arg: u64, prio: u64) -> u64 {
    let code: u64;
    #[cfg(target_arch = "x86_64")]
    // SAFETY: siehe Funktionsdoku. ABI -> GPR: x0=rax, x1=rdi, x2=rsi, x3=rdx, x4=r10, x5=r8.
    unsafe {
        core::arch::asm!("int 0x80",
                         inlateout("rax") caprock_abi::sys::SPAWN => code,
                         inlateout("rdi") sub => _,
                         in("rsi") slot, in("rdx") entry, in("r10") arg, in("r8") prio,
                         clobber_abi("sysv64"));
    }
    #[cfg(target_arch = "aarch64")]
    // SAFETY: siehe Funktionsdoku.
    unsafe {
        core::arch::asm!("svc #0",
                         inlateout("x0") caprock_abi::sys::SPAWN => code,
                         inlateout("x1") sub => _,
                         in("x2") slot, in("x3") entry, in("x4") arg, in("x5") prio,
                         clobber_abi("C"));
    }
    code
}

/// **Der Elternthread** (EL0): er erzeugt die zwei Kinder ueber die ABI und parkt.
///
/// **`.user_text` ist keine Formalie** — s. die Kinder oben. Jeder Helfer, den er ruft, ist
/// `#[inline(always)]`, sonst laege JENER in `.text`.
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
    let entry = lies(P_ENTRY);
    let prio = crate::system::IDLE_PRIO as u64;

    // **Zwei Fenster in EINER Cap** — jedes Kind bekommt seines, und genau daher stammen die zwei
    // VERSCHIEDENEN Thread-Pointer. Ohne die Teilregion (K1b) waere jede Fensterbasis dieselbe.
    let mut maske = 0u64;
    let mut i = 0u64;
    while i < KINDER as u64 {
        let off = i * STACK_PAGES;
        let sub = (off << 32) | STACK_PAGES;
        let basis = arena + off * PAGE;
        // SAFETY: User-Kontext.
        let code = unsafe { user_spawn(sub, SLOT_ARENA, entry, basis, prio) };
        if code == caprock_abi::result::OK {
            maske |= 1 << i;
        }
        i += 1;
    }
    schreib(P_MASKE, maske);

    // **Die Schranke, aus EL0 gefahren.** `SETTLS` wirkt ohne Cap -- was ihn traegt, ist allein
    // diese Pruefung, und ohne einen Negativfall waere sie eine Behauptung.
    // SAFETY: User-Kontext.
    let c = unsafe { user_syscall1(caprock_abi::sys::SETTLS, SCHRANKE_PROBE) };
    schreib(P_SCHRANKE, c);

    schreib(P_FERTIG, 1);
    loop {
        // SAFETY: User-Kontext.
        unsafe { user_syscall1(caprock_abi::sys::PARK, 0) };
    }
}

/// Ein Syscall mit einem Argument in **x1**.
///
/// # Safety
/// Nur aus User-Kontext zu rufen. `#[inline(always)]` — s. [`user_spawn`].
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

/// Warten auf eine Groesse, mit Frist — **nicht** auf eine Rundenzahl: eine Zaehlschleife maesse
/// die Geschwindigkeit des Wartenden.
fn warte(ticks: u64) {
    let t0 = caprock_hal::timer::ticks(0);
    let mut wache = 0u64;
    while caprock_hal::timer::ticks(0).wrapping_sub(t0) < ticks && wache < 400_000_000 {
        core::hint::spin_loop();
        wache += 1;
    }
}

/// Eine Region nullen — **der Kernel tut das, nicht der gemessene Pfad**: sonst waere „hier steht
/// ein Wert" von „hier stand schon einer" nicht zu unterscheiden.
///
/// # Safety
/// `base` muss eine frisch allozierte, identitaetsgemappte Region der Laenge `len` sein.
unsafe fn nullen(base: u64, len: u64) {
    let mut o = 0u64;
    while o < len {
        // SAFETY: siehe Funktionsdoku.
        unsafe { core::ptr::write_volatile((base + o) as *mut u64, 0) };
        o += 8;
    }
}

/// Die Ablage eines Kindes lesen.
fn kind(base: u64, off: u64) -> u64 {
    // SAFETY: identitaetsgemappte Arena-Seite, festes Wort.
    unsafe { core::ptr::read_volatile((base + off) as *const u64) }
}

/// **Die Messung.**
pub fn messen() {
    let Some(pd) = system::create_pd() else {
        println!("tls    : SKIP (keine PD frei)");
        TLS_OK.uebersprungen();
        return;
    };
    let Some(arena) = system::alloc(ARENA_PAGES * PAGE, PAGE) else {
        println!("tls    : SKIP (kein Speicher fuer die Arena)");
        TLS_OK.uebersprungen();
        let _ = system::free_pd_slot(pd);
        return;
    };
    let (a_base, a_len) = (arena.base(), arena.len());
    // SAFETY: frisch alloziert und identitaetsgemappt.
    unsafe { nullen(a_base, a_len) };
    let Ok(cap_a) = system::cap_install(arena) else {
        println!("tls    : SKIP (Memory-Cap nicht installierbar)");
        TLS_OK.uebersprungen();
        let _ = system::free_pd_slot(pd);
        return;
    };
    if !system::install_pd_cap(pd, SLOT_ARENA as usize, cap_a) {
        println!("tls    : SKIP (Cap-Slot der PD nicht belegbar)");
        TLS_OK.uebersprungen();
        let _ = system::free_pd_slot(pd);
        return;
    }

    let param = a_base + PARAM_OFF;
    let entry = core::ptr::addr_of!(caprock_tls_kind) as u64;
    // SAFETY: identitaetsgemappte Seite der eben allozierten Arena.
    unsafe {
        core::ptr::write_volatile((param + P_ARENA) as *mut u64, a_base);
        core::ptr::write_volatile((param + P_ENTRY) as *mut u64, entry);
    }

    let Some(p) =
        system::spawn_user_parked(elter as *const () as usize, param as usize, system::IDLE_PRIO)
    else {
        println!("tls    : SKIP (Elternthread nicht startbar)");
        TLS_OK.uebersprungen();
        let _ = system::free_pd_slot(pd);
        return;
    };
    system::bind_pd_parked(&p, pd);
    if system::admit(p).is_none() {
        println!("tls    : SKIP (Elternthread nicht zulassbar)");
        TLS_OK.uebersprungen();
        let _ = system::free_pd_slot(pd);
        return;
    }

    // Auf die GROESSE warten, mit Frist.
    let mut w = 0;
    while w < 40 && kind(param, P_FERTIG) == 0 {
        warte(1);
        w += 1;
    }
    // Und danach noch einmal, damit die Kinder ihre Runden drehen konnten.
    let mut w = 0;
    while w < 40
        && !(0..KINDER as u64).all(|i| kind(a_base + i * STACK_PAGES * PAGE, K_RUNDEN) > 0)
    {
        warte(1);
        w += 1;
    }
    urteilen(a_base, param);
}

/// Das Urteil — **jeder Wert einzeln gelesen und genannt**, nicht als Sammelaussage.
fn urteilen(a_base: u64, param: u64) {
    let b0 = a_base;
    let b1 = a_base + STACK_PAGES * PAGE;
    let maske = kind(param, P_MASKE);

    let schranken_code = kind(param, P_SCHRANKE);
    let schranke_beisst = schranken_code == caprock_abi::result::ERR_BADTLS;
    let mut beide_liefen = true;
    let mut tp_gesetzt = true;
    let mut getrennt = true;
    let mut ueberlebt_wechsel = true;
    let mut ueberlebt_syscall = true;
    for b in [b0, b1] {
        let gesetzt = kind(b, K_GESETZT);
        let sofort = kind(b, K_SOFORT);
        let wechsel = kind(b, K_NACH_WECHSEL);
        let syscall = kind(b, K_NACH_SYSCALL);
        let runden = kind(b, K_RUNDEN);
        println!(
            "tls    : Kind {b:#x}: gesetzt={gesetzt:#x} sofort={sofort:#x} \
             nach-wechsel={wechsel:#x} nach-syscall={syscall:#x} runden={runden}"
        );
        if runden == 0 {
            beide_liefen = false;
        }
        if gesetzt != b || sofort != b {
            tp_gesetzt = false;
        }
        if wechsel != b {
            ueberlebt_wechsel = false;
        }
        if syscall != b {
            ueberlebt_syscall = false;
        }
    }
    // **`getrennt` ist die eigentliche Aussage** — und die erste Fassung konnte wahr werden, WEIL
    // NICHTS PASSIERT IST.
    //
    // Sie verglich `K_SOFORT` an zwei Fensteradressen, die der **Pruefer** gewaehlt hat. Meldet
    // ein Kind gar nichts, steht dort `0`, und `0 != b0` las sich als „getrennt". Gefangen hat es
    // die Gegenprobe M3 (beide Kinder teilen ein Fenster): das zweite Fenster blieb leer, und das
    // Konjunkt blieb gruen. Woertlich die Klasse aus todo D18 — ein Pruefer, der nicht scheitern
    // kann, im eigenen Werkzeug.
    //
    // Verlangt wird deshalb dreierlei: **beide haben gemeldet** (`!= 0`), sie melden
    // **Verschiedenes**, und die Basen sind ueberhaupt verschieden. Dass jeder SEINEN Wert liest,
    // sagt `tp-gesetzt` daneben — zwei Konjunkte, zwei Aussagen.
    let (s0, s1) = (kind(b0, K_SOFORT), kind(b1, K_SOFORT));
    if b0 == b1 || s0 == 0 || s1 == 0 || s0 == s1 {
        getrennt = false;
    }
    // --- T5: der Treiber, BEDINGT ------------------------------------------------------------
    //
    // Die Hauptsuite hat keine Treiber-PD, die Lade-Suite hat zwei — und nur eine davon
    // (`virtio-blk`) faehrt TLS. Ein unbedingtes Konjunkt haette deshalb genau zwei Enden: rot in
    // der Hauptsuite aus einem zulaessigen Grund, oder abgeschaltet und damit blind. Also
    // `treiber-meldet ⟹ treiber-tls-stimmt`, und **beide Zahlen gedruckt** -- dieselbe Disziplin
    // wie `angeboten-dann-da` und `b4-aktiv`.
    let mut m = [system::DriverMsi::default(); system::MAX_OFFERED_DEVICES];
    let n = system::driver_msi(&mut m);
    let mut treiber_meldet = false;
    let mut treiber_tls_stimmt = true;
    for e in m.iter().take(n) {
        // SAFETY: identitaetsgemappte DMA-Region dieser Zuteilung, zwei feste Worte.
        let (marke, wert) = unsafe {
            (
                core::ptr::read_volatile((e.dma_phys + D_TLS_MAGIC_OFF) as *const u64),
                core::ptr::read_volatile((e.dma_phys + D_TLS_WERT_OFF) as *const u64),
            )
        };
        if marke != D_TLS_MAGIC {
            continue;
        }
        treiber_meldet = true;
        // SAFETY: wie oben.
        let (tp, fs0) = unsafe {
            (
                core::ptr::read_volatile((e.dma_phys + D_TLS_TP_OFF) as *const u64),
                core::ptr::read_volatile((e.dma_phys + D_TLS_FS0_OFF) as *const u64),
            )
        };
        println!(
            "tls    : Treiber {}: thread-lokale Variable gelesen={wert:#x} erwartet={D_TLS_WERT:#x} \
             | tp={tp:#x} roher-fs0={fs0:#x} (gleich = FS_BASE wirkt, dann liegt der Fehler in der \
             ADRESSIERUNG der Variablen; ungleich = das Register wirkt nicht)",
            e.program_id
        );
        if wert != D_TLS_WERT {
            treiber_tls_stimmt = false;
        }
    }

    let ok = beide_liefen
        && maske == (1 << KINDER) - 1
        && schranke_beisst
        && tp_gesetzt
        && getrennt
        && ueberlebt_wechsel
        && ueberlebt_syscall
        && (!treiber_meldet || treiber_tls_stimmt);
    println!(
        "tls    : {} (spawn-maske={maske:#x} beide-liefen={beide_liefen} \
         schranke-beisst={schranke_beisst} (code={schranken_code}) tp-gesetzt={tp_gesetzt} \
         getrennt={getrennt} ueberlebt-wechsel={ueberlebt_wechsel} \
         ueberlebt-syscall={ueberlebt_syscall} treiber-meldet={treiber_meldet} \
         treiber-tls-stimmt={treiber_tls_stimmt} -- T1+T2 messen das REGISTER: der Kernel haelt \
         EINE Zahl je Thread und spiegelt sie. T5 misst eine echte thread-lokale VARIABLE in einer \
         Treiber-PD, und das Konjunkt ist bedingt: die Hauptsuite hat keine Treiber-PD. Das LAYOUT \
         (Variante 1 gegen 2) lebt im Userspace -- der Kernel kennt es nicht)",
        if ok { "ALL PASS" } else { "FAILURES" }
    );
    TLS_OK.gemessen(ok);
}
