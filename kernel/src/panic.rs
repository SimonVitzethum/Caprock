//! Panic-Handler. Bare-metal: kein Unwinding. Wir geben die Meldung lock-frei
//! aus (der reguläre Konsolen-Lock könnte gerade gehalten werden) und halten an.

use core::panic::PanicInfo;
use sel4lake_hal as hal;

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    hal::console::emit_raw("\n[KERNEL PANIC] ");
    hal::console::emit_fmt(format_args!("{info}"));
    hal::console::emit_raw("\n");
    hal::cpu::halt();
}
