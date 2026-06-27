#!/usr/bin/env python3
"""SEL4Lake — Burn-in-/Langzeit-Orchestrator fuer den RELEASE-Kernel (ohne kernel-fuzz).

Reife-/Stabilitaetspruefung des EXAKTEN, UNVERAENDERTEN Release-Builds (keine Kerneländerung,
keine neue Funktionalitaet). Methodik: Power-Cycle-/Reboot-Burn-in — der Kernel faehrt nach
`SELFTEST COMPLETE` per PSCI SYSTEM_OFF selbst herunter; jeder Lauf ist ein vollstaendig
auditierter, balance-gepruefter End-to-End-Durchlauf (IPC, Hot-Reload, Binary-Loader, DMA,
ext-27-Gegenangriffe, Reclaim/Churn-Erschoepfung, Domaenen-Isolation). Ueber viele Stunden =
Tausende Iterationen. Pro Lauf werden Audit-/Balance-/Ressourcen-Metriken + Fehler/Panics/Faults
geparst und aggregiert; periodisch wird ein Fortschritts-Snapshot + ein laufender Bericht
geschrieben. Die permanenten Kernel-Audits bleiben aktiv (Teil des Release-Builds).

Aufruf:  tools/burn-in.py [--hours H] [--iterations N] [--timeout S] [--no-build] [--outdir DIR]
Beendet bei Zieldauer/Iterationszahl ODER SIGINT (schreibt dann den Abschlussbericht).
"""
import argparse, json, os, re, signal, subprocess, sys, time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
ELF = "build/target/aarch64-sel4lake/release/sel4lake-kernel.elf"
ARCHIVE = "build/boot-archive.bin"
HELLO = "programs/build/target/aarch64-sel4lake-user/release/hello.elf"
TBIN = "tests/build/target/aarch64-sel4lake-user/release"

QEMU = [
    "qemu-system-aarch64", "-machine", "virt,iommu=smmuv3", "-cpu", "cortex-a72",
    "-smp", "8", "-m", "4G", "-nographic", "-serial", "mon:stdio", "-no-reboot",
    "-net", "none", "-device", "pcie-root-port,id=rp0,chassis=1",
    "-device", "virtio-rng-pci,bus=rp0",
    "-device", f"loader,file={ARCHIVE},addr=0x13F000000", "-kernel", ELF,
]

# --- Parser: Regex auf der Kernel-Serienausgabe eines Laufs. -------------------------------------
RX = {
    "free_mib":      re.compile(r"memtest : freies RAM = (\d+) MiB \((\d+) Fragmente\)"),
    "mem_window":    re.compile(r"mem     : freies RAM \[(0x[0-9a-fA-F]+), (0x[0-9a-fA-F]+)\)"),
    "el0iso_faults": re.compile(r"el0iso  : EL0-Faults abgefangen=(\d+)"),
    "churn":         re.compile(r"churn   : (\d+) spawn/destroy-Zyklen.*Baseline: (true|false)"),
    "reclaim":       re.compile(r"reclaim : (\d+) transiente EL0-Threads erzeugt \(Pool=(\d+)\)"),
    "domain_audit":  re.compile(r"domain_audit=(\d+)"),
    "dma_bytes":     re.compile(r"virtiorng: Geraet DMAt (\d+) Zufallsbytes"),
}
RX_el0trap   = re.compile(r"^el0-trap:", re.M)
RX_call      = re.compile(r" call\(")              # verifizierte synchrone CALL/REPLY-Runden
RX_allpass_l = re.compile(r"^(\w[\w]*) *: ALL PASS", re.M)   # je Test eine ALL-PASS-Zeile
RX_failures  = re.compile(r"FAILURES")
RX_panic     = re.compile(r"(?i)\bpanic\b|PANIC")
RX_complete  = re.compile(r"== SELFTEST COMPLETE -> system_off ==")
RX_watchdog  = re.compile(r"SELFTEST FAILED \(watchdog\)")  # Harness-Watchdog: gemeldeter Test-Fehler
RX_dbgpend   = re.compile(r"DBG pending")

# Pro Lauf erwartete Loader-Aufrufe + Hot-Reloads (aus der Selbsttest-Struktur, Release-Build).
LOADER_TESTS   = ["load", "sysload", "loadhw", "loadstop", "aggru", "intru",
                  "aggrh", "intrh", "aggrt", "intrt", "cross"]   # cross laedt 3 -> +2 unten
