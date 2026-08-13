package body S2_Hal_Stubs is

   use type Caprock_Sched.U64;

   function Init_Thread_Frame
     (Stack_Top : Caprock_Sched.U64;
      Entry_Pt  : Caprock_Sched.U64;
      Arg       : Caprock_Sched.U64;
      El0       : Boolean;
      El0_Sp    : Caprock_Sched.U64) return Caprock_Sched.U64
   is
      pragma Unreferenced (Entry_Pt, Arg, El0, El0_Sp);
   begin
      --  Der Vertrag ist `Ergebnis <= Stack_Top`; ein Trap-Frame liegt am oberen Ende.
      return (if Stack_Top >= 256 then Stack_Top - 256 else 0);
   end Init_Thread_Frame;

   function Stapeladresse return Caprock_Sched.U64 is
      Anker : constant Integer := 0;
   begin
      --  Fuer die Gegenprobe genuegt ein Wert; die D15-Aussage haengt an der echten
      --  Stack-Adresse und ist hier nicht der Gegenstand.
      return Caprock_Sched.U64 (Anker);
   end Stapeladresse;

end S2_Hal_Stubs;
