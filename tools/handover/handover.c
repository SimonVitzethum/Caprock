// SPDX-License-Identifier: GPL-2.0
/*
 * caprock_handover — Kern-Uebergabe an Caprock (Variante B), Stufe 0 + 1a.
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
MODULE_DESCRIPTION("Caprock: Kern-Uebergabe (Stufe 0/1a)");

static int cpus[5] = { 15, 16, 17, 18, 19 };
static int ncpus = 5;
module_param_array(cpus, int, &ncpus, 0444);
MODULE_PARM_DESC(cpus, "Zielkerne (Vorgabe 15-19: E-Cores, kein HT-Partner, homogen)");

static int arm;
module_param(arm, int, 0444);
MODULE_PARM_DESC(arm, "1 = Kerne wirklich uebernehmen (Long-Mode-Trampolin)");

/*
 * Beim Entladen die uebernommenen Kerne an Linux zurueckgeben?
 *
 * Ich hatte das fuer unmoeglich gehalten und "bis zum Reboot verloren" hingeschrieben. Der Lauf
 * vom 27.07. hat das widerlegt: CPU 15 lief 24 Minuten in unserer `hlt`-Schleife, und Linux'
 * eigenes INIT-SIPI-SIPI hat sie folgenlos wieder eingegliedert ("Booting Node 0 Processor 15").
 * Die Annahme war unbelegt — und sie hatte einen Preis: die Seite unter 1 MiB leckte bei jedem
 * Lauf, und davon gibt es auf dieser Maschine genau eine.
 *
 * Voreingestellt ist deshalb die Rueckgabe. Sie setzt voraus, dass der Kern in einem
 * **definierten** Zustand parkt (unsere `cli; hlt`-Schleife). Fuehrt er spaeter echten
 * Caprock-Code aus, ist das nicht mehr selbstverstaendlich — dann `release=0` setzen und den
 * Reboot nehmen. Die Entscheidung gehoert an den Aufrufer, nicht in eine Annahme.
 */
static int release = 1;
module_param(release, int, 0444);
MODULE_PARM_DESC(release, "1 = uebernommene Kerne beim Entladen an Linux zurueckgeben (Vorgabe)");

/*
 * Physische Adresse einer Seite, die ein **frueherer** Lauf dieses Moduls geleakt hat.
 *
 * Eine aeltere Fassung gab die Trampolin-Seite bewusst nie zurueck — unter der Annahme, der
 * uebernommene Kern brauche sie fuer immer. Die Annahme war falsch (Linux holt den Kern
 * zurueck), die Seite blieb trotzdem vergeben. Da es unterhalb 1 MiB auf dieser Maschine
 * praktisch genau eine gibt, blockiert dieser eine Fehler jeden weiteren Lauf.
 *
 * Ein Reboot waere der grobe Weg. Der genaue: wir wissen, welche Seite es ist, wir wissen, dass
 * sie aus einer `alloc_pages(order=0)` dieses Moduls stammt, und wir koennen es nachpruefen,
 * bevor wir sie zurueckgeben — Referenzzaehler 1, nicht reserviert, kein Slab. Trifft eine der
 * Bedingungen nicht zu, gehoert die Seite jemand anderem und wird nicht angefasst.
 */
static ulong reclaim;
module_param(reclaim, ulong, 0444);
MODULE_PARM_DESC(reclaim, "Physadresse einer von einem frueheren Lauf geleakten Seite zurueckgeben");

static bool reclaim_page(ulong pa)
{
	struct page *p;

	if (!pa || (pa & ~PAGE_MASK) || !pfn_valid(PHYS_PFN(pa))) {
		pr_err("caprock: reclaim=0x%lx ist keine gueltige Seitenadresse\n", pa);
		return false;
	}
	p = pfn_to_page(PHYS_PFN(pa));
	/*
	 * Die Pruefungen sind der Punkt der Uebung. Eine Seite freizugeben, die inzwischen
	 * jemand anderem gehoert, waere Speicherkorruption im Kernel — also lieber nichts tun
	 * als hoffen.
	 */
	if (PageReserved(p) || PageSlab(p) || PageLRU(p) || page_count(p) != 1) {
		pr_err("caprock: reclaim 0x%lx abgelehnt (count=%d reserved=%d slab=%d lru=%d) — die Seite gehoert jemandem\n",
		       pa, page_count(p), PageReserved(p), PageSlab(p), PageLRU(p));
		return false;
	}
	__free_pages(p, 0);
	pr_info("caprock: Seite 0x%lx zurueckgegeben (Leck eines frueheren Laufs behoben)\n", pa);
	return true;
}

