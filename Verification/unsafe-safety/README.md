# Verifikation — Speichersicherheit der Software-`unsafe`-Stellen (Kani)

> **Status:** wachsend — die einzelnen **Kategorie-A**-`unsafe`-Stellen (reine RAM-Zeiger/Slice) werden
> **nach und nach** mit Kani memory-safe bewiesen. Eigenständiges Verifikationsartefakt; Kernel
> unverändert. Eigenständig verständlich (ohne Quellcode).

Bezug: [ADR 0021](../../docs/adr/0021-unsafe-memory-safety.md),
`ARMTest/unsafe-memory-safety-aufwand.md` (Aufwandsanalyse + Kategorisierung), `docs/verification.md`
(Kani region/sync), [proofs](kani/src/lib.rs).

## 1. Motivation und Ziel

Der safe-Rust-Kern ist by-compiler speichersicher; zu beweisen bleibt nur die Speichersicherheit der
`unsafe`-Blöcke. Die ~200 Stellen zerfallen (s. Aufwandsanalyse) in **Kategorie A** (normales RAM,
Bounds-Check schützt den rohen Zugriff — Kani-beweisbar; `region`/`sync` bereits erledigt) und
**Kategorie B** (Hardware-/Maschinenmodell — Forschungsklasse, als HAL-TCB gekapselt). **Ziel hier:**
die Kategorie-A-Stellen einzeln **maschinengeprüft** memory-safe machen — mit den **echten**
`core::ptr`-Operationen.

## 2. Methode

Eigenständiges Kani-Crate ([`kani/`](kani/)): jede `*_logic`-Funktion ist eine **byte-genaue Kopie** der
unsafe-Glue einer konkreten Kernel-Stelle (Zeilenverweis im Code). Ein `#[kani::proof]`-Harness führt
die echten `copy_nonoverlapping`/`write_bytes`/… auf einem **modellierten Puffer** aus; Kani (CBMC)
meldet **jeden** Out-of-Bounds-Zugriff, Underflow oder UB. Bewiesen wird die Speichersicherheit **unter
der dokumentierten Vorbedingung** — die zusätzlich am Aufrufer als reine Arithmetik bewiesen wird.

**Warum ein eigenständiges Artefakt (nicht `#[cfg(kani)]` im Kernel):** der Kernel-Crate baut nur unter
Custom-Target + build-std + HW-Deps; unter `cargo kani` (Host-Target) ist er nicht baubar. Die getreue
Kopie hält den Kernel unverändert; die **Treue Kopie↔Kernel** ist die kleine, auditierbare TCB dieses
Beweises (je Stelle ein Zeilenverweis; bei Kernel-Änderung nachzuziehen).

## 3. Auswahlkriterium

Aufgenommen wird eine Stelle, wenn sie (a) **Kategorie A** (normales RAM) und (b) in **1–3 h gut
beweisbar** ist. **Nicht** aufgenommen (sondern als Vertrag dokumentiert): reine Trust-Primitive ohne
in-Funktion-Bounds — `mem::peek_u64/poke_u64` (beliebige Cap-Adresse; Gültigkeit aus dem **verifizierten
Cap-System**), MMIO-`volatile` (HAL-Gerätevertrag), fixe RAM-Fenster-Slices (`MOD_BASE..MOD_WINDOW`,
Boot-Kontrakt). Kategorie B (Pagetables/MMIO/Assembly) bleibt die axiomatisierte HAL-TCB.

## 4. Bewiesene Stellen

| # | Kernel-Stelle | Harness(es) | Eigenschaft | Status |
|---|---|---|---|---|
| 1 | [`system.rs::copy_segment`](../../kernel/src/system.rs) (einzige unsafe-Stelle des Ladepfads, ADR 0011 §2) | `copy_segment_in_bounds`, `copy_segment_zeroes_tail`, `loader_precondition_holds` | Kopie (`filesz` B) + `.bss`-Nullung (`[filesz,total)`) bleiben **im Ziel-Frame**; Schwanz sauber genullt; Vorbedingung `filesz<=total` aus `total=round_up_4k(memsz)>=memsz>=filesz` (ELF) am Aufrufer etabliert (overflow-frei) | ✅ |
| 2 | [`heap.rs`](../../crates/sel4lake-region/src/heap.rs) (Slab-Free-Liste: `read`/`write` des im freien Slot eingebetteten Nachfolger-Zeigers) | `slab_class_holds_pointer`, `slab_freelist_roundtrip` | jede Größenklasse (`[16..2048]`) fasst einen `usize` **und** ist usize-ausgerichtet (größen-ausgerichteter Slot ⟹ usize-aligned); roher `read`/`write` des Slot-Zeigers in-bounds + aligned (Round-Trip erhält den Wert) | ✅ |

## 5. Ausführen

```sh
tools/kani-verify.sh unsafe      # nur dieses Ziel
tools/kani-verify.sh             # alle Kani-Ziele (loader region sync unsafe)
```

CI-Gate: `.gitea/workflows/kani.yml` (läuft `tools/kani-verify.sh` über alle Ziele).

## 6. Trusted Computing Base / Grenzen

1. **Fidelity** der `*_logic`-Kopie ↔ Kernel-Stelle (kleiner Zeilenverweis je Stelle).
2. Die **Vorbedingung** stammt aus einer separat geprüften Quelle (hier: ELF-Parser-Garantie
   `filesz<=memsz`, safe-Rust + host-getestet) — am Aufrufer als Arithmetik mitbewiesen.
3. Die **Gültigkeit der Zielregion** (`[base,base+total)` ist frisch alloziertes, gemapptes RW-RAM)
   ist eine **Cap-System-/Allokator-Vorbedingung** (außerhalb dieser Crate) — vgl. die RegionView-
   Annahme in `docs/verification.md`.
4. **Kategorie B** (Pagetables/MMIO/Kontextwechsel) bleibt außerhalb (HAL-TCB, Aufwandsanalyse).

## 7. Nächste Stellen (Kandidaten)

- `system.rs::alloc_zeroed` (`write_bytes(base,0,len)` — Nullung einer frischen `len`-Byte-Region).
- `system.rs` Code-Kopie (`copy_nonoverlapping(code, cbase, code_len)` — RAM→RAM, Längen-Vorbedingung).
- `threads/mod.rs` ist überwiegend **MMIO** (RTC-Register, Kategorie B / HAL-Vertrag) — kein Kat-A-Ziel.
