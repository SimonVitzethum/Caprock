//! Panic-Handler. Bare-metal: kein Unwinding. Wir geben die Meldung lock-frei
//! aus (der reguläre Konsolen-Lock könnte gerade gehalten werden) und halten an.

use core::panic::PanicInfo;

#[cfg(target_arch = "aarch64")]
use sel4lake_hal as hal;

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    #[cfg(target_arch = "aarch64")]
    {
        hal::console::emit_raw("\n[KERNEL PANIC] ");
        hal::console::emit_fmt(format_args!("{info}"));
        hal::console::emit_raw("\n");
        hal::cpu::halt();
    }
    #[cfg(target_arch = "x86_64")]
    {
        crate::arch::x86_64::emit_raw("\n[KERNEL PANIC] ");
        crate::arch::x86_64::emit_fmt(format_args!("{info}"));
        crate::arch::x86_64::emit_raw("\n");
        crate::arch::x86_64::halt();
    }
}
