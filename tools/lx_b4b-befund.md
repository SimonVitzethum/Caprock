# B4b-Messstand: Befund 2026-09-10 (virtio-blk sendet keinen MSI-X)

Werkzeug: `tools/lx_b4b-messstand.sh` (baut/erneuert Archiv, bootet eine Variante mit
Protokoll-Auszug). Logs unter `build/diag/lx-b4b-*.{log,auszug,qemu-err}` (gitignored).

## Messstand (je ~3 Min, 4 Laeufe + Setup)

| Lauf | Variante | used.idx | Queue-Vektor | IRTE | Faults | IRQ? |
|---|---|---|---|---|---|---|
| Setup | TCG+intremap, altes Archiv | – | – | – | – | Fehlstart: Manifest band nicht an Kernel (Archiv 08-28 vs. Kernel 09-09) |
| L1 | TCG, intremap=on, B4 aus | 1 | `0xffff` (keine) | – | – | nein (erwartbar: Queue nie programmiert) |
| M1 | TCG, intremap=on, Messmodus | 1 | `0x0000` | present, SVT/SID, Zeile `addr=0xfee00008` unmaskiert, `msix-ctrl=0x8004` | leer | nein (`weckrufe=0`, nur Self-IPI) |
| M2 | TCG, intremap=on, **eim=on**, Messmodus | 1 | `0x0000` | identisch | leer | nein (zeichengleich zu M1) |
| Rueckbau | TCG, intremap=on, B4 aus | 1 | `0xffff` | – | – | Sanity: `drv`/`blkdev` wieder ALL PASS |

Durchgehend belegt: `irtevgb: ALL PASS`, KOMPAT-Self-IPI `angekommen=true zugestellt=true`
(Vektor 0x64), `apic: x2APIC (MSR-Pfad)`.

Neuer Messmodus im Demo-Treiber (`programs/hardware/virtio-blk`, Standard AUS): `B4_MESSEN`
programmiert die Queue auf Zeile 0 wie der Produktweg, wartet mit Frist des Messenden
(`wait_frist`, 200 Ticks, `ERR_TIMEOUT`, kein Poll-Rueckfall). Ohne ihn sprechen alle
Umgebungsläufe ueber eine nie programmierte Queue (`0xffff`).

## Ursache (Stand Hypothese, kein Beweis)

- **KVM entlastet:** M1 unter TCG reproduziert den KVM-Befund exakt (programmierter
  Queue-Vektor + `used.idx=1` + kein IRQ + keine Faults). „KVM/split-irqchip" war es nicht.
- **QEMU-Flag allein genuegt nicht:** M2 mit `eim=on` ist zeichengleich zu M1.
- **Verbleibender Verdacht (Format, kernelseitig):** Der Kern laeuft **x2APIC**
  (`intc.rs:init_cpu` schaltet ihn ein; Log: MSR-Pfad), aber die IOMMU-Seite ist
  **xAPIC-formatig**: `vtd.rs:976-979` laesst `IRTA.EIME` (Bit 11) aus, `irte.rs:34-37/96-113`
  kodiert das Ziel als 8-Bit-ID in Bits 47:40 (Abweisung >255). x2APIC-CPU gegen
  EIME=0-IRTE: QEMU stellt eine remappte Nachricht dann still nicht zu (kein Fault).
  Naechster Schritt ist der EIME-Lauf (Patch unten + `eim=on`), nicht `B4_WARTEN`.
- Ohne-intremap-Lauf nicht gefahren (Budget): `irte_vergabe_moeglich()==false` →
  `msi_grant()=None` → Treiber pollt rechtmaessig. Traegt zur Zustellfrage nichts bei.

## PATCH-TEXT (HAL-Besitz, NICHT angewendet -- gegen VT-d-Manual pruefen)

QEMU-Seite: `-device intel-iommu,caching-mode=on,intremap=on,eim=on` mit
`-machine q35,kernel-irqchip=split`. `eim` nur mit `intremap` sinnvoll; allein heilt es
nichts (M2). Voraussetzung dafuer, dass eine EIME=1-IRTE ueberhaupt an einen
x2APIC-LAPIC zugestellt werden kann.

```patch
--- a/crates/caprock-hal/src/x86_64/vtd.rs   (ir_enable, Kontext Zeilen 969-979)
+++ b/crates/caprock-hal/src/x86_64/vtd.rs
@@
     let Some(table) = alloc() else { return false };
     // Groessenfeld in den unteren Bits, Adresse in 63:12. EIME (Bit 11) bleibt aus: es gilt nur
-    // im x2APIC-Modus, und den meldet diese Plattform nicht zwingend -- ein gesetztes EIME ohne
-    // x2APIC waere ein reserviertes Bit.
-    write64(REG_IRTA, (table & !0xfff) | IRTA_SIZE_FIELD);
+    // im x2APIC-Modus, und den meldet diese Plattform nicht zwingend -- ein gesetztes EIME ohne
+    // x2APIC waere ein reserviertes Bit. B4b-Messstand (M1/M2, TCG): die CPU laeuft x2APIC
+    // (apic-Zeile: MSR-Pfad), die IRTE bleibt xAPIC-formatig -- programmierter Queue-Vektor,
+    // used.idx=1, keine Faults, kein IRQ. EIME deshalb genau dann setzen, wenn der Kern
+    // wirklich x2APIC faehrt:
+    //   let irta = (table & !0xfff) | IRTA_SIZE_FIELD
+    //       | if super::intc::x2apic_active() { 1 << 11 } else { 0 };
+    //   write64(REG_IRTA, irta);
+    // BEGLEITEND, gleiche Stufe: irte_build muss das Ziel dann im x2APIC-Format kodieren
+    // (32-Bit-Destination statt 8-Bit in 47:40; die Abweisung apic_id > 0xFF entfaellt dort)
+    // -- Bitlage vor dem Anwenden gegen das VT-d-Manual (IRTA.EIME / IRTE-Format) pruefen.
+    // Ohne diese zweite Haelfte waere EIME=1 mit 8-Bit-Ziel ein neues stilles Fehlformat.
+    write64(REG_IRTA, (table & !0xfff) | IRTA_SIZE_FIELD);
```
