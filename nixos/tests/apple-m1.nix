# The apple-m1 CPU and Apple's paravirtual pointer authentication in
# qemu-vmapple (docs/macos-arm64.md), checked by a small firmware on the
# vmapple machine under TCG: once with signatures checked, once with
# pac-auth=off accepting every one. It needs no Apple software and no KVM,
# so it runs on every builder.
{ pkgs, qemu-vmapple }:
let
  inherit (pkgs) llvmPackages;
in
pkgs.runCommand "apple-m1-check"
  {
    nativeBuildInputs = [
      qemu-vmapple
      llvmPackages.clang-unwrapped
      llvmPackages.lld
      llvmPackages.llvm
    ];
  }
  ''
    flags="--target=aarch64-none-elf -march=armv8.3-a -mgeneral-regs-only
           -ffreestanding -nostdinc -fno-stack-protector -fno-builtin -O2
           -Wall -Werror"
    clang $flags -c ${./apple-m1/start.S} -o start.o
    clang $flags -c ${./apple-m1/test.c} -o test.o
    # The variant for KVM on a host that is not a Mac (docs/macos-arm64.md,
    # "How it is checked") needs an arm64 host with KVM to run, which no
    # builder has; building it keeps it from rotting.
    clang $flags -DGENERIC_KVM_HOST -c ${./apple-m1/test.c} -o test-kvm.o
    ld.lld -T ${./apple-m1/link.ld} start.o test.o -o firmware.elf
    llvm-objcopy -O binary firmware.elf firmware
    # vmapple wants its two pflash drives, which this firmware never reads.
    truncate -s 16384 aux root

    run() {
      timeout 300 qemu-system-aarch64 \
        -machine "vmapple,uuid=1,accel=tcg$1" \
        -smp 1 -m 1G -nodefaults -display none -monitor none \
        -serial stdio -no-reboot \
        -bios firmware \
        -drive file=aux,if=pflash,format=raw \
        -drive file=root,if=pflash,format=raw
    }

    run "" | tee enforced.log
    grep -qx APPLE_M1_PASS enforced.log
    grep -qx '# authentication enforced' enforced.log

    run ",pac-auth=off" | tee accept-all.log
    grep -qx APPLE_M1_PASS accept-all.log
    grep -qx '# authentication accepts every pointer' accept-all.log

    mkdir $out
    cp enforced.log accept-all.log $out/
  ''
