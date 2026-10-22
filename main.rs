#![no_std]
#![no_main]
// Debug builds compile out the sleeps and interrupts in main_app, leaving their helpers unused
#![cfg_attr(debug_assertions, allow(dead_code))]

use core::arch::asm;
use core::panic::PanicInfo;
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicU32, Ordering};

// Core Entry Point Setup
core::arch::global_asm!(include_str!("startup.S"));

// --- UART PL011 Constants ---
const UART0_BASE: usize = 0x0900_0000;
const UART0_DR: *mut u32 = UART0_BASE as *mut u32;
const UART0_FR: *const u32 = (UART0_BASE + 0x18) as *const u32;
const UART_FR_TXFF: u32 = 1 << 5;

// --- GICv2 (QEMU virt Memory Map) ---
const GIC_DIST_BASE: usize = 0x0800_0000; // Distributor Base Address
const GIC_CPU_BASE: usize = 0x0801_0000; // CPU Interface Base Address

// Distributor Registers
const GICD_CTLR: *mut u32 = GIC_DIST_BASE as *mut u32;
const GICD_ISENABLER: *mut u32 = (GIC_DIST_BASE + 0x100) as *mut u32;
const GICD_SGIR: *mut u32 = (GIC_DIST_BASE + 0xF00) as *mut u32;

// CPU Interface Registers
const GICC_CTLR: *mut u32 = GIC_CPU_BASE as *mut u32;
const GICC_PMR: *mut u32 = (GIC_CPU_BASE + 0x004) as *mut u32;
const GICC_IAR: *const u32 = (GIC_CPU_BASE + 0x00C) as *const u32;
const GICC_EOIR: *mut u32 = (GIC_CPU_BASE + 0x010) as *mut u32;

// Assigning SGI IDs
const HW_INT_SGI_ID: u32 = 1;
const SW_INT_SGI_ID: u32 = 2;

// Shared variable across foreground code and async ISR layers
static SHARED_DATA: AtomicU32 = AtomicU32::new(0);

fn uart_putc(byte: u8) {
    unsafe {
        // Wait until UART TX FIFO has space
        while (read_volatile(UART0_FR) & UART_FR_TXFF) != 0 {}
        write_volatile(UART0_DR, byte as u32);
    }
}

fn uart_puts(s: &str) {
    for byte in s.bytes() {
        uart_putc(byte);
    }
}

fn uart_put_hex(value: u32) {
    for i in (0..8).rev() {
        let nibble = ((value >> (i * 4)) & 0xF) as u8;
        uart_putc(if nibble < 10 {
            b'0' + nibble
        } else {
            b'A' + (nibble - 10)
        });
    }
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

fn trigger_sgi(sgi_id: u32) {
    // Bits [25:24]: Target List Filter (0b10 = forward strictly to the current requesting CPU core)
    // Bits [3:0]: SGI Interrupt ID Number
    unsafe {
        write_volatile(GICD_SGIR, (2 << 24) | (sgi_id & 0xF));
    }
}

// The Unified Interrupt Vector Handler, called from irq_entry in startup.S
#[unsafe(no_mangle)]
pub extern "C" fn handle_irq() {
    // Read the Interrupt Acknowledge Register to get the pending ID
    let iar = unsafe { read_volatile(GICC_IAR) };
    let interrupt_id = iar & 0x3FF; // Isolate lower 10 bits

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
        uart_puts("      Critical Read complete! Value is: 0x");
        uart_put_hex(local_copy);
        uart_puts("\r\n");

        enable_interrupts();
        // --- CRITICAL SECTION END ---
    }

    // Signal completion by writing back the processed ID to End of Interrupt register
    unsafe {
        write_volatile(GICC_EOIR, iar);
    }
}

fn gic_init() {
    unsafe {
        write_volatile(GICD_CTLR, 0); // Disable distributor for configuration
        write_volatile(GICC_CTLR, 0); // Disable CPU interface for configuration

        // Leave SGIs in Group 0 (the reset default). GICD_CTLR/GICC_CTLR = 1 only
        // enable Group 0, and Group 1 interrupts can't be acknowledged via GICC_IAR
        // without GICC_CTLR.AckCtl, which would cause an endless IRQ storm.

        // Enable SGI 1 and SGI 2 (Bits 1 and 2 in the first ISENABLER register)
        write_volatile(GICD_ISENABLER, (1 << HW_INT_SGI_ID) | (1 << SW_INT_SGI_ID));

        // Set Priority Mask Register to allow all interrupts through (0xFF)
        write_volatile(GICC_PMR, 0xFF);

        // Turn both the distributor and CPU interface back on
        write_volatile(GICD_CTLR, 1);
        write_volatile(GICC_CTLR, 1);
    }
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
fn panic(_info: &PanicInfo) -> ! {
    uart_puts("\r\n!!! PANIC TRIGGERED IN RUST ENGINE !!!\r\n");
    loop {}
}
