/* Linker script for the example payload.
 *
 * Linked at 0x200000 — the loader's default writable-window base (WINDOW_MIN).
 * The loader reads this link address and the entry point from the ELF, transfers
 * the loadable bytes, zero-fills the BSS, sets SP, and jumps to `_start`. See
 * ../docs/ENTRY_CONTRACT.md for the state established at entry.
 */

ENTRY(_start)

PAYLOAD_BASE = 0x200000;

SECTIONS
{
    . = PAYLOAD_BASE;

    .text   : ALIGN(8) { *(.text .text.*) }
    .rodata : ALIGN(8) { *(.rodata .rodata.*) }
    .data   : ALIGN(8) { *(.data .data.*) }

    . = ALIGN(16);
    .bss (NOLOAD) : ALIGN(16)
    {
        *(.bss .bss.*)
        *(COMMON)
    }

    /DISCARD/ : { *(.comment) *(.note*) *(.eh_frame*) *(.eh_frame_hdr) }
}
