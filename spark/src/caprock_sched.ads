--  Caprock -- SPARK-Portierung von `crates/caprock-sched/src/lib.rs` (Experiment S2).
--
--  ZWECK: die Frage beantworten, WIEVIEL des Scheduler-Kerns unter `SPARK_Mode => On`
--  kommt -- und was GNATprove dort findet, das weder das Verus-Modell
--  (`Verification/scheduler/proofs/*.rs`) noch die Testsuite sieht.
--
--  Anders als der Cap-Space (S1, reine Datenstruktur) bringt der Scheduler DREI Dinge mit,
--  die es dort nicht gab. Sie sind hier ausdruecklich modelliert, nicht wegvereinfacht:
--
--   (a) NEBENLAEUFIGKEIT. `DIRECTORY` ist eine Tabelle von `AtomicU64`, die JEDER Kern
--       lock-frei liest, waehrend der besitzende Kern schreibt. Das ist hier eine
--       EXTERNE Zustandsabstraktion mit `Async_Writers => True`: GNATprove darf ueber
--       zwei Lesevorgaenge desselben Eintrags NICHTS annehmen. Genau das ist die
--       Wirklichkeit -- der Thread kann zwischen zwei Zeilen migrieren.
--       `CORE_LOAD` und die D15-Zaehler sind `Async_Readers => True`: wir schreiben,
--       andere lesen lock-frei.
--   (b) ZEIT. `now`, `next_refill`, `remaining`, `budget`, `period` sind RANGE-Typen,
--       keine modularen -- sonst stellte sich die Ueberlauffrage gar nicht. `[profile.release]`
--       in `Cargo.toml` setzt `overflow-checks` NICHT (nur `panic = "abort"`), also ist
--       jedes `-= 1` unter einer Null ein stiller Umlauf und kein Panic. Dieselbe Lage wie
--       bei `refcount` in S1 (Fund F6).
--   (c) DER GEPARKTE THREAD. Rusts `Parked` (in `kernel/src/system.rs`, NICHT in dieser
--       Crate) ist `#[must_use]`, hat kein `Drop` und keinen oeffentlichen Weg an die
--       `ThreadId`. Das ist eine LINEARE Zusage in Rust-Verkleidung -- und `#[must_use]`
--       ist eine Warnung, kein Fehler. Siehe `Parked` unten und `demo/s2_parked.adb`.
--
--  PORTIERUNGSREGELN (Abweichungen davon sind Befunde ueber die Portierung, nicht ueber
--  Caprock). Sie sind die von `caprock_cap.ads`, mit vier Zusaetzen:
--
--   1. KEINE Vorbedingung, die der Rust-Code nicht auch erzwingt. Wer `Remove_From_Ready`
--      ein `Pre => Queues (P).Count > 0` gibt, hat den Unterlauf wegdefiniert.
--   2. Verkettungsindizes sind vom TYP her BREITER als ihre Tabelle. In Rust sind
--      `qnext`/`qprev` ein `u32` mit `NIL = u32::MAX`, `sc_donor`/`sc_donee`/`current`
--      ein `Option<usize>`, `zhead`/`ztail` ein `usize` -- und der Code indiziert damit
--      ROH (`self.tcbs[tail as usize]`, `self.tcbs[acct]`, `self.zombies[self.zhead]`).
--      Ein auf die Tabelle beschraenkter Indextyp haette genau die Frage wegmodelliert.
--   3. **`priority` ist ein `U8` (0 .. 255), die Queue-Tabelle hat NPRIO = 8 Faecher.**
--      Das ist keine Modellierungsfreiheit, sondern der Rust-Typ: `Tcb::priority: u8`, und
--      `enqueue_ready` schreibt `self.queues[self.tcbs[local].priority as usize]`.
--      Geklemmt wird an GENAU EINER Aufrufstelle (`kernel/src/loader.rs`, Manifestpfad);
--      die uebrigen ~40 `spawn*`-Aufrufe reichen die Zahl durch.
--   4. Zaehler (`used`, `depleted_count`, `budget_blocked_count`, `zcount`, `count` je
--      Queue, `now`, `depletions`, `refills`, `migrations_*`) sind RANGE-Typen. Grund
--      s. (b).
--   5. `gen.wrapping_add(1)` ist ABSICHT -> modularer Typ, kein Check (wie S1, Regel 5).
--   6. Die Tabellen sind vollstaendig angehaengt (`Nslots = Max_Tcbs`), damit die
--      Ada-Indexpruefung mit der Slab-Schranke aus Rust zusammenfaellt (wie S1, Regel 6).
--      Die TCB-Freiliste liefert deshalb `Tcb_Range` -- in Rust ist sie mit genau
--      `capacity` angehaengt, ein Fund dort waere erfunden.
--   7. Der `Slab`/`FreeList`-Rohspeicher (`unsafe attach`) ist NICHT portiert: er liegt in
--      `caprock-slab`, nicht in `lib.rs`. Hier sind es Arrays.
--   8. `init_thread_frame` ist ein HAL-Aufruf (schreibt einen Trap-Frame). Portiert ist
--      die ARITHMETIK, die in `lib.rs` steht (`stack_base + stack_len`); der Rest ist ein
--      importiertes Unterprogramm ohne Rumpf.
--   9. `GID_FREE` liegt in Rust hinter einem `SpinLock` und ist deshalb hier GEWOEHNLICHER
--      Zustand, nicht extern. Das unterstellt wechselseitigen Ausschluss -- den liefert der
--      Spinlock, und SPARK hat keine Ausdrucksform fuer „der Aufrufer haelt die Sperre".
--      Dasselbe gilt fuer die `Scheduler`-Instanz selbst (`SCHEDS[core]`).
--  10. Die Zyklenabrechnung (`cycles.rs`) ist NICHT portiert -- eigene Datei, eigener
--      Host-Test, und nicht Teil des benannten Umfangs.

