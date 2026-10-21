# Hibernation

> [!NOTE]
> No machine running this OS hibernates. Closing a laptop's lid suspends it, and
> it stays suspended until it is opened or its battery runs out; a battery that
> reaches critical while the machine is awake powers it off. The command
> palette's Hibernate command does not show, because logind reports that the
> machine cannot hibernate.

## Why not

The kernel is [locked down](kernel-hardening.md), and a locked-down kernel
refuses to hibernate in any mode. A hibernation image is the whole of kernel
memory written to disk and read back on the next boot. Lockdown exists to keep
root from changing the running kernel, and an image root could edit before a
resume would undo that. Nothing in mainline Linux authenticates the image well
enough for lockdown to allow it.

The image used to hibernate laptops that booted through UEFI and had a TPM.
Every boot formatted the swap partition as LUKS2 with a new key, sealed the
key to the TPM's PCRs 4, 7 and 12, and forgot it. Closing the lid on battery
suspended the laptop and hibernated it after three hours, and the initrd
resumed by unsealing the key. The project had to pick that or lockdown, and
picked lockdown, so `hibernate.nix`, its VM test and the TPM-sealed swap are
gone. Swap is encrypted with a random per-boot key on every machine, as
[Where the OS lives](layout.md) describes.

## What would bring it back

It needs a kernel that authenticates the image, for example by encrypting
and signing it with a key sealed to the TPM, so that lockdown could allow a
resume. Patches for that have been proposed to the kernel, but none has been
merged.
