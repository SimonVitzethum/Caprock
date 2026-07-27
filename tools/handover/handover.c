// SPDX-License-Identifier: GPL-2.0
/*
 * sel4lake_handover — Kern-Uebergabe an SEL4Lake (Variante B), Stufe 0 + 1a.
 *
 * Stufe 0 (`arm=0`, Voreinstellung): meldet die APIC-ID der Zielkerne und nimmt sie offline.
 *   Vollstaendig reversibel — `rmmod` bringt sie zurueck.
 *
 * Stufe 1a (`arm=1`): schickt EINEM offline genommenen Kern INIT-SIPI-SIPI in ein winziges
 *   16-Bit-Trampolin, das eine Signatur in eine bekannte Seite schreibt und dann anhaelt.
 *   Das ist das Go/No-Go der ganzen Variante: erreicht unser Code den Kern, und ueberlebt
 *   Linux die Uebernahme? Bewusst OHNE Long-Mode-Aufbau — ein Schritt, eine Frage.
 *
 * **Nach Stufe 1a ist der Kern bis zum Reboot verloren.** Linux haelt ihn fuer geparkt, er
 * fuehrt aber unseren Code aus. `rmmod` bringt ihn deshalb NICHT zurueck; das Modul weigert
 * sich, ihn wieder online zu nehmen, statt Linux' Hotplug in einen unbekannten Zustand laufen
 * zu lassen. Der Reboot ist der Ausweg — genau die Eigenschaft, die gefordert war.
 *
 * Nichts davon ueberlebt einen Reboot: kein Bootparameter, keine Datei, kein reservierter
 * Speicher. Das Trampolin liegt in einer Seite, die Linux' eigener Allokator hergegeben hat.
 */
#include <linux/module.h>
#include <linux/kernel.h>
#include <linux/cpu.h>
#include <linux/delay.h>
#include <linux/gfp.h>
#include <linux/mm.h>
#include <asm/msr.h>
#include <asm/apic.h>

MODULE_LICENSE("GPL");
MODULE_DESCRIPTION("SEL4Lake: Kern-Uebergabe (Stufe 0/1a)");

static int cpus[5] = { 15, 16, 17, 18, 19 };
static int ncpus = 5;
module_param_array(cpus, int, &ncpus, 0444);
MODULE_PARM_DESC(cpus, "Zielkerne (Vorgabe 15-19: E-Cores, kein HT-Partner, homogen)");

static int arm;
module_param(arm, int, 0444);
MODULE_PARM_DESC(arm, "1 = Stufe 1a: einen Kern wirklich uebernehmen (bis Reboot verloren)");

/* Offset der Signatur innerhalb der Trampolin-Seite. */
#define SIG_OFF   0x0F0
#define SIG_VALUE 0x5345 /* 'ES' little-endian -> "SE" */

/*
 * 16-Bit-Real-Mode-Trampolin. Die SIPI setzt CS = Vektor<<8 und IP = 0; das Trampolin liegt
 * also am Segmentanfang und kann sich ueber CS selbst adressieren.
 *
 *   fa              cli
 *   8c c8           mov ax, cs
 *   8e d8           mov ds, ax
 *   c7 06 f0 00 45 53   mov word [0x00f0], 0x5345
 *   f4              hlt
 *   eb fd           jmp $-1
 */
static const u8 tramp[] = {
	0xfa,
	0x8c, 0xc8,
	0x8e, 0xd8,
	0xc7, 0x06, (SIG_OFF & 0xff), (SIG_OFF >> 8), (SIG_VALUE & 0xff), (SIG_VALUE >> 8),
	0xf4,
	0xeb, 0xfd,
};

static struct page *low_page;
static bool armed;

/*
 * Eine Seite **unterhalb 1 MiB** besorgen.
 *
 * Der SIPI-Vektor ist eine Seitennummer in 0x00..0xFF, das Trampolin muss also unter 1 MiB
 * liegen. Es gibt dafuer keine Laufzeit-API: `GFP_DMA` liefert ZONE_DMA (hier 0..16 MiB), und
 * 0x0..0x9efff ist auf dieser Maschine als "System RAM" beim Allokator. Also wiederholt
 * anfordern und behalten, was tief genug liegt — es werden ausschliesslich Seiten benutzt, die
 * der Allokator hergegeben hat, nie geraten.
 */
static struct page *alloc_low_page(void)
{
	struct page *keep = NULL, *pool[512];
	int n = 0, i;

