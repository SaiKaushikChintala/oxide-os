.global trampoline_start
.global trampoline_end
.global trampoline_cr3
.global trampoline_stack_top
.global trampoline_entry

.set TRAMPOLINE_PHYS, 0x8000

.section .text
.code16
trampoline_start:
    cli
    xor ax, ax
    mov ds, ax
    mov es, ax
    mov ss, ax

    lgdt [ABS_GDT_PTR]

    mov eax, cr0
    or eax, 1
    mov cr0, eax

    .byte 0xEA
    .word ABS_PROTECTED_MODE
    .word 0x08

.code32
protected_mode:
    mov ax, 0x10
    mov ds, ax
    mov es, ax
    mov ss, ax

    mov eax, cr4
    or eax, (1 << 5)
    mov cr4, eax

    mov eax, [ABS_TRAMPOLINE_CR3]
    mov cr3, eax

    mov ecx, 0xC0000080
    rdmsr
    or eax, (1 << 8) | (1 << 11)
    wrmsr

    mov eax, cr0
    or eax, (1 << 31)
    mov cr0, eax

    .byte 0xEA
    .long ABS_LONG_MODE
    .word 0x18

.code64
long_mode:
    xor ax, ax
    mov ds, ax
    mov es, ax
    mov ss, ax

    mov rsp, [ABS_TRAMPOLINE_STACK_TOP]
    mov rax, [ABS_TRAMPOLINE_ENTRY]
    jmp rax

.align 8
gdt_start:
    .quad 0x0000000000000000
    .quad 0x00CF9A000000FFFF
    .quad 0x00CF92000000FFFF
    .quad 0x00AF9A000000FFFF
gdt_end:
gdt_ptr:
    .word gdt_end - gdt_start - 1
    .long ABS_GDT_START

.align 8
trampoline_cr3:
    .quad 0
trampoline_stack_top:
    .quad 0
trampoline_entry:
    .quad 0

trampoline_end:

.set ABS_GDT_PTR, TRAMPOLINE_PHYS + (gdt_ptr - trampoline_start)
.set ABS_GDT_START, TRAMPOLINE_PHYS + (gdt_start - trampoline_start)
.set ABS_PROTECTED_MODE, TRAMPOLINE_PHYS + (protected_mode - trampoline_start)
.set ABS_LONG_MODE, TRAMPOLINE_PHYS + (long_mode - trampoline_start)
.set ABS_TRAMPOLINE_CR3, TRAMPOLINE_PHYS + (trampoline_cr3 - trampoline_start)
.set ABS_TRAMPOLINE_STACK_TOP, TRAMPOLINE_PHYS + (trampoline_stack_top - trampoline_start)
.set ABS_TRAMPOLINE_ENTRY, TRAMPOLINE_PHYS + (trampoline_entry - trampoline_start)
