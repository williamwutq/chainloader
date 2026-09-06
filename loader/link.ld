/* Linker script for the Raspberry Pi 2 AArch64 chainloader.
 *
 * The Pi firmware loads `kernel8.img` (a flat binary) to 0x80000 and enters at
 * its first byte on core 0. `.text.boot` is placed first so `_start` sits
 * exactly at 0x80000.
 *
 * The `__loader_start`/`__loader_end` symbols bracket everything the loader
 * occupies — code, data, BSS, and its own stack — so the receive path can
 * reject any image whose destination window would overlap the running loader.
 */

ENTRY(_start)

/* Firmware entry point for 64-bit kernels. */
LOADER_BASE = 0x80000;
/* Descending stack of this size is reserved above BSS. */
STACK_SIZE  = 0x80000; /* 512 KiB */

SECTIONS
{
    . = LOADER_BASE;
    __loader_start = .;

    .text : ALIGN(8)
    {
        KEEP(*(.text.boot))
        *(.text .text.*)
    }

    .rodata : ALIGN(8) { *(.rodata .rodata.*) }
    .data   : ALIGN(8) { *(.data .data.*) }

    . = ALIGN(16);
    __bss_start = .;
    .bss (NOLOAD) : ALIGN(16)
    {
        *(.bss .bss.*)
        *(COMMON)
    }
    . = ALIGN(16);
    __bss_end = .;

    /* Stack grows down from __stack_top toward __bss_end. */
    . = ALIGN(16);
    . = . + STACK_SIZE;
    . = ALIGN(16);
    __stack_top = .;

    __loader_end = .;

    /DISCARD/ : { *(.comment) *(.note*) *(.eh_frame*) *(.eh_frame_hdr) }
}
