#!/usr/bin/env python3
"""SEL4Lake — TrustedSAS-Zertifikat erzeugen + signieren (ext-28, ADR 0014).

Die vollstaendige Vertrauenskette host-seitig:
  1. UNSAFE-AUDIT des gesamten App-Dependency-Baums (cargo metadata): das Programm-Crate muss
     `#![forbid(unsafe_code)]` haben (+ 0 unsafe), ALLE projektinternen Crates 0 unsafe; `unsafe` ist
     NUR in der Allowlist erlaubt (genau `libsel4lake`, die Syscall-ABI). Jede Verletzung -> Abbruch,
     KEIN Zertifikat. Ein Audit-Bericht listet die unsafe-Anzahl je Crate; sein SHA-256 wird im
     Zertifikat verankert. (Sysroot core/alloc/compiler_builtins = vertraute Sprach-Laufzeit, ausser
     Scope -- erscheint nicht in cargo metadata.)
  2. SHA-256(ELF) + SHA-256(Manifest).
  3. Zertifikatsnachricht (eingefrorenes Format, s. sel4lake-loader::cert) fuellen -- inkl.
     unsafe_status, unsafe_audit_hash, build_info, Algorithmus-/Policy-IDs + Verfahrens-Versionen.
  4. Die GESAMTE Nachricht mit dem PRIVATEN Ed25519-Schluessel signieren.

Ohne den privaten Schluessel entsteht kein gueltiges Zertifikat.

Aufruf:
  tools/sign_trusted.py --crate <prog-crate-dir> --elf <elf> [--manifest <blob>]
                        --program-id N --version N [--policy internal-test]
                        --key keys/trusted-test.ed25519 --out <cert.bin>
"""
import argparse
import hashlib
import os
import re
import struct
import subprocess
import sys

from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
from cryptography.hazmat.primitives import serialization

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

# Muss EXAKT zu crates/sel4lake-loader/src/cert.rs passen (eingefrorenes Format).
MAGIC = 0x5453_4331
CERT_FORMAT_VERSION = 1
SIG_FORMAT_VERSION = 1
SIG_ALG_ED25519 = 1
BUILD_RULES_VERSION = 1
AUDIT_PROTOCOL_VERSION = 1
UNSAFE_RULES_VERSION = 1
ALLOWLIST_RULES_VERSION = 1
HEADER_LEN = 152

UNSAFE_PROGRAM_FORBID = 1 << 0
UNSAFE_PROJECT_CLEAN = 1 << 1
UNSAFE_ALLOWLIST_OK = 1 << 2
UNSAFE_ALL_PASS = UNSAFE_PROGRAM_FORBID | UNSAFE_PROJECT_CLEAN | UNSAFE_ALLOWLIST_OK

POLICIES = {"trustedsas-v1": 1, "formal": 2, "internal-test": 3, "production": 4}

# Genau eine Crate darf `unsafe` enthalten: die Syscall-ABI.
ALLOWLIST = {"libsel4lake"}

# Reale unsafe-Nutzung (nicht jedes Vorkommen des Worts in Kommentaren/Strings): unsafe vor
# fn/impl/trait/extern/Block.
RX_UNSAFE = re.compile(r"\bunsafe\s+(fn|impl|trait|extern|\{)")
RX_FORBID = re.compile(r"#!\[\s*forbid\s*\(\s*unsafe_code\s*\)\s*\]")


def sh_json(args, cwd):
    import json
    out = subprocess.run(args, cwd=cwd, capture_output=True, text=True)
    if out.returncode != 0:
        sys.exit(f"cargo metadata fehlgeschlagen:\n{out.stderr}")
    return json.loads(out.stdout)


def count_unsafe(src_dir):
    n = 0
    for root, _, files in os.walk(src_dir):
        for fn in files:
            if fn.endswith(".rs"):
                try:
                    txt = open(os.path.join(root, fn), encoding="utf-8", errors="replace").read()
                except OSError:
                    continue
                n += len(RX_UNSAFE.findall(txt))
    return n


def has_forbid(src_dir):
    for root, _, files in os.walk(src_dir):
        for fn in files:
            if fn in ("lib.rs", "main.rs"):
                txt = open(os.path.join(root, fn), encoding="utf-8", errors="replace").read()
                if RX_FORBID.search(txt):
                    return True
    return False


