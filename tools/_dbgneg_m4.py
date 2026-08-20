"""M4: `debug_stop` weist den zweiten Halter nicht mehr ab."""
import pathlib
p = pathlib.Path("crates/caprock-sched/src/lib.rs"); s = p.read_text()
i = s.index("pub fn debug_stop(&mut self, tid: ThreadId) -> bool {")
j = s.index("pub fn debug_continue")
alt = "if self.tcbs[s].reasons.has(BlockReasons::DEBUG) {\n            return false;\n        }"
teil = s[i:j]
assert alt in teil, "Anker fehlt"
p.write_text(s[:i] + teil.replace(alt, "", 1) + s[j:])
