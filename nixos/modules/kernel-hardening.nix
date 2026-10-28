# The kernel's own defences, on by default: lockdown, the IOMMU and BPF JIT
# hardening (docs/kernel-hardening.md). losos-security reports each of them
# (docs/security-report.md), and without this module all three failed on
# every machine.
#
# The sysctls apply wherever this OS runs, a phone's vendor kernel included.
# Lockdown and the IOMMU's command line are for the kernel the PC image
# boots; a Halium build carries no kernel (halium/boot.nix).
{
  config,
  lib,
  pkgs,
  ...
}:

let
  inherit (pkgs.stdenv.hostPlatform) isx86;
  pc = config.boot.kernel.enable;

  inherit (config.system.boot.loader) kernelFile;

  # The kernel image with the certificate of every module the system
  # installs, the kernel's own and nvidia.nix's, from the same tree modprobe
  # loads them from.
  kernelImage = config.boot.kernelPackages.rebuildImage {
    target = kernelFile;
    inherit (config.system) modulesTree;
  };
in
{
  boot = {
    # BPF loads programs into the kernel and compiles them to machine code, so
    # it has been a steady source of privilege escalation. nixpkgs' kernel
    # already starts with unprivileged BPF off (BPF_UNPRIV_DEFAULT_OFF), as 2,
    # which root may turn back on. 1 is the setting nothing can undo until the
    # next boot; every BPF user in the image (systemd's cgroup filters,
    # libvirt) runs as root.
    kernel.sysctl."kernel.unprivileged_bpf_disabled" = 1;

    # 2 blinds the constants in every JIT-compiled program, root's included,
    # so a program cannot smuggle chosen machine code into executable kernel
    # memory (JIT spraying). 1 would spare root's programs, which are the ones
    # systemd loads, and those cost too little to compile to be worth sparing.
    kernel.sysctl."net.core.bpf_jit_harden" = 2;

    # nixpkgs' kernel has the lockdown LSM and module signatures but enforces
    # neither, so both are forced here, and that kernel and every module
    # package built against it come from this project's CI rather than
    # cache.nixos.org.
    #
    # Lockdown keeps root from changing the running kernel: no /dev/mem, no
    # unsigned kexec, no raw PCI or MSR writes, and no unsigned modules,
    # which is the part that matters, since a module is kernel code. nixpkgs
    # signs every module with kmodigest, which makes each one a certificate
    # of its own from the module's contents, so a build is the same each
    # time and a module package built later is signed the same way
    # (nvidia.nix). The kernel trusts those certificates only once they are
    # in its image, which is what the image below is for.
    #
    # Integrity mode is forced in the build rather than set with lockdown=
    # on the command line: systemd-stub takes a command line from the boot
    # loader when Secure Boot is off, and a forced mode cannot be lowered
    # from there. Integrity rather than confidentiality, which would also
    # stop root reading kernel memory through perf, kprobes, tracefs and
    # BPF and so take profilers and tracing away (docs/kernel-hardening.md).
    #
    # Locked down, the kernel refuses to hibernate, however well the image
    # is protected, which is why hibernate.nix is gone (docs/hibernation.md).
    kernelPatches = lib.mkIf pc [
      {
        name = "losos-lockdown";
        patch = null;
        # mkForce where nixpkgs' common-config.nix said no until October
        # 2026, so that it cannot again.
        structuredExtraConfig = with lib.kernel; {
          SECURITY_LOCKDOWN_LSM = lib.mkForce yes;
          # Before the other LSMs and the rest of early boot, so nothing the
          # kernel does on its way up runs unlocked.
          SECURITY_LOCKDOWN_LSM_EARLY = yes;
          LOCK_DOWN_KERNEL_FORCE_INTEGRITY = yes;
          MODULE_SIG = lib.mkForce yes;
          MODULE_SIG_ALL = yes;
          MODULE_SIG_SHA512 = yes;
          # Lockdown already refuses an unsigned module. Forcing it as well
          # makes /sys/module/module/parameters/sig_enforce say so, which is
          # what losos-security reads.
          MODULE_SIG_FORCE = yes;
        };
      }
    ];

    # The kernel image as built carries no module certificates: nixpkgs
    # leaves room for them and puts them in only when asked. Without them
    # this kernel would load no module at all, its own included.
    uki.settings.UKI.Linux = lib.mkIf pc "${kernelImage}/${kernelFile}";

    kernelParams = lib.mkIf pc (
      [
        # Translate every device's DMA through the IOMMU rather than letting a
        # driver ask for an identity mapping. That is already the build's
        # default; saying it keeps a future default from turning it off.
        "iommu.passthrough=0"
        # Invalidate the IOMMU's cache when a driver unmaps a buffer, not in a
        # later batch. In lazy mode a device can still reach a buffer the
        # kernel has freed and reused, which is a window a malicious
        # peripheral can aim for. It costs throughput only on links far faster
        # than a desktop's.
        "iommu.strict=1"
      ]
      # AMD's IOMMU is on whenever the firmware describes one. Intel's is off
      # unless asked for (INTEL_IOMMU_DEFAULT_ON is unset in nixpkgs), and
      # without it a Thunderbolt or PCIe device can read all of memory.
      ++ lib.optional isx86 "intel_iommu=on"
    );
  };

  # The system's own kernel link, which QEMU boots in the VM tests, to the
  # same image. NixOS links the plain build first.
  system.systemBuilderCommands = lib.mkIf pc (
    lib.mkAfter ''
      ln -sf ${kernelImage}/${kernelFile} $out/kernel
    ''
  );
}