def audit(crate_dir):
    """Unsafe-Audit ueber den transitiven App-Dep-Baum. Gibt (status, report, root_name)."""
    meta = sh_json(["cargo", "metadata", "--format-version", "1",
                    "--manifest-path", os.path.join(crate_dir, "Cargo.toml")], ROOT)
    pkgs = {p["id"]: p for p in meta["packages"]}
    nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
    root_id = meta["resolve"].get("root")
    if root_id is None:
        # Workspace ohne eindeutige root: Crate ueber den manifest_path finden.
        target = os.path.abspath(os.path.join(crate_dir, "Cargo.toml"))
        for pid, p in pkgs.items():
            if os.path.abspath(p["manifest_path"]) == target:
                root_id = pid
                break
    if root_id is None:
        sys.exit("Programm-Crate in cargo metadata nicht gefunden")
    # Transitive Dep-Menge (inkl. root).
    seen, stack = set(), [root_id]
    while stack:
        cur = stack.pop()
        if cur in seen:
            continue
        seen.add(cur)
        for dep in nodes.get(cur, {}).get("dependencies", []):
            stack.append(dep)
    root_name = pkgs[root_id]["name"]
    lines, violations = [], []
    prog_forbid = False
    proj_clean = True
    root_unsafe = 0
    lines.append(f"TrustedSAS Unsafe-Audit (Audit-Protokoll v{AUDIT_PROTOCOL_VERSION}, "
                 f"Allowlist-Regeln v{ALLOWLIST_RULES_VERSION})")
    lines.append(f"Programm: {root_name}")
    lines.append(f"Allowlist: {{{', '.join(sorted(ALLOWLIST))}}}")
    for pid in sorted(seen, key=lambda i: pkgs[i]["name"]):
        p = pkgs[pid]
        name = p["name"]
        src_dir = os.path.dirname(p["manifest_path"])
        cnt = count_unsafe(os.path.join(src_dir, "src"))
        allow = name in ALLOWLIST
        is_root = pid == root_id
        tag = " [ALLOWLIST]" if allow else (" [PROGRAMM]" if is_root else "")
        forbid = ""
        if is_root:
            root_unsafe = cnt
            prog_forbid = has_forbid(src_dir)
            forbid = f" forbid_unsafe_code={'ja' if prog_forbid else 'NEIN'}"
        lines.append(f"  {name}: {cnt} unsafe{tag}{forbid}")
        if cnt > 0 and not allow:
            violations.append(f"{name} ({cnt} unsafe, nicht in Allowlist)")
            proj_clean = False
    status = 0
    if prog_forbid and root_unsafe == 0:
        status |= UNSAFE_PROGRAM_FORBID
    if proj_clean:
        status |= UNSAFE_PROJECT_CLEAN
    if not violations:
        status |= UNSAFE_ALLOWLIST_OK
    ok = status == UNSAFE_ALL_PASS
    lines.append(f"Status: {'ALL_PASS' if ok else 'FAIL'} (0x{status:x})")
    if violations:
        lines.append("Verletzungen: " + "; ".join(violations))
    return status, "\n".join(lines) + "\n", root_name, ok


def build_cert(args, status, audit_hash):
    priv = open(os.path.join(ROOT, args.key), "rb").read()
    sk = Ed25519PrivateKey.from_private_bytes(priv)
    pub = sk.public_key().public_bytes(serialization.Encoding.Raw,
                                        serialization.PublicFormat.Raw)
    key_id = hashlib.sha256(pub).digest()[:16]
    elf = open(os.path.join(ROOT, args.elf), "rb").read()
    binary_hash = hashlib.sha256(elf).digest()
    man = open(os.path.join(ROOT, args.manifest), "rb").read() if args.manifest else b""
    manifest_hash = hashlib.sha256(man).digest()
    policy = POLICIES[args.policy]

    rustc = subprocess.run(["rustc", "--version"], capture_output=True, text=True)
    rustc_ver = rustc.stdout.strip() if rustc.returncode == 0 else "rustc ?"
    build_info = (f"sel4lake-trusted; buildregeln v{BUILD_RULES_VERSION}; "
                  f"target aarch64-sel4lake-user; profile release; {rustc_ver}").encode("utf-8")

    msg = bytearray(HEADER_LEN + len(build_info))
    struct.pack_into("<I", msg, 0, MAGIC)
    struct.pack_into("<HHH", msg, 4, CERT_FORMAT_VERSION, SIG_FORMAT_VERSION, SIG_ALG_ED25519)
    struct.pack_into("<I", msg, 10, policy)
    struct.pack_into("<HHHH", msg, 14, BUILD_RULES_VERSION, AUDIT_PROTOCOL_VERSION,
                     UNSAFE_RULES_VERSION, ALLOWLIST_RULES_VERSION)
    struct.pack_into("<HH", msg, 22, 0, 0)  # flags, reserved
    struct.pack_into("<II", msg, 26, args.program_id, args.version)
    msg[34:66] = binary_hash
    msg[66:98] = manifest_hash
    msg[98:114] = key_id
    struct.pack_into("<I", msg, 114, status)
    msg[118:150] = audit_hash
    struct.pack_into("<H", msg, 150, len(build_info))
    msg[152:152 + len(build_info)] = build_info

    sig = sk.sign(bytes(msg))
    return bytes(msg) + sig


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--crate", required=True, help="Programm-Crate-Verzeichnis (mit Cargo.toml)")
    ap.add_argument("--elf", required=True)
    ap.add_argument("--manifest", default="", help="Laufzeit-Manifest-Blob (leer = kein Manifest)")
    ap.add_argument("--program-id", type=int, required=True)
    ap.add_argument("--version", type=int, required=True)
    ap.add_argument("--policy", default="internal-test", choices=list(POLICIES))
    ap.add_argument("--key", default="keys/trusted-test.ed25519")
    ap.add_argument("--out", required=True)
    args = ap.parse_args()

    status, report, root_name, ok = audit(os.path.join(ROOT, args.crate))
    sys.stderr.write(report)
    if not ok:
        sys.exit(f"\nUNSAFE-AUDIT FEHLGESCHLAGEN fuer {root_name} -> KEIN Zertifikat.")
    audit_hash = hashlib.sha256(report.encode("utf-8")).digest()
    cert = build_cert(args, status, audit_hash)
    with open(os.path.join(ROOT, args.out), "wb") as f:
        f.write(cert)
    with open(os.path.join(ROOT, args.out + ".audit.txt"), "w") as f:
        f.write(report)
    print(f"signiert: {args.out} ({len(cert)} B, Policy {args.policy}, Status ALL_PASS)")


if __name__ == "__main__":
    main()
