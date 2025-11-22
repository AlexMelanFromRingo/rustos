/* Linker script for RustOS user programs */

OUTPUT_FORMAT("elf64-x86-64")
OUTPUT_ARCH(i386:x86-64)
ENTRY(_start)

SECTIONS
{
    /* Start at user space address (must match RustOS USER_SPACE_START) */
    . = 0x400000;

    /* Code section */
    .text : {
        *(.text._start)
        *(.text)
        *(.text.*)
    }

    /* Read-only data */
    .rodata : {
        *(.rodata)
        *(.rodata.*)
    }

    /* Data section */
    .data : {
        *(.data)
        *(.data.*)
    }

    /* BSS section (uninitialized data) */
    .bss : {
        *(.bss)
        *(.bss.*)
        *(COMMON)
    }

    /* Discard unnecessary sections */
    /DISCARD/ : {
        *(.eh_frame)
        *(.note.GNU-stack)
        *(.comment)
        *(.note.gnu.property)
    }
}