	for (i = 0; i < ARRAY_SIZE(pool); i++) {
		struct page *p = alloc_pages(GFP_KERNEL | GFP_DMA, 0);

		if (!p)
			break;
		if (page_to_phys(p) < 0x100000 && (page_to_phys(p) & 0xfff) == 0) {
			keep = p;
			break;
		}
		pool[n++] = p;
	}
	while (n--)
		__free_pages(pool[n], 0);
	return keep;
}

static void report_cpu(int cpu)
{
	pr_info("sel4lake: CPU %d -> APIC-ID %u, online=%d\n",
		cpu, cpu_physical_id(cpu), cpu_online(cpu));
}

/* INIT-SIPI-SIPI ueber die x2APIC-ICR-MSR (0x830). Diese Maschine faehrt x2APIC, der
 * MMIO-Pfad des LAPIC ist dort abgeschaltet. */
static void send_init_sipi(u32 apicid, u8 vector)
{
	/* x2APIC-Registerblock beginnt bei MSR 0x800; ICR liegt bei APIC_ICR>>4 dahinter. */
	const u32 icr = 0x800 + (APIC_ICR >> 4);

	/* INIT, Level assert, edge. */
	wrmsrl(icr, ((u64)apicid << 32) | 0x00004500ULL);
	udelay(200);
	/* Zweimal SIPI, wie von der Spezifikation empfohlen. */
	wrmsrl(icr, ((u64)apicid << 32) | 0x00004600ULL | vector);
	udelay(200);
	wrmsrl(icr, ((u64)apicid << 32) | 0x00004600ULL | vector);
	udelay(200);
}

static int __init handover_init(void)
{
	int i, rc;

	pr_info("sel4lake: Stufe %s, Zielkerne:", arm ? "1a (arm=1)" : "0");
	for (i = 0; i < ncpus; i++)
		report_cpu(cpus[i]);

	for (i = 0; i < ncpus; i++) {
		rc = remove_cpu(cpus[i]);
		if (rc) {
			pr_err("sel4lake: CPU %d offline fehlgeschlagen (%d)\n", cpus[i], rc);
			goto undo;
		}
		pr_info("sel4lake: CPU %d offline\n", cpus[i]);
	}

	if (!arm) {
		pr_info("sel4lake: Stufe 0 fertig — rmmod bringt die Kerne zurueck\n");
		return 0;
	}

	low_page = alloc_low_page();
	if (!low_page) {
		pr_err("sel4lake: keine Seite unter 1 MiB bekommen\n");
		rc = -ENOMEM;
		goto undo;
	}
	{
		phys_addr_t pa = page_to_phys(low_page);
		void *va = page_address(low_page);
		u32 apicid = cpu_physical_id(cpus[0]);
		u16 sig;

		memset(va, 0, PAGE_SIZE);
		memcpy(va, tramp, sizeof(tramp));
		wmb();
		pr_info("sel4lake: Trampolin @ phys %pa, SIPI-Vektor 0x%02llx, Ziel-APIC-ID %u\n",
			&pa, (u64)(pa >> 12), apicid);

		send_init_sipi(apicid, (u8)(pa >> 12));
		mdelay(50);
		sig = *(volatile u16 *)((u8 *)va + SIG_OFF);
		if (sig == SIG_VALUE) {
			pr_info("sel4lake: STUFE 1a BESTANDEN — der Kern hat unseren Code ausgefuehrt (Signatur 0x%04x)\n",
				sig);
			armed = true;
		} else {
			pr_err("sel4lake: STUFE 1a FEHLGESCHLAGEN — Signatur 0x%04x (erwartet 0x%04x)\n",
			       sig, SIG_VALUE);
		}
	}
	return 0;

undo:
	while (--i >= 0)
		add_cpu(cpus[i]);
	return rc;
}

static void __exit handover_exit(void)
{
	int i;

	if (armed) {
		/*
		 * Der uebernommene Kern fuehrt unseren Code aus, nicht Linux' Park-Schleife.
		 * Ihn per `add_cpu` zurueckzuholen hiesse, Linux' Hotplug auf einen Kern in
		 * unbekanntem Zustand loszulassen. Ein verlorener Kern bis zum Reboot ist der
		 * bessere Failure-Mode — und der Reboot war ausdruecklich der Ausweg.
		 */
		pr_warn("sel4lake: Kern %d bleibt uebernommen; Reboot stellt ihn wieder her\n",
			cpus[0]);
		return; /* low_page bewusst NICHT freigeben: der Kern liest sie noch. */
	}
	for (i = ncpus - 1; i >= 0; i--) {
		if (!add_cpu(cpus[i]))
			pr_info("sel4lake: CPU %d wieder online\n", cpus[i]);
	}
	if (low_page)
		__free_pages(low_page, 0);
}

module_init(handover_init);
module_exit(handover_exit);
