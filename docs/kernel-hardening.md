# Kernel hardening

`nixos/modules/kernel-hardening.nix` turns on three of the kernel's own
defences in every image: lockdown with signed modules, a strict IOMMU, and
locked-down BPF. The [security report](security-report.md) checks each of
them.

| Defence | Where it is set | What the report reads |
|---|---|---|
| Lockdown, integrity mode | the kernel's build | `/sys/kernel/security/lockdown` says `[integrity]` |
| Module signatures | the kernel's build | `/sys/module/module/parameters/sig_enforce` is `Y` |
| IOMMU, strict | the kernel's command line | `/sys/class/iommu` lists the machine's IOMMUs |
| Unprivileged BPF off | `kernel.unprivileged_bpf_disabled = 1` | the sysctl |
| BPF JIT hardening | `net.core.bpf_jit_harden = 2` | the sysctl |

The sysctls apply on a phone too, where the vendor's kernel reads them.
Lockdown and the IOMMU's command line are for the kernel the PC image boots,
since a [Halium](halium.md) build carries no kernel of its own.

## Lockdown

Lockdown keeps even root from changing the running kernel. In integrity mode
the kernel refuses `/dev/mem` and `/dev/port`, an unsigned `kexec`, writes to
model-specific registers and raw PCI configuration space, custom ACPI tables,
and any kernel module that does not carry a valid signature. Without that
last rule the rest would not hold. A module is kernel code, so if root could
load any module, it could switch lockdown off again.

nixpkgs builds its kernel with the lockdown module and module signatures but
enforces neither. This image builds its own kernel that enforces both. The
cost is that the kernel, and every package built against it, comes from this
project's CI and its cache instead of cache.nixos.org.

nixpkgs signs each module with kmodigest as it is installed. kmodigest makes
every module a certificate of its own from the module's contents, so the
same build always gives the same signatures, and a module built later as a
separate package, such as NVIDIA's open module in `nvidia.nix`, is signed the
same way as the kernel's own. The kernel trusts those certificates only once
they are inside its image. nixpkgs' kernel build leaves room for them and
adds none, so `kernel-hardening.nix` links the compiled kernel into a second
image with the certificates of the system's whole module tree in it, which
takes under a minute. The UKI and the VM tests both boot that image. The plain build would load no module at all.

The build forces integrity mode with `LOCK_DOWN_KERNEL_FORCE_INTEGRITY`
instead of asking for it with `lockdown=` on the command line. Without Secure
Boot, systemd-stub accepts a command line from the boot loader, and nothing
on the command line can lower a forced mode. Confidentiality mode would also
stop root from reading kernel memory through perf, kprobes, tracefs and BPF.
That takes profilers and tracing away from anyone developing on the machine,
and the gain only matters once root is the attacker and the boot chain is
signed.

### What it costs

- **Hibernation.** A locked-down kernel refuses to hibernate in any mode,
  because a hibernation image is kernel memory written to disk and read back,
  and nothing in mainline Linux authenticates it. Laptops suspend when the lid
  closes and never hibernate ([Hibernation](hibernation.md)).
- **Out-of-tree modules.** A module that is not built inside the kernel's
  build cannot load. NVIDIA's is the only one the image carries. Anything
  else, such as a DKMS-style driver or a module in a system extension, is
  refused.
- **Low-level tools.** `/dev/mem` readers, MSR writers, `ioperm` users and
  flashing tools that poke hardware directly fail with `EPERM`.

## IOMMU

The IOMMU translates every device's DMA, so a device can reach only the memory
its driver mapped for it. Without one, any device that can master the bus, a
Thunderbolt dock or a malicious PCIe card among them, can read and write all of
memory.

- `intel_iommu=on`, on x86. AMD's IOMMU is on whenever the firmware describes
  one. Intel's is off unless asked for, because nixpkgs leaves
  `INTEL_IOMMU_DEFAULT_ON` unset.
- `iommu.passthrough=0` translates DMA for every device instead of letting a
  driver ask for an identity mapping. That is already the build's default,
  and setting it explicitly keeps a later default from changing it.
- `iommu.strict=1` invalidates the IOMMU's cache as soon as a driver unmaps a
  buffer, not in a later batch. In lazy mode a device can still reach a buffer
  for a short while after the kernel freed and reused it. The cost is
  throughput on links much faster than a desktop's.

> [!NOTE]
> A virtual machine has an IOMMU only if its hypervisor emulates one (QEMU's
> `-device intel-iommu`), so the report's IOMMU row fails in most VMs whatever the
> image sets.

## BPF

BPF loads programs into the kernel and compiles them to machine code. Many
Linux privilege escalation bugs have gone through it.

- `kernel.unprivileged_bpf_disabled = 1`. nixpkgs' kernel already starts with
  unprivileged BPF off, through `BPF_UNPRIV_DEFAULT_OFF`, which is the value
  2, and root may turn that back on. The value 1 cannot be undone until the
  next boot. Every BPF user in the image, such as systemd's cgroup filters
  and libvirt, runs as root.
- `net.core.bpf_jit_harden = 2` blinds the constants in every JIT-compiled
  program, root's included, so a program cannot plant chosen machine code in
  executable kernel memory. That attack is called JIT spraying. The value 1
  would spare root's programs, which are the ones systemd loads, but those
  are cheap to compile, so sparing them gains nothing.

## Not done

- **Secure Boot off.** Lockdown protects the kernel that is running, not the
  next one. With Secure Boot off, or on an image built without LosOS's key,
  root can still write a kernel of its choosing to the ESP and reboot into
  it. With [Secure Boot](secure-boot.md) on and LosOS's key enrolled, the
  firmware and shim refuse a kernel that key did not sign.
- **The pre-boot DMA window.** `efi=disable_early_pci_dma` would clear bus
  mastering on PCI bridges before the kernel's IOMMU takes over. It is left
  off because the kernel's own help for it warns that it breaks some
  machines, and the image boots on hardware nobody tested.
