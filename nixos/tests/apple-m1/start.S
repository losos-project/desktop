/*
 * Entry, exception vectors and the EL0 trips for the Apple M1 / PAC test.
 *
 * The vmapple machine starts this at EL1 with the MMU off.  EL0 code here
 * touches no memory: with the MMU off its data accesses would be Device
 * memory.  Each EL0 routine does one pointer-auth instruction on x0 with
 * x1 as the modifier and comes back with SVC.
 */

    .section .text.boot, "ax"
    .global _start
_start:
    ldr     x0, =0x70100000
    mov     sp, x0
    adr     x0, vectors
    msr     vbar_el1, x0
    isb
    bl      main
1:  wfi
    b       1b

/* uint64_t run_el0(void (*fn)(void), uint64_t x0, uint64_t x1) */
    .text
    .global run_el0
run_el0:
    ldr     x9, =el1_save
    stp     x19, x20, [x9, #0]
    stp     x21, x22, [x9, #16]
    stp     x23, x24, [x9, #32]
    stp     x25, x26, [x9, #48]
    stp     x27, x28, [x9, #64]
    stp     x29, x30, [x9, #80]
    mov     x10, sp
    str     x10, [x9, #96]
    msr     elr_el1, x0
    mov     x10, #0x3c0             /* EL0t, interrupts masked */
    msr     spsr_el1, x10
    mov     x0, x1
    mov     x1, x2
    eret

/* SVC from EL0: hand x0 back to run_el0's caller. */
el0_returned:
    ldr     x9, =el1_save
    ldp     x19, x20, [x9, #0]
    ldp     x21, x22, [x9, #16]
    ldp     x23, x24, [x9, #32]
    ldp     x25, x26, [x9, #48]
    ldp     x27, x28, [x9, #64]
    ldp     x29, x30, [x9, #80]
    ldr     x10, [x9, #96]
    mov     sp, x10
    ret

unexpected:
    mrs     x0, esr_el1
    mrs     x1, elr_el1
    mrs     x2, far_el1
    bl      unexpected_exception
2:  wfi
    b       2b

    .global el0_pacia, el0_autia, el0_pacib, el0_autib, el0_pacga
el0_pacia:
    pacia   x0, x1
    svc     #0
el0_autia:
    autia   x0, x1
    svc     #0
el0_pacib:
    pacib   x0, x1
    svc     #0
el0_autib:
    autib   x0, x1
    svc     #0
el0_pacga:
    pacga   x0, x0, x1
    svc     #0

    .balign 2048
vectors:
    .rept   8                       /* current EL, SP0 and SPx */
    b       unexpected
    .balign 128
    .endr
    b       lower_el_sync           /* lower EL, AArch64, synchronous */
    .balign 128
    .rept   7
    b       unexpected
    .balign 128
    .endr

lower_el_sync:
    mrs     x9, esr_el1
    lsr     x9, x9, #26
    cmp     x9, #0x15               /* SVC from AArch64 */
    b.ne    unexpected
    b       el0_returned

    .bss
    .balign 16
el1_save:
    .skip   112
