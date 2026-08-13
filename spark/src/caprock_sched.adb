--  Rumpf der SPARK-Portierung von `crates/caprock-sched/src/lib.rs`.
--
--  Jede Fundstelle traegt einen Marker `[S2-Fn]` mit der Rust-Zeile daneben. Der Marker
--  steht NUR dort, wo GNATprove etwas offen laesst, das am Rust-Original auch gilt.

pragma Ada_2022;

with Ada.Unchecked_Deallocation;

package body Caprock_Sched with SPARK_Mode => On,
   Refined_State => (Dir_State  => Directory,
                     Gid_State  => Gid_Free,
                     Load_State => Core_Load_Tbl,
                     Zomb_State => (Zombie_Gesamt, Zombie_Unter_Fuessen))
is

   ---------------------------------------------------------------------------
   --  Der geteilte Zustand
   ---------------------------------------------------------------------------

   --  Rust: `static DIRECTORY: AtomicTable<AtomicU64>` -- von JEDEM Kern lock-frei
   --  gelesen, vom besitzenden Kern geschrieben. `Async_Writers` heisst: GNATprove darf
   --  ueber zwei Lesevorgaenge desselben Eintrags NICHTS annehmen.
   type Dir_Array is array (Gid_Range) of Word64 with Atomic_Components;
   Directory : Dir_Array := (others => 0)
     with Volatile, Async_Writers => True, Async_Readers => True;

   --  Rust: `static GID_FREE: SpinLock<FreeList>` -- durch einen Spinlock serialisiert,
   --  deshalb GEWOEHNLICHER Zustand (Portierungsregel 9).
   type Opt_Gid is record
      Present : Boolean   := False;
      Value   : Gid_Range := 0;
   end record;
   type Gid_Next_Array is array (Gid_Range) of Opt_Gid;
   type Gid_Free_List is record
      Next : Gid_Next_Array := (others => (Present => False, Value => 0));
      Head : Opt_Gid        := (Present => False, Value => 0);
   end record;
   Gid_Free : Gid_Free_List;

   --  Rust: `static CORE_LOAD: [AtomicUsize; MAX_CORES]` -- wir schreiben, andere lesen.
   type Core_Load_Array is array (Core_Range) of Idx with Atomic_Components;
   Core_Load_Tbl : Core_Load_Array := (others => 0)
     with Volatile, Async_Readers => True;

   --  Rust: die beiden D15-Melder. **Modular, nicht Range** -- und das ist kein Versehen:
   --  `AtomicU64::fetch_add` laeuft in Rust PER DEFINITION um (in beiden Profilen, ohne
   --  Panic), anders als ein `+= 1` auf einem gewoehnlichen `u64`. Die erste Fassung dieser
   --  Portierung nahm hier `U64` und erzeugte damit zwei Funde, die es am Original nicht
   --  gibt.
   Zombie_Gesamt        : Word64 := 0 with Volatile, Async_Readers => True;
   Zombie_Unter_Fuessen : Word64 := 0 with Volatile, Async_Readers => True;

   ---------------------------------------------------------------------------
   --  Directory-Packung (Rust: `pack_dir`)
   ---------------------------------------------------------------------------
   --
   --  `<<` und `>>` auf `u64` sind hier Multiplikation/Division mit einer Zweierpotenz
   --  auf einem modularen Typ -- bitgleich, und ohne einen importierten Intrinsic.

   D_Used       : constant Word64 := 2**63;
   D_Gen_Scale  : constant Word64 := 2**32;
   D_Gen_Mask   : constant Word64 := 16#7fff_ffff#;
   D_Core_Scale : constant Word64 := 2**16;
   D_Core_Mask  : constant Word64 := 16#ffff#;
   D_Local_Mask : constant Word64 := 16#ffff#;

   function Pack_Dir (Used : Boolean; G : Gen_T; C : Idx; L : Idx) return Word64
     is ((if Used then D_Used else 0)
         or (((Word64 (G) and D_Gen_Mask) * D_Gen_Scale)
             or (((Word64 (C) and D_Core_Mask) * D_Core_Scale)
                 or (Word64 (L) and D_Local_Mask))))
     with Global => null;

   function Dir_Gen (E : Word64) return Gen_T
     is (Gen_T ((E / D_Gen_Scale) and D_Gen_Mask)) with Global => null;

   function Dir_Core (E : Word64) return Idx
     is (Idx ((E / D_Core_Scale) and D_Core_Mask)) with Global => null;

   function Dir_Local (E : Word64) return Idx
     is (Idx (E and D_Local_Mask)) with Global => null;

   ---------------------------------------------------------------------------
   --  HAL (Portierungsregel 8)
   ---------------------------------------------------------------------------

   --  Rust: `caprock_hal::exception::init_thread_frame(stack_top, entry, arg, el0, sp)`.
   --  Der Rumpf schreibt einen Trap-Frame; portiert ist nur, dass er einen SP liefert.
   function Init_Thread_Frame
     (Stack_Top : U64; Entry_Pt : U64; Arg : U64; El0 : Boolean; El0_Sp : U64) return U64
     with Import, Convention => Ada, External_Name => "caprock_init_thread_frame",
          Global => null,
          Post => Init_Thread_Frame'Result <= Stack_Top;

   --  Rust: `stapeladresse()` -- eine Adresse im aktuellen Stack-Rahmen.
   function Stapeladresse return U64
     with Import, Convention => Ada, External_Name => "caprock_stapeladresse",
          Global => null;

   --  Der `Parked` wird bei `Admit` VERBRAUCHT. Ohne diese Freigabe meldet SPARK auch den
   --  richtigen Pfad als Leck -- und dann saehe man den falschen nicht mehr.
   procedure Free_Parked is new Ada.Unchecked_Deallocation (Parked_Rec, Parked);

   ---------------------------------------------------------------------------
   --  Bitmaske der Prioritaeten
   ---------------------------------------------------------------------------
   --
   --  Rust: `self.bitmap |= 1 << p` mit `p = priority as usize`. Als Tabelle statt als
   --  Schiebeoperation, damit der Beweiser nicht an `2**n` scheitert -- die INDEXFRAGE
   --  bleibt dieselbe und ist genau der Punkt (Regel 3).
   --
   --  Nebenbei sichtbar: in Rust ist `1u32 << p` fuer `p >= 32` in `debug` ein Panic
   --  („attempt to shift left with overflow") und in `release` eine MASKIERUNG auf
   --  `p mod 32` -- eine Prioritaet 32 setzte also das Bit von Prioritaet 0.
   type Bit_Table is array (Natural range 0 .. 31) of Bitmap_T;
   All_Bits : constant Bit_Table :=
     (16#0000_0001#, 16#0000_0002#, 16#0000_0004#, 16#0000_0008#,
      16#0000_0010#, 16#0000_0020#, 16#0000_0040#, 16#0000_0080#,
      16#0000_0100#, 16#0000_0200#, 16#0000_0400#, 16#0000_0800#,
      16#0000_1000#, 16#0000_2000#, 16#0000_4000#, 16#0000_8000#,
      16#0001_0000#, 16#0002_0000#, 16#0004_0000#, 16#0008_0000#,
      16#0010_0000#, 16#0020_0000#, 16#0040_0000#, 16#0080_0000#,
      16#0100_0000#, 16#0200_0000#, 16#0400_0000#, 16#0800_0000#,
      16#1000_0000#, 16#2000_0000#, 16#4000_0000#, 16#8000_0000#);

   ---------------------------------------------------------------------------
   --  BlockReasons
   ---------------------------------------------------------------------------

   procedure Insert_Reason (R : in out Reasons; G : Reasons) is
   begin
      R := R or G;
   end Insert_Reason;

   procedure Remove_Reason (R : in out Reasons; G : Reasons) is
   begin
      R := R and not G;
   end Remove_Reason;

   ---------------------------------------------------------------------------
   --  Freilisten
   ---------------------------------------------------------------------------

   procedure Fl_Init (F : out Free_List)
     with Global => null, Always_Terminates
   is
   begin
      F.Next := (others => (Present => False, Value => 0));
      F.Head := (Present => True, Value => 0);
      for I in Tcb_Range range 0 .. Max_Tcbs - 2 loop
         F.Next (I) := (Present => True, Value => I + 1);
      end loop;
      F.Next (Max_Tcbs - 1) := No_Tcb;
   end Fl_Init;

   procedure Fl_Alloc (F : in out Free_List; I : out Tcb_Range; Ok : out Boolean)
     with Global => null, Always_Terminates
   is
   begin
      if not F.Head.Present then
         I  := 0;
         Ok := False;
         return;
      end if;
      I      := F.Head.Value;
      F.Head := F.Next (I);
      Ok     := True;
   end Fl_Alloc;

   procedure Fl_Free (F : in out Free_List; I : Tcb_Range)
     with Global => null, Always_Terminates
   is
   begin
      F.Next (I) := F.Head;
      F.Head     := (Present => True, Value => I);
   end Fl_Free;

   procedure Gid_Alloc (G : out Gid_Range; Ok : out Boolean)
     with Global => (In_Out => Gid_Free), Always_Terminates
   is
   begin
      if not Gid_Free.Head.Present then
         G  := 0;
         Ok := False;
         return;
      end if;
      G             := Gid_Free.Head.Value;
      Gid_Free.Head := Gid_Free.Next (G);
      Ok            := True;
   end Gid_Alloc;

   ---------------------------------------------------------------------------
   --  Directory-Zugriffe
   ---------------------------------------------------------------------------

   procedure Attach_Directory is
   begin
      Directory := (others => 0);
      Gid_Free.Next := (others => (Present => False, Value => 0));
      Gid_Free.Head := (Present => True, Value => 0);
      for I in Gid_Range range 0 .. Max_Gids - 2 loop
         Gid_Free.Next (I) := (Present => True, Value => I + 1);
      end loop;
      Gid_Free.Next (Max_Gids - 1) := (Present => False, Value => 0);
   end Attach_Directory;

   procedure Dir_Load (G : Idx; E : out Word64; Ok : out Boolean) is
   begin
      if G > Max_Gids - 1 then
         E  := 0;
         Ok := False;
         return;
      end if;
      --  Der EINE Lesevorgang. Alles Weitere entscheidet der Aufrufer auf dieser Kopie --
      --  ein zweites Lesen duerfte einen anderen Wert liefern.
      E  := Directory (G);
      Ok := True;
   end Dir_Load;

   procedure Dir_Store (G : Gid_Range; E : Word64)
     with Global => (In_Out => Directory), Always_Terminates
   is
   begin
      Directory (G) := E;
   end Dir_Store;

   procedure Owner_Core (T : Thread_Id; C : out Idx; Ok : out Boolean) is
      E  : Word64;
      Ld : Boolean;
   begin
      Dir_Load (T.Slot, E, Ld);
      if not Ld then
         C := 0; Ok := False; return;
      end if;
      if (E and D_Used) = 0 or else Dir_Gen (E) /= T.Gen then
         C := 0; Ok := False; return;
      end if;
      C  := Dir_Core (E);
      Ok := True;
   end Owner_Core;

   function Is_Live (T : Thread_Id) return Boolean is
      C  : Idx;
      Ok : Boolean;
   begin
      Owner_Core (T, C, Ok);
      return Ok;
   end Is_Live;

   function Slot_In_Use (G : Idx) return Boolean is
      E  : Word64;
      Ok : Boolean;
   begin
      Dir_Load (G, E, Ok);
      return Ok and then (E and D_Used) /= 0;
   end Slot_In_Use;

   procedure Release_Gid (G : Link) is
   begin
      --  Rust: `GID_FREE.lock().free(gid as usize)` -- die Freiliste ist mit der
      --  Directory-Kapazitaet angehaengt, ein `gid` ausserhalb kaeme aus einem
      --  beschaedigten TCB.
      if G <= Link (Max_Gids - 1) then
         Gid_Free.Next (Gid_Range (G)) := Gid_Free.Head;
         Gid_Free.Head := (Present => True, Value => Gid_Range (G));
      end if;
   end Release_Gid;

   function Core_Load (C : Idx) return Idx is
   begin
      if C > Max_Cores - 1 then
         return Idx'Last;
      end if;
      return Core_Load_Tbl (C);
   end Core_Load;

   procedure Zombie_Fuss_Stats (Gesamt : out Word64; Unter_Fuessen : out Word64) is
   begin
      Gesamt        := Zombie_Gesamt;
      Unter_Fuessen := Zombie_Unter_Fuessen;
   end Zombie_Fuss_Stats;

   ---------------------------------------------------------------------------
   --  Aufloesung (Rust: `resolve`)
   ---------------------------------------------------------------------------

   procedure Resolve (S : Scheduler; T : Thread_Id; L : out Idx; Ok : out Boolean)
     with Global => (Input => Directory), Always_Terminates,
          Post => (if Ok then L <= Max_Tcbs - 1)
   is
      E  : Word64;
      Ld : Boolean;
   begin
      L  := 0;
      Ok := False;
      Dir_Load (T.Slot, E, Ld);
      if not Ld then
         return;
      end if;
      if (E and D_Used) = 0 then
         return;
      end if;
      if Dir_Gen (E) /= T.Gen then
         return;
      end if;
      if Dir_Core (E) /= S.Core then
         return;   --  gehoert (nicht mehr) diesem Kern
      end if;
      --  Rust: `self.tcbs.get(local)?` -- eine GEPRUEFTE Abfrage, kein roher Index. Der
      --  Wert kommt aus einem Wort, das ein fremder Kern beschreibt.
      declare
         Local : constant Idx := Dir_Local (E);
      begin
         if Local > Max_Tcbs - 1 then
            return;
         end if;
         if S.Tcbs (Local).Used and then S.Tcbs (Local).Gen = T.Gen then
            L  := Local;
            Ok := True;
         end if;
      end;
   end Resolve;

   procedure Publish_Dir (S : Scheduler; Local : Tcb_Range)
     with Global => (In_Out => Directory), Always_Terminates
   is
      G : constant Link := S.Tcbs (Local).Gid;
   begin
      --  Rust: `if let Some(e) = DIRECTORY.get(t.gid as usize)`.
      if G <= Link (Max_Gids - 1) then
         Dir_Store (Gid_Range (G),
                    Pack_Dir (True, S.Tcbs (Local).Gen, S.Core, Local));
      end if;
   end Publish_Dir;

   function Sched_Id (S : Scheduler; Local : Tcb_Range) return Thread_Id
     is (Thread_Id'(Slot => Idx (S.Tcbs (Local).Gid mod 2**31),
                    Gen  => S.Tcbs (Local).Gen))
     with Global => null;

   ---------------------------------------------------------------------------
   --  Ready-Queue (intrusiv)
   ---------------------------------------------------------------------------

   --  Rust: `enqueue_ready`.
   procedure Enqueue_Ready (S : in out Scheduler; Local : Tcb_Range)
     with Global => null, Always_Terminates
   is
   begin
      if S.Tcbs (Local).Queued /= Not_Queued then
         return;
      end if;
      --  [S2-F1] `let p = self.tcbs[local].priority as usize; ... self.queues[p]`
      --  `priority` ist ein `u8` (0..255), `queues` hat NPRIO = 8 Faecher. Geklemmt wird
      --  an genau EINER Aufrufstelle (kernel/src/loader.rs), nicht im Scheduler.
      declare
         P : constant Idx := Idx (S.Tcbs (Local).Priority);
      begin
         declare
            Tail : constant Link := S.Queues (P).Tail;
         begin
            S.Tcbs (Local).Qprev  := Tail;
            S.Tcbs (Local).Qnext  := Nil_Link;
            S.Tcbs (Local).Queued := U8 (P);
            if Tail = Nil_Link then
               S.Queues (P).Head := Link (Local);
            else
               --  [S2-F2] `self.tcbs[tail as usize].qnext = local as u32`
               --  Roher Index aus der Verkettung -- nichts im Code stuetzt ihn.
               S.Tcbs (Tcb_Range (Tail)).Qnext := Link (Local);
            end if;
            S.Queues (P).Tail  := Link (Local);
            --  [S2-F3] `self.queues[p].count += 1` -- u32-Ueberlauf.
            S.Queues (P).Count := S.Queues (P).Count + 1;
            S.Bitmap := S.Bitmap or All_Bits (Natural (P));
         end;
      end;
   end Enqueue_Ready;

   --  Rust: `remove_from_ready`.
   procedure Remove_From_Ready (S : in out Scheduler; Local : Tcb_Range)
     with Global => null, Always_Terminates
   is
      Q : constant U8 := S.Tcbs (Local).Queued;
   begin
      if Q = Not_Queued then
         return;
      end if;
      declare
         --  [S2-F4] `let p = p as usize; ... self.queues[p]`
         --  `queued` ist ein `u8`; er wurde aus `priority` gesetzt und traegt dessen
         --  Wertebereich, nicht den der Tabelle.
         P    : constant Idx  := Idx (Q);
         Prev : constant Link := S.Tcbs (Local).Qprev;
         Next : constant Link := S.Tcbs (Local).Qnext;
      begin
         if Prev = Nil_Link then
            S.Queues (P).Head := Next;
         else
            --  [S2-F5] `self.tcbs[prev as usize].qnext = next`
            S.Tcbs (Tcb_Range (Prev)).Qnext := Next;
         end if;
         if Next = Nil_Link then
            S.Queues (P).Tail := Prev;
         else
            --  [S2-F6] `self.tcbs[next as usize].qprev = prev`
            S.Tcbs (Tcb_Range (Next)).Qprev := Prev;
         end if;
         S.Tcbs (Local).Qnext  := Nil_Link;
         S.Tcbs (Local).Qprev  := Nil_Link;
         S.Tcbs (Local).Queued := Not_Queued;
         --  [S2-F7] `self.queues[p].count -= 1` -- UNTERLAUF, ohne jede Bedingung.
         --  `[profile.release]` setzt `overflow-checks` nicht: aus 0 wird 0xFFFF_FFFF,
         --  `count == 0` wird nie wahr, das Bitmap-Bit bleibt stehen, und
         --  `dequeue_highest` liefert eine leere Liste als „hoechste Prioritaet".
         S.Queues (P).Count := S.Queues (P).Count - 1;
         if S.Queues (P).Count = 0 then
            S.Bitmap := S.Bitmap and not All_Bits (Natural (P));
         end if;
      end;
   end Remove_From_Ready;

   --  Rust: `dequeue_highest`.
   --
   --  Die Nachbedingung ist BEWEISBAR und steht hier, damit [S2-F8] EINMAL gezaehlt wird
   --  statt fuenfmal: ohne sie melden alle vier Aufrufstellen ihr eigenes `S.Tcbs (Nxt)`
   --  als Fund, obwohl es dieselbe Frage ist. Sie behauptet nichts -- die Umwandlung unten
   --  bleibt ungeprueft, und genau die IST der Fund.
   procedure Dequeue_Highest (S : in out Scheduler; L : out Idx; Ok : out Boolean)
     with Global => null, Always_Terminates,
          Post => (if Ok then L <= Max_Tcbs - 1)
   is
   begin
      L  := 0;
      Ok := False;
      if S.Bitmap = 0 then
         return;
      end if;
      declare
         --  Rust: `let p = (31 - self.bitmap.leading_zeros()) as usize;`
         --  [S2-F8] `self.queues[p]` -- `p` ist die hoechste gesetzte Bitstelle und damit
         --  0..31, die Tabelle hat 8 Faecher. Erreichbar ueber jede `priority >= 8`.
         P : Idx := 0;
      begin
         for B in reverse Natural range 0 .. 31 loop
            if (S.Bitmap and All_Bits (B)) /= 0 then
               P := Idx (B);
               exit;
            end if;
         end loop;
         declare
            Head : constant Link := S.Queues (P).Head;
         begin
            if Head = Nil_Link then
               return;
            end if;
            declare
               Local : constant Tcb_Range := Tcb_Range (Head);
            begin
               Remove_From_Ready (S, Local);
               L  := Local;
               Ok := True;
            end;
         end;
      end;
   end Dequeue_Highest;

   ---------------------------------------------------------------------------
   --  Grund-Menge: der einzige Weg zurueck in die Ready-Queue
   ---------------------------------------------------------------------------

   --  Rust: `wecke_falls_lauffaehig`.
   procedure Wecke_Falls_Lauffaehig (S : in out Scheduler; Local : Tcb_Range)
     with Global => null, Always_Terminates
   is
   begin
      if Is_Empty (S.Tcbs (Local).Reasons_Set)
        and then not S.Tcbs (Local).Depleted
        and then not (S.Current.Present and then S.Current.Value = Local)
      then
         Enqueue_Ready (S, Local);
      end if;
   end Wecke_Falls_Lauffaehig;

   --  Rust: `set_budget_blocked` -- die EINZIGE Stelle, die das Feld aendert (D10).
   procedure Set_Budget_Blocked (S : in out Scheduler; Local : Tcb_Range; An : Boolean)
     with Global => null, Always_Terminates
   is
   begin
      if Has (S.Tcbs (Local).Reasons_Set, R_Budget) = An then
         return;
      end if;
      if An then
         Insert_Reason (S.Tcbs (Local).Reasons_Set, R_Budget);
         --  [S2-F9] `self.budget_blocked_count += 1` -- usize-Ueberlauf.
         S.Budget_Blocked_Count := S.Budget_Blocked_Count + 1;
      else
         Remove_Reason (S.Tcbs (Local).Reasons_Set, R_Budget);
         --  [S2-F10] `self.budget_blocked_count -= 1` -- Unterlauf. Der Zaehler
         --  entscheidet, ob der teure Weckelauf laeuft; luegt er nach oben, laeuft er fuer
         --  immer (die `depleted_count`-Form aus D8/M5).
         S.Budget_Blocked_Count := S.Budget_Blocked_Count - 1;
      end if;
   end Set_Budget_Blocked;

   ---------------------------------------------------------------------------
   --  TCB-Vergabe
   ---------------------------------------------------------------------------

   --  Rust: `alloc_tcb`.
   procedure Alloc_Tcb (S : in out Scheduler; Sp : U64; Priority : U8;
                        L : out Idx; Ok : out Boolean)
     with Global => (In_Out => (Directory, Gid_Free, Core_Load_Tbl)),
          Always_Terminates,
          Post => (if Ok then L <= Max_Tcbs - 1)
   is
      I     : Tcb_Range;
      Got_I : Boolean;
      G     : Gid_Range;
      Got_G : Boolean;
      E     : Word64;
      Ld    : Boolean;
   begin
      L  := 0;
      Ok := False;
      Fl_Alloc (S.Free, I, Got_I);
      if not Got_I then
         return;
      end if;
      Gid_Alloc (G, Got_G);
      if not Got_G then
         Fl_Free (S.Free, I);   --  sonst leckt der TCB-Slot
         return;
      end if;
      Dir_Load (G, E, Ld);
      if not Ld then
         Fl_Free (S.Free, I);
         Gid_Free.Next (G) := Gid_Free.Head;
         Gid_Free.Head     := (Present => True, Value => G);
         return;
      end if;
      S.Tcbs (I) := Tcb_Empty;
      S.Tcbs (I).Used     := True;
      S.Tcbs (I).Gid      := Link (G);
      S.Tcbs (I).Gen      := Dir_Gen (E);
      S.Tcbs (I).Sp       := Sp;
      S.Tcbs (I).Priority := Priority;
      --  [S2-F11] `self.used += 1` -- usize-Ueberlauf.
      S.Used := S.Used + 1;
      if S.Core <= Max_Cores - 1 then
         Core_Load_Tbl (S.Core) := S.Used;
      end if;
      Publish_Dir (S, I);
      L  := I;
      Ok := True;
   end Alloc_Tcb;

   ---------------------------------------------------------------------------
   --  Zombie-Buchfuehrung
   ---------------------------------------------------------------------------

   --  Rust: `record_zombie`.
   procedure Record_Zombie (S : in out Scheduler; Local : Tcb_Range)
     with Global => (In_Out => (Directory, Core_Load_Tbl,
                                Zombie_Gesamt, Zombie_Unter_Fuessen)),
          Always_Terminates
   is
   begin
      --  H-b: ALLE Empfaenger dieser Spende loesen, nicht nur die Spitze.
      for D in Tcb_Range loop
         if D /= Local and then S.Tcbs (D).Used
           and then S.Tcbs (D).Sc_Donor.Present
           and then S.Tcbs (D).Sc_Donor.Value = Local
         then
            S.Tcbs (D).Sc_Donor := No_Idx;
            if Has (S.Tcbs (D).Reasons_Set, R_Budget) then
               Set_Budget_Blocked (S, D, False);
               Wecke_Falls_Lauffaehig (S, D);
            end if;
         end if;
      end loop;

      if S.Tcbs (Local).Sc_Donor.Present then
         --  [S2-F12] `if let Some(a) = self.tcbs[local].sc_donor { self.tcbs[a] ... }`
         --  `sc_donor` ist ein `Option<usize>` und wird ROH indiziert.
         declare
            A : constant Idx := S.Tcbs (Local).Sc_Donor.Value;
         begin
            if S.Tcbs (Tcb_Range (A)).Sc_Donee.Present
              and then S.Tcbs (Tcb_Range (A)).Sc_Donee.Value = Local
            then
               S.Tcbs (Tcb_Range (A)).Sc_Donee := No_Idx;
            end if;
         end;
      end if;

      Remove_From_Ready (S, Local);

      declare
         Base : constant U64   := S.Tcbs (Local).Stack_Base;
         Len  : constant U64   := S.Tcbs (Local).Stack_Len;
         G    : constant Link  := S.Tcbs (Local).Gid;
         --  `wrapping_add(1)` ist Absicht (Portierungsregel 5).
         Neu  : constant Gen_T := (S.Tcbs (Local).Gen + 1) and Gen_T (D_Gen_Mask);
      begin
         if G <= Link (Max_Gids - 1) then
            Dir_Store (Gid_Range (G), Pack_Dir (False, Neu, 0, 0));
         end if;
         if S.Tcbs (Local).Depleted then
            --  [S2-F13] `self.depleted_count -= 1` -- Unterlauf. UND: anders als
            --  `budget_blocked_count` wird dieser Zaehler von `audit` NICHT nachgezaehlt
            --  (es gibt keinen Code 11). Genau der Zaehler, der in D8/M5 gelogen hat.
            S.Depleted_Count := S.Depleted_Count - 1;
         end if;
         if Has (S.Tcbs (Local).Reasons_Set, R_Budget) then
            --  [S2-F14] `self.budget_blocked_count -= 1` (Bulk-Stelle, ohne den Helfer).
            S.Budget_Blocked_Count := S.Budget_Blocked_Count - 1;
         end if;
         S.Tcbs (Local) := Tcb_Empty;
         Fl_Free (S.Free, Local);
         --  [S2-F15] `self.used -= 1` -- Unterlauf.
         S.Used := S.Used - 1;
         if S.Core <= Max_Cores - 1 then
            Core_Load_Tbl (S.Core) := S.Used;
         end if;

         --  D15-Melder: liegt der eigene Rahmen in der Region, die freigegeben wird?
         if Len /= 0 then
            --  `fetch_add` laeuft um -- kein Fund (s. oben bei der Deklaration).
            Zombie_Gesamt := Zombie_Gesamt + 1;
            declare
               Hier : constant U64 := Stapeladresse;
            begin
               --  [S2-F16] `hier < base + len` -- usize-Ueberlauf in der Adressrechnung.
               if Hier >= Base and then Hier < Base + Len then
                  Zombie_Unter_Fuessen := Zombie_Unter_Fuessen + 1;
               end if;
            end;
         end if;

         --  Zombie IMMER aufzeichnen (auch ohne Stack): die `gid` muss zurueck.
         if S.Zcount < Max_Zombies then
            --  [S2-F17] `self.zombies[self.ztail] = ..` und
            --  `self.ztail = (self.ztail + 1) % self.zombies.len()`
            --  `ztail` ist ein roher `usize`; die Schranke oben prueft `zcount`, nicht ihn.
            S.Zombies (Zomb_Range (S.Ztail)) := (Base => Base, Len => Len, Gid => G);
            S.Ztail  := (S.Ztail + 1) mod Max_Zombies;
            S.Zcount := S.Zcount + 1;
         end if;
      end;
   end Record_Zombie;

   procedure Reap (S : in out Scheduler; Base : out U64; Len : out U64;
                   Gid : out Link; Ok : out Boolean) is
   begin
      Base := 0; Len := 0; Gid := Nil_Link; Ok := False;
      if S.Zcount = 0 then
         return;
      end if;
      --  [S2-F18] `let z = self.zombies[self.zhead];` -- roher `usize`-Index.
      declare
         Z : constant Zombie := S.Zombies (Zomb_Range (S.Zhead));
      begin
         S.Zhead  := (S.Zhead + 1) mod Max_Zombies;
         S.Zcount := S.Zcount - 1;
         Base := Z.Base; Len := Z.Len; Gid := Z.Gid; Ok := True;
      end;
   end Reap;

   ---------------------------------------------------------------------------
   --  Erzeugen, Zulassen, Beenden
   ---------------------------------------------------------------------------

   procedure Init_Core (S : in out Scheduler; C : Idx; Priority : U8;
                        T : out Thread_Id; Ok : out Boolean) is
      Idle : Idx;
      Got  : Boolean;
   begin
      T  := (Slot => 0, Gen => 0);
      Fl_Init (S.Free);
      S.Core := C;
      Alloc_Tcb (S, 0, Priority, Idle, Got);
      if not Got then
         Ok := False;
         return;
      end if;
      S.Current := Some_Idx (Idle);
      T  := Sched_Id (S, Tcb_Range (Idle));
      Ok := True;
   end Init_Core;

   procedure Spawn_Parked
     (S          : in out Scheduler;
      C          : Idx;
      Entry_Pt   : U64;
      Arg        : U64;
      Stack_Base : U64;
      Stack_Len  : U64;
      Priority   : U8;
      P          : out Parked)
   is
      Sp  : U64;
      T   : Idx;
      Got : Boolean;
   begin
      pragma Assert (C = S.Core);
      --  [S2-F19] `init_thread_frame(stack_base + stack_len, ..)` -- usize-Ueberlauf in
      --  der Adressrechnung des Aufrufers, bevor die HAL ueberhaupt drankommt.
      Sp := Init_Thread_Frame (Stack_Base + Stack_Len, Entry_Pt, Arg, False, 0);
      Alloc_Tcb (S, Sp, Priority, T, Got);
      if not Got then
         P := null;
         return;
      end if;
      S.Tcbs (Tcb_Range (T)).Stack_Base := Stack_Base;
      S.Tcbs (Tcb_Range (T)).Stack_Len  := Stack_Len;
      --  **Kein `enqueue_ready`** -- das ist D0, und es ist die eine fehlende Zeile.
      P := new Parked_Rec'(Tid => Sched_Id (S, Tcb_Range (T)));
   end Spawn_Parked;

   procedure Admit (S : in out Scheduler; P : in out Parked; Ok : out Boolean) is
      L   : Idx;
      Got : Boolean;
      T   : Thread_Id;
   begin
      if P = null then
         Ok := False;
         return;
      end if;
      T := P.all.Tid;
      Resolve (S, T, L, Got);
      if not Got then
         --  Auch hier wird der Besitz VERBRAUCHT: der Thread ist tot oder migriert.
         Free_Parked (P);
         Ok := False;
         return;
      end if;
      S.Tcbs (Tcb_Range (L)).Admitted := True;
      Enqueue_Ready (S, Tcb_Range (L));
      Free_Parked (P);
      Ok := True;
   end Admit;

   procedure Exit_Current (S : in out Scheduler; C : Idx; Sp : out U64) is
      Nxt : Idx;
      Got : Boolean;
   begin
      pragma Assert (C = S.Core);
      --  Rust: `self.current.expect("kein laufender Thread")`.
      if not S.Current.Present then
         Sp := 0;
         return;
      end if;
      --  [S2-F20] `self.record_zombie(cur)` mit `cur: usize` aus `Option` -- roher Index.
      Record_Zombie (S, Tcb_Range (S.Current.Value));
      Dequeue_Highest (S, Nxt, Got);
      if not Got then
         --  Rust: `.expect("Idle-Thread sollte immer bereit sein")` -- ein Panic.
         Sp := 0;
         return;
      end if;
      S.Current := Some_Idx (Nxt);
      Sp := S.Tcbs (Tcb_Range (Nxt)).Sp;
   end Exit_Current;

   procedure Kill (S : in out Scheduler; T : Thread_Id; C : Idx; Ok : out Boolean) is
      L   : Idx;
      Got : Boolean;
   begin
      pragma Assert (C = S.Core);
      Resolve (S, T, L, Got);
      if not Got then
         Ok := False;
         return;
      end if;
      if S.Current.Present and then S.Current.Value = L then
         Ok := False;
         return;
      end if;
      Remove_From_Ready (S, Tcb_Range (L));
      Record_Zombie (S, Tcb_Range (L));
      Ok := True;
   end Kill;

   ---------------------------------------------------------------------------
   --  Blockieren und Wecken
   ---------------------------------------------------------------------------

   procedure Block_Current_Mit (S : in out Scheduler; C : Idx; Frame : U64;
                                Grund : Reasons; Sp : out U64) is
      Nxt : Idx;
      Got : Boolean;
   begin
      pragma Assert (C = S.Core);
      Sp := 0;
      if not S.Current.Present then
         return;
      end if;
      --  [S2-F21] `self.tcbs[cur]` mit `cur` aus `Option<usize>`.
      declare
         Cur : constant Tcb_Range := Tcb_Range (S.Current.Value);
      begin
         S.Tcbs (Cur).Sp := Frame;
         Insert_Reason (S.Tcbs (Cur).Reasons_Set, Grund);
      end;
      Dequeue_Highest (S, Nxt, Got);
      if not Got then
         return;
      end if;
      S.Current := Some_Idx (Nxt);
      Sp := S.Tcbs (Tcb_Range (Nxt)).Sp;
   end Block_Current_Mit;

   procedure Switch_To (S : in out Scheduler; C : Idx; Frame : U64;
                        Target : Thread_Id; Sp : out U64) is
      L   : Idx;
      Got : Boolean;
      Nxt : Idx;
      Ok2 : Boolean;
   begin
      pragma Assert (C = S.Core);
      Sp := 0;
      if not S.Current.Present then
         return;
      end if;
      declare
         Cur : constant Tcb_Range := Tcb_Range (S.Current.Value);
      begin
         S.Tcbs (Cur).Sp := Frame;
         Insert_Reason (S.Tcbs (Cur).Reasons_Set, R_Ipc);
         Resolve (S, Target, L, Got);
         if not Got then
            --  Rust: `.expect("Zielthread ungueltig/fremder Kern")` -- ein Panic.
            return;
         end if;
         --  EINEN Grund entfernen, nicht alle (Z24).
         Remove_Reason (S.Tcbs (Tcb_Range (L)).Reasons_Set, R_Ipc);
         if not Is_Empty (S.Tcbs (Tcb_Range (L)).Reasons_Set) then
            Dequeue_Highest (S, Nxt, Ok2);
            if not Ok2 then
               return;
            end if;
            S.Current := Some_Idx (Nxt);
            Sp := S.Tcbs (Tcb_Range (Nxt)).Sp;
            return;
         end if;
         Remove_From_Ready (S, Tcb_Range (L));
         --  Budget-Donation: belastet wird das WURZEL-Konto des Aufrufers.
         --  [S2-F22] `let account = self.tcbs[cur].sc_donor.unwrap_or(cur);
         --            self.tcbs[account].sc_donee = Some(t);` -- roher Index.
         declare
            Account : constant Idx :=
              (if S.Tcbs (Cur).Sc_Donor.Present then S.Tcbs (Cur).Sc_Donor.Value
               else Idx (Cur));
         begin
            S.Tcbs (Tcb_Range (L)).Sc_Donor := Some_Idx (Account);
            S.Tcbs (Tcb_Range (Account)).Sc_Donee := Some_Idx (L);
         end;
         S.Current := Some_Idx (L);
         Sp := S.Tcbs (Tcb_Range (L)).Sp;
      end;
   end Switch_To;

   procedure Unblock (S : in out Scheduler; T : Thread_Id; Ok : out Boolean) is
      L   : Idx;
      Got : Boolean;
   begin
      Resolve (S, T, L, Got);
      if not Got then
         Ok := False;
         return;
      end if;
      Ok := True;
      declare
         Sl : constant Tcb_Range := Tcb_Range (L);
      begin
         if Is_Empty (S.Tcbs (Sl).Reasons_Set) then
            return;
         end if;
         --  [S2-F23] `let acct = self.tcbs[s].sc_donor.unwrap_or(s);
         --            if acct != s && self.tcbs[acct].depleted` -- roher Index.
         declare
            Acct : constant Idx :=
              (if S.Tcbs (Sl).Sc_Donor.Present then S.Tcbs (Sl).Sc_Donor.Value
               else Idx (Sl));
         begin
            if Acct /= Idx (Sl) and then S.Tcbs (Tcb_Range (Acct)).Depleted then
               Remove_Reason (S.Tcbs (Sl).Reasons_Set, R_Ipc);
               Set_Budget_Blocked (S, Sl, True);
               return;
            end if;
         end;
         Remove_Reason (S.Tcbs (Sl).Reasons_Set, R_Ipc);
         --  Eingereiht wird NUR bei leerer Menge (Z24) und nicht auf leerem Konto (D8).
         if Is_Empty (S.Tcbs (Sl).Reasons_Set) and then not S.Tcbs (Sl).Depleted then
            Enqueue_Ready (S, Sl);
         end if;
      end;
   end Unblock;

   procedure Park_Current (S : in out Scheduler; C : Idx; Frame : U64;
                           Sp : out U64; Blocked : out Boolean) is
   begin
      pragma Assert (C = S.Core);
      Sp := 0;
      Blocked := False;
      if not S.Current.Present then
         return;
      end if;
      declare
         Cur : constant Tcb_Range := Tcb_Range (S.Current.Value);
      begin
         if S.Tcbs (Cur).Park_Wake then
            --  Verbraucht: eine Marke weckt genau einmal.
            S.Tcbs (Cur).Park_Wake := False;
            return;
         end if;
      end;
      Block_Current_Mit (S, C, Frame, R_Park, Sp);
      Blocked := True;
   end Park_Current;

   procedure Unpark (S : in out Scheduler; T : Thread_Id; Ok : out Boolean) is
      L   : Idx;
      Got : Boolean;
   begin
      Resolve (S, T, L, Got);
      if not Got then
         Ok := False;
         return;
      end if;
      Ok := True;
      declare
         Sl : constant Tcb_Range := Tcb_Range (L);
      begin
         --  Die Marke wird IMMER hinterlegt -- sonst gaebe es ein verlorenes Wecken.
         S.Tcbs (Sl).Park_Wake := True;
         if Has (S.Tcbs (Sl).Reasons_Set, R_Park) then
            S.Tcbs (Sl).Park_Wake := False;
            Remove_Reason (S.Tcbs (Sl).Reasons_Set, R_Park);
            Wecke_Falls_Lauffaehig (S, Sl);
         end if;
      end;
   end Unpark;

   procedure Pause (S : in out Scheduler; T : Thread_Id; Ok : out Boolean) is
      L   : Idx;
      Got : Boolean;
   begin
      Resolve (S, T, L, Got);
      if not Got then
         Ok := False;
         return;
      end if;
      Insert_Reason (S.Tcbs (Tcb_Range (L)).Reasons_Set, R_Pause);
      Remove_From_Ready (S, Tcb_Range (L));
      Ok := True;
   end Pause;

   procedure Resume (S : in out Scheduler; T : Thread_Id; Ok : out Boolean) is
      L   : Idx;
      Got : Boolean;
   begin
      Resolve (S, T, L, Got);
      if not Got then
         Ok := False;
         return;
      end if;
      Remove_Reason (S.Tcbs (Tcb_Range (L)).Reasons_Set, R_Pause);
      Wecke_Falls_Lauffaehig (S, Tcb_Range (L));
      Ok := True;
   end Resume;

   procedure Mark_Handler_Wait (S : in out Scheduler; T : Thread_Id; Ok : out Boolean) is
      L   : Idx;
      Got : Boolean;
   begin
      Resolve (S, T, L, Got);
      if not Got then
         Ok := False;
         return;
      end if;
      Insert_Reason (S.Tcbs (Tcb_Range (L)).Reasons_Set, R_Handler);
      Remove_From_Ready (S, Tcb_Range (L));
      Ok := True;
   end Mark_Handler_Wait;

   procedure Handler_Reply (S : in out Scheduler; T : Thread_Id; Ok : out Boolean) is
      L   : Idx;
      Got : Boolean;
   begin
      Resolve (S, T, L, Got);
      if not Got then
         Ok := False;
         return;
      end if;
      Remove_Reason (S.Tcbs (Tcb_Range (L)).Reasons_Set, R_Handler);
      Wecke_Falls_Lauffaehig (S, Tcb_Range (L));
      Ok := True;
   end Handler_Reply;

   procedure Load_Reply (S : in out Scheduler; T : Thread_Id; Ok : out Boolean) is
      L   : Idx;
      Got : Boolean;
   begin
      Resolve (S, T, L, Got);
      if not Got then
         Ok := False;
         return;
      end if;
      Remove_Reason (S.Tcbs (Tcb_Range (L)).Reasons_Set, R_Load);
      Wecke_Falls_Lauffaehig (S, Tcb_Range (L));
      Ok := True;
   end Load_Reply;

   ---------------------------------------------------------------------------
   --  Zeit: Tick, Erschoepfung, Refill
   ---------------------------------------------------------------------------

   --  Rust: `refill_depleted`.
   procedure Refill_Depleted (S : in out Scheduler)
     with Global => null, Always_Terminates
   is
   begin
      for Slot in Tcb_Range loop
         if S.Tcbs (Slot).Used
           and then S.Tcbs (Slot).Budget > 0
           and then S.Tcbs (Slot).Depleted
           and then S.Now >= S.Tcbs (Slot).Next_Refill
         then
            S.Tcbs (Slot).Remaining := S.Tcbs (Slot).Budget;
            S.Tcbs (Slot).Depleted  := False;
            --  [S2-F24] `self.depleted_count -= 1` -- Unterlauf, und dieser Zaehler wird
            --  von `audit` NICHT nachgezaehlt.
            S.Depleted_Count := S.Depleted_Count - 1;
            --  [S2-F25] `self.refills += 1` -- u64-Ueberlauf.
            S.Refills := S.Refills + 1;

            --  D10: der Weckelauf nur, wenn ueberhaupt jemand budget-blockiert ist.
            if S.Budget_Blocked_Count > 0 then
               for D in Tcb_Range loop
                  if D /= Slot and then S.Tcbs (D).Used
                    and then Has (S.Tcbs (D).Reasons_Set, R_Budget)
                    and then S.Tcbs (D).Sc_Donor.Present
                    and then S.Tcbs (D).Sc_Donor.Value = Idx (Slot)
                  then
                     Set_Budget_Blocked (S, D, False);
                     Wecke_Falls_Lauffaehig (S, D);
                  end if;
               end loop;
            end if;

            if not (S.Tcbs (Slot).Sc_Donee.Present
                    and then S.Tcbs (Slot).Sc_Donee.Value /= Idx (Slot))
            then
               --  Einen PAUSIERTEN Thread weckt der Refill nicht -- seit Z24 keine
               --  eigene Regel mehr, sondern die leere Menge.
               if not (S.Current.Present and then S.Current.Value = Idx (Slot)) then
                  Wecke_Falls_Lauffaehig (S, Slot);
               end if;
            end if;
         end if;
      end loop;
   end Refill_Depleted;

   procedure On_Tick (S : in out Scheduler; C : Idx; Frame : U64; Tick : Boolean;
                      Sp : out U64) is
      Nxt : Idx;
      Got : Boolean;
   begin
      pragma Assert (C = S.Core);
      if Tick then
         --  [S2-F26] `self.now += 1` -- u64-Ueberlauf.
         S.Now := S.Now + 1;
         if S.Depleted_Count > 0 then
            Refill_Depleted (S);
         end if;
      end if;
      if S.Current.Present then
         declare
            Cur     : constant Tcb_Range := Tcb_Range (S.Current.Value);
            Wieder_Ein : Boolean;
         begin
            S.Tcbs (Cur).Sp := Frame;
            Wieder_Ein := Is_Empty (S.Tcbs (Cur).Reasons_Set);
            --  [S2-F27] `let acct = self.tcbs[cur].sc_donor.unwrap_or(cur);` -- roher Index.
            declare
               Acct : constant Tcb_Range :=
                 Tcb_Range (if S.Tcbs (Cur).Sc_Donor.Present
                            then S.Tcbs (Cur).Sc_Donor.Value else Idx (Cur));
            begin
               if Tick and then S.Tcbs (Acct).Budget > 0 then
                  --  `saturating_sub(1)` -- kein Ueberlauf, absichtlich saettigend.
                  S.Tcbs (Acct).Remaining :=
                    (if S.Tcbs (Acct).Remaining = 0 then 0
                     else S.Tcbs (Acct).Remaining - 1);
                  if S.Tcbs (Acct).Remaining = 0 then
                     S.Tcbs (Acct).Depleted := True;
                     --  [S2-F28] `self.now + self.tcbs[acct].period as u64` -- u64-Ueberlauf.
                     S.Tcbs (Acct).Next_Refill := S.Now + U64 (S.Tcbs (Acct).Period);
                     --  [S2-F29] `self.depleted_count += 1`, `self.depletions += 1`.
                     S.Depleted_Count := S.Depleted_Count + 1;
                     S.Depletions     := S.Depletions + 1;
                     Wieder_Ein := False;
                     if Acct /= Cur then
                        Set_Budget_Blocked (S, Cur, True);
                     end if;
                  end if;
               end if;
            end;
            if Wieder_Ein then
               Enqueue_Ready (S, Cur);
            end if;
         end;
      end if;
      Dequeue_Highest (S, Nxt, Got);
      if Got then
         S.Current := Some_Idx (Nxt);
         Sp := S.Tcbs (Tcb_Range (Nxt)).Sp;
      else
         Sp := Frame;   --  nichts lauffaehig (Idle sollte immer dabei sein)
      end if;
   end On_Tick;

   procedure Set_Budget (S : in out Scheduler; T : Thread_Id;
                         Budget : U32; Period : U32; Ok : out Boolean) is
      L   : Idx;
      Got : Boolean;
   begin
      Resolve (S, T, L, Got);
      if not Got then
         Ok := False;
         return;
      end if;
      Ok := True;
      declare
         Sl           : constant Tcb_Range := Tcb_Range (L);
         Was_Depleted : constant Boolean   := S.Tcbs (Sl).Depleted;
      begin
         S.Tcbs (Sl).Budget      := Budget;
         S.Tcbs (Sl).Period      := (if Period = 0 then 1 else Period);
         S.Tcbs (Sl).Remaining   := Budget;
         --  [S2-F30] `self.now + period as u64` -- u64-Ueberlauf.
         S.Tcbs (Sl).Next_Refill := S.Now + U64 (Period);
         S.Tcbs (Sl).Depleted    := False;
         if Was_Depleted then
            --  [S2-F31] `self.depleted_count -= 1` -- Unterlauf.
            S.Depleted_Count := S.Depleted_Count - 1;
            if S.Budget_Blocked_Count > 0 then
               for D in Tcb_Range loop
                  if D /= Sl and then S.Tcbs (D).Used
                    and then Has (S.Tcbs (D).Reasons_Set, R_Budget)
                    and then S.Tcbs (D).Sc_Donor.Present
                    and then S.Tcbs (D).Sc_Donor.Value = Idx (Sl)
                  then
                     Set_Budget_Blocked (S, D, False);
                     Wecke_Falls_Lauffaehig (S, D);
                  end if;
               end loop;
            end if;
            if not (S.Current.Present and then S.Current.Value = Idx (Sl)) then
               Wecke_Falls_Lauffaehig (S, Sl);
            end if;
         end if;
      end;
   end Set_Budget;

   procedure End_Donation (S : in out Scheduler; C : Idx) is
   begin
      pragma Assert (C = S.Core);
      if not S.Current.Present then
         return;
      end if;
      declare
         Cur : constant Tcb_Range := Tcb_Range (S.Current.Value);
      begin
         if S.Tcbs (Cur).Sc_Donor.Present then
            declare
               --  [S2-F32] `if let Some(acct) = ..sc_donor.take() { self.tcbs[acct] }`
               Acct : constant Idx := S.Tcbs (Cur).Sc_Donor.Value;
            begin
               S.Tcbs (Cur).Sc_Donor := No_Idx;
               if S.Tcbs (Tcb_Range (Acct)).Sc_Donee.Present
                 and then S.Tcbs (Tcb_Range (Acct)).Sc_Donee.Value = Idx (Cur)
               then
                  S.Tcbs (Tcb_Range (Acct)).Sc_Donee := No_Idx;
               end if;
            end;
         end if;
      end;
   end End_Donation;

   ---------------------------------------------------------------------------
   --  Migration
   ---------------------------------------------------------------------------

   procedure Detach_For_Migration (S : in out Scheduler; T : Thread_Id;
                                   M : out Tcb; Was_Ready : out Boolean;
                                   Ok : out Boolean) is
      L   : Idx;
      Got : Boolean;
   begin
      M := Tcb_Empty; Was_Ready := False; Ok := False;
      Resolve (S, T, L, Got);
      if not Got then
         return;
      end if;
      declare
         Sl : constant Tcb_Range := Tcb_Range (L);
      begin
         if S.Current.Present and then S.Current.Value = Idx (Sl) then
            return;
         end if;
         if S.Tcbs (Sl).Sc_Donor.Present or else S.Tcbs (Sl).Sc_Donee.Present then
            return;
         end if;
         Was_Ready := S.Tcbs (Sl).Queued /= Not_Queued;
         Remove_From_Ready (S, Sl);
         M := S.Tcbs (Sl);
         M.Queued := Not_Queued;
         M.Qnext  := Nil_Link;
         M.Qprev  := Nil_Link;
         if S.Tcbs (Sl).Depleted then
            --  [S2-F33] `self.depleted_count -= 1` -- Unterlauf.
            S.Depleted_Count := S.Depleted_Count - 1;
         end if;
         if Has (S.Tcbs (Sl).Reasons_Set, R_Budget) then
            --  [S2-F34] `self.budget_blocked_count -= 1` -- Unterlauf.
            S.Budget_Blocked_Count := S.Budget_Blocked_Count - 1;
         end if;
         S.Tcbs (Sl) := Tcb_Empty;
         Fl_Free (S.Free, Sl);
         --  [S2-F35] `self.used -= 1` -- Unterlauf.
         S.Used := S.Used - 1;
         if S.Core <= Max_Cores - 1 then
            Core_Load_Tbl (S.Core) := S.Used;
         end if;
         --  [S2-F36] `self.migrations_out += 1` -- u64-Ueberlauf.
         S.Migrations_Out := S.Migrations_Out + 1;
         Ok := True;
      end;
   end Detach_For_Migration;

   procedure Attach_Migrated (S : in out Scheduler; M : Tcb; Was_Ready : Boolean;
                              Ok : out Boolean) is
      I   : Tcb_Range;
      Got : Boolean;
      T   : Tcb := M;
   begin
      Fl_Alloc (S.Free, I, Got);
      if not Got then
         Ok := False;
         return;
      end if;
      if T.Depleted then
         --  [S2-F37] `tcb.next_refill = self.now + tcb.period as u64` -- u64-Ueberlauf.
         T.Next_Refill    := S.Now + U64 (T.Period);
         S.Depleted_Count := S.Depleted_Count + 1;
      end if;
      if Has (T.Reasons_Set, R_Budget) then
         S.Budget_Blocked_Count := S.Budget_Blocked_Count + 1;
      end if;
      T.Queued := Not_Queued;
      T.Qnext  := Nil_Link;
      T.Qprev  := Nil_Link;
      S.Tcbs (I) := T;
      --  [S2-F38] `self.used += 1`, `self.migrations_in += 1` -- Ueberlauf.
      S.Used := S.Used + 1;
      if S.Core <= Max_Cores - 1 then
         Core_Load_Tbl (S.Core) := S.Used;
      end if;
      S.Migrations_In := S.Migrations_In + 1;
      Publish_Dir (S, I);
      if Was_Ready then
         Enqueue_Ready (S, I);
      end if;
      Ok := True;
   end Attach_Migrated;

   procedure Migration_Candidate (S : Scheduler; T : out Thread_Id; Ok : out Boolean) is
   begin
      T  := (Slot => 0, Gen => 0);
      Ok := False;
      for P in Prio_Range loop
         declare
            I : Link := S.Queues (P).Head;
         begin
            --  [S2-T1] `while i != NIL { .. i = t.qnext; }`
            --  **Keine Schrittgrenze.** Daneben, in `audit`, steht ueber derselben Kette
            --  `if n > q.count { return 5 }`. Ist die Verkettung zyklisch, laeuft diese
            --  Schleife fuer immer -- im Lastausgleich, unter dem Kern-Lock.
            while I /= Nil_Link loop
               declare
                  Sl : constant Tcb_Range := Tcb_Range (I);
               begin
                  I := S.Tcbs (Sl).Qnext;
                  if not ((S.Current.Present and then S.Current.Value = Idx (Sl))
                          or else S.Tcbs (Sl).Sc_Donor.Present
                          or else S.Tcbs (Sl).Sc_Donee.Present
                          or else S.Tcbs (Sl).Stack_Len = 0)
                  then
                     T  := Sched_Id (S, Sl);
                     Ok := True;
                     return;
                  end if;
               end;
            end loop;
         end;
      end loop;
   end Migration_Candidate;

   ---------------------------------------------------------------------------
   --  Audit
   ---------------------------------------------------------------------------

   procedure Audit_Sched (S : Scheduler; Code : out U32) is
      Bb : Idx := 0;
   begin
      for P in Prio_Range loop
         declare
            Q : constant List_Head := S.Queues (P);
         begin
            if ((S.Bitmap and All_Bits (Natural (P))) /= 0) /= (Q.Count > 0) then
               Code := 5;
               return;
            end if;
            declare
               I    : Link := Q.Head;
               Prev : Link := Nil_Link;
               N    : U32  := 0;
            begin
               --  Anders als `migration_candidate` hat DIESE Kettenwanderung eine
               --  Schrittgrenze (`n > q.count`) -- daher terminiert sie.
               while I /= Nil_Link loop
                  pragma Loop_Invariant (N <= Q.Count);
                  pragma Loop_Variant (Increases => N);
                  if I > Link (Max_Tcbs - 1) then
                     Code := 1;      --  Rust: `self.tcbs.get(s)` -> None
                     return;
                  end if;
                  declare
                     Sl : constant Tcb_Range := Tcb_Range (I);
                  begin
                     if not S.Tcbs (Sl).Used then
                        Code := 1;
                        return;
                     end if;
                     if not Is_Empty (S.Tcbs (Sl).Reasons_Set) then
                        Code := 2;
                        return;
                     end if;
                     if S.Tcbs (Sl).Depleted then
                        Code := 9;
                        return;
                     end if;
                     if Idx (S.Tcbs (Sl).Queued) /= P
                       or else Idx (S.Tcbs (Sl).Priority) /= P
                     then
                        Code := 6;
                        return;
                     end if;
                     if S.Tcbs (Sl).Qprev /= Prev then
                        Code := 3;
                        return;
                     end if;
                     if S.Current.Present and then S.Current.Value = Idx (Sl) then
                        Code := 4;
                        return;
                     end if;
                     Prev := I;
                     I    := S.Tcbs (Sl).Qnext;
                     N    := N + 1;
                     if N > Q.Count then
                        Code := 5;
                        return;
                     end if;
                  end;
               end loop;
               if N /= Q.Count or else Q.Tail /= Prev then
                  Code := 5;
                  return;
               end if;
            end;
         end;
      end loop;

      for Local in Tcb_Range loop
         if S.Tcbs (Local).Used then
            if Has (S.Tcbs (Local).Reasons_Set, R_Budget) then
               Bb := Bb + 1;
            end if;
            declare
               E  : Word64;
               Ld : Boolean;
            begin
               Dir_Load (Idx (S.Tcbs (Local).Gid mod 2**31), E, Ld);
               if not Ld
                 or else (E and D_Used) = 0
                 or else Dir_Gen (E) /= S.Tcbs (Local).Gen
                 or else Dir_Core (E) /= S.Core
                 or else Dir_Local (E) /= Idx (Local)
               then
                  Code := 8;
                  return;
               end if;
            end;
            --  `t.admitted` gehoert in diese Bedingung (2026-08-07).
            if Is_Empty (S.Tcbs (Local).Reasons_Set)
              and then not S.Tcbs (Local).Depleted
              and then S.Tcbs (Local).Admitted
              and then not (S.Current.Present and then S.Current.Value = Idx (Local))
              and then S.Tcbs (Local).Queued = Not_Queued
            then
               Code := 7;
               return;
            end if;
         end if;
         pragma Loop_Invariant (Bb <= Idx (Local) + 1);
      end loop;

      --  D10: die unabhaengige Nachzaehlung -- fuer `budget_blocked_count`, und NUR
      --  fuer den. `depleted_count` hat kein Gegenstueck (kein Code 11).
      if Bb /= S.Budget_Blocked_Count then
         Code := 10;
         return;
      end if;
      Code := 0;
   end Audit_Sched;

   ---------------------------------------------------------------------------
   --  Inspektion
   ---------------------------------------------------------------------------

   procedure Priority_Of (S : Scheduler; T : Thread_Id; P : out U8; Ok : out Boolean) is
      L : Idx;
   begin
      Resolve (S, T, L, Ok);
      P := (if Ok then S.Tcbs (Tcb_Range (L)).Priority else 0);
   end Priority_Of;

   procedure Reasons_Of (S : Scheduler; T : Thread_Id; R : out Reasons; Ok : out Boolean) is
      L : Idx;
   begin
      Resolve (S, T, L, Ok);
      R := (if Ok then S.Tcbs (Tcb_Range (L)).Reasons_Set else R_None);
   end Reasons_Of;

   procedure Frame_Of (S : Scheduler; T : Thread_Id; Sp : out U64; Ok : out Boolean) is
      L : Idx;
   begin
      Resolve (S, T, L, Ok);
      Sp := (if Ok then S.Tcbs (Tcb_Range (L)).Sp else 0);
   end Frame_Of;

   procedure Admitted_Of (S : Scheduler; T : Thread_Id; A : out Boolean; Ok : out Boolean) is
      L : Idx;
   begin
      Resolve (S, T, L, Ok);
      A := (if Ok then S.Tcbs (Tcb_Range (L)).Admitted else False);
   end Admitted_Of;

end Caprock_Sched;
