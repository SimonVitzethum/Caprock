"""M5c: ALLE DREI Ringwort-Schichten weg.

Die dritte ist die Vorgabe-Absage: `writeback_erlaubt` ordnet Wort 18 keiner schreibbaren Klasse
zu und weist es deshalb als `UeberDerStufe` ab, auch wenn Politik- und HAL-Gatter fehlen. Erst wenn
18 als Flagregister GILT, kann der Schreibversuch durchgehen -- und genau dann muss die
Ausgangs-Zeile fallen. Ohne M5c waere sie eine Zeile, von der niemand weiss, ob sie ueberhaupt
fallen KANN.
"""
import pathlib
r = pathlib.Path("crates/caprock-sched/src/redirect.rs"); s = r.read_text()
a1 = "    if ix.ring.contains(&i) {"
a2 = "    ring: &[15, 16, 18, 21],"
a3 = "    flags: Some(19),"
for a in (a1, a2, a3):
    assert a in s, f"Anker fehlt: {a}"
s = s.replace(a1, "    if false {", 1)
s = s.replace(a2, "    ring: &[15, 16, 21],", 1)
s = s.replace(a3, "    flags: Some(18),", 1)
r.write_text(s)

e = pathlib.Path("crates/caprock-hal/src/x86_64/exception.rs"); s = e.read_text()
a = "        15 | 16 | 18 | 21 => false,"
assert a in s, "HAL-Anker fehlt"
s = s.replace(a, "        15 | 16 | 21 => false,\n        18 => { f.cs = v; true }", 1)
e.write_text(s)

# **Und der geschriebene WERT muss harmlos sein.**
#
# Die erste Fassung liess die Sonde `0x33` schreiben -- den User-Selektor -- in den Frame eines
# KERNEL-Threads. Der Schreibversuch ging durch (das war ja der Zweck), der Thread kehrte danach
# nach Ring 3 mit einer Kernel-RIP zurueck, und der Lauf starb, bevor die Zeile gedruckt wurde.
# Die Wache meldete korrekt „Konjunkt kommt im Protokoll gar nicht vor".
#
# Das ist ein Beleg fuer die Wirksamkeit des Gatters und **kein Beleg dafuer, dass die Zeile fallen
# KANN** -- und genau das soll M5c zeigen. Geschrieben wird deshalb `KERNEL_CS`, also der Wert, der
# ohnehin dort steht: die Pruefung faellt, die Maschine nicht. Eine Gegenprobe muss ISOLIEREN,
# sonst misst sie die Reihenfolge der Katastrophen.
b = pathlib.Path("kernel/src/arch/x86_64/bringup.rs"); s = b.read_text()
a = "system::debug_write_reg(0, ctrl, raw, 18, 0x33)"
assert a in s, "Sonden-Anker fehlt"
b.write_text(s.replace(a, "system::debug_write_reg(0, ctrl, raw, 18, 0x08)", 1))
