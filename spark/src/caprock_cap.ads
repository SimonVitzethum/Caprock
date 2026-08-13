--  Caprock -- SPARK-Portierung von `crates/caprock-cap/src/space.rs` (Experiment).
--
--  ZWECK: die Frage beantworten, ob GNATprove auf Silver Level (Abwesenheit von
--  Laufzeitfehlern) am Cap-Space etwas findet, das das Verus-Modell nicht sieht.
--
--  Das Verus-Modell (verus/cap_cdt_*.rs) fuehrt Referenzzaehler als `nat` und Tabellen
--  als `Seq` -- beides UNBESCHRAENKT, und Indizes werden dort per `0 <= s < len`
--  VORAUSGESETZT statt geprueft. Ueber Ueberlauf, Unterlauf und Indexgrenzen kann es
--  deshalb strukturell nichts sagen. Genau diese Luecke ist hier das Ziel.
--
--  PORTIERUNGSREGELN (Abweichungen davon sind Befunde ueber die Portierung, nicht
--  ueber Caprock):
--
--   1. KEINE Vorbedingung, die der Rust-Code nicht auch erzwingt. Wer `Delete_Leaf`
--      ein `Pre => Refcount > 0` gibt, hat den Befund wegdefiniert. Die Vorbedingungen,
--      die hier stehen, sind ausnahmslos solche, die der Rust-Aufrufer STRUKTURELL
--      einhaelt (z. B. "der Slot kommt aus `resolve`") -- und sie sind an der
--      Aufrufstelle BEWIESEN, nicht angenommen.
--   2. Verkettungsindizes sind vom TYP her breiter als die Tabelle (`Idx` gegen
--      `Slot_Range`). In Rust ist ein `Option<usize>` in `Mdb` genau das: eine Zahl,
--      die ausserhalb der Tabelle liegen darf. `descend_to_leaf` und `count_children`
--      sind gegen diesen Fall gebaut -- ein auf die Tabelle beschraenkter Indextyp
--      haette ihn wegmodelliert.
--   3. `slots[i]` (Rust: `Index`-Impl von Slab, PANIKT bei `i >= len`) wird zu einem
--      direkten Feldzugriff, dessen Indexpruefung GNATprove fuehren muss.
--      `slots.get(i)` (Rust: `Option`) wird zu einer ausgeschriebenen Bereichsabfrage.
--   4. `refcount: u32` wird ein RANGE-Typ, kein modularer. Grund: `[profile.release]`
--      in Cargo.toml setzt `overflow-checks` NICHT -- `refcount -= 1` bei 0 ist dort
--      ein stiller Umlauf auf 0xFFFF_FFFF, im Dev-Profil ein Panic. Beides ist ein
--      Fehler. Ein modularer Ada-Typ wuerde die Frage gar nicht erst stellen.
--   5. `gen.wrapping_add(1)` ist ABSICHT -> modularer Typ, kein Check. Ebenso `u64`.
--   6. Die Tabellen sind hier vollstaendig angehaengt (`Nslots = Max_Slots`). Damit
--      faellt die Ada-Indexpruefung genau mit der Slab-Schranke aus Rust zusammen.
--   7. `ObjectKind` ist FLACH statt als Variantensatz modelliert. Ein Ada-Variantensatz
--      erzeugt Diskriminanten-Pruefungen an `out`-Parametern, die in Rust kein
--      Gegenstueck haben (dort ist eine `enum`-Zuweisung immer gueltig) -- das waeren
--      erfundene Befunde. Fuer die RTE-Frage geht nichts verloren: die `match`-Arme
--      und die gelesenen Nutzfelder bleiben.

pragma Ada_2022;

