/* The firmware window is read-only; state and the stack live in RAM. */
ENTRY(_start)
SECTIONS
{
    . = 0x100000;
    .text : { *(.text.boot) *(.text*) *(.rodata*) }
    . = 0x70000000;
    .bss (NOLOAD) : { *(.bss*) *(COMMON) }
    /DISCARD/ : { *(.data*) *(.comment) *(.note*) *(.eh_frame*) }
}
