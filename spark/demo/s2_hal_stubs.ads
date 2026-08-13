--  Stubs fuer die zwei HAL-Aufrufe der Scheduler-Portierung (Portierungsregel 8).
--
--  Sie stehen NUR in der ausfuehrbaren Gegenprobe -- der Beweislauf sieht sie nicht
--  (`caprock_sched.gpr` fuehrt nur die beiden Quelldateien der Portierung), er arbeitet
--  mit dem Vertrag.
--
--  **Warum Ada und nicht C:** `Caprock_Sched.U64` ist `range 0 .. 2**64 - 1` und damit auf
--  dieser Maschine ein 128-Bit-Typ (`Long_Long_Long_Integer`) -- ein C-Stub mit `uint64_t`
--  gibt sein Ergebnis in EINEM Register zurueck, die Ada-Seite liest zwei. Die erste
--  Fassung dieser Probe tat genau das und meldete in ALLEN DREI Ausgaengen
--  CONSTRAINT_ERROR, also auch in der Positivkontrolle -- daran ist sie aufgefallen.

with Caprock_Sched;

package S2_Hal_Stubs is

   function Init_Thread_Frame
     (Stack_Top : Caprock_Sched.U64;
      Entry_Pt  : Caprock_Sched.U64;
      Arg       : Caprock_Sched.U64;
      El0       : Boolean;
      El0_Sp    : Caprock_Sched.U64) return Caprock_Sched.U64
     with Export, Convention => Ada, External_Name => "caprock_init_thread_frame";

   function Stapeladresse return Caprock_Sched.U64
     with Export, Convention => Ada, External_Name => "caprock_stapeladresse";

end S2_Hal_Stubs;
