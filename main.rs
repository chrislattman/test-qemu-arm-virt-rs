#![no_std]
#![no_main]
// Debug builds compile out the sleeps and interrupts in main_app, leaving their helpers unused
#![cfg_attr(debug_assertions, allow(dead_code))]

use arm_gic::gicv2::registers::{Gicc, Gicd};
use arm_gic::gicv2::{GicV2, SgiTarget, SgiTargetListFilter};
use arm_gic::{IntId, InterruptGroup};
use arm_pl011_uart::{PL011Registers, Uart, UniqueMmioPointer};
use core::arch::asm;
use core::cell::UnsafeCell;
use core::fmt::Write;
use core::panic::PanicInfo;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicU32, Ordering};

// Core Entry Point Setup
core::arch::global_asm!(include_str!("startup.S"));

// --- QEMU virt Memory Map ---
const UART0_BASE: usize = 0x0900_0000; // PL011 UART
const GIC_DIST_BASE: usize = 0x0800_0000; // GICv2 Distributor Base Address
const GIC_CPU_BASE: usize = 0x0801_0000; // GICv2 CPU Interface Base Address

// Assigning SGI IDs
const HW_INT_SGI_ID: IntId = IntId::sgi(1);
const SW_INT_SGI_ID: IntId = IntId::sgi(2);

// Shared variable across foreground code and async ISR layers
static SHARED_DATA: AtomicU32 = AtomicU32::new(0);

/// A driver used by both main_app and handle_irq. The drivers need `&mut self`, so access is
/// only handed out with IRQs masked, which on this single core makes it exclusive.
struct IrqShared<T>(UnsafeCell<T>);

// SAFETY: Only core 0 runs Rust code, and `with` masks the only other context (IRQs)
unsafe impl<T> Sync for IrqShared<T> {}

impl<T> IrqShared<T> {
    const fn new(value: T) -> Self {
        Self(UnsafeCell::new(value))
    }

    /// Runs `f` with IRQs masked, then restores the previous mask (so it's safe inside handle_irq).
    /// `f` must not call `with` on the same value again.
    fn with<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        let daif: u64;
        // No `nomem` option, so these also act as compiler memory barriers
        unsafe {
            asm!("mrs {}, daif", "msr daifset, #2", out(reg) daif, options(nostack));
        }
        let result = f(unsafe { &mut *self.0.get() });
        unsafe {
            asm!("msr daif, {}", in(reg) daif, options(nostack));
        }
        result
    }
}

// QEMU leaves the PL011 enabled at reset, so it can be used without calling Uart::enable()
static UART: IrqShared<Uart<'static>> = IrqShared::new(Uart::new(
    // SAFETY: UART0_BASE is the PL011 on QEMU virt, the MMU is off (so it's device memory),
    // and nothing else accesses it
    unsafe { UniqueMmioPointer::new(NonNull::new_unchecked(UART0_BASE as *mut PL011Registers)) },
));

// GicV2::new isn't const, so the GIC is created in gic_init
static GIC: IrqShared<Option<GicV2<'static>>> = IrqShared::new(None);

fn uart_puts(s: &str) {
    UART.with(|uart| uart.write_str(s).unwrap());
}