package Caprock_Cap with SPARK_Mode => On is

   ---------------------------------------------------------------------------
   --  Grundtypen
   ---------------------------------------------------------------------------

   --  "usize"-Ersatz. Breiter als jede Tabelle -- s. Portierungsregel 2.
   type Idx is range 0 .. 2**31 - 1;

   Max_Slots   : constant Idx := 256;
   Max_Objects : constant Idx := 256;

   subtype Slot_Range is Idx range 0 .. Max_Slots - 1;
   subtype Obj_Range  is Idx range 0 .. Max_Objects - 1;

   --  Modell von `Option<usize>`.
   type Opt_Idx is record
      Present : Boolean := False;
      Value   : Idx     := 0;
   end record;

   No_Idx : constant Opt_Idx := (Present => False, Value => 0);

   function Some_Idx (I : Idx) return Opt_Idx is ((Present => True, Value => I))
     with Global => null;

   --  Referenzzaehler -- s. Portierungsregel 4.
   type U32 is range 0 .. 2**32 - 1;

   --  Generationszaehler -- `wrapping_add`, s. Portierungsregel 5.
   type Gen_T is mod 2**32;

   type U64 is mod 2**64;

   --  Rechte als Bitmaske; `intersect` ist ein bitweises Und.
   type Rights_T is mod 2**16;
   Rights_None : constant Rights_T := 0;

   function Intersect (A, B : Rights_T) return Rights_T is (A and B)
     with Global => null;

   ---------------------------------------------------------------------------
   --  Objekte  (flach -- s. Portierungsregel 7)
   ---------------------------------------------------------------------------

   type Dma_Dir is (Device_Read, Device_Write, Bidirectional);
   type Dma_Coherence is (Coherent, Non_Coherent);

   type Kind_Tag is
     (K_Memory, K_Endpoint, K_Tcb, K_Notification, K_Sched_Context,
      K_Reply, K_Pd_Control, K_Loader, K_Mmio, K_Irq, K_Dma,
      K_Syscall_Handler, K_Fault_Handler);

   type Object_Kind is record
      Tag : Kind_Tag       := K_Memory;
      --  Memory.base / Dma.phys / Mmio.phys / Tcb.raw / Reply.caller / sidecar
      A64 : U64            := 0;
      --  Memory.len / Dma.len / Mmio.len / handler.len
      B64 : U64            := 0;
      --  Endpoint / Notification / Reply.ep / budget / pd / source / intid
      A32 : U32            := 0;
      --  period / handler.pd
      B32 : U32            := 0;
      Dir : Dma_Dir        := Bidirectional;
      Coh : Dma_Coherence  := Non_Coherent;
   end record;

   Kind_Empty : constant Object_Kind :=
     (Tag => K_Memory, A64 => 0, B64 => 0, A32 => 0, B32 => 0,
      Dir => Bidirectional, Coh => Non_Coherent);

   type Obj is record
      Used     : Boolean     := False;
      Kind     : Object_Kind := Kind_Empty;
      Refcount : U32         := 0;
      Gen      : Gen_T       := 0;
   end record;

   ---------------------------------------------------------------------------
   --  Slots + CDT
   ---------------------------------------------------------------------------

   type Mdb is record
      Parent       : Opt_Idx := No_Idx;
      First_Child  : Opt_Idx := No_Idx;
      Next_Sibling : Opt_Idx := No_Idx;
      Prev_Sibling : Opt_Idx := No_Idx;
   end record;

   Mdb_Empty : constant Mdb := (others => No_Idx);

   type Cap_Slot is record
      Used   : Boolean   := False;
      Gen    : Gen_T     := 0;
      Object : Idx       := 0;   --  Index in die Objekttabelle (Rust: usize)
      Rights : Rights_T  := Rights_None;
      Badge  : U64       := 0;
      M      : Mdb       := Mdb_Empty;
   end record;

   Slot_Empty : constant Cap_Slot :=
     (Used => False, Gen => 0, Object => 0, Rights => Rights_None,
      Badge => 0, M => Mdb_Empty);

   --  Modell von `CapPtr` (Slot-Index + Generation).
   type Cap_Ptr is record
      Slot : Idx   := 0;
      Gen  : Gen_T := 0;
   end record;

   type Cap_Error is
     (E_Ok, No_Slot, No_Object, Invalid, Has_Children, Unaligned, Zu_Klein);

   ---------------------------------------------------------------------------
   --  Finalized -- der Rueckmeldepuffer (Rust: geliehene Slices)
   ---------------------------------------------------------------------------

   Max_Fin : constant Idx := Max_Objects;

   type Reply_Item is record
      Ep     : U32 := 0;
      Caller : U64 := 0;
   end record;

   type Dma_Item is record
      Phys : U64 := 0;
      Len  : U64 := 0;
   end record;

   type Reply_Array is array (Idx range 0 .. Max_Fin - 1) of Reply_Item;
   type Dma_Array   is array (Idx range 0 .. Max_Fin - 1) of Dma_Item;

   --  `Items_Len`/`Dma_Len` sind Rusts `items.len()`/`dma.len()`: der AUFRUFER bringt
   --  die Flaeche mit und darf sie zu klein waehlen (A-3.3). Deshalb eigene Felder.
   type Finalized is record
      Items     : Reply_Array := (others => (Ep => 0, Caller => 0));
      N         : Idx         := 0;
      Items_Len : Idx         := Max_Fin;
      Dma       : Dma_Array   := (others => (Phys => 0, Len => 0));
      Dn        : Idx         := 0;
      Dma_Len   : Idx         := Max_Fin;
      Overflow  : Boolean     := False;
   end record;

   ---------------------------------------------------------------------------
   --  Der Cap-Space
   ---------------------------------------------------------------------------

   type Slot_Array is array (Slot_Range) of Cap_Slot;
   type Obj_Array  is array (Obj_Range) of Obj;

   --  Rust: `seen: &mut [bool]` bzw. `refs: &mut [u32]` -- der Aufrufer bringt die
   --  Flaeche mit, und sie darf ZU KURZ sein. Deshalb je ein eigenes Laengenfeld.
   type Mark_Array is array (Idx range 0 .. Max_Slots - 1) of Boolean;
   type Slot_Marks is record
      Bits : Mark_Array := (others => False);
      Len  : Idx        := Max_Slots;
   end record;

   type Count_Array is array (Idx range 0 .. Max_Objects - 1) of U32;
   type Ref_Counts is record
      Vals : Count_Array := (others => 0);
      Len  : Idx         := Max_Objects;
   end record;

   type Cap_Space is record
      Slots             : Slot_Array := (others => Slot_Empty);
      Objects           : Obj_Array  := (others => (Used => False, Kind => Kind_Empty,
                                                    Refcount => 0, Gen => 0));
      Peak_Slots        : Idx := 0;
      Peak_Objects      : Idx := 0;
      Peak_Cdt_Walk     : Idx := 0;
      Peak_Revoke_Ops   : Idx := 0;
      Cdt_Walk_Overruns : U32 := 0;
   end record;

   ---------------------------------------------------------------------------
   --  Freie Funktionen ueber der Slot-Tabelle (Rust: freie fn ueber `&[CapSlot]`)
   ---------------------------------------------------------------------------

   --  Rust: `descend_to_leaf`. `Ok = False` entspricht `Err(())`.
   --
   --  `Pre => Limit < Idx'Last`: in Rust ist `limit` immer `slots.len()`; ein
   --  `limit = usize::MAX` liesse `steps += 1` nach 2^64 Runden ueberlaufen. Das ist
   --  unerreichbar, aber nicht ableitbar -- die Bedingung steht deshalb hier statt als
   --  Fund.
   procedure Descend_To_Leaf
     (Slots : Slot_Array;
      Start : Idx;
      Limit : Idx;
      Leaf  : out Idx;
      Steps : out Idx;
      Ok    : out Boolean)
   with Global => null,
        Always_Terminates,
        Pre  => Limit < Idx'Last,
        Post => (if Ok then Leaf in Slot_Range and then Steps <= Limit);

   --  Rust: `count_children`.
   procedure Count_Children
     (Slots  : Slot_Array;
      Parent : Idx;
      Limit  : Idx;
      N      : out Idx;
      Ok     : out Boolean)
   with Global => null,
        Always_Terminates,
        Pre  => Limit < Idx'Last,
        Post => (if Ok then N <= Limit);

   ---------------------------------------------------------------------------
   --  Operationen des Cap-Space
   ---------------------------------------------------------------------------

   procedure Install
     (S      : in out Cap_Space;
      Kind   : Object_Kind;
      Rights : Rights_T;
      P      : out Cap_Ptr;
      Err    : out Cap_Error)
   with Global => null, Always_Terminates;

   procedure Copy
     (S      : in out Cap_Space;
      Src    : Cap_Ptr;
      Rights : Rights_T;
      P      : out Cap_Ptr;
      Err    : out Cap_Error)
   with Global => null, Always_Terminates,
        Post => (if Err = E_Ok then P.Slot in Slot_Range);

   procedure Mint
     (S      : in out Cap_Space;
      Src    : Cap_Ptr;
      Rights : Rights_T;
      Badge  : U64;
      P      : out Cap_Ptr;
      Err    : out Cap_Error)
   with Global => null, Always_Terminates;

   procedure Move_Cap
     (S   : in out Cap_Space;
      Src : Cap_Ptr;
      P   : out Cap_Ptr;
      Err : out Cap_Error)
   with Global => null, Always_Terminates;

   procedure Delete
     (S   : in out Cap_Space;
      P   : Cap_Ptr;
      Rf  : in out Finalized;
      Err : out Cap_Error)
   with Global => null, Always_Terminates,
        Pre => Rf.N <= Rf.Items_Len and then Rf.Items_Len <= Max_Fin
               and then Rf.Dn <= Rf.Dma_Len and then Rf.Dma_Len <= Max_Fin;

   procedure Revoke
     (S   : in out Cap_Space;
      P   : Cap_Ptr;
      Rf  : in out Finalized;
      Err : out Cap_Error)
   with Global => null, Always_Terminates,
        Pre => Rf.N <= Rf.Items_Len and then Rf.Items_Len <= Max_Fin
               and then Rf.Dn <= Rf.Dma_Len and then Rf.Dma_Len <= Max_Fin;

   ---------------------------------------------------------------------------
   --  Inspektion
   ---------------------------------------------------------------------------

   function Used_Slots (S : Cap_Space) return Idx
     with Global => null, Post => Used_Slots'Result <= Max_Slots;

   function Used_Objects (S : Cap_Space) return Idx
     with Global => null, Post => Used_Objects'Result <= Max_Objects;

   procedure Mark_Slot
     (S    : Cap_Space;
      C    : Cap_Ptr;
      Seen : in out Slot_Marks;
      Ok   : out Boolean)
   with Global => null, Always_Terminates, Pre => Seen.Len <= Max_Slots;

   --  Rust: `unmarked_used_slots`. `Ok = False` entspricht `None` ("konnte nicht laufen").
   procedure Unmarked_Used_Slots
     (S    : Cap_Space;
      Seen : Slot_Marks;
      N    : out Idx;
      Ok   : out Boolean)
   with Global => null, Always_Terminates;

   --  Rust: `audit_cdt`. `Code` = 0 bei Konsistenz, sonst Anomaliecode 1..8.
   procedure Audit_Cdt
     (S    : Cap_Space;
      Refs : in out Ref_Counts;
      Code : out U32)
   with Global => null, Always_Terminates, Pre => Refs.Len <= Max_Objects;

   procedure Inspect
     (S        : Cap_Space;
      P        : Cap_Ptr;
      Kind     : out Object_Kind;
      Rights   : out Rights_T;
      Badge    : out U64;
      Refcount : out U32;
      Kids     : out Idx;
      Valid    : out Boolean)
   with Global => null, Always_Terminates;

end Caprock_Cap;