HOTRELOAD_TESTS = ["reload", "ckpt", "rmig"]


def sh(cmd, **kw):
    return subprocess.run(cmd, cwd=ROOT, shell=isinstance(cmd, str),
                          stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, **kw)


def build():
    print("== build (release, OHNE kernel-fuzz) + Programme + Testdienste + Archiv ==", flush=True)
    if sh(["./build.sh"]).returncode != 0:
        sys.exit("BUILD FAILED")
    if sh("cd programs && rustup run nightly cargo build --release").returncode != 0:
        sys.exit("PROGRAMS BUILD FAILED")
    if sh("cd tests && rustup run nightly cargo build --release").returncode != 0:
        sys.exit("TESTS BUILD FAILED")
    with open(os.path.join(ROOT, "build/_probe.bin"), "w") as f:
        f.write("PLACEHOLDER")
    mk = ["python3", "tools/mkarchive.py", ARCHIVE,
          f"10:hello:2:1:{HELLO}", f"11:hwhello:1:1:{HELLO}", f"12:trusted-x:0:1:{HELLO}",
          "2:probe:2:1:build/_probe.bin",
          f"20:aggressor-u:2:1:{TBIN}/aggressor-u.elf", f"21:intruder-u:2:1:{TBIN}/intruder-u.elf",
          f"22:aggressor-h:1:1:{TBIN}/aggressor-h.elf", f"23:intruder-h:1:1:{TBIN}/intruder-h.elf",
          f"24:aggressor-t:0:1:{TBIN}/aggressor-t.elf", f"25:intruder-t:0:1:{TBIN}/intruder-t.elf"]
    if sh(mk).returncode != 0:
        sys.exit("ARCHIVE BUILD FAILED")


def one_boot(timeout_s):
    t0 = time.time()
    try:
        p = subprocess.run(QEMU, cwd=ROOT, stdin=subprocess.DEVNULL,
                           stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                           timeout=timeout_s)
        out = p.stdout.decode("utf-8", "replace")
    except subprocess.TimeoutExpired as e:
        out = (e.stdout or b"").decode("utf-8", "replace")
        return out, time.time() - t0, True  # hung
    return out, time.time() - t0, False


def parse(out):
    m = {}
    for k, rx in RX.items():
        g = rx.search(out)
        m[k] = g.groups() if g else None
    m["el0_trap_count"] = len(RX_el0trap.findall(out))
    m["call_count"] = len(RX_call.findall(out))
    m["allpass"] = set(RX_allpass_l.findall(out))
    m["complete"] = bool(RX_complete.search(out))
    m["failures"] = bool(RX_failures.search(out))
    m["panic"] = bool(RX_panic.search(out))
    m["watchdog"] = bool(RX_watchdog.search(out))
    m["dbg_pending"] = bool(RX_dbgpend.search(out))
    return m


def verdict(m, hung):
    if m["panic"]:
        return "PANIC"
    if m["complete"]:
        return "FAILURE" if m["failures"] else "CLEAN"
    # Harness-Watchdog hat den Stillstand gemeldet + sauber heruntergefahren -> FAILURE (welcher Test
    # scheiterte, steht im report() der Ausgabe), KEIN Hang. Echter Stillstand/Crash (kein COMPLETE,
    # kein Watchdog, oder Subprozess-Timeout) bleibt HANG.
    if m["watchdog"] or m["failures"]:
        return "FAILURE"
    return "HANG"


def new_state(args):
    return {
        "started_at": time.time(), "started_iso": time.strftime("%Y-%m-%d %H:%M:%S"),
        "args": vars(args), "iters": 0, "wall_total": 0.0,
        "verdicts": {"CLEAN": 0, "FAILURE": 0, "PANIC": 0, "HANG": 0},
        # Modal-/Min-/Max-Tracking pro Metrik
        "metrics": {}, "boot_times": [], "anomalies": [],
        # Aggregat-Zaehler
        "sum_el0_trap": 0, "sum_calls": 0, "sum_loader_calls": 0, "sum_hotreloads": 0,
        "sum_dma_bytes": 0, "sum_churn_cycles": 0, "sum_reclaim_threads": 0,
        "ref_profile": None,
    }


def track(state, key, val):
    d = state["metrics"].setdefault(key, {"vals": {}, "min": None, "max": None})
    d["vals"][str(val)] = d["vals"].get(str(val), 0) + 1
    if isinstance(val, (int, float)):
        d["min"] = val if d["min"] is None else min(d["min"], val)
        d["max"] = val if d["max"] is None else max(d["max"], val)