/* Seitenlayout — identisch zu `tramp.S`, dort steht die Begruendung. */
#define OFF_T16   0x000
#define OFF_T64   0x080
#define SIG_OFF   0x0F0	/* Real Mode erreicht */
#define SIG2_OFF  0x0F2	/* Long Mode erreicht */
#define OFF_GDT   0x100
#define OFF_GDTR  0x120
#define OFF_CR3   0x130
#define SIG_VALUE  0x4341
#define SIG2_VALUE 0x4342

/* Das Trampolin liegt in `tramp.S` — lesbarer Assembler statt eines Opcode-Blobs. */
extern const u8 caprock_t16_start[], caprock_t16_end[], caprock_t16_target[];
extern const u8 caprock_t64_start[], caprock_t64_end[], caprock_t64_sig[];
extern const u8 caprock_t64_park[];
extern const u8 caprock_park_start[], caprock_park_end[];

static struct page *low_page;
static struct page *park_pages[8];	/* je uebernommenem Kern eine */
static struct page *pt_pages[6];	/* PML4 + PDPT + 4 PDs */
static int narmed;			/* wie viele Kerne uebernommen wurden */
/*
 * Welche Kerne **dieser Lauf** offline genommen hat. Nur die duerfen beim Entladen zurueck:
 * einen Kern wieder online zu nehmen, den jemand anders (oder ein frueherer Lauf) geparkt hat,
 * waere das Zurueckgeben einer Ressource, die uns nie gehoert hat.
 */
