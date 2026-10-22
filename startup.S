.section ".text.boot", "ax"
.global _start

_start:
    .cfi_startproc
    .cfi_undefined x30          // Outermost frame for debuggers
    // Force CPU cores other than Core 0 into an infinite sleep loop
    mrs     x0, mpidr_el1
    and     x0, x0, #0xFF
    cbz     x0, master_cpu
hang:
    wfe
    b       hang

master_cpu:
    // Set the Vector Base Address Register to point to our table defined below
    ldr     x0, =vector_table
    msr     vbar_el1, x0

    // CRUCIAL: Explicitly clear DAIF bits directly in the processor state.
    // Bit 1 = FIQ Mask, Bit 2 = IRQ Mask, Bit 3 = SError Mask, Bit 4 = Debug Mask.
    // Clearing them using code 0x0 unmasks ALL core level exceptions. (the GIC stays off until gic_init)
    msr     DAIFClr, #0xf

    // Set up a temporary safe stack pointer region inside RAM
    ldr     x0, =_stack_top
    mov     sp, x0

    // Zero .bss, which Rust requires for zero-initialized statics
    ldr     x0, =__bss_start
    ldr     x1, =__bss_end
zero_bss:
    cmp     x0, x1
    b.hs    zero_bss_done
    str     xzr, [x0], #8
    b       zero_bss
zero_bss_done:

    // Jump straight to main architecture loop
    bl      main_app

    // Defensive fallback hang loop
    b       hang
    .cfi_endproc

// --- ARMv8 Exception Vector Table Alignment Layout ---
.align 11
vector_table:
    // Current Exception Level with SP_EL0 (Unused offsets 0x000 - 0x180)
    .align 7; b hang; .align 7; b hang; .align 7; b hang; .align 7; b hang

    // Current Exception Level with SP_ELx (Offsets 0x200 - 0x380)
    .align 7; b hang            // Synchronous
    .align 7; b irq_entry       // IRQ (This is where our GIC routing triggers!)
    .align 7; b hang            // FIQ
    .align 7; b hang            // SError

    // Lower Exception Level AArch64 (Offsets 0x400 - 0x580)
    .align 7; b hang; .align 7; b hang; .align 7; b hang; .align 7; b hang

    // Lower Exception Level AArch32 (Offsets 0x600 - 0x780)
    .align 7; b hang; .align 7; b hang; .align 7; b hang; .align 7; b hang

irq_entry:
    .cfi_startproc
    .cfi_undefined x30          // Outermost frame for debuggers: return address is in ELR_EL1, not x30
    // Context Switch Save Pass: Push caller-saved registers onto the stack
    sub     sp, sp, #256
    stp     x0, x1, [sp, #0]
    stp     x2, x3, [sp, #16]
    stp     x4, x5, [sp, #32]
    stp     x6, x7, [sp, #48]
    stp     x8, x9, [sp, #64]
    stp     x10, x11, [sp, #80]
    stp     x12, x13, [sp, #96]
    stp     x14, x15, [sp, #112]
    stp     x16, x17, [sp, #128]
    str     x18, [sp, #144]
    stp     x29, x30, [sp, #240] // Save Link Register

    bl      handle_irq          // Run our Rust function

    // Context Switch Restore Pass: Pop registers cleanly off stack memory
    ldp     x29, x30, [sp, #240]
    ldr     x18, [sp, #144]
    ldp     x16, x17, [sp, #128]
    ldp     x14, x15, [sp, #112]
    ldp     x12, x13, [sp, #96]
    ldp     x10, x11, [sp, #80]
    ldp     x8, x9, [sp, #64]
    ldp     x6, x7, [sp, #48]
    ldp     x4, x5, [sp, #32]
    ldp     x2, x3, [sp, #16]
    ldp     x0, x1, [sp, #0]
    add     sp, sp, #256
    eret                        // Exception Return back to your super loop
    .cfi_endproc
