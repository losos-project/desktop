/*
 * Checks the apple-m1 CPU and Apple's paravirtual pointer authentication
 * on the vmapple machine under TCG, with no Apple software involved.
 *
 * It makes the hypercalls XNU's VMAPPLE kernel makes (osfmk/arm64/hv_hvc.h)
 * and checks what a kernel relies on: keys set through them sign and
 * authenticate, EL0's pointers do not authenticate at EL1 unless the kernel
 * borrows EL0's keys, and changing a key breaks every pointer signed with
 * the old one.  It prints TAP-style lines on the PL011 and powers off.
 */

/* Built with -nostdinc: a freestanding firmware needs no C library. */
typedef unsigned int uint32_t;
typedef unsigned long uint64_t;
typedef _Bool bool;
#define true 1
#define false 0

#define UART_BASE 0x20010000UL
#define UART_DR   0x00
#define UART_FR   0x18
#define UART_CR   0x30
#define UART_FR_TXFF (1u << 5)

#define PSCI_SYSTEM_OFF 0x84000008UL

#define PAC_SET_INITIAL_STATE          0xc1000000UL
#define PAC_GET_DEFAULT_KEYS           0xc1000001UL
#define PAC_SET_A_KEYS                 0xc1000002UL
#define PAC_SET_B_KEYS                 0xc1000003UL
#define PAC_SET_EL0_DIVERSIFIER        0xc1000004UL
#define PAC_SET_EL0_DIVERSIFIER_AT_EL1 0xc1000005UL
#define PAC_SET_G_KEY                  0xc1000006UL
#define PAC_NOP                        0xc10000f0UL

#define KEY_BASE 0xfeedfacefeedfacfUL

/* SCTLR_EL1.EnIA, EnIB, EnDA, EnDB */
#define SCTLR_PAC_KEYS ((1UL << 31) | (1UL << 30) | (1UL << 27) | (1UL << 13))

/* A pointer in RAM, and the modifier every signature here uses */
#define PTR 0x70001230UL
#define MOD 0x42UL

uint64_t run_el0(void (*fn)(void), uint64_t x0, uint64_t x1);
void el0_pacia(void);
void el0_autia(void);
void el0_pacib(void);
void el0_autib(void);
void el0_pacga(void);

static volatile uint32_t *const uart = (volatile uint32_t *)UART_BASE;
static int tests, failures;

static void putc_(char c)
{
    while (uart[UART_FR / 4] & UART_FR_TXFF) {
    }
    uart[UART_DR / 4] = c;
}

static void puts_(const char *s)
{
    while (*s) {
        putc_(*s++);
    }
}

static void puthex(uint64_t v)
{
    puts_("0x");
    for (int i = 60; i >= 0; i -= 4) {
        putc_("0123456789abcdef"[(v >> i) & 0xf]);
    }
}

/*
 * Built with -DGENERIC_KVM_HOST for KVM on an arm64 host that is not a Mac:
 * the CPU is the host's, and without Apple's KERNKEY register EL0 shares
 * EL1's keys, so the checks that need separate EL0 keys are skipped.
 */
#ifdef GENERIC_KVM_HOST
#define SEPARATE_EL0_KEYS false
#else
#define SEPARATE_EL0_KEYS true
#endif

static void check(bool ok, const char *what)
{
    tests++;
    if (!ok) {
        failures++;
        puts_("not ");
    }
    puts_("ok - ");
    puts_(what);
    putc_('\n');
}

struct hvc_result {
    uint64_t x0, x1, x2, x3, x4;
};

static struct hvc_result hvc(uint64_t fn, uint64_t a1, uint64_t a2)
{
    register uint64_t x0 asm("x0") = fn;
    register uint64_t x1 asm("x1") = a1;
    register uint64_t x2 asm("x2") = a2;
    register uint64_t x3 asm("x3") = 0;
    register uint64_t x4 asm("x4") = 0;

    asm volatile("hvc #0"
                 : "+r"(x0), "+r"(x1), "+r"(x2), "+r"(x3), "+r"(x4)
                 :
                 : "memory");
    return (struct hvc_result){ x0, x1, x2, x3, x4 };
}

static uint64_t call(uint64_t fn, uint64_t a1, uint64_t a2)
{
    return hvc(fn, a1, a2).x0;
}

