# arm64 macOS machines

macOS 26 is the last release for Intel Macs. Every macOS after it runs on
Apple silicon only, so a macOS machine that keeps getting updates has to be
an arm64 one. This page covers the emulator that runs one on a PC:
`qemu-vmapple`, QEMU's `vmapple` machine under TCG with an emulated M1 CPU.

> [!WARNING]
> Apple's licence lets macOS run in a virtual machine only on Apple
> hardware. Booting it on a PC breaks that licence. Nothing in this
> repository or the image contains macOS or Apple's boot ROM, and CI never
> boots macOS. Whoever boots it supplies both, from their own Mac.

## Two routes

| Host | How the guest runs | State |
| --- | --- | --- |
| An Apple silicon Mac running LosOS | KVM, as Asahi Linux runs it, with two KVM patches | LosOS does not boot on these Macs yet |
| Any PC, x86-64 or arm64 | `qemu-vmapple`, every instruction translated by TCG | the emulator and its check are built, macOS not yet booted on it |

`vmapple` is the virtual Mac that Apple's own hypervisor
(Virtualization.framework) gives a macOS guest. It is not a model of a
physical M1. Its devices are paravirtual, so QEMU has most of them already,
and the guest kernel is XNU's `VMAPPLE` build. What a guest needs from
the CPU is what an M1 shows it, plus a few hypercalls.

## What qemu-vmapple is

`nixos/pkgs/default.nix` builds it from nixpkgs' QEMU with the patches in
`nixos/pkgs/patches/qemu-vmapple`, for the aarch64 system emulator only.

