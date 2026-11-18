/* Banna OS x86_64 kernel linker script. */
OUTPUT_FORMAT(elf64-x86-64)
OUTPUT_ARCH(i386:x86-64)
ENTRY(_start)

PHDRS
{
    text   PT_LOAD FLAGS(5); /* R | X */
    rodata PT_LOAD FLAGS(4); /* R     */
    data   PT_LOAD FLAGS(6); /* R | W */
}

SECTIONS
{
    /* Limine v8 yüklenebilir bölümleri canonical üst yarıda ister. */
    . = 0xffffffff80000000;

    .text ALIGN(4K) : {
        *(.text .text.*)
    } :text

    .rodata ALIGN(4K) : {
        *(.rodata .rodata.*)
        . = ALIGN(8);
        __initcall_start = .;
        KEEP(*(SORT_BY_NAME(.initcall.*)))
        __initcall_end = .;
    } :rodata

    .data ALIGN(4K) : {
        KEEP(*(.requests_start_marker))
        KEEP(*(.requests))
        KEEP(*(.requests_end_marker))
        *(.data .data.*)
    } :data

    .bss : {
        *(COMMON)
        *(.bss .bss.*)
    } :data

    __kernel_end = .;

    /DISCARD/ : {
        *(.eh_frame*)
        *(.note .note.*)
        *(.comment)
    }
}