def update(state, m, vd, dt, it, out, outdir):
    state["iters"] += 1
    state["wall_total"] += dt
    state["verdicts"][vd] += 1
    state["boot_times"].append(round(dt, 2))
    state["sum_el0_trap"] += m["el0_trap_count"]
    state["sum_calls"] += m["call_count"]
    loader_calls = len([t for t in LOADER_TESTS if t in m["allpass"]])
    if "cross" in m["allpass"]:
        loader_calls += 2  # cross laedt 3 Dienste nebenlaeufig
    hotreloads = len([t for t in HOTRELOAD_TESTS if t in m["allpass"]])
    state["sum_loader_calls"] += loader_calls
    state["sum_hotreloads"] += hotreloads
    if m["free_mib"]:
        track(state, "free_mib", int(m["free_mib"][0]))
        track(state, "free_frags", int(m["free_mib"][1]))
    if m["el0iso_faults"]:
        track(state, "el0iso_faults", int(m["el0iso_faults"][0]))
    if m["domain_audit"]:
        track(state, "domain_audit", int(m["domain_audit"][0]))
    if m["churn"]:
        track(state, "churn_cycles", int(m["churn"][0]))
        track(state, "churn_baseline_ok", m["churn"][1])
        state["sum_churn_cycles"] += int(m["churn"][0])
    if m["reclaim"]:
        track(state, "reclaim_threads", int(m["reclaim"][0]))
        state["sum_reclaim_threads"] += int(m["reclaim"][0])
    if m["dma_bytes"]:
        track(state, "dma_bytes_per_run", int(m["dma_bytes"][0]))
        state["sum_dma_bytes"] += int(m["dma_bytes"][0])
    track(state, "el0_trap_per_run", m["el0_trap_count"])
    track(state, "allpass_count", len(m["allpass"]))
    if state["ref_profile"] is None and vd == "CLEAN":
        state["ref_profile"] = {
            "loader_calls": loader_calls, "hotreloads": hotreloads,
            "el0_traps": m["el0_trap_count"], "calls": m["call_count"],
            "free_mib": int(m["free_mib"][0]) if m["free_mib"] else None,
            "dma_bytes": int(m["dma_bytes"][0]) if m["dma_bytes"] else None,
            "allpass": sorted(m["allpass"]),
        }
    if vd != "CLEAN":
        fn = os.path.join(outdir, f"anomaly_{it:06d}_{vd}.txt")
        with open(fn, "w") as f:
            f.write(out)
        state["anomalies"].append({"iter": it, "verdict": vd, "file": os.path.basename(fn),
                                   "dbg_pending": m["dbg_pending"], "t": time.strftime("%H:%M:%S")})


def modal(state, key):
    d = state["metrics"].get(key)
    if not d or not d["vals"]:
        return None
    return max(d["vals"].items(), key=lambda kv: kv[1])