- **0001** is the code of
  [27on86](https://github.com/steelbrain/experiment-macOS-arm64-on-linux-x86),
  steelbrain's fork, at `509a4ce`. It lets `vmapple` start without HVF,
  sets the guest's 24 MHz timebase, and keeps guest time steady across
  several vCPUs. With it, macOS 13 boots to a shell over SSH on an x86-64
  PC. It accepted every pointer signature, because nobody outside Apple
  has the cipher an M1 signs with.
- **0002** adds an `apple-m1` CPU, an M1 performance core, and makes it
  `vmapple`'s default under TCG.
- **0003** answers Apple's pointer authentication hypercalls and checks
  signatures again.

### The apple-m1 CPU

QEMU's `max` CPU offers every feature QEMU emulates. That includes SVE,
SME and MTE, which no M1 had. The `VMAPPLE` kernel probes for SME at run
time and would turn it on. The `apple-m1` CPU shows the guest an M1's feature set
instead. The sources are macOS's own `sysctl hw.optional.arm` on an M1 and
the `VMAPPLE` kernel's build configuration (`pexpert/arm64/VMAPPLE.h`):

| | |
| --- | --- |
| `MIDR_EL1` | `0x611f0231`, implementer Apple, part `0x023` (t8103 Firestorm) |
| Present | AES, PMULL, SHA1, SHA256, SHA512, SHA3, CRC32, LSE, LSE2, RDM, FP16, FHM, DotProd, JSCVT, FCMA, LRCPC2, FlagM2, DPB2, SB, FRINTTS, SSBS2, DIT, CSV2, CSV3, ECV, PAuth with Apple's own algorithm |
| Absent | SVE, SME, MTE, BTI, BF16, I8MM, RNDR, SPECRES, PAuth2, FPAC |
| `VMAPPLE.h` asks for | TLBI by range, 16K pages, a GICv3 |
| Pages | 16K and 4K, no 64K |
| Addresses | 36 bits, what Apple's hypervisor gives a guest on an M1 |

The PMU stays the architected PMUv3 that 27on86's guest booted with. An
M1's own counters are Apple's and are not modelled.

### Apple's pointer authentication

A guest of Apple's hypervisor never writes its pointer authentication
keys. XNU's `VMAPPLE` kernel asks the hypervisor for them, through these
hypercalls (`osfmk/arm64/hv_hvc.h`):

| Function | Call | What it does |
| --- | --- | --- |
| `0xc1000000` | `SET_INITIAL_STATE` | every key to its default |
| `0xc1000001` | `GET_DEFAULT_KEYS` | the default A, B, EL0 and G keys, in x1 to x4 |
| `0xc1000002` | `SET_A_KEYS` | the A keys, which the kernel signs its heap with |
| `0xc1000003` | `SET_B_KEYS` | the B keys, each thread's return-address key |
| `0xc1000004` | `SET_EL0_DIVERSIFIER` | a process's own key for EL0 |
| `0xc1000005` | `SET_EL0_DIVERSIFIER_AT_EL1` | the same, and whether EL1 uses it too |
| `0xc1000006` | `SET_G_KEY` | the key for `PACGA` |
| `0xc10000f0` | `NOP` | tells a current host from an old one |

Without 0003 those calls returned "not supported". macOS 13 never looked.
From macOS 26 on, XNU checks the result of `SET_INITIAL_STATE` and spins
forever if it is not 0, so that guest would stop at its first one.

On an M1 the EL0 key lives in the `KERNKEY` register pair, and `APCTL`
says which exception levels mix it into their keys. Asahi's register
notes and m1n1's hypervisor name both. The KVM patches in
[aquarat/apple-macos-arm64-vm-guest](https://github.com/aquarat/apple-macos-arm64-vm-guest)
showed which values Apple's hypervisor puts there. Under TCG the EL0 key
is mixed into QEMU's own PAC algorithm instead of Apple's. The guest
cannot tell the difference, because it only ever compares signatures this
CPU made. What it does rely on now holds:

- A pointer EL0 signed fails at EL1, and the other way round.
- EL1 accepts EL0's signatures while the kernel borrows EL0's key.
- A new key breaks every pointer signed with the old one.

`-machine vmapple,pac-auth=off` brings back 27on86's behaviour, which
accepts every signature. That is for comparing against a guest that
booted before 0003.

## Running a guest

You need three things from a Mac, none of which the image carries:

- `AVPBooter.vmapple2.bin`, the boot ROM, from
  `/System/Library/Frameworks/Virtualization.framework/Versions/A/Resources/`
- the guest's AUX and root disks, restored from Apple's IPSW on the Mac,
  for instance with [macosvm](https://github.com/s-u/macosvm)
- the guest's ECID, from that restore

Then:

```sh
qemu-system-aarch64 \
  -machine vmapple,uuid=$ECID,accel=tcg \
  -smp 4 -m 8G -nographic \
  -bios AVPBooter.vmapple2.bin \
  -drive file=aux.img,if=pflash,format=raw \
  -drive file=root.img,if=pflash,format=raw
```

27on86's scripts and experiment records cover the rest: serial access,
an SSH service, boot arguments and how it was validated.

## How it is checked

The `apple-m1` flake check (`nixos/tests/apple-m1.nix`) builds a small
firmware from `nixos/tests/apple-m1/` and boots it on `vmapple` under TCG.
It reads the CPU's ID registers, makes every hypercall above, and signs and
authenticates from EL0 and EL1. It then runs again with `pac-auth=off`. It
needs no Apple software and no KVM, so it runs on both architectures'
builders. The same firmware on the unpatched fork fails the feature checks
and the first three hypercalls.

## What is not done

- **Booting macOS on it.** Nobody has booted a macOS guest on these
  patches yet. The next steps are 27on86's macOS 13 guest with
  authentication on, then macOS 26.
- **Installing macOS without a Mac.** The restore needs Virtualization.framework.
- **A GPU.** The guest is headless. Reims draws `vmapple`'s paravirtual
  GPU through Vulkan in aquarat's QEMU on Asahi. That device is not in
  this build.
- **Speed.** TCG translates every instruction, so expect a guest many
  times slower than on a Mac.
- **A machine in derisk.** Nothing creates or starts this guest from the
  desktop. Once it boots, `macos-app-server` inside it and `app-windows`
  on the host would show its apps as desktop windows, over TCP port 7180
  with a shared token. Intel macOS on a PC takes the other route, dockur's
  container under rootless Podman.