#define PAC_OP(name, insn)                                            \
    static uint64_t name(uint64_t p)                                  \
    {                                                                 \
        asm volatile(insn " %0, %1" : "+r"(p) : "r"(MOD));            \
        return p;                                                     \
    }

PAC_OP(pacia, "pacia")
PAC_OP(autia, "autia")
PAC_OP(pacib, "pacib")
PAC_OP(autib, "autib")

static uint64_t pacga(uint64_t p)
{
    uint64_t r;

    asm volatile("pacga %0, %1, %2" : "=r"(r) : "r"(p), "r"(MOD));
    return r;
}

#define READ_SYSREG(name)                                             \
    ({                                                                \
        uint64_t v_;                                                  \
        asm volatile("mrs %0, " #name : "=r"(v_));                    \
        v_;                                                           \
    })

static unsigned field(uint64_t reg, int shift)
{
    return (reg >> shift) & 0xf;
}

static void check_cpu(void)
{
    uint64_t pfr0 = READ_SYSREG(id_aa64pfr0_el1);
    uint64_t pfr1 = READ_SYSREG(id_aa64pfr1_el1);
    uint64_t isar0 = READ_SYSREG(id_aa64isar0_el1);
    uint64_t isar1 = READ_SYSREG(id_aa64isar1_el1);
    uint64_t mmfr0 = READ_SYSREG(id_aa64mmfr0_el1);

    check(READ_SYSREG(midr_el1) == 0x611f0231, "MIDR is an M1 Firestorm");
    check(field(pfr0, 32) == 0, "no SVE");
    check(field(pfr1, 24) == 0, "no SME");
    check(field(pfr1, 8) == 0, "no MTE");
    check(field(pfr1, 0) == 0, "no BTI");
    check(field(pfr1, 4) == 2, "SSBS2");
    check(field(isar0, 56) == 2, "TLBI by range");
    check(field(isar0, 60) == 0, "no RNDR");
    check(field(isar1, 4) == 0 && field(isar1, 8) == 1,
          "Apple's PAuth (IMPDEF, no PAuth2 or FPAC)");
    check(field(isar1, 44) == 0 && field(isar1, 52) == 0, "no BF16, no I8MM");
    check(field(mmfr0, 24) == 0xf && field(mmfr0, 20) == 1,
          "16K pages, no 64K");
    check(field(mmfr0, 60) == 1, "ECV");
}

/* Returns whether authentication is enforced (pac-auth=on). */
static bool check_el1_keys(void)
{
    uint64_t p, q;
    bool enforced;

    p = pacia(PTR);
    check(p != PTR, "pacia inserts a code");
    check(autia(p) == PTR, "autia accepts its own signature");
    enforced = autia(p ^ (1UL << 45)) != PTR;
    puts_(enforced ? "# authentication enforced\n"
                   : "# authentication accepts every pointer\n");
    if (!enforced) {
        return false;
    }
    check(autia(pacib(PTR)) != PTR, "an IB signature fails IA");

    check(call(PAC_SET_A_KEYS, 0x1111222233334444UL, 0) == 0, "SET_A_KEYS");
    check(autia(p) != PTR, "a new A key breaks old A signatures");
    q = pacia(PTR);
    check(q != p && autia(q) == PTR, "the new A key signs and authenticates");

    p = pacib(PTR);
    check(call(PAC_SET_B_KEYS, 0x5555666677778888UL, 0) == 0, "SET_B_KEYS");
    check(autib(p) != PTR, "a new B key breaks old B signatures");
    check(autib(pacib(PTR)) == PTR, "the new B key signs and authenticates");
    return true;
}

/* A check that holds only where EL0 has keys of its own. */
static void check_el0(bool ok, const char *what)
{
    if (SEPARATE_EL0_KEYS) {
        check(ok, what);
        return;
    }
    tests++;
    puts_("ok - ");
    puts_(what);
    puts_(" # SKIP EL0 shares EL1's keys on this host\n");
}

static void check_el0_keys(void)
{
    uint64_t el0_key = KEY_BASE + 4;
    uint64_t q, r;

    q = run_el0(el0_pacia, PTR, MOD);
    check(run_el0(el0_autia, q, MOD) == PTR, "EL0 authenticates its own IA");
    check_el0(q != pacia(PTR), "EL0 signs with other keys than EL1");
    check_el0(autia(q) != PTR, "EL1 rejects an EL0 signature");
    check_el0(run_el0(el0_autia, pacia(PTR), MOD) != PTR,
          "EL0 rejects an EL1 signature");
    r = run_el0(el0_pacib, PTR, MOD);
    check_el0(autib(r) != PTR && run_el0(el0_autib, r, MOD) == PTR,
          "the diversifier covers the B keys too");

    check(call(PAC_SET_EL0_DIVERSIFIER_AT_EL1, 1, el0_key) == 0,
          "SET_EL0_DIVERSIFIER_AT_EL1 on");
    check(autia(q) == PTR, "EL1 with EL0's keys accepts EL0's signature");
    check(run_el0(el0_autia, pacia(PTR), MOD) == PTR,
          "EL0 accepts what EL1 signed with EL0's keys");
    check(call(PAC_SET_EL0_DIVERSIFIER_AT_EL1, 0, el0_key) == 0,
          "SET_EL0_DIVERSIFIER_AT_EL1 off");
    check_el0(autia(q) != PTR, "EL1 is back on its own keys");

    check(call(PAC_SET_EL0_DIVERSIFIER, 0x9999aaaabbbbccccUL, 0) == 0,
          "SET_EL0_DIVERSIFIER");
    check_el0(run_el0(el0_autia, q, MOD) != PTR,
          "another process's EL0 key rejects the first one's pointers");

    r = pacga(PTR);
    check(run_el0(el0_pacga, PTR, MOD) == r,
          "the G key is the same at EL0 and EL1");
    check(call(PAC_SET_G_KEY, 0xddddeeeeffff0000UL, 0) == 0, "SET_G_KEY");
    check(pacga(PTR) != r, "a new G key changes PACGA");
}

void unexpected_exception(uint64_t esr, uint64_t elr, uint64_t far)
{
    puts_("not ok - unexpected exception, ESR ");
    puthex(esr);
    puts_(" ELR ");
    puthex(elr);
    puts_(" FAR ");
    puthex(far);
    puts_("\nAPPLE_M1_FAIL\n");
    hvc(PSCI_SYSTEM_OFF, 0, 0);
}

int main(void)
{
    struct hvc_result keys;
    uint64_t sctlr;

    uart[UART_CR / 4] = 0x301;
    puts_(SEPARATE_EL0_KEYS ? "# apple-m1 on vmapple under TCG\n"
                            : "# vmapple under KVM on a host that is not a Mac\n");

    if (SEPARATE_EL0_KEYS) {
        check_cpu();
    } else {
        puts_("# the CPU is the host's under KVM, MIDR ");
        puthex(READ_SYSREG(midr_el1));
        putc_('\n');
    }

    check(call(PAC_NOP, 0, 0) == 0, "PAC_NOP answers like a current host");
    check(call(PAC_SET_INITIAL_STATE, 0, 0) == 0, "SET_INITIAL_STATE");
    keys = hvc(PAC_GET_DEFAULT_KEYS, 0, 0);
    check(keys.x0 == 0 && keys.x1 == KEY_BASE + 6 && keys.x2 == KEY_BASE &&
          keys.x3 == KEY_BASE + 4 && keys.x4 == KEY_BASE + 10,
          "GET_DEFAULT_KEYS gives A, B, EL0 and G");
    check(call(0xc1000007UL, 0, 0) == (uint64_t)-1,
          "an unknown PAC call is NOT_SUPPORTED");

    /* A 39-bit address space, so the code sits in bits 54:39. */
    asm volatile("msr tcr_el1, %0; isb" : : "r"(25UL));
    sctlr = READ_SYSREG(sctlr_el1) | SCTLR_PAC_KEYS;
    asm volatile("msr sctlr_el1, %0; isb" : : "r"(sctlr));

    if (check_el1_keys()) {
        check_el0_keys();
    }

    puts_("1..");
    putc_('0' + tests / 10);
    putc_('0' + tests % 10);
    puts_(failures ? "\nAPPLE_M1_FAIL\n" : "\nAPPLE_M1_PASS\n");
    hvc(PSCI_SYSTEM_OFF, 0, 0);
    return 0;
}