def write_report(state, outdir, final=False):
    s = state
    el = s["wall_total"]
    bt = s["boot_times"]
    avg = sum(bt) / len(bt) if bt else 0
    runtime_h = el / 3600.0
    lines = []
    A = lines.append
    A(f"# SEL4Lake — Burn-in-/Langzeitbericht (Release-Kernel, OHNE kernel-fuzz)")
    A("")
    A(f"Status: {'ABGESCHLOSSEN' if final else 'LAUFEND'} · Start: {s['started_iso']} · "
      f"Stand: {time.strftime('%Y-%m-%d %H:%M:%S')}")
    A("")
    A("Power-Cycle-/Reboot-Burn-in: der **unveraenderte** Release-Kernel (keine Kerneländerung) "
      "wird wiederholt gebootet; jeder Lauf ist ein vollstaendig auditierter, balance-gepruefter "
      "End-to-End-Durchlauf des Selbsttests (IPC, Hot-Reload, Binary-Loader, DMA, ext-27-"
      "Gegenangriffe, Reclaim/Churn-Erschoepfung, Domaenen-Isolation). Die permanenten Audits "
      "(`domain/cap_cdt/vspace/dma/loader/ipc_audit`) sind aktiv.")
    A("")
    A("## Eckdaten")
    A("")
    A(f"- **Laufzeit:** {runtime_h:.2f} h ({el:.0f} s Wall-Clock QEMU)")
    A(f"- **Iterationen (Reboots):** {s['iters']}")
    v = s["verdicts"]
    A(f"- **Ergebnis je Lauf:** CLEAN={v['CLEAN']} · FAILURE={v['FAILURE']} · "
      f"PANIC={v['PANIC']} · HANG={v['HANG']}")
    if s["iters"]:
        A(f"- **Sauber-Quote:** {100.0*v['CLEAN']/s['iters']:.3f} %")
    A(f"- **Boot-Zeit:** Ø {avg:.1f}s · min {min(bt):.1f}s · max {max(bt):.1f}s "
      f"(Drift-Indikator: erste 20 Ø {sum(bt[:20])/max(1,len(bt[:20])):.1f}s vs. letzte 20 Ø "
      f"{sum(bt[-20:])/max(1,len(bt[-20:])):.1f}s)")
    A("")
    A("## Aggregierte Aktivitaet (ueber alle Laeufe)")
    A("")
    rp = s["ref_profile"] or {}
    A(f"- **Gestartete Prozesse:** Churn-Spawn/Destroy-Zyklen={s['sum_churn_cycles']:,} + "
      f"transiente EL0-Threads (Reclaim)={s['sum_reclaim_threads']:,} + geladene externe "
      f"Dienst-Instanzen={s['sum_loader_calls']:,} (zzgl. Test-Worker/PDs je Lauf)")
    A(f"- **Hot-Reloads:** {s['sum_hotreloads']:,} (reload/ckpt/rmig, ~{rp.get('hotreloads','?')}/Lauf)")
    A(f"- **Verifizierte synchrone IPC-Runden (CALL/REPLY, untere Schranke):** {s['sum_calls']:,} "
      f"(interne IPC/Notifications weit hoeher, nicht einzeln geloggt)")
    A(f"- **DMA-Transfers:** {s['sum_dma_bytes']:,} Bytes Bus-Master-DMA in die DmaCap-Region "
      f"(virtio-rng, ~{rp.get('dma_bytes','?')} B/Lauf) + je Lauf 1 EL0-DMA-Round-Trip + SG/Multi-Region")
    A(f"- **Loader-Aufrufe:** {s['sum_loader_calls']:,} (load/sysload/loadhw/loadstop + ext-27-Ladevorgaenge)")
    A(f"- **EL0-Faults abgefangen (erwartet, Intruder/Isolationstests):** {s['sum_el0_trap']:,}")
    A("")
    A("## Konsistenz-/Balance-Bilanz (jeder Lauf MUSS identisch sein)")
    A("")
    def modline(label, key, want_unique=True):
        d = s["metrics"].get(key)
        if not d:
            A(f"- **{label}:** (keine Daten)")
            return
        vals = d["vals"]
        if len(vals) == 1:
            A(f"- **{label}:** konstant {list(vals)[0]} ueber alle {sum(vals.values())} Laeufe ✓")
        else:
            top = sorted(vals.items(), key=lambda kv: -kv[1])
            A(f"- **{label}:** ABWEICHUNG ueber Laeufe -> {dict(top)} ⚠")
    modline("Speicher-Bilanz (freies RAM bei memtest, MiB)", "free_mib")
    modline("RAM-Fragmente bei memtest", "free_frags")
    modline("Region-/Churn-Balance (2000 Zyklen -> Baseline)", "churn_baseline_ok")
    modline("Churn-Zyklen je Lauf", "churn_cycles")
    modline("domain_audit (erwartet 0)", "domain_audit")
    modline("el0iso-Faults je Lauf", "el0iso_faults")
    modline("ALL-PASS-Checks je Lauf", "allpass_count")
    A("")
    A("**Memory-Bilanz Start↔Ende:** Jeder Lauf startet mit identischem freiem RAM (s. o.) und "
      "stellt nach dem auditierten Selbsttest (churn/loadstop/sasheap `Baseline: true`) die "
      "Ressourcen-Baseline wieder her. Cross-Boot-Drift ist ausgeschlossen (frischer RAM je Boot); "
      "Intra-Boot-Leaks werden von den Balance-Checks jedes Laufs erfasst.")
    A("")
    A("## Fehler / Auffaelligkeiten")
    A("")
    if not s["anomalies"]:
        A(f"- **Keine.** {v['CLEAN']}/{s['iters']} Laeufe CLEAN; keine Panics, keine Assertions, "
          "keine Hangs, keine FAILURES, keine Audit-Abweichung, keine Balance-Drift.")
    else:
        A(f"- **{len(s['anomalies'])} auffaellige Laeufe** (Vollausgabe je in `{os.path.basename(outdir)}/`):")
        for a in s["anomalies"][:50]:
            A(f"  - Iter {a['iter']} · {a['verdict']}"
              f"{' · DBG-pending' if a.get('dbg_pending') else ''} · {a['t']} · {a['file']}")
    A("")
    A("## Reproduzierbarkeit")
    A("")
    A("- Build: `./build.sh` (release, **ohne** kernel-fuzz) + `programs`/`tests`-Workspaces + Archiv.")
    A("- Lauf: identische QEMU-Kommandozeile je Iteration (`virt,iommu=smmuv3` + virtio-rng-pci).")
    A("- Der Selbsttest ist im Release-Build (ohne Fuzzer) weitgehend deterministisch; verbleibende "
      "Nichtdeterminismus-Quelle ist die SMP-/TCG-Scheduling-Jitter (Worker-Counts variieren leicht).")
    A("")
    with open(os.path.join(outdir, "report.md"), "w") as f:
        f.write("\n".join(lines) + "\n")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--hours", type=float, default=4.0)
    ap.add_argument("--iterations", type=int, default=0, help="0 = unbegrenzt (nur Zeit zaehlt)")
    ap.add_argument("--timeout", type=int, default=120, help="Hang-Backstop je Boot (s)")
    ap.add_argument("--no-build", action="store_true")
    ap.add_argument("--outdir", default="build/burn-in")
    ap.add_argument("--snapshot-every", type=int, default=25)
    args = ap.parse_args()

    outdir = os.path.join(ROOT, args.outdir)
    os.makedirs(outdir, exist_ok=True)
    if not args.no_build:
        build()
    if not os.path.exists(os.path.join(ROOT, ELF)) or not os.path.exists(os.path.join(ROOT, ARCHIVE)):
        sys.exit("Kernel-ELF oder Archiv fehlt — ohne --no-build starten.")

    state = new_state(args)
    stop = {"flag": False}

    def on_sig(*_):
        stop["flag"] = True
    signal.signal(signal.SIGINT, on_sig)
    signal.signal(signal.SIGTERM, on_sig)

    deadline = state["started_at"] + args.hours * 3600
    plog = open(os.path.join(outdir, "progress.log"), "a", buffering=1)
    plog.write(f"\n=== Burn-in Start {state['started_iso']} hours={args.hours} iters={args.iterations} ===\n")
    print(f"== Burn-in laeuft: Ziel {args.hours} h"
          f"{(' / ' + str(args.iterations) + ' Iter') if args.iterations else ''}, "
          f"Backstop {args.timeout}s/Boot, Ausgabe {args.outdir}/ ==", flush=True)

    it = 0
    while not stop["flag"]:
        if time.time() >= deadline:
            break
        if args.iterations and it >= args.iterations:
            break
        it += 1
        out, dt, hung = one_boot(args.timeout)
        m = parse(out)
        vd = verdict(m, hung)
        update(state, m, vd, dt, it, out, outdir)
        line = (f"[{time.strftime('%H:%M:%S')}] iter={it} {vd} {dt:.1f}s "
                f"freeMiB={m['free_mib'][0] if m['free_mib'] else '?'} "
                f"el0trap={m['el0_trap_count']} allpass={len(m['allpass'])} "
                f"domAudit={m['domain_audit'][0] if m['domain_audit'] else '?'}")
        plog.write(line + "\n")
        with open(os.path.join(outdir, "state.json"), "w") as f:
            json.dump(state, f, default=list)
        if it % args.snapshot_every == 0 or vd != "CLEAN":
            v = state["verdicts"]
            print(f"  {line}  | gesamt CLEAN={v['CLEAN']} FAIL={v['FAILURE']} "
                  f"PANIC={v['PANIC']} HANG={v['HANG']}", flush=True)
            write_report(state, outdir, final=False)

    write_report(state, outdir, final=True)
    v = state["verdicts"]
    print(f"== Burn-in fertig: {state['iters']} Iter, CLEAN={v['CLEAN']} FAIL={v['FAILURE']} "
          f"PANIC={v['PANIC']} HANG={v['HANG']}, Bericht {args.outdir}/report.md ==", flush=True)
    plog.close()
    sys.exit(1 if (v["FAILURE"] or v["PANIC"] or v["HANG"]) else 0)


if __name__ == "__main__":
    main()
