# Z7 offline: was `lx_attest_check.py` belegt — und was nicht

Stand 2026-09-09. Das Werkzeug rechnet die SW-PCR-Messkette ausserhalb der Maschine
nach (kein Boot, kein Schreiben an bestehenden Dateien). Aufrufe:

```sh
tools/lx_attest_check.py demo               # Formel gegen Goldwerte (kein Boot)
tools/lx_attest_check.py kette bericht.json # PD-Bericht: Kette + Nonce + Modell-Signatur
tools/lx_attest_check.py log build/diag/lauf.log  # Kernel-Log: Konsistenz der messkette-Zeilen
```

## Woher die Stuecke kommen

| Stueck | Quelle | Warum dort |
|---|---|---|
| Genesis (Anker) | `tools/kernel_hash.py <kernel.elf>` | Der Anker IST der Kernel-Code-Hash (`[__text_start, __rodata_end)`) — derselbe, an den das Manifest per `kernel_hash` bindet. Ein Bericht ueber einem anderen Anker ist ein Bericht ueber einen anderen Kernel. |
| `image_hash` je Eintrag | Build-Artefakte / Manifest (`e.sha256`) | Was der Kernel misst, ist `SHA-256(ELF-Bytes)` — dieselben Bytes, deren Hash im Manifest steht. Bekannte Hashes heisst: aus DEM Build, nicht aus dem Gedaechtnis. |
| `program_id`/`domain` | Manifest-Eintrag | Die Kette bindet ID + Domaene MIT in den Extend ein — ein Bericht, der ein anderes Programm nennt, rechnet sich nicht. |
| Nonce | Tenant (frisch je Anfrage) | Replay-Schutz: Der Tenant schickt eine Nonce, die PD bindet sie in den signierten Bericht. `nonce_erwartet` ist die gesendete, `bericht.nonce` die zurueckgekommene. |
| Signatur (Produktion) | PD-Key (Manifest-Endowment), Ed25519 | Der Tenant prueft gegen den Manifest-Pubkey der Attestierungs-PD. DAS steht hier NICHT — es ist Tenant-Seite mit bestehender Krypto (`caprock-trust`), kein neues Werkzeug. |
| Signatur (Modell) | `--modell-key` + `signatur` im JSON | Die TEST-MAC aus `programs/attest` (`TestSchluessel`). Damit ist die PROTOKOLLOGIK offline belegbar (Schluesselwechsel bricht, Faelschung bricht), ohne Ed25519 zu behaupten. |

## Was „bestanden" heisst — und was nicht

* `kette: ALL PASS` heisst: die Kette rechnet sich vom Anker bis zum Kopf, der Kopf
  steht im Bericht, die Anzahl passt zur Liste, die Nonce ist frisch, die (Modell-)
  Signatur stammt von diesem Key. Nicht mehr.
* Es heisst NICHT „die Maschine ist echt": ohne TPM gibt es keinen Hardware-Anker
  (SW-PCR + signierter Bericht, kein Remote-Trust gegen physischen Angreifer).
  Die TPM-Messkette (Firmware → Bootloader → Kernel + Modul) kommt von aussen
  (`tools/lx_bootentscheidung.md` §2); dieses Werkzeug beginnt erst beim Kernel.
* `log` ist Konsistenz, kein Beleg: Die Kernel-Zeilen tragen je 4-Byte-Praefixe zu
  Diagnosezwecken — vollständig prüft nur `kette` über den JSON-Bericht der PD.
* `verworfen != 0` ist FAILURES, kein Schönheitsfehler: Eine Kette mit Lücke belegt
  nichts über das Verworfene — sie benennt nur, dass etwas fehlt.
