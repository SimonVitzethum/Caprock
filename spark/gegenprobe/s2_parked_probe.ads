--  Gegenprobe zur Frage: **kann SPARK die lineare Zusage von Rusts `Parked` besser
--  ausdruecken?**
--
--  Rusts `Parked` (kernel/src/system.rs:8555) traegt `#[must_use]`, kein `Drop` und kein
--  oeffentliches Feld. Wer ihn fallen laesst, bekommt eine WARNUNG (abschaltbar mit
--  `#[allow(unused_must_use)]`, und `let _ = p;` schweigt ohnehin) -- der Thread laeuft
--  danach nie, und nichts bricht.
--
--  Hier sind es zwei Unterprogramme, die sich in GENAU EINER Zeile unterscheiden.
--  GNATprove entscheidet zwischen ihnen:
--
--     1 Positivkontrolle : `Admit` verbraucht den Parked  -> kein Befund
--     2 Fund             : der Parked faellt aus dem Bereich -> „resource or memory leak"
--
--  Diese Datei liegt NICHT in `caprock.gpr` -- sonst zaehlte der absichtliche Fund in die
--  Bilanz des Moduls.

with Caprock_Sched;

package S2_Parked_Probe with SPARK_Mode => On is

   use type Caprock_Sched.Parked;

   --  Rust: `attach_directory` -- damit die Probe sprechfaehig ist.
   procedure Aufsetzen (S : in out Caprock_Sched.Scheduler; Ok : out Boolean)
     with Global => (In_Out => Caprock_Sched.Load_State,
                     Output => (Caprock_Sched.Dir_State, Caprock_Sched.Gid_State)),
          Always_Terminates;

   --  1 Positivkontrolle: der Besitz geht an `Admit` ueber.
   procedure Richtig (S : in out Caprock_Sched.Scheduler; Ok : out Boolean)
     with Global => (In_Out => (Caprock_Sched.Dir_State,
                                Caprock_Sched.Gid_State,
                                Caprock_Sched.Load_State)),
          Always_Terminates;

   --  2 Fund: derselbe Ablauf OHNE `Admit`. In Rust ist das eine Warnung.
   procedure Fallengelassen (S : in out Caprock_Sched.Scheduler; Ok : out Boolean)
     with Global => (In_Out => (Caprock_Sched.Dir_State,
                                Caprock_Sched.Gid_State,
                                Caprock_Sched.Load_State)),
          Always_Terminates;

end S2_Parked_Probe;
