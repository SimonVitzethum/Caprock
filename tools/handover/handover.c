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

/* Seitenlayout — identisch zu `tramp.S`, dort steht die Begruendung. */
#define OFF_T16   0x000
#define OFF_T64   0x080
#define SIG_OFF   0x0F0	/* Real Mode erreicht */
#define SIG2_OFF  0x0F2	/* Long Mode erreicht */
#define OFF_GDT   0x100
#define OFF_GDTR  0x120
#define OFF_CR3   0x130
#define SIG_VALUE  0x5345
#define SIG2_VALUE 0x5346

/* Das Trampolin liegt in `tramp.S` — lesbarer Assembler statt eines Opcode-Blobs. */
extern const u8 sel4lake_t16_start[], sel4lake_t16_end[], sel4lake_t16_target[];
extern const u8 sel4lake_t64_start[], sel4lake_t64_end[], sel4lake_t64_sig[];

static struct page *low_page;
static struct page *pt_pages[6];	/* PML4 + PDPT + 4 PDs */
static bool armed;

/*
 * Identitaetsabbildung der ersten 4 GiB mit 2-MiB-Seiten.
 *
 * Bewusst nicht mit 1-GiB-Seiten: die sind eine CPU-Faehigkeit (`PDPE1GB`), und eine ungeprueft
 * angenommene Faehigkeit ist genau die Fehlerform, die in diesem Projekt reihenweise aufgefallen
 * ist. 2-MiB-Seiten kann jede Long-Mode-faehige CPU. Sechs Seiten Tabellen sind der Preis.
 *
 * Die Tabellen duerfen ueberall liegen — nur das Trampolin muss unter 1 MiB liegen, weil der
 * SIPI-Vektor eine Seitennummer ist.
 */
static u64 build_identity_tables(void)
{
	u64 *pml4, *pdpt, *pd;
	int i, j;

	for (i = 0; i < ARRAY_SIZE(pt_pages); i++) {
		pt_pages[i] = alloc_page(GFP_KERNEL | __GFP_ZERO);
		if (!pt_pages[i])
			return 0;
	}
	pml4 = page_address(pt_pages[0]);
	pdpt = page_address(pt_pages[1]);
	pml4[0] = page_to_phys(pt_pages[1]) | 0x3;	/* present + writable */
	for (i = 0; i < 4; i++) {
		pd = page_address(pt_pages[2 + i]);
		pdpt[i] = page_to_phys(pt_pages[2 + i]) | 0x3;
		for (j = 0; j < 512; j++)
			pd[j] = (((u64)i << 30) | ((u64)j << 21)) | 0x83; /* 2 MiB, present+rw */
	}
	return page_to_phys(pt_pages[0]);
}

static void free_identity_tables(void)
{
	int i;

	for (i = 0; i < ARRAY_SIZE(pt_pages); i++)
		if (pt_pages[i])
			__free_page(pt_pages[i]);
}

/* GDT mit einem 64-Bit-Codesegment (Selektor 0x10) und einem Datensegment. */
static void build_gdt(void *page, phys_addr_t pa)
{
	u64 *gdt = (u64 *)((u8 *)page + OFF_GDT);
	struct __packed { u16 limit; u32 base; } *gdtr = (void *)((u8 *)page + OFF_GDTR);

	gdt[0] = 0;
	gdt[1] = 0;
	gdt[2] = 0x00209A0000000000ULL;	/* 64-Bit-Code: P, DPL0, S, Code, L=1 */
	gdt[3] = 0x0000920000000000ULL;	/* Daten: P, DPL0, S, RW */
	gdtr->limit = 4 * 8 - 1;
	gdtr->base = (u32)(pa + OFF_GDT);	/* **lineare** Basis — Paging ist noch aus */
}

/*
 * Eine Seite **unterhalb 1 MiB** besorgen.
 *
 * Der SIPI-Vektor ist eine Seitennummer in 0x00..0xFF, das Trampolin muss also unter 1 MiB
 * liegen. Es gibt dafuer keine Laufzeit-API: `GFP_DMA` liefert ZONE_DMA (hier 0..16 MiB), und
 * 0x0..0x9efff ist auf dieser Maschine als "System RAM" beim Allokator. Also wiederholt
 * anfordern und behalten, was tief genug liegt — es werden ausschliesslich Seiten benutzt, die
 * der Allokator hergegeben hat, nie geraten.
 */