fn with_gic<R>(f: impl FnOnce(&mut GicV2<'static>) -> R) -> R {
    GIC.with(|gic| f(gic.as_mut().expect("gic_init not called")))
}

fn sleep_ms(ms: u32) {
    let frq: u64;
    let start: u64;
    let mut current: u64;

    unsafe {
        asm!("mrs {}, cntfrq_el0", out(reg) frq);
        asm!("mrs {}, cntpct_el0", out(reg) start);
    }

    let ticks_needed = (frq / 1000) * ms as u64;

    loop {
        unsafe {
            asm!("mrs {}, cntpct_el0", out(reg) current);
        }
        if (current - start) >= ticks_needed {
            break;
        }
    }
}

// No `nomem` option, so these also act as compiler memory barriers
// (arm_gic::irq_enable/irq_disable use `nomem`, so they don't)
fn enable_interrupts() {
    unsafe {
        asm!("msr daifclr, #2", options(nostack));
    }
}

fn disable_interrupts() {
    unsafe {
        asm!("msr daifset, #2", options(nostack));
    }
}

fn trigger_sgi(sgi_id: IntId) {
    // Forward strictly to the current requesting CPU core
    let target = SgiTarget::List {
        target_list_filter: SgiTargetListFilter::ForwardSelfOnly,
        target_list: 0,
    };
    with_gic(|gic| gic.send_sgi(sgi_id, target));
}

// The Unified Interrupt Vector Handler, called from irq_entry in startup.S
#[unsafe(no_mangle)]
pub extern "C" fn handle_irq() {
    // Read the Interrupt Acknowledge Register to get the pending ID
    let Some(interrupt_id) =
        with_gic(|gic| gic.get_and_acknowledge_interrupt(InterruptGroup::Group0))
    else {
        return; // Spurious interrupt (ID 1023), nothing to end
    };

    if interrupt_id == HW_INT_SGI_ID {
        uart_puts("[ISR] 'Hardware' Interrupt tripped! Writing data to shared space...\r\n");
        SHARED_DATA.store(0xAABB_CCDD, Ordering::Relaxed); // Write to shared variable

        // Instantly trigger our secondary nested "Software" interrupt handling line
        trigger_sgi(SW_INT_SGI_ID);
    } else if interrupt_id == SW_INT_SGI_ID {
        uart_puts(
            "[ISR] 'Software' Interrupt tripped! Processing data inside a Critical Section...\r\n",
        );

        // --- CRITICAL SECTION BEGIN ---
        disable_interrupts(); // Lock out incoming requests safely
        let local_copy = SHARED_DATA.load(Ordering::Relaxed);

        // Print proof of successful read
        UART.with(|uart| {
            write!(
                uart,
                "      Critical Read complete! Value is: 0x{local_copy:08X}\r\n"
            )
            .unwrap()
        });

        enable_interrupts();
        // --- CRITICAL SECTION END ---
    }

    // Signal completion by writing back the processed ID to End of Interrupt register
    with_gic(|gic| gic.end_interrupt(interrupt_id, InterruptGroup::Group0));
}

fn gic_init() {
    // SAFETY: These are the GICv2 distributor and CPU interface on QEMU virt, the MMU is off
    // (so they're device memory), and this is the only driver instance
    let mut gic = unsafe { GicV2::new(GIC_DIST_BASE as *mut Gicd, GIC_CPU_BASE as *mut Gicc) };

    // QEMU virt's GIC has no Security Extensions unless run with `-M virt,secure=on`, so setup()
    // puts every interrupt in Group 0, enables Group 0, and sets the priority mask to 0xFF.
    // (Group 1 interrupts can't be acknowledged via GICC_IAR, which would cause an IRQ storm.)
    gic.setup();
    assert!(
        !gic.typer().has_security_extension(),
        "SGIs must be in Group 0"
    );

    gic.enable_interrupt(HW_INT_SGI_ID, true).unwrap();
    gic.enable_interrupt(SW_INT_SGI_ID, true).unwrap();

    GIC.with(|g| *g = Some(gic));
}

#[unsafe(no_mangle)]
pub extern "C" fn main_app() -> ! {
    #[cfg(not(debug_assertions))]
    {
        gic_init();
        enable_interrupts(); // Unmask IRQs inside the Cortex-A53 core
    }
    loop {
        uart_puts("Hello\r\n");
        #[cfg(not(debug_assertions))]
        sleep_ms(1000);
        uart_puts("World!\r\n");
        #[cfg(not(debug_assertions))]
        {
            trigger_sgi(HW_INT_SGI_ID); // Kick off the hardware -> software cascade
            sleep_ms(1000);
        }
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    disable_interrupts();
    // Take over the UART rather than going through UART.with, since the panic may have
    // happened inside it. This never returns, so the interrupted access is never resumed.
    // SAFETY: As for UART, and the previous owner is never used again
    let mut uart = Uart::new(unsafe {
        UniqueMmioPointer::new(NonNull::new_unchecked(UART0_BASE as *mut PL011Registers))
    });
    let _ = write!(
        uart,
        "\r\n!!! PANIC TRIGGERED IN RUST ENGINE !!!\r\n{info}\r\n"
    );
    loop {}
}
