--  Ausfuehrbare Gegenprobe zu Fund [F13] in `audit_cdt`.
--
--  DIE AUSSAGE: derselbe kaputte Verkettungswert (`first_child = 999`, ausserhalb der
--  Tabelle) wird EINMAL korrekt als Code 6 gemeldet und EINMAL nicht -- er reisst statt
--  dessen einen Indexfehler. Was sich zwischen beiden Faellen aendert, ist NUR die
--  Position des tragenden Slots relativ zum Kind. Damit ist die Ursache die
--  Reihenfolge der Pruefungen und nicht die Verfaelschung.
--
--  Drei Faelle, damit die Probe sprechfaehig ist:
--    1  Positivkontrolle -- sauberer CDT, muss 0 melden.
--    2  Kontrolle        -- Verfaelschung am NIEDRIGEREN Index, muss 6 melden.
--    3  Der Fund         -- dieselbe Verfaelschung am HOEHEREN Index.

with Ada.Text_IO; use Ada.Text_IO;
with Caprock_Cap;  use Caprock_Cap;

procedure F13_Gegenprobe is

   Bad : constant Idx := 999;   --  ausserhalb der Slot-Tabelle (Max_Slots = 256)

   --  Ein Space, in dem die Refcount-Pruefung (Codes 1..3) BESTEHT -- sonst kehrt
   --  `audit_cdt` zurueck, bevor der CDT-Durchlauf ueberhaupt beginnt, und die Probe
   --  wuerde etwas anderes messen als sie behauptet.
   function Basis return Cap_Space is
      S : Cap_Space;
   begin
      --  Zwei belegte Slots auf EIN Objekt -> refcount 2.
      S.Slots (0) := (Used => True, Gen => 0, Object => 0, Rights => 0, Badge => 0,
                      M => Mdb_Empty);
      S.Slots (1) := (Used => True, Gen => 0, Object => 0, Rights => 0, Badge => 0,
                      M => Mdb_Empty);
      S.Objects (0) := (Used => True, Kind => Kind_Empty, Refcount => 2, Gen => 0);
      return S;
   end Basis;

   procedure Melde (Name : String; S : Cap_Space) is
      Refs : Ref_Counts;
      Code : U32;
   begin
      Audit_Cdt (S, Refs, Code);
      Put_Line (Name & ": audit_cdt = " & Code'Image);
   exception
      when Constraint_Error =>
         Put_Line (Name & ": CONSTRAINT_ERROR (Indexpruefung) -- kein Urteil");
   end Melde;

   S1, S2, S3 : Cap_Space;
begin
   ------------------------------------------------------------------
   --  1  Positivkontrolle: sauberer CDT (0 ist Kind von 1).
   ------------------------------------------------------------------
   S1 := Basis;
   S1.Slots (1).M.First_Child  := Some_Idx (0);
   S1.Slots (0).M.Parent       := Some_Idx (1);
   Melde ("1 Positivkontrolle (sauber)      ", S1);

   ------------------------------------------------------------------
   --  2  Kontrolle: kaputter first_child am NIEDRIGEREN Index (Slot 0),
   --     das Kind haengt an Slot 1.
   ------------------------------------------------------------------
   S2 := Basis;
   S2.Slots (0).M.First_Child  := Some_Idx (Bad);
   S2.Slots (1).M.Parent       := Some_Idx (0);
   Melde ("2 Kontrolle (kaputt bei Slot 0)  ", S2);

   ------------------------------------------------------------------
   --  3  Der Fund: DIESELBE Verfaelschung, nur am HOEHEREN Index (Slot 1),
   --     das Kind haengt an Slot 0.
   ------------------------------------------------------------------
   S3 := Basis;
   S3.Slots (1).M.First_Child  := Some_Idx (Bad);
   S3.Slots (0).M.Parent       := Some_Idx (1);
   Melde ("3 Fund      (kaputt bei Slot 1)  ", S3);
end F13_Gegenprobe;
