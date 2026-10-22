ENTRY(_start)

SECTIONS
{
    . = 0x40080000; /* RAM base + 0x80000: where QEMU loads a -kernel image (DTB sits at RAM base) */

    .text : {
        KEEP(*(.text.boot))
        *(.text .text.*)
    }

    .rodata : { *(.rodata .rodata.*) }
    .eh_frame : { KEEP(*(.eh_frame)) } /* Unwind info, used by debuggers for backtraces */
    .data : { *(.data .data.*) }

    /* Zeroed by startup.S */
    .bss (NOLOAD) : {
        . = ALIGN(16);
        __bss_start = .;
        *(.bss .bss.*)
        *(COMMON)
        . = ALIGN(16);
        __bss_end = .;
    }

    . = ALIGN(16);
    . += 0x4000; /* Allocate 16KB stack space */
    _stack_top = .;
}
