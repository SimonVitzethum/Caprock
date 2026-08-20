"""M1: die Praegung VOR die Messung ziehen (s. tools/dbg-negativ.sh)."""
import pathlib
p = pathlib.Path("kernel/src/arch/x86_64/bringup.rs"); s = p.read_text()
alt = "    let keine_autoritaet = !system::any_debug_authority_over(zielpd);"
assert alt in s, "Anker fehlt"
p.write_text(s.replace(alt, "    let _m1 = system::mint_debuggable(zielpd);\n" + alt, 1))