pragma Ada_2022;

package Caprock_Sched with SPARK_Mode => On,
   Abstract_State =>
     ((Dir_State    with External => (Async_Writers => True, Async_Readers => True)),
      (Gid_State),
      (Load_State   with External => (Async_Readers => True)),
      (Zomb_State   with External => (Async_Readers => True))),
   Initializes => (Dir_State, Gid_State, Load_State, Zomb_State)
is

   ---------------------------------------------------------------------------
   --  Grundtypen
   ---------------------------------------------------------------------------

   --  "usize"-Ersatz.
   type Idx is range 0 .. 2**31 - 1;

   Max_Tcbs    : constant Idx := 64;   --  TCB-Tabelle EINES Kerns
   Max_Zombies : constant Idx := 64;   --  Zombie-Ring EINES Kerns
   Max_Gids    : constant Idx := 128;  --  globales Thread-Directory (alle Kerne)
   Max_Cores   : constant Idx := 4;
   NPRIO       : constant Idx := 8;

   subtype Tcb_Range  is Idx range 0 .. Max_Tcbs - 1;
   subtype Zomb_Range is Idx range 0 .. Max_Zombies - 1;
   subtype Gid_Range  is Idx range 0 .. Max_Gids - 1;
   subtype Core_Range is Idx range 0 .. Max_Cores - 1;
   subtype Prio_Range is Idx range 0 .. NPRIO - 1;

   --  Rust: `u8`. BREITER als Prio_Range -- s. Portierungsregel 3.
   type U8  is range 0 .. 2**8 - 1;
   type U32 is range 0 .. 2**32 - 1;
   --  Rust: `u64`. RANGE, nicht modular -- s. Portierungsregel 4.
   type U64 is range 0 .. 2**64 - 1;

   --  Rust: `u32` mit `NIL = u32::MAX`. BREITER als Tcb_Range -- Portierungsregel 2.
   type Link is range 0 .. 2**32 - 1;
   Nil_Link : constant Link := 2**32 - 1;

   --  Rust: `queued: u8` mit `NOT_QUEUED = 0xff`.
   Not_Queued : constant U8 := 16#FF#;

   --  `gen.wrapping_add(1)` -- Portierungsregel 5.
   type Gen_T is mod 2**32;
   --  Packung des Directory-Eintrags.
   type Word64 is mod 2**64;
   --  Rust: `bitmap: u32`.
   type Bitmap_T is mod 2**32;

   --  Modell von `Option<usize>` (Rust: `current`, `sc_donor`, `sc_donee`).
   type Opt_Idx is record
      Present : Boolean := False;
      Value   : Idx     := 0;
   end record;

   No_Idx : constant Opt_Idx := (Present => False, Value => 0);

   function Some_Idx (I : Idx) return Opt_Idx is ((Present => True, Value => I))
     with Global => null;

   ---------------------------------------------------------------------------
   --  ThreadId
   ---------------------------------------------------------------------------

   type Thread_Id is record
      Slot : Idx   := 0;   --  gid
      Gen  : Gen_T := 0;
   end record;

   ---------------------------------------------------------------------------
   --  BlockReasons -- die Grund-MENGE (Z24)
   ---------------------------------------------------------------------------
   --
   --  Rust: `struct BlockReasons(u8)`. Lauffaehig ist ein Thread GENAU DANN, wenn die
   --  Menge leer ist; ein Grund wird EINZELN entfernt, und eingereiht wird NUR bei leerer
   --  Menge. Beide Halbsaetze stehen im Code -- der zweite ist die Aussage, die den Umbau
   --  traegt, und er ist unten in `Wecke_Falls_Lauffaehig` die einzige Stelle.

   type Reasons is mod 2**8;

   R_None    : constant Reasons := 0;
   R_Ipc     : constant Reasons := 2**0;
   R_Budget  : constant Reasons := 2**1;
   R_Pause   : constant Reasons := 2**2;
   R_Park    : constant Reasons := 2**3;
   R_Handler : constant Reasons := 2**4;
   R_Load    : constant Reasons := 2**5;

   function Is_Empty (R : Reasons) return Boolean is (R = 0) with Global => null;
   function Has (R : Reasons; G : Reasons) return Boolean is ((R and G) /= 0)
     with Global => null;

   procedure Insert_Reason (R : in out Reasons; G : Reasons)
     with Global => null, Always_Terminates,
          Post => R = (R'Old or G);

   --  **EINEN** Grund entfernen -- nie „alle".
   procedure Remove_Reason (R : in out Reasons; G : Reasons)
     with Global => null, Always_Terminates,
          Post => R = (R'Old and not G);

   ---------------------------------------------------------------------------
   --  TCB
   ---------------------------------------------------------------------------

   type Tcb is record
      Used        : Boolean := False;
      Gid         : Link    := Nil_Link;
      Gen         : Gen_T   := 0;
      Sp          : U64     := 0;
      Priority    : U8      := 0;          --  BREITER als Prio_Range (Regel 3)
      Reasons_Set : Reasons := R_None;
      Stack_Base  : U64     := 0;
      Stack_Len   : U64     := 0;
      Queued      : U8      := Not_Queued;
      Admitted    : Boolean := False;
      Qnext       : Link    := Nil_Link;
      Qprev       : Link    := Nil_Link;
      Budget      : U32     := 0;
      Period      : U32     := 0;
      Remaining   : U32     := 0;
      Next_Refill : U64     := 0;
      Depleted    : Boolean := False;
      Park_Wake   : Boolean := False;
      Sc_Donor    : Opt_Idx := No_Idx;
      Sc_Donee    : Opt_Idx := No_Idx;
      Has_Handler : Boolean := False;
   end record;

   Tcb_Empty : constant Tcb :=
     (Used        => False,     Gid         => Nil_Link, Gen        => 0,
      Sp          => 0,         Priority    => 0,        Reasons_Set => R_None,
      Stack_Base  => 0,         Stack_Len   => 0,        Queued     => Not_Queued,
      Admitted    => False,     Qnext       => Nil_Link, Qprev      => Nil_Link,
      Budget      => 0,         Period      => 0,        Remaining  => 0,
      Next_Refill => 0,         Depleted    => False,    Park_Wake  => False,
      Sc_Donor    => No_Idx,    Sc_Donee    => No_Idx,   Has_Handler => False);

   ---------------------------------------------------------------------------
   --  Ready-Queue (intrusiv) + Zombie-Ring
   ---------------------------------------------------------------------------

   type List_Head is record
      Head  : Link := Nil_Link;
      Tail  : Link := Nil_Link;
      Count : U32  := 0;
   end record;

   List_Head_Empty : constant List_Head :=
     (Head => Nil_Link, Tail => Nil_Link, Count => 0);

   type Zombie is record
      Base : U64  := 0;
      Len  : U64  := 0;
      Gid  : Link := Nil_Link;
   end record;

   Zombie_Empty : constant Zombie := (Base => 0, Len => 0, Gid => Nil_Link);

   type Tcb_Array    is array (Tcb_Range)  of Tcb;
   type Queue_Array  is array (Prio_Range) of List_Head;
   type Zombie_Array is array (Zomb_Range) of Zombie;

   --  TCB-Freiliste. Kopf und Verkettung liegen im `Tcb_Range` -- s. Portierungsregel 6:
   --  in Rust ist die Freiliste mit GENAU `capacity` angehaengt, ein Indexfund hier waere
   --  erfunden.
   type Opt_Tcb is record
      Present : Boolean   := False;
      Value   : Tcb_Range := 0;
   end record;
   No_Tcb : constant Opt_Tcb := (Present => False, Value => 0);

   type Free_Next_Array is array (Tcb_Range) of Opt_Tcb;
   type Free_List is record
      Next : Free_Next_Array := (others => (Present => False, Value => 0));
      Head : Opt_Tcb         := No_Tcb;
   end record;

   ---------------------------------------------------------------------------
   --  Scheduler EINES Kerns
   ---------------------------------------------------------------------------

   type Scheduler is record
      Core                 : Idx          := 0;
      Tcbs                 : Tcb_Array    := (others => Tcb_Empty);
      Free                 : Free_List;
      Current              : Opt_Idx      := No_Idx;
      Queues               : Queue_Array  := (others => List_Head_Empty);
      Bitmap               : Bitmap_T     := 0;
      Zombies              : Zombie_Array := (others => Zombie_Empty);
      Zhead                : Idx          := 0;   --  Rust: usize, roh indiziert (Regel 2)
      Ztail                : Idx          := 0;
      Zcount               : Idx          := 0;
      Now                  : U64          := 0;
      Depletions           : U64          := 0;
      Refills              : U64          := 0;
      Used                 : Idx          := 0;
      Depleted_Count       : Idx          := 0;
      Budget_Blocked_Count : Idx          := 0;
      Migrations_Out       : U64          := 0;
      Migrations_In        : U64          := 0;
   end record;

   ---------------------------------------------------------------------------
   --  Der geparkte Thread als LINEARE Zusage
   ---------------------------------------------------------------------------
   --
   --  Rusts `Parked` (kernel/src/system.rs:8555) traegt drei Eigenschaften:
   --    * `#[must_use]`      -- eine WARNUNG, kein Fehler
   --    * kein `Drop`        -- ein weggeworfener `Parked` ist STILL
   --    * kein Weg an die `ThreadId` ausser durch `admit`/`admit_in_pd`
   --
   --  Die dritte prueft rustc (privates Feld). Die ersten beiden nicht: `let _ = p;`
   --  schweigt, und `#[must_use]` laesst sich mit `#[allow]` abschalten.
   --
   --  Hier ist `Parked` ein BESITZZEIGER. SPARKs Eigentumspruefung verlangt, dass der
   --  Besitz beim Verlassen des Gueltigkeitsbereichs uebergeben ist -- ein fallengelassener
   --  `Parked` ist ein „resource or memory leak", also ein BEWEISFEHLER und keine Warnung.
   --  Gemessen in `demo/s2_parked.adb`.
   type Parked_Rec is record
      Tid : Thread_Id;
   end record;
   type Parked is access Parked_Rec;

   ---------------------------------------------------------------------------
   --  Directory (lock-frei, extern beschrieben)
   ---------------------------------------------------------------------------

   --  Rust: `attach_directory` -- EINMALIG beim Boot, vor dem Erzeugen von Threads und vor
   --  dem Start der Sekundaerkerne. Portiert ist die Verkettung der gid-Freiliste; der
   --  Rohspeicher (`unsafe attach`) liegt in `caprock-slab` (Portierungsregel 7).
   procedure Attach_Directory
     with Global => (Output => (Dir_State, Gid_State)), Always_Terminates;

   --  Rust: `thread_capacity` / `threads_available`.
   function Thread_Capacity return Idx is (Max_Gids) with Global => null;

   --  Rust: `dir_load` -> `Option<u64>`. `Ok = False` entspricht `None` (gid ausserhalb
   --  der Tabelle).
   procedure Dir_Load (G : Idx; E : out Word64; Ok : out Boolean)
     with Global => (Input => Dir_State), Always_Terminates,
          Post => (if Ok then G <= Max_Gids - 1);

   --  Rust: `owner_core`.
   procedure Owner_Core (T : Thread_Id; C : out Idx; Ok : out Boolean)
     with Global => (Input => Dir_State), Always_Terminates;

   --  Rust: `is_live`.
   function Is_Live (T : Thread_Id) return Boolean
     with Global => (Input => Dir_State), Volatile_Function;

   --  Rust: `slot_in_use` (D15-Melder).
   function Slot_In_Use (G : Idx) return Boolean
     with Global => (Input => Dir_State), Volatile_Function;

   --  Rust: `release_gid`.
   procedure Release_Gid (G : Link)
     with Global => (In_Out => Gid_State), Always_Terminates;

   --  Rust: `core_load`. Kein `Volatile_Function`: `Load_State` hat nur `Async_Readers`
   --  (wir schreiben, andere lesen) -- ein Lesen ist damit nicht fluechtig.
   function Core_Load (C : Idx) return Idx
     with Global => (Input => Load_State);

   --  Rust: `zombie_fuss_stats`. `Word64` (modular), weil `AtomicU64::fetch_add` umlaeuft.
   procedure Zombie_Fuss_Stats (Gesamt : out Word64; Unter_Fuessen : out Word64)
     with Global => (Input => Zomb_State), Always_Terminates;

   ---------------------------------------------------------------------------
   --  Operationen des Schedulers
   ---------------------------------------------------------------------------

   --  Rust: `init_core`.
   procedure Init_Core (S : in out Scheduler; C : Idx; Priority : U8;
                        T : out Thread_Id; Ok : out Boolean)
     with Global => (In_Out => (Dir_State, Gid_State, Load_State)), Always_Terminates;

   --  Rust: `spawn_parked`. Gibt einen `Parked` -- s. oben.
   procedure Spawn_Parked
     (S          : in out Scheduler;
      C          : Idx;
      Entry_Pt   : U64;
      Arg        : U64;
      Stack_Base : U64;
      Stack_Len  : U64;
      Priority   : U8;
      P          : out Parked)
   with Global => (In_Out => (Dir_State, Gid_State, Load_State)), Always_Terminates;

   --  Rust: `admit`. VERBRAUCHT den `Parked` (Besitzuebergabe).
   procedure Admit (S : in out Scheduler; P : in out Parked; Ok : out Boolean)
     with Global => (Input => Dir_State), Always_Terminates,
          Post => P = null;

   --  Rust: `exit_current`.
   procedure Exit_Current (S : in out Scheduler; C : Idx; Sp : out U64)
     with Global => (In_Out => (Dir_State, Load_State, Zomb_State)), Always_Terminates;

   --  Rust: `kill`.
   procedure Kill (S : in out Scheduler; T : Thread_Id; C : Idx; Ok : out Boolean)
     with Global => (In_Out => (Dir_State, Load_State, Zomb_State)), Always_Terminates;

   --  Rust: `reap`.
   procedure Reap (S : in out Scheduler; Base : out U64; Len : out U64;
                   Gid : out Link; Ok : out Boolean)
     with Global => null, Always_Terminates;

   --  Rust: `block_current_mit` / `block_current`.
   procedure Block_Current_Mit (S : in out Scheduler; C : Idx; Frame : U64;
                                Grund : Reasons; Sp : out U64)
     with Global => null, Always_Terminates;

   --  Rust: `switch_to` (Rendezvous-Fastpath + Budget-Donation).
   procedure Switch_To (S : in out Scheduler; C : Idx; Frame : U64;
                        Target : Thread_Id; Sp : out U64)
     with Global => (Input => Dir_State), Always_Terminates;

   --  Rust: `unblock`.
   procedure Unblock (S : in out Scheduler; T : Thread_Id; Ok : out Boolean)
     with Global => (Input => Dir_State), Always_Terminates;

   --  Rust: `park_current`. `Blocked = False` entspricht `None`.
   procedure Park_Current (S : in out Scheduler; C : Idx; Frame : U64;
                           Sp : out U64; Blocked : out Boolean)
     with Global => null, Always_Terminates;

   --  Rust: `unpark`.
   procedure Unpark (S : in out Scheduler; T : Thread_Id; Ok : out Boolean)
     with Global => (Input => Dir_State), Always_Terminates;

   --  Rust: `pause`.
   procedure Pause (S : in out Scheduler; T : Thread_Id; Ok : out Boolean)
     with Global => (Input => Dir_State), Always_Terminates;

   --  Rust: `resume`.
   procedure Resume (S : in out Scheduler; T : Thread_Id; Ok : out Boolean)
     with Global => (Input => Dir_State), Always_Terminates;

   --  Rust: `mark_handler_wait`.
   procedure Mark_Handler_Wait (S : in out Scheduler; T : Thread_Id; Ok : out Boolean)
     with Global => (Input => Dir_State), Always_Terminates;

   --  Rust: `handler_reply` -- der EINZIGE Wecker des Handler-Grundes.
   procedure Handler_Reply (S : in out Scheduler; T : Thread_Id; Ok : out Boolean)
     with Global => (Input => Dir_State), Always_Terminates;

   --  Rust: `load_reply` -- der EINZIGE Wecker des Lade-Grundes (C8).
   procedure Load_Reply (S : in out Scheduler; T : Thread_Id; Ok : out Boolean)
     with Global => (Input => Dir_State), Always_Terminates;

   --  Rust: `on_tick`.
   procedure On_Tick (S : in out Scheduler; C : Idx; Frame : U64; Tick : Boolean;
                      Sp : out U64)
     with Global => null, Always_Terminates;

   --  Rust: `set_budget`.
   procedure Set_Budget (S : in out Scheduler; T : Thread_Id;
                         Budget : U32; Period : U32; Ok : out Boolean)
     with Global => (Input => Dir_State), Always_Terminates;

   --  Rust: `end_donation`.
   procedure End_Donation (S : in out Scheduler; C : Idx)
     with Global => null, Always_Terminates;

   --  Rust: `detach_for_migration`. `Ok = False` entspricht `None`.
   procedure Detach_For_Migration (S : in out Scheduler; T : Thread_Id;
                                   M : out Tcb; Was_Ready : out Boolean;
                                   Ok : out Boolean)
     with Global => (In_Out => Load_State, Input => Dir_State), Always_Terminates;

   --  Rust: `attach_migrated`. `Ok = False` entspricht `Err(m)`.
   procedure Attach_Migrated (S : in out Scheduler; M : Tcb; Was_Ready : Boolean;
                              Ok : out Boolean)
     with Global => (In_Out => (Dir_State, Load_State)), Always_Terminates;

   --  Rust: `migration_candidate`. `Ok = False` entspricht `None`.
   --
   --  `Always_Terminates` steht hier ABSICHTLICH, obwohl es nicht haelt: die Rust-Fassung
   --  laeuft die Ready-Kette OHNE Schrittgrenze ab, waehrend `audit` daneben eine hat
   --  (`if n > q.count { return 5 }`). Die Zusage wegzulassen haette den Unterschied
   --  unsichtbar gemacht -- so wird er GEMESSEN (Befund [S2-T1]).
   procedure Migration_Candidate (S : Scheduler; T : out Thread_Id; Ok : out Boolean)
     with Global => null, Always_Terminates;

   --  Rust: `audit`. 0 = konsistent, sonst Anomaliecode 1..10.
   procedure Audit_Sched (S : Scheduler; Code : out U32)
     with Global => (Input => Dir_State), Always_Terminates;

   ---------------------------------------------------------------------------
   --  Inspektion
   ---------------------------------------------------------------------------

   procedure Priority_Of (S : Scheduler; T : Thread_Id; P : out U8; Ok : out Boolean)
     with Global => (Input => Dir_State), Always_Terminates;

   procedure Reasons_Of (S : Scheduler; T : Thread_Id; R : out Reasons; Ok : out Boolean)
     with Global => (Input => Dir_State), Always_Terminates;

   procedure Frame_Of (S : Scheduler; T : Thread_Id; Sp : out U64; Ok : out Boolean)
     with Global => (Input => Dir_State), Always_Terminates;

   procedure Admitted_Of (S : Scheduler; T : Thread_Id; A : out Boolean; Ok : out Boolean)
     with Global => (Input => Dir_State), Always_Terminates;

   function Load (S : Scheduler) return Idx is (S.Used) with Global => null;
   function Ticks (S : Scheduler) return U64 is (S.Now) with Global => null;

end Caprock_Sched;
