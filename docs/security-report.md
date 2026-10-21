# The security report

GNOME Settings has a shield in Privacy & Security. Clicking it opens a dialog
that says "Protected" or "Security Checks Failed" and offers to copy a
technical report. Everything it knows comes from fwupd's
`GetHostSecurityAttrs`, and fwupd's Host Security ID is an index of *firmware*
protections: Secure Boot, the SPI descriptor, Intel BootGuard, TPM
reconstruction.

Those matter, but a desktop user is asking something closer to "is this
machine set up the way a careful person would set it up". Is the disk
encrypted? Is the kernel locked down, are modules signed, is the IOMMU on, are
the usual hardening sysctls set? None of that appears in the dialog, because
fwupd reports on the OS only where it has a plugin for it. lynis answers those
questions for servers, as a shell script that prints a long text report.

This OS answers both sets of questions in one list.

## Two providers, one shape

`losos-security`, in `src/losos-security/`, runs the OS-level checks and
serves them on the system bus as `io.losos.Security1`. It answers
`GetHostSecurityAttrs`, the same method name fwupd uses, with the same `a{sv}`
dictionary keys. Because the answers have fwupd's shape, the
gnome-control-center patch only asks a second bus name and files the answers
separately. It needs no second parser and no second widget. `src/attr.rs` pins
the key set against the code that reads it, gnome-control-center's
`panels/privacy/firmware-security/cc-firmware-security-utils.c`.

The attributes go into their own table, not into the HSI tables. fwupd
defines the HSI levels for firmware, and filing "disk encryption" under HSI-1
would claim the two are equivalent when they are not.

## The checks

Each check reports a `FwupdSecurityAttrResult` and a level. Unlike fwupd's,
it also reports the path it read and what it found there.

| Check | Reads |
|---|---|
| UEFI Secure Boot | `/sys/firmware/efi/efivars/SecureBoot-8be4df61-…`, fifth byte |
| TPM 2.0 present | `/sys/class/tpm/tpm0/tpm_version_major` |
| Measured boot | `/sys/kernel/security/tpm0/binary_bios_measurements` |
| Disk encryption | `/sys/block/*/dm/uuid`, prefix `CRYPT-LUKS` |
| Verified system image | `/sys/block/*/dm/uuid`, prefix `CRYPT-VERITY` |
| Kernel lockdown | `/sys/kernel/security/lockdown`, the bracketed mode |
| Module signature enforcement | `/sys/module/module/parameters/sig_enforce` |
| IOMMU | `/sys/class/iommu` |
| Kernel address exposure | `kernel.kptr_restrict` ≥ 1 |
| Kernel log access | `kernel.dmesg_restrict` ≥ 1 |
| Unprivileged BPF | `kernel.unprivileged_bpf_disabled` ≥ 1 |
| BPF JIT hardening | `net.core.bpf_jit_harden` ≥ 1 |

Two rules matter here, and a careless change can break either.

**Every check names where it looked.** If a verdict comes without its
evidence, nobody can tell when it is wrong. The evidence goes in the
`Description` field, so the panel shows it when the row is expanded.

**Every check is rooted.** No check opens an absolute path. They all go
through `Context::path`, which prefixes a configurable root. That one
function call makes the suite testable: `tests/checks.rs` builds a fixture
directory of sysfs files and tests every check passing and failing, with no
TPM and no root.

The overall level is the weakest link, the highest level at which every check
passed. It is not a score or a percentage. A machine with nine passes and one
level-1 failure has a level-0 problem, and an average would hide it. A level
with no checks in it does not count as passed. `all()` over an empty iterator
is true, so without that rule an untested level would report the best possible
answer.

## The panel change

`nixos/pkgs/patches/gnome-control-center/0002-firmware-security-report-os-level-checks.patch`
does three things:

- Adds a third D-Bus proxy, to `io.losos.Security1`, beside the two fwupd
  ones, and files its reply into a separate table through the existing parser.
- Counts replies to decide whether the page is available, instead of leaving
  it to one callback. The two providers can reply in either order, so neither
  can decide alone that there is nothing to show. The "unavailable" page
  appears only when both have failed. The fwupd failure path now counts
  itself, which upstream never needed. Without that, a machine with the OS
  provider and no fwupd would wait forever for a verdict. Upstream hides the
  page on a virtual machine, where a firmware index means nothing. If any
  provider answered, the page shows anyway, because the OS checks still apply
  there.
- Lists the checks in the dialog. Upstream shows them only inside the copied
  technical report. That made sense while every check was about firmware the
  reader cannot change from here. This list includes a lockdown mode and four
  sysctls, and a reader who is told which one failed can fix it.

## Running it

```sh
losos-security                 # human-readable; exit status is the verdict
losos-security --json
losos-security --root ./fixture   # every check against a fixture tree
```

The service is bus-activated, not enabled. The unit has no `[Install]`
section. `io.losos.Security1.service` in `dbus-1/system-services` names
`SystemdService=`, so dbus-daemon hands the start to systemd and the sandbox
in `losos-security.service` applies. It runs as root, because two of the paths
above are not world-readable. The sandbox then takes away everything else a
root process could do. It has no capabilities, no writable filesystem and no
network namespace, may open only `AF_UNIX` sockets, and may make only the
`@system-service` system calls. A security checker that an attacker could
exploit would make the machine less safe.