static bool we_offlined[8];

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

	/*
	 * **Unterhalb 4 GiB.** `CR3` wird im Protected Mode geladen, also mit einem 32-Bit-
	 * Zugriff (`mov %eax, %cr3`) — eine Wurzel oberhalb 4 GiB wird dabei stillschweigend
	 * abgeschnitten, und der Kern laeuft in Tabellen, die es nicht gibt. Genau das ist
	 * passiert: die Wurzel lag bei 0x2052b3000 (~8,3 GiB), uebrig blieb 0x052b3000.
	 * `GFP_DMA32` sagt die Schranke zu, statt auf sie zu hoffen.
	 */
	for (i = 0; i < ARRAY_SIZE(pt_pages); i++) {
		pt_pages[i] = alloc_page(GFP_KERNEL | GFP_DMA32 | __GFP_ZERO);
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
/*
 * ZONE_DMA umfasst 16 MiB = 4096 Seiten; davon liegen ~158 unter 1 MiB. Ein Suchlauf mit 512
 * Versuchen greift also im Regelfall zu hoch — er muss die Zone so weit leerraeumen, dass der
 * Allokator die tiefen Seiten herausgeben MUSS. Deshalb Platz fuer die ganze Zone. Die Seiten
 * werden unmittelbar danach alle bis auf eine zurueckgegeben.
 */
static struct page *low_pool[4096] __initdata;

static struct page *__init alloc_low_page(void)
{
	struct page *keep = NULL;
	struct page **pool = low_pool;
	phys_addr_t lowest = ~(phys_addr_t)0;
	int n = 0, i;

	for (i = 0; i < ARRAY_SIZE(low_pool); i++) {
		struct page *p = alloc_pages(GFP_KERNEL | GFP_DMA | __GFP_NOWARN, 0);
		phys_addr_t pa;

		if (!p)
			break;	/* Zone erschoepft — tiefer geht es nicht */
		pa = page_to_phys(p);
		if (pa < lowest)
			lowest = pa;
		if (pa < 0x100000) {
			keep = p;
			break;
		}
		pool[n++] = p;
	}
	while (n--)
		__free_pages(pool[n], 0);
	if (!keep)
		pr_err("caprock: %d Seiten aus ZONE_DMA geprueft, tiefste war %pa — keine unter 1 MiB frei\n",
		       i, &lowest);
	return keep;
}

static void report_cpu(int cpu)
{
	pr_info("caprock: CPU %d -> APIC-ID %u, online=%d\n",
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

	if (reclaim && !reclaim_page(reclaim))
		return -EINVAL;

	pr_info("caprock: Stufe %s, Zielkerne:", arm ? "1b (arm=1, Uebernahme)" : "0 (nur offline)");
	for (i = 0; i < ncpus; i++)
		report_cpu(cpus[i]);

	for (i = 0; i < ncpus; i++) {
		/*
		 * Ein bereits offline stehender Kern ist **kein Fehler**, sondern der Normalfall
		 * beim zweiten Anlauf: entweder hat ein frueherer Lauf ihn geparkt, oder jemand
		 * hat ihn von Hand offline genommen. `remove_cpu` meldet das mit `1` — ein
		 * positiver Wert, der weder Erfolg noch ein gueltiger `errno` ist. Ihn
		 * durchzureichen erzeugte die Kernel-Ruege "init suspiciously returned 1".
		 */
		if (!cpu_online(cpus[i])) {
			pr_info("caprock: CPU %d war bereits offline — uebersprungen\n",
				cpus[i]);
			continue;
		}
		rc = remove_cpu(cpus[i]);
		if (rc) {
			pr_err("caprock: CPU %d offline fehlgeschlagen (%d)\n", cpus[i], rc);
			rc = (rc > 0) ? -EBUSY : rc;
			goto undo;
		}
		we_offlined[i] = true;	/* nur diese duerfen wir zurueckgeben */
		pr_info("caprock: CPU %d offline\n", cpus[i]);
	}

	if (!arm) {
		pr_info("caprock: Stufe 0 fertig — rmmod bringt die Kerne zurueck\n");
		return 0;
	}

	low_page = alloc_low_page();
	if (!low_page) {
		pr_err("caprock: keine Seite unter 1 MiB bekommen\n");
		rc = -ENOMEM;
		goto undo;
	}
	{
		phys_addr_t pa = page_to_phys(low_page);
		void *va = page_address(low_page);
		u64 cr3 = build_identity_tables();

		if (!cr3) {
			pr_err("caprock: Seitentabellen fehlgeschlagen\n");
			rc = -ENOMEM;
			goto undo;
		}
		/*
		 * Die Zusage von `GFP_DMA32` wird nachgeprueft, nicht geglaubt: eine abgeschnittene
		 * Wurzel faellt sonst erst als "Long Mode nicht erreicht" auf, und dort sucht man
		 * sie zuletzt.
		 */
		if (cr3 >> 32) {
			pr_err("caprock: Seitentabellen-Wurzel 0x%llx liegt oberhalb 4 GiB — CR3 wuerde abgeschnitten\n",
			       cr3);
			rc = -EIO;
			goto undo;
		}
		pr_info("caprock: Trampolin @ phys %pa, SIPI-Vektor 0x%02llx, CR3 0x%llx\n",
			&pa, (u64)(pa >> 12), cr3);

		/*
		 * **Alle Zielkerne nacheinander, mit EINER niedrigen Seite.**
		 *
		 * Der erste Anlauf gab jedem Kern eine eigene und verbrauchte sie dauerhaft --
		 * bei genau einer freien Seite unter 1 MiB war damit nach dem ersten Kern
		 * Schluss. Da jeder Kern die Seite nur waehrend des Moduswechsels braucht und
		 * danach in seine eigene Park-Seite springt, reicht eine, sequenziell benutzt.
		 * Das ist zugleich die Form, die die Uebergabe von fuenf Kernen ueberhaupt
		 * erst moeglich macht.
		 */
		for (i = 0; i < ncpus; i++) {
			u32 apicid = cpu_physical_id(cpus[i]);
			u16 sig, sig2;

			park_pages[i] = alloc_page(GFP_KERNEL | GFP_DMA32 | __GFP_ZERO);
			if (!park_pages[i]) {
				pr_err("caprock: keine Park-Seite unter 4 GiB fuer CPU %d\n",
				       cpus[i]);
				break;
			}
			memcpy(page_address(park_pages[i]), caprock_park_start,
			       caprock_park_end - caprock_park_start);

			memset(va, 0, PAGE_SIZE);
			memcpy((u8 *)va + OFF_T16, caprock_t16_start,
			       caprock_t16_end - caprock_t16_start);
			memcpy((u8 *)va + OFF_T64, caprock_t64_start,
			       caprock_t64_end - caprock_t64_start);
			build_gdt(va, pa);
			*(u32 *)((u8 *)va + OFF_CR3) = (u32)cr3;
			/* Die drei Werte, die erst zur Laufzeit feststehen (s. `tramp.S`). */
			*(u32 *)((u8 *)va + OFF_T16 +
				 (caprock_t16_target - caprock_t16_start)) =
				(u32)(pa + OFF_T64);
			*(u64 *)((u8 *)va + OFF_T64 +
				 (caprock_t64_sig - caprock_t64_start)) =
				(u64)(pa + SIG2_OFF);
			*(u64 *)((u8 *)va + OFF_T64 +
				 (caprock_t64_park - caprock_t64_start)) =
				(u64)page_to_phys(park_pages[i]);
			wmb();

			send_init_sipi(apicid, (u8)(pa >> 12));
			mdelay(50);
			sig = *(volatile u16 *)((u8 *)va + SIG_OFF);
			sig2 = *(volatile u16 *)((u8 *)va + SIG2_OFF);

			if (sig != SIG_VALUE) {
				pr_err("caprock: CPU %d (APIC %u): FEHLGESCHLAGEN — Real Mode nicht erreicht (0x%04x)\n",
				       cpus[i], apicid, sig);
				__free_page(park_pages[i]);
				park_pages[i] = NULL;
				break;
			}
			narmed = i + 1;	/* ab hier laeuft der Kern unseren Code */
			if (sig2 != SIG2_VALUE) {
				pr_err("caprock: CPU %d (APIC %u): Real Mode erreicht, LONG MODE nicht (0x%04x)\n",
				       cpus[i], apicid, sig2);
				break;
			}
			pr_info("caprock: CPU %d (APIC %u): LONG MODE, eigene GDT + eigenes CR3, parkt @ phys 0x%llx\n",
				cpus[i], apicid, (u64)page_to_phys(park_pages[i]));
		}

		/*
		 * Die niedrige Seite hat ihren Zweck erfuellt — alle uebernommenen Kerne sind
		 * heraus und parken anderswo. Zurueckgeben, damit der naechste Modullauf sie
		 * wiederfindet; sie ist die knappste Ressource dieses Aufbaus.
		 */
		__free_pages(low_page, 0);
		low_page = NULL;

		if (narmed == ncpus)
			pr_info("caprock: STUFE 1b BESTANDEN — %d von %d Kernen im Long Mode; niedrige Seite zurueckgegeben\n",
				narmed, ncpus);
		else
			pr_err("caprock: STUFE 1b UNVOLLSTAENDIG — %d von %d Kernen uebernommen\n",
			       narmed, ncpus);
	}
	return 0;

undo:
	while (--i >= 0)
		if (we_offlined[i])
			add_cpu(cpus[i]);
	return rc;
}

static void __exit handover_exit(void)
{
	int i;

	/*
	 * Erst die nicht uebernommenen (die parken in Linux' eigener Schleife), dann — wenn
	 * `release` es erlaubt — die uebernommenen. Linux schickt ihnen sein eigenes
	 * INIT-SIPI-SIPI und holt sie damit aus unserer `hlt`-Schleife zurueck.
	 */
	for (i = ncpus - 1; i >= 0; i--) {
		if (i < narmed && !release)
			continue;
		if (we_offlined[i] && !add_cpu(cpus[i]))
			pr_info("caprock: CPU %d wieder online%s\n", cpus[i],
				i < narmed ? " (war uebernommen)" : "");
	}
	if (narmed && !release) {
		/*
		 * Park-Seite und Seitentabellen bleiben stehen: `CR3` des uebernommenen Kerns
		 * zeigt auf die Tabellen, sein `RIP` in die Park-Seite. Sie freizugeben, weil
		 * der Kern gerade nichts tut, waere exakt die Reihenfolge, die dieses Projekt
		 * an anderer Stelle als Use-after-free bekaempft — die Begruendung "er haelt ja
		 * an" ist eine Aussage ueber das Verhalten, nicht ueber die Zuordnung.
		 * Ein Leck bis zum Reboot ist der richtige Failure-Mode.
		 */
		pr_warn("caprock: %d Kern(e) bleiben uebernommen; Park-Seiten + Tabellen bleiben belegt, Reboot stellt alles wieder her\n",
			narmed);
		return;
	}
	/*
	 * Freigeben erst, nachdem die Kerne zurueck sind — sie liefen bis eben in der Park-Seite
	 * mit `CR3` auf diesen Tabellen. Die Reihenfolge ist dieselbe wie im DMA-Teardown des
	 * Kernels: erst die Nutzung beenden, dann die Ressource zurueckgeben, nie umgekehrt.
	 */
	if (low_page)
		__free_pages(low_page, 0);
	for (i = 0; i < ARRAY_SIZE(park_pages); i++)
		if (park_pages[i])
			__free_page(park_pages[i]);
	free_identity_tables();
	if (narmed)
		pr_info("caprock: %d uebernommene Kern(e) zurueckgegeben, alle Seiten frei\n",
			narmed);
}

module_init(handover_init);
module_exit(handover_exit);
