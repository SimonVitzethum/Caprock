"""M5b: BEIDE Ringwort-Gatter weg -- Politik und HAL."""
import pathlib
r = pathlib.Path("crates/caprock-sched/src/redirect.rs"); s = r.read_text()
alt = "    if ix.ring.contains(&i) {"
assert alt in s, "Politik-Anker fehlt"
r.write_text(s.replace(alt, "    if false {", 1))
e = pathlib.Path("crates/caprock-hal/src/x86_64/exception.rs"); s = e.read_text()
alt = "        15 | 16 | 18 | 21 => false,"
assert alt in s, "HAL-Anker fehlt"
neu = "        15 | 16 => false,\n        18 => { f.cs = v; true }\n        21 => { f.ss = v; true }"
e.write_text(s.replace(alt, neu, 1))
