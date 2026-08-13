package body S2_Parked_Probe with SPARK_Mode => On is

   procedure Aufsetzen (S : in out Caprock_Sched.Scheduler; Ok : out Boolean) is
      T : Caprock_Sched.Thread_Id;
   begin
      Caprock_Sched.Attach_Directory;
      Caprock_Sched.Init_Core (S, 0, 0, T, Ok);
   end Aufsetzen;

   procedure Richtig (S : in out Caprock_Sched.Scheduler; Ok : out Boolean) is
      P : Caprock_Sched.Parked;
   begin
      Caprock_Sched.Spawn_Parked (S, 0, 16#1000#, 0, 16#20_0000#, 16#1000#, 3, P);
      if P = null then
         Ok := False;
         return;
      end if;
      Caprock_Sched.Admit (S, P, Ok);   --  <== die eine Zeile
   end Richtig;

   procedure Fallengelassen (S : in out Caprock_Sched.Scheduler; Ok : out Boolean) is
      P : Caprock_Sched.Parked;
   begin
      Caprock_Sched.Spawn_Parked (S, 0, 16#1000#, 0, 16#20_0000#, 16#1000#, 3, P);
      Ok := P /= null;
      --  Kein `Admit`. In Rust: `#[must_use]`-Warnung, sonst nichts.
   end Fallengelassen;

end S2_Parked_Probe;
