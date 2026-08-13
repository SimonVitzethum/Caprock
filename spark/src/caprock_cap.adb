--  Caprock -- SPARK-Portierung von `crates/caprock-cap/src/space.rs` (Experiment).
--  Regeln s. Spezifikation. Jede Abweichung vom Rust-Original ist hier kommentiert.
--
--  Die `Loop_Invariant`-Zeilen sind BEWEISPFLICHTEN, keine Annahmen: GNATprove weist
--  jede einzeln nach (Erhaltung + Gueltigkeit beim Eintritt). Sie stehen hier, damit
--  die uebrig bleibenden roten Pruefungen die INHALTLICHEN sind und nicht die, die
--  bloss ein fehlender Beweishinweis erzeugt.

pragma Ada_2022;

package body Caprock_Cap with SPARK_Mode => On is

   --  Rust: `alloc.free_region(region)` -- der RAM-Allokator liegt ausserhalb dieser
   --  Crate. Einzige Stelle, die hier nichts zu suchen hat; sie beruehrt den Cap-Space
   --  nicht und ist deshalb ein leerer Rumpf, kein `SPARK_Mode => Off`.
   procedure Free_Region (Base, Len : U64) with Global => null, Always_Terminates;
   procedure Free_Region (Base, Len : U64) is null;

   ---------------------------------------------------------------------------
   --  Freie Funktionen ueber der Slot-Tabelle
   ---------------------------------------------------------------------------

   procedure Descend_To_Leaf
     (Slots : Slot_Array;
      Start : Idx;
      Limit : Idx;
      Leaf  : out Idx;
      Steps : out Idx;
      Ok    : out Boolean)
   is
      Cur : Idx := Start;
      St  : Idx := 0;
   begin
      loop
         pragma Loop_Invariant (St <= Limit);
         pragma Loop_Variant (Increases => St);

         --  Rust: `let Some(s) = slots.get(leaf) else { return Err(()) }`
         if Cur > Slot_Range'Last then
            Leaf := 0; Steps := 0; Ok := False;
            return;
         end if;

         --  Rust: `let Some(c) = s.mdb.first_child else { return Ok((leaf, steps)) }`
         if not Slots (Cur).M.First_Child.Present then
            Leaf := Cur; Steps := St; Ok := True;
            return;
         end if;

         St := St + 1;                  --  Rust: `steps += 1`
         if St > Limit then             --  Rust: `if steps > limit { return Err(()) }`
            Leaf := 0; Steps := 0; Ok := False;
            return;
         end if;
         Cur := Slots (Cur).M.First_Child.Value;
      end loop;
   end Descend_To_Leaf;

   procedure Count_Children
     (Slots  : Slot_Array;
      Parent : Idx;
      Limit  : Idx;
      N      : out Idx;
      Ok     : out Boolean)
   is
      Cnt : Idx := 0;
      Cur : Opt_Idx;
   begin
      --  Rust: `match slots.get(parent) { Some(s) => s.mdb.first_child, None => Err }`
      if Parent > Slot_Range'Last then
         N := 0; Ok := False;
         return;
      end if;
      Cur := Slots (Parent).M.First_Child;

      while Cur.Present loop
         pragma Loop_Invariant (Cnt <= Limit);
         pragma Loop_Variant (Increases => Cnt);

         Cnt := Cnt + 1;                --  Rust: `n += 1`
         if Cnt > Limit then
            N := 0; Ok := False;
            return;
         end if;
         --  Rust: `let Some(s) = slots.get(i) else { return Err(()) }`
         if Cur.Value > Slot_Range'Last then
            N := 0; Ok := False;
            return;
         end if;
         Cur := Slots (Cur.Value).M.Next_Sibling;
      end loop;
      N := Cnt; Ok := True;
   end Count_Children;

   ---------------------------------------------------------------------------
   --  intern
   ---------------------------------------------------------------------------

   --  Rust: `cdt_step_limit` -- `self.slots.len()`.
   function Cdt_Step_Limit return Idx is (Max_Slots) with Global => null;

   function Used_Slots (S : Cap_Space) return Idx is
      N : Idx := 0;
   begin
      for I in Slot_Range loop
         if S.Slots (I).Used then
            N := N + 1;
         end if;
         pragma Loop_Invariant (N <= I + 1);
      end loop;
      return N;
   end Used_Slots;

   function Used_Objects (S : Cap_Space) return Idx is
      N : Idx := 0;
   begin
      for I in Obj_Range loop
         if S.Objects (I).Used then
            N := N + 1;
         end if;
         pragma Loop_Invariant (N <= I + 1);
      end loop;
      return N;
   end Used_Objects;

   procedure Note_Walk (S : in out Cap_Space; Steps : Idx)
     with Global => null, Always_Terminates;
   procedure Note_Walk (S : in out Cap_Space; Steps : Idx) is
   begin
      if Steps > S.Peak_Cdt_Walk then
         S.Peak_Cdt_Walk := Steps;
      end if;
   end Note_Walk;

   --  Rust: `self.cdt_walk_overruns.saturating_add(1)`.
   procedure Note_Overrun (S : in out Cap_Space)
     with Global => null, Always_Terminates;
   procedure Note_Overrun (S : in out Cap_Space) is
   begin
      if S.Cdt_Walk_Overruns < U32'Last then
         S.Cdt_Walk_Overruns := S.Cdt_Walk_Overruns + 1;
      end if;
   end Note_Overrun;

   --  Rust: `fn ptr(&self, slot: usize) -> CapPtr`. Jeder Aufrufer liefert einen Slot
   --  aus `resolve` oder `alloc_slot` -- die Vorbedingung ist an jeder Aufrufstelle
   --  bewiesen, nicht angenommen.
   function Ptr_Of (S : Cap_Space; Slot : Idx) return Cap_Ptr is
     ((Slot => Slot, Gen => S.Slots (Slot).Gen))
   with Global => null, Pre => Slot in Slot_Range;

   --  Rust: `fn resolve(&self, ptr) -> Result<usize, CapError>`
   procedure Resolve
     (S    : Cap_Space;
      P    : Cap_Ptr;
      Slot : out Idx;
      Err  : out Cap_Error)
   with Global => null, Always_Terminates,
        Post => (if Err = E_Ok then Slot in Slot_Range);
   procedure Resolve
     (S    : Cap_Space;
      P    : Cap_Ptr;
      Slot : out Idx;
      Err  : out Cap_Error)
   is
   begin
      if P.Slot <= Slot_Range'Last
        and then S.Slots (P.Slot).Used
        and then S.Slots (P.Slot).Gen = P.Gen
      then
         Slot := P.Slot; Err := E_Ok;
      else
         Slot := 0; Err := Invalid;
      end if;
   end Resolve;

   --  Rust: `fn free_slot_index(&self) -> Result<usize, CapError>`
   procedure Free_Slot_Index (S : Cap_Space; I : out Idx; Err : out Cap_Error)
     with Global => null, Always_Terminates,
          Post => (if Err = E_Ok then I in Slot_Range);
   procedure Free_Slot_Index (S : Cap_Space; I : out Idx; Err : out Cap_Error) is
   begin
      for J in Slot_Range loop
         if not S.Slots (J).Used then
            I := J; Err := E_Ok;
            return;
         end if;
      end loop;
      I := 0; Err := No_Slot;
   end Free_Slot_Index;

   --  Rust: `fn alloc_slot(&mut self, object, rights, badge) -> Result<usize, CapError>`
   procedure Alloc_Slot
     (S      : in out Cap_Space;
      Object : Idx;
      Rights : Rights_T;
      Badge  : U64;
      I      : out Idx;
      Err    : out Cap_Error)
   with Global => null, Always_Terminates,
        Post => (if Err = E_Ok then I in Slot_Range);
   procedure Alloc_Slot
     (S      : in out Cap_Space;
      Object : Idx;
      Rights : Rights_T;
      Badge  : U64;
      I      : out Idx;
      Err    : out Cap_Error)
   is
      G   : Gen_T;
      Now : Idx;
   begin
      Free_Slot_Index (S, I, Err);
      if Err /= E_Ok then
         return;
      end if;
      G := S.Slots (I).Gen;
      S.Slots (I) := (Used   => True,
                      Gen    => G,
                      Object => Object,
                      Rights => Rights,
                      Badge  => Badge,
                      M      => Mdb_Empty);
      Now := Used_Slots (S);
      if Now > S.Peak_Slots then
         S.Peak_Slots := Now;
      end if;
   end Alloc_Slot;

   --  Rust: `fn release_slot(&mut self, slot: usize)`
   procedure Release_Slot (S : in out Cap_Space; Slot : Idx)
     with Global => null, Always_Terminates, Pre => Slot in Slot_Range;
   procedure Release_Slot (S : in out Cap_Space; Slot : Idx) is
      G : constant Gen_T := S.Slots (Slot).Gen + 1;   --  wrapping_add(1)
   begin
      S.Slots (Slot) := Slot_Empty;
      S.Slots (Slot).Gen := G;
   end Release_Slot;

   --  Rust: `fn alloc_object_inner(&mut self, kind) -> Result<usize, CapError>`
   procedure Alloc_Object_Inner
     (S    : in out Cap_Space;
      Kind : Object_Kind;
      I    : out Idx;
      Err  : out Cap_Error)
   with Global => null, Always_Terminates,
        Post => (if Err = E_Ok then I in Obj_Range);
   procedure Alloc_Object_Inner
     (S    : in out Cap_Space;
      Kind : Object_Kind;
      I    : out Idx;
      Err  : out Cap_Error)
   is
      G : Gen_T;
   begin
      for J in Obj_Range loop
         if not S.Objects (J).Used then
            G := S.Objects (J).Gen;
            S.Objects (J) := (Used => True, Kind => Kind, Refcount => 1, Gen => G);
            I := J; Err := E_Ok;
            return;
         end if;
      end loop;
      I := 0; Err := No_Object;
   end Alloc_Object_Inner;

   --  Rust: `fn alloc_object(&mut self, kind) -> Result<usize, CapError>`
   procedure Alloc_Object
     (S    : in out Cap_Space;
      Kind : Object_Kind;
      I    : out Idx;
      Err  : out Cap_Error)
   with Global => null, Always_Terminates,
        Post => (if Err = E_Ok then I in Obj_Range);
   procedure Alloc_Object
     (S    : in out Cap_Space;
      Kind : Object_Kind;
      I    : out Idx;
      Err  : out Cap_Error)
   is
      Now : Idx;
   begin
      Alloc_Object_Inner (S, Kind, I, Err);
      if Err = E_Ok then
         Now := Used_Objects (S);
         if Now > S.Peak_Objects then
            S.Peak_Objects := Now;
         end if;
      end if;
   end Alloc_Object;

   --  Rust: `fn child_count(&self, slot) -> Result<usize, ()>`
   procedure Child_Count (S : Cap_Space; Slot : Idx; N : out Idx; Ok : out Boolean)
     with Global => null, Always_Terminates;
   procedure Child_Count (S : Cap_Space; Slot : Idx; N : out Idx; Ok : out Boolean) is
   begin
      Count_Children (S.Slots, Slot, Cdt_Step_Limit, N, Ok);
   end Child_Count;

   --  Rust: `fn link_child(&mut self, parent: usize, child: usize)`
   procedure Link_Child (S : in out Cap_Space; Parent, Child : Idx)
     with Global => null, Always_Terminates,
          Pre => Parent in Slot_Range and then Child in Slot_Range;
   procedure Link_Child (S : in out Cap_Space; Parent, Child : Idx) is
      Old_First : constant Opt_Idx := S.Slots (Parent).M.First_Child;
   begin
      S.Slots (Child).M.Parent       := Some_Idx (Parent);
      S.Slots (Child).M.Prev_Sibling := No_Idx;
      S.Slots (Child).M.Next_Sibling := Old_First;
      if Old_First.Present then
         --  [F1] Rust: `self.slots[f].mdb.prev_sibling = Some(child)` -- `f` kommt aus
         --  der Verkettung und ist NICHT schrankengeprueft.
         S.Slots (Old_First.Value).M.Prev_Sibling := Some_Idx (Child);
      end if;
      S.Slots (Parent).M.First_Child := Some_Idx (Child);
   end Link_Child;

   --  Rust: `fn unlink(&mut self, slot: usize)`
   procedure Unlink (S : in out Cap_Space; Slot : Idx)
     with Global => null, Always_Terminates, Pre => Slot in Slot_Range;
   procedure Unlink (S : in out Cap_Space; Slot : Idx) is
      M : constant Mdb := S.Slots (Slot).M;
   begin
      if M.Prev_Sibling.Present then
         --  [F2] Rust: `self.slots[p].mdb.next_sibling = ...`
         S.Slots (M.Prev_Sibling.Value).M.Next_Sibling := M.Next_Sibling;
      elsif M.Parent.Present then
         --  [F3] Rust: `self.slots[par].mdb.first_child = ...`
         S.Slots (M.Parent.Value).M.First_Child := M.Next_Sibling;
      end if;
      if M.Next_Sibling.Present then
         --  [F4] Rust: `self.slots[n].mdb.prev_sibling = ...`
         S.Slots (M.Next_Sibling.Value).M.Prev_Sibling := M.Prev_Sibling;
      end if;
      S.Slots (Slot).M := Mdb_Empty;
   end Unlink;

   --  Rust: `fn push(&mut self, ep, caller)` von `Finalized`
   procedure Push (Rf : in out Finalized; Ep : U32; Caller : U64)
     with Global => null, Always_Terminates,
          Pre  => Rf.N <= Rf.Items_Len and then Rf.Items_Len <= Max_Fin,
          Post => Rf.N <= Rf.Items_Len and then Rf.Items_Len = Rf.Items_Len'Old
                  and then Rf.Dn = Rf.Dn'Old and then Rf.Dma_Len = Rf.Dma_Len'Old;
   procedure Push (Rf : in out Finalized; Ep : U32; Caller : U64) is
   begin
      if Rf.N < Rf.Items_Len then
         Rf.Items (Rf.N) := (Ep => Ep, Caller => Caller);
         Rf.N := Rf.N + 1;
      else
         Rf.Overflow := True;
      end if;
   end Push;

   --  Rust: `fn push_dma(&mut self, phys, len)`
   procedure Push_Dma (Rf : in out Finalized; Phys, Len : U64)
     with Global => null, Always_Terminates,
          Pre  => Rf.Dn <= Rf.Dma_Len and then Rf.Dma_Len <= Max_Fin,
          Post => Rf.Dn <= Rf.Dma_Len and then Rf.Dma_Len = Rf.Dma_Len'Old
                  and then Rf.N = Rf.N'Old and then Rf.Items_Len = Rf.Items_Len'Old;
   procedure Push_Dma (Rf : in out Finalized; Phys, Len : U64) is
   begin
      if Rf.Dn < Rf.Dma_Len then
         Rf.Dma (Rf.Dn) := (Phys => Phys, Len => Len);
         Rf.Dn := Rf.Dn + 1;
      else
         Rf.Overflow := True;
      end if;
   end Push_Dma;

   --  Rust: `fn delete_leaf(&mut self, alloc, slot, rf)`
   procedure Delete_Leaf (S : in out Cap_Space; Slot : Idx; Rf : in out Finalized)
     with Global => null, Always_Terminates,
          Pre  => Slot in Slot_Range
                  and then Rf.N <= Rf.Items_Len and then Rf.Items_Len <= Max_Fin
                  and then Rf.Dn <= Rf.Dma_Len and then Rf.Dma_Len <= Max_Fin,
          Post => Rf.N <= Rf.Items_Len and then Rf.Items_Len <= Max_Fin
                  and then Rf.Dn <= Rf.Dma_Len and then Rf.Dma_Len <= Max_Fin;
   procedure Delete_Leaf (S : in out Cap_Space; Slot : Idx; Rf : in out Finalized) is
      O : constant Idx := S.Slots (Slot).Object;
      G : Gen_T;
   begin
      Unlink (S, Slot);
      Release_Slot (S, Slot);

      --  [F5] Rust: `self.objects[obj].refcount -= 1;` -- `obj` ist ein roher usize aus
      --  dem Slot, ohne Schrankenpruefung gegen `objects.len()`.
      --  [F6] und der Zaehler wird ohne jede Bedingung dekrementiert.
      S.Objects (O).Refcount := S.Objects (O).Refcount - 1;
      if S.Objects (O).Refcount = 0 then
         case S.Objects (O).Kind.Tag is
            when K_Memory =>
               Free_Region (S.Objects (O).Kind.A64, S.Objects (O).Kind.B64);
            when K_Dma =>
               Push_Dma (Rf, S.Objects (O).Kind.A64, S.Objects (O).Kind.B64);
            when K_Reply =>
               Push (Rf, S.Objects (O).Kind.A32, S.Objects (O).Kind.A64);
            when others =>
               null;
         end case;
         G := S.Objects (O).Gen + 1;   --  wrapping_add(1)
         S.Objects (O) := (Used     => False,
                           Kind     => Kind_Empty,
                           Refcount => 0,
                           Gen      => G);
      end if;
   end Delete_Leaf;

   ---------------------------------------------------------------------------
   --  Oeffentliche Operationen
   ---------------------------------------------------------------------------

   procedure Install
     (S      : in out Cap_Space;
      Kind   : Object_Kind;
      Rights : Rights_T;
      P      : out Cap_Ptr;
      Err    : out Cap_Error)
   is
      O    : Idx;
      Slot : Idx;
   begin
      P := (Slot => 0, Gen => 0);
      Alloc_Object (S, Kind, O, Err);
      if Err /= E_Ok then
         return;
      end if;
      Alloc_Slot (S, O, Rights, 0, Slot, Err);
      if Err /= E_Ok then
         S.Objects (O).Used := False;   --  Rust: Objekt zurueckrollen
         return;
      end if;
      P := Ptr_Of (S, Slot);
   end Install;

   procedure Copy
     (S      : in out Cap_Space;
      Src    : Cap_Ptr;
      Rights : Rights_T;
      P      : out Cap_Ptr;
      Err    : out Cap_Error)
   is
      Sl         : Idx;
      O          : Idx;
      New_Rights : Rights_T;
      Badge      : U64;
      Dst        : Idx;
   begin
      P := (Slot => 0, Gen => 0);
      Resolve (S, Src, Sl, Err);
      if Err /= E_Ok then
         return;
      end if;
      --  [F7] Rust: `let obj = self.slots[s].object;` -- roher usize, ungeprueft.
      O          := S.Slots (Sl).Object;
      New_Rights := Intersect (S.Slots (Sl).Rights, Rights);
      Badge      := S.Slots (Sl).Badge;
      Alloc_Slot (S, O, New_Rights, Badge, Dst, Err);
      if Err /= E_Ok then
         return;
      end if;
      --  [F8] Rust: `self.objects[obj].refcount += 1;` -- Index UND Ueberlauf offen.
      S.Objects (O).Refcount := S.Objects (O).Refcount + 1;
      Link_Child (S, Sl, Dst);
      P := Ptr_Of (S, Dst);
   end Copy;

   procedure Mint
     (S      : in out Cap_Space;
      Src    : Cap_Ptr;
      Rights : Rights_T;
      Badge  : U64;
      P      : out Cap_Ptr;
      Err    : out Cap_Error)
   is
   begin
      Copy (S, Src, Rights, P, Err);
      if Err /= E_Ok then
         return;
      end if;
      S.Slots (P.Slot).Badge := Badge;
   end Mint;

   procedure Move_Cap
     (S   : in out Cap_Space;
      Src : Cap_Ptr;
      P   : out Cap_Ptr;
      Err : out Cap_Error)
   is
      Sl    : Idx;
      Dst   : Idx;
      G     : Gen_T;
      M     : Mdb;
      Limit : constant Idx := Cdt_Step_Limit;
      Child : Opt_Idx;
      Steps : Idx := 0;
   begin
      P := (Slot => 0, Gen => 0);
      Resolve (S, Src, Sl, Err);
      if Err /= E_Ok then
         return;
      end if;
      Free_Slot_Index (S, Dst, Err);
      if Err /= E_Ok then
         return;
      end if;

      --  Inhalt uebernehmen (Generation des Zielslots beibehalten).
      G := S.Slots (Dst).Gen;
      S.Slots (Dst) := S.Slots (Sl);
      S.Slots (Dst).Gen := G;

      --  Alle Verweise auf `Sl` auf `Dst` umbiegen.
      M := S.Slots (Sl).M;
      if M.Prev_Sibling.Present then
         --  [F9] wie [F2]
         S.Slots (M.Prev_Sibling.Value).M.Next_Sibling := Some_Idx (Dst);
      elsif M.Parent.Present then
         --  [F10] wie [F3]
         S.Slots (M.Parent.Value).M.First_Child := Some_Idx (Dst);
      end if;
      if M.Next_Sibling.Present then
         --  [F11] wie [F4]
         S.Slots (M.Next_Sibling.Value).M.Prev_Sibling := Some_Idx (Dst);
      end if;

      Child := M.First_Child;
      while Child.Present loop
         pragma Loop_Invariant (Steps <= Limit);
         pragma Loop_Variant (Increases => Steps);
         Steps := Steps + 1;
         if Steps > Limit then
            Note_Overrun (S);
            exit;
         end if;
         --  [F12] Rust: `self.slots[c].mdb.parent = Some(dst);` -- roher Kettenindex.
         S.Slots (Child.Value).M.Parent := Some_Idx (Dst);
         Child := S.Slots (Child.Value).M.Next_Sibling;
      end loop;
      Note_Walk (S, Steps);

      Release_Slot (S, Sl);
      P := Ptr_Of (S, Dst);
   end Move_Cap;

   procedure Delete
     (S   : in out Cap_Space;
      P   : Cap_Ptr;
      Rf  : in out Finalized;
      Err : out Cap_Error)
   is
      Slot : Idx;
   begin
      Resolve (S, P, Slot, Err);
      if Err /= E_Ok then
         return;
      end if;
      if S.Slots (Slot).M.First_Child.Present then
         Err := Has_Children;
         return;
      end if;
      Delete_Leaf (S, Slot, Rf);
      Err := E_Ok;
   end Delete;

   procedure Revoke
     (S   : in out Cap_Space;
      P   : Cap_Ptr;
      Rf  : in out Finalized;
      Err : out Cap_Error)
   is
      Slot  : Idx;
      Limit : constant Idx := Cdt_Step_Limit;
      Ops   : Idx := 0;
      Leaf  : Idx;
      Steps : Idx;
      Good  : Boolean;
   begin
      Resolve (S, P, Slot, Err);
      if Err /= E_Ok then
         return;
      end if;

      while S.Slots (Slot).M.First_Child.Present loop
         pragma Loop_Invariant (Ops <= Limit);
         pragma Loop_Invariant (Rf.N <= Rf.Items_Len and then Rf.Items_Len <= Max_Fin);
         pragma Loop_Invariant (Rf.Dn <= Rf.Dma_Len and then Rf.Dma_Len <= Max_Fin);
         pragma Loop_Variant (Increases => Ops);

         Descend_To_Leaf (S.Slots, S.Slots (Slot).M.First_Child.Value, Limit,
                          Leaf, Steps, Good);
         if not Good then
            Note_Overrun (S);
            exit;
         end if;
         Note_Walk (S, Steps);
         Delete_Leaf (S, Leaf, Rf);
         Ops := Ops + 1;
         if Ops > Limit then
            Note_Overrun (S);
            exit;
         end if;
      end loop;

      if Ops > S.Peak_Revoke_Ops then
         S.Peak_Revoke_Ops := Ops;
      end if;
      Err := E_Ok;
   end Revoke;

   ---------------------------------------------------------------------------
   --  Inspektion
   ---------------------------------------------------------------------------

   procedure Mark_Slot
     (S    : Cap_Space;
      C    : Cap_Ptr;
      Seen : in out Slot_Marks;
      Ok   : out Boolean)
   is
   begin
      --  Rust: `let Some(slot) = self.slots.get(cap.slot) else { return false }`
      if C.Slot > Slot_Range'Last then
         Ok := False;
         return;
      end if;
      if not S.Slots (C.Slot).Used
        or else S.Slots (C.Slot).Gen /= C.Gen
        or else C.Slot >= Seen.Len
      then
         Ok := False;
         return;
      end if;
      Seen.Bits (C.Slot) := True;
      Ok := True;
   end Mark_Slot;

   procedure Unmarked_Used_Slots
     (S    : Cap_Space;
      Seen : Slot_Marks;
      N    : out Idx;
      Ok   : out Boolean)
   is
      Cnt : Idx := 0;
   begin
      if Seen.Len < Max_Slots then
         N := 0; Ok := False;
         return;
      end if;
      for I in Slot_Range loop
         if S.Slots (I).Used and then not Seen.Bits (I) then
            Cnt := Cnt + 1;
         end if;
         pragma Loop_Invariant (Cnt <= I + 1);
      end loop;
      N := Cnt; Ok := True;
   end Unmarked_Used_Slots;

   procedure Audit_Cdt
     (S    : Cap_Space;
      Refs : in out Ref_Counts;
      Code : out U32)
   is
      Nslots   : constant Idx := Max_Slots;
      Nobjects : constant Idx := Max_Objects;
      Limit    : constant Idx := Cdt_Step_Limit;
      O        : Idx;
      M        : Mdb;
      C        : Opt_Idx;
      Found    : Boolean;
      Steps    : Idx;
      Pp       : Opt_Idx;
   begin
      if Refs.Len < Nobjects then
         Code := 8;
         return;
      end if;
      for I in Obj_Range loop
         Refs.Vals (I) := 0;
         pragma Loop_Invariant
           (for all J in Obj_Range range 0 .. I => Refs.Vals (J) = 0);
      end loop;

      --  (1)+(2)+(3)
      for Sl in Slot_Range loop
         pragma Loop_Invariant
           (for all J in Obj_Range => Refs.Vals (J) <= U32 (Sl));
         if S.Slots (Sl).Used then
            O := S.Slots (Sl).Object;
            if O >= Nobjects or else not S.Objects (O).Used then
               Code := 1;
               return;
            end if;
            Refs.Vals (O) := Refs.Vals (O) + 1;
         end if;
      end loop;

      for Ob in Obj_Range loop
         if S.Objects (Ob).Used then
            if S.Objects (Ob).Refcount /= Refs.Vals (Ob) then
               Code := 2;
               return;
            end if;
            if Refs.Vals (Ob) = 0 then
               Code := 3;
               return;
            end if;
         elsif Refs.Vals (Ob) /= 0 then
            Code := 3;
            return;
         end if;
      end loop;

      --  (4)+(5)+(6)
      for Sl in Slot_Range loop
         if S.Slots (Sl).Used then
            M := S.Slots (Sl).M;
            if M.Parent.Present then
               if M.Parent.Value >= Nslots
                 or else not S.Slots (M.Parent.Value).Used
                 or else S.Slots (M.Parent.Value).Object /= S.Slots (Sl).Object
               then
                  Code := 4;
                  return;
               end if;
               --  Sl muss in der Kinderliste von Parent vorkommen.
               C     := S.Slots (M.Parent.Value).M.First_Child;
               Found := False;
               Steps := 0;
               while C.Present loop
                  pragma Loop_Invariant (Steps <= Limit);
                  pragma Loop_Variant (Increases => Steps);
                  if C.Value = Sl then
                     Found := True;
                     exit;
                  end if;
                  --  [F13] Rust: `c = self.slots[ci].mdb.next_sibling;` -- roher
                  --  Kettenindex im PRUEFER selbst.
                  C := S.Slots (C.Value).M.Next_Sibling;
                  Steps := Steps + 1;
                  if Steps > Limit then
                     Code := 7;
                     return;
                  end if;
               end loop;
               if not Found then
                  Code := 4;
                  return;
               end if;
            end if;

            if M.First_Child.Present then
               if M.First_Child.Value >= Nslots
                 or else not S.Slots (M.First_Child.Value).Used
                 or else S.Slots (M.First_Child.Value).M.Parent /= Some_Idx (Sl)
                 or else S.Slots (M.First_Child.Value).M.Prev_Sibling.Present
               then
                  Code := 6;
                  return;
               end if;
            end if;

            if M.Next_Sibling.Present then
               if M.Next_Sibling.Value >= Nslots
                 or else not S.Slots (M.Next_Sibling.Value).Used
                 or else S.Slots (M.Next_Sibling.Value).M.Prev_Sibling /= Some_Idx (Sl)
                 or else S.Slots (M.Next_Sibling.Value).M.Parent /= M.Parent
               then
                  Code := 5;
                  return;
               end if;
            end if;

            if M.Prev_Sibling.Present then
               if M.Prev_Sibling.Value >= Nslots
                 or else not S.Slots (M.Prev_Sibling.Value).Used
                 or else S.Slots (M.Prev_Sibling.Value).M.Next_Sibling /= Some_Idx (Sl)
               then
                  Code := 5;
                  return;
               end if;
            end if;
         end if;
      end loop;

      --  (7): keine Zyklen in der Eltern-Kette.
      for Sl in Slot_Range loop
         if S.Slots (Sl).Used then
            Pp    := S.Slots (Sl).M.Parent;
            Steps := 0;
            while Pp.Present loop
               pragma Loop_Invariant (Steps <= Limit);
               pragma Loop_Variant (Increases => Steps);
               Steps := Steps + 1;
               if Steps > Limit then
                  Code := 7;
                  return;
               end if;
               --  [F14] Rust: `p = self.slots[pi].mdb.parent;` -- roher Kettenindex.
               Pp := S.Slots (Pp.Value).M.Parent;
            end loop;
         end if;
      end loop;

      Code := 0;
   end Audit_Cdt;

   procedure Inspect
     (S        : Cap_Space;
      P        : Cap_Ptr;
      Kind     : out Object_Kind;
      Rights   : out Rights_T;
      Badge    : out U64;
      Refcount : out U32;
      Kids     : out Idx;
      Valid    : out Boolean)
   is
      Slot : Idx;
      Err  : Cap_Error;
      O    : Idx;
      N    : Idx;
      Good : Boolean;
   begin
      Kind := Kind_Empty;
      Rights := Rights_None; Badge := 0; Refcount := 0; Kids := 0;

      Resolve (S, P, Slot, Err);
      if Err /= E_Ok then
         Valid := False;
         return;
      end if;
      --  [F15] Rust: `let obj = self.slots[slot].object;` dann `self.objects[obj]`.
      O := S.Slots (Slot).Object;
      Child_Count (S, Slot, N, Good);
      if not Good then
         Valid := False;
         return;
      end if;
      Kind     := S.Objects (O).Kind;
      Rights   := S.Slots (Slot).Rights;
      Badge    := S.Slots (Slot).Badge;
      Refcount := S.Objects (O).Refcount;
      Kids     := N;
      Valid    := True;
   end Inspect;

end Caprock_Cap;