/*
 * Der Zwischenspeicher steht `__initdata` und nicht auf dem Stack: 512 Zeiger sind 4 KiB, und
 * ein 4-KiB-Stapelrahmen im Kernel ist eine schlechte Idee (der Compiler warnt zu Recht).
 * Der Bereich wird nach `init` ohnehin freigegeben.
 */
static struct page *low_pool[512] __initdata;

static struct page *__init alloc_low_page(void)
{
	struct page *keep = NULL;
	struct page **pool = low_pool;
	int n = 0, i;

	for (i = 0; i < ARRAY_SIZE(low_pool); i++) {
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

		u16 sig2;
		u64 cr3 = build_identity_tables();

		if (!cr3) {
			pr_err("sel4lake: Seitentabellen fehlgeschlagen\n");
			rc = -ENOMEM;
			goto undo;
		}
		memset(va, 0, PAGE_SIZE);
		memcpy((u8 *)va + OFF_T16, sel4lake_t16_start,
		       sel4lake_t16_end - sel4lake_t16_start);
		memcpy((u8 *)va + OFF_T64, sel4lake_t64_start,
		       sel4lake_t64_end - sel4lake_t64_start);
		build_gdt(va, pa);
		*(u32 *)((u8 *)va + OFF_CR3) = (u32)cr3;
		/* Die zwei Werte, die erst zur Laufzeit feststehen (s. `tramp.S`). */
		*(u32 *)((u8 *)va + OFF_T16 + (sel4lake_t16_target - sel4lake_t16_start)) =
			(u32)(pa + OFF_T64);
		*(u64 *)((u8 *)va + OFF_T64 + (sel4lake_t64_sig - sel4lake_t64_start)) =
			(u64)(pa + SIG2_OFF);
		wmb();
		pr_info("sel4lake: Trampolin @ phys %pa, SIPI-Vektor 0x%02llx, CR3 0x%llx, Ziel-APIC-ID %u\n",
			&pa, (u64)(pa >> 12), cr3, apicid);

		send_init_sipi(apicid, (u8)(pa >> 12));
		mdelay(50);
		sig = *(volatile u16 *)((u8 *)va + SIG_OFF);
		sig2 = *(volatile u16 *)((u8 *)va + SIG2_OFF);
		if (sig != SIG_VALUE) {
			pr_err("sel4lake: STUFE 1a FEHLGESCHLAGEN — Real-Mode-Signatur 0x%04x (erwartet 0x%04x)\n",
			       sig, SIG_VALUE);
		} else if (sig2 != SIG2_VALUE) {
			armed = true;	/* der Kern laeuft — nur der Uebergang misslang */
			pr_err("sel4lake: STUFE 1b FEHLGESCHLAGEN — Real Mode erreicht, Long Mode nicht (0x%04x)\n",
			       sig2);
		} else {
			pr_info("sel4lake: STUFE 1b BESTANDEN — der Kern laeuft im LONG MODE mit eigener GDT und eigenem CR3 (0x%04x/0x%04x)\n",
				sig, sig2);
			armed = true;
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

	/*
	 * Nur der TATSAECHLICH uebernommene Kern bleibt verloren. Ihn per `add_cpu`
	 * zurueckzuholen hiesse, Linux' Hotplug auf einen Kern in unbekanntem Zustand
	 * loszulassen; ein verlorener Kern bis zum Reboot ist der bessere Failure-Mode.
	 *
	 * Die uebrigen sind lediglich offline und voellig unberuehrt — sie pauschal
	 * mitverlieren zu lassen waere die stille Ueberdehnung einer Einschraenkung auf
	 * Faelle, fuer die ihre Begruendung gar nicht gilt. Stufe 1a nimmt genau einen Kern.
	 */
	for (i = ncpus - 1; i >= (armed ? 1 : 0); i--) {
		if (!add_cpu(cpus[i]))
			pr_info("sel4lake: CPU %d wieder online\n", cpus[i]);
	}
	if (armed) {
		pr_warn("sel4lake: CPU %d bleibt uebernommen; Reboot stellt sie wieder her\n",
			cpus[0]);
		return; /* low_page bewusst NICHT freigeben: der Kern liest sie noch. */
	}
	if (low_page)
		__free_pages(low_page, 0);
	free_identity_tables();
}

module_init(handover_init);
module_exit(handover_exit);
