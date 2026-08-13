--  Ausfuehrbare Gegenprobe zu Fund [S2-F1]/[S2-F8]:
--
--     `Tcb::priority` ist ein `u8` (0 .. 255), `Scheduler::queues` ist ein
--     `[ListHead; NPRIO]` mit NPRIO = 8, und `enqueue_ready` indiziert
--     `self.queues[self.tcbs[local].priority as usize]` ROH.
--
--  Drei Ausgaenge, und zwischen 2 und 3 wandert AUSSCHLIESSLICH die Prioritaet --
--  damit isoliert die Probe die Ursache (Wertebereich) von allem anderen.
--
--     1 Positivkontrolle (prio 0) : zugelassen
--     2 Kontrolle        (prio 7) : zugelassen   <- letzte gueltige Stufe
--     3 Fund             (prio 8) : CONSTRAINT_ERROR
--
--  In Rust ist Ausgang 3 ein `panic!("index out of bounds")`, und `panic = "abort"`
--  steht in BEIDEN Profilen -- der Knoten stirbt.

with Ada.Text_IO;   use Ada.Text_IO;
with Caprock_Sched; use Caprock_Sched;
with S2_Hal_Stubs;  pragma Unreferenced (S2_Hal_Stubs);   --  nur fuer die zwei Symbole

procedure S2_Prio_Gegenprobe is

   S : Scheduler;

   procedure Versuch (Nr : String; Prio : U8) is
      P  : Parked;
      Ok : Boolean;
   begin
      Spawn_Parked (S, 0, 16#1000#, 0, 16#20_0000#, 16#1000#, Prio, P);
      if P = null then
         Put_Line (Nr & ": kein Slot -- die Probe ist nicht sprechfaehig");
         return;
      end if;
      Admit (S, P, Ok);
      Put_Line (Nr & ": zugelassen=" & Boolean'Image (Ok));
   exception
      when Constraint_Error =>
         Put_Line (Nr & ": CONSTRAINT_ERROR (Indexpruefung) -- kein Einreihen");
   end Versuch;

   T   : Thread_Id;
   Ok0 : Boolean;

begin
   Attach_Directory;
   Init_Core (S, 0, 0, T, Ok0);
   if not Ok0 then
      Put_Line ("Init_Core fehlgeschlagen -- die Probe ist nicht sprechfaehig");
      return;
   end if;

   Versuch ("1 Positivkontrolle (prio 0)", 0);
   Versuch ("2 Kontrolle        (prio 7)", 7);
   Versuch ("3 Fund             (prio 8)", 8);
end S2_Prio_Gegenprobe;
