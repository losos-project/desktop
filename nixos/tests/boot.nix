# Boot the real image -- UEFI, GRUB, the UKI, a disk first boot has to
# lay out -- and check that each systemd piece this OS is built on is doing its
# job, not merely installed.
#
# The pm tree could not do this at all: nothing there had ever booted, and
# docs/boot.md said so. This test is the first thing in the repository that
# can fail because the design is wrong rather than because a build is.
{ self }:

{ lib, ... }:

let
  image = import ./image.nix { inherit self; };
in
{
  name = "losos-desktop-boot";

  # The module adds its own overlay, which the test framework's shared,
  # read-only package set would refuse.
  node.pkgsReadOnly = false;

  nodes.machine = image.node;

  testScript =
    { nodes, ... }:
    image.disk nodes.machine
    + ''

      machine.wait_for_unit("multi-user.target")

      with subtest("first boot laid out the disk with systemd-repart"):
          machine.succeed("findmnt --kernel --types ext4 /")
          machine.succeed("findmnt --kernel /home")
          # slot B exists and is waiting for sysupdate
          machine.succeed("sfdisk --json /dev/vda | grep -c '\"name\": \"_empty\"' | grep -qx 2")
          machine.succeed("swapon --show=NAME --noheadings | grep -q dm-")

      with subtest("GRUB chainloaded the UKI and named the disk it came from"):
          # bli's LoaderInfo, and the LoaderDevicePartUUID gpt-auto found root
          # and the ESP by (grub.nix).
          loader = machine.succeed(
              "tail -c +5 /sys/firmware/efi/efivars/LoaderInfo-4a67b082-0a4c-41cf-b6c7-440b29bb8c4f | tr -d '\\0'"
          )
          assert loader.startswith("GRUB"), loader
          machine.succeed("grep -q 'usrhash=' /proc/cmdline")
          esp = machine.succeed("bootctl --print-esp-path").strip()
          machine.succeed(f"test -f {esp}/EFI/Linux/losos-desktop_1.efi")
          machine.succeed(f"test -f {esp}/EFI/losos/grubenv")
          # The bless runs after boot-complete.target, which comes after
          # multi-user.target, so it can still be queued here; waiting
          # still fails if its job was dropped or it failed.
          machine.wait_for_unit("losos-grub-bless.service")

      with subtest("/usr is dm-verity and the store lives on it"):
          assert "ACTIVE" in machine.succeed("dmsetup info --target verity usr")
          machine.succeed("df --output=source /nix/store | tail -n1 | grep -qx /dev/mapper/usr")

      with subtest("the systemd services the design rests on are running"):
          for unit in [
              "systemd-homed.service",
              "systemd-userdbd.socket",
              "NetworkManager.service",
              "systemd-resolved.service",
              "systemd-oomd.service",
              "systemd-factory-reset.socket",
              "systemd-sysupdate.timer",
              "display-manager.service",
          ]:
              machine.wait_for_unit(unit)
          machine.fail("systemctl is-active systemd-networkd.service")

      with subtest("the image chose drivers from facter's report and kept NVIDIA's unloaded"):
          # QEMU's display is not an NVIDIA card, so no rule matches and the
          # NVIDIA modules the image carries stay out (tests/hardware.nix
          # covers a matching report).
          machine.succeed("systemctl is-active losos-hardware.service losos-hardware-fallback.service")
          machine.succeed("grep -q graphics_card /run/losos/hardware/facter.json")
          machine.succeed("grep -q '\"matched\": \\[\\]' /run/losos/hardware/plan.json")
          machine.succeed("modinfo -n nvidia")
          machine.fail("test -d /sys/module/nvidia")

      with subtest("the kernel is locked down, takes only signed modules and hardens BPF"):
          # kernel-hardening.nix. The mode is forced in the kernel's build,
          # so the command line has nothing to say about it.
          machine.succeed("grep -q '\\[integrity\\]' /sys/kernel/security/lockdown")
          machine.succeed("grep -qx Y /sys/module/module/parameters/sig_enforce")
          # NVIDIA's open module is signed (nvidia.nix), or it could never
          # load. kmodigest's signatures name no signer, only their type;
          # tests/hardware.nix loads it.
          machine.succeed("modinfo -F sig_id nvidia | grep -qx 'PKCS#7'")
          machine.succeed("test \"$(sysctl -n kernel.unprivileged_bpf_disabled)\" = 1")
          machine.succeed("test \"$(sysctl -n net.core.bpf_jit_harden)\" = 2")
          # QEMU emulates no IOMMU here, so only the asking can be checked.
          machine.succeed("grep -qw intel_iommu=on /proc/cmdline")
          machine.succeed("grep -qw iommu.strict=1 /proc/cmdline")

      with subtest("a user is a homed LUKS volume"):
          # homed's first-boot wizard runs only while no regular user exists,
          # so nothing the image ships -- derisk-greeter included -- may count
          # as one. accounts.nix silences a NixOS warning on that reading.
          # userdbctl also prints "begin/end systemd-homed users" marker rows
          # for the UID ranges a filter covers, and homed's range is regular,
          # so without --boundaries=no the output is never empty.
          machine.fail(
              "userdbctl user --disposition=regular --no-legend --boundaries=no | grep -q ."
          )
          machine.succeed(
              "NEWPASSWORD=correct-horse homectl create alice --storage=luks "
              "--disk-size=256M --member-of=wheel --enforce-password-policy=no"
          )
          machine.succeed("userdbctl user alice | grep -q 'Storage: luks'")
          # glibc's NSS reaches nss-systemd through the configured nscd
          # forwarder, so homed users must be visible to ordinary lookups.
          machine.succeed("getent passwd alice")

      with subtest("the first login after setup goes straight to the desktop"):
          # What `derisk setup` leaves when its last page is done (derisk's
          # first_login.rs): the new user and password, root's alone, which
          # the display manager logs in with once instead of showing the
          # login screen. A restart starts the round that takes it.
          machine.succeed("umask 077; printf 'alice\\0correct-horse' > /run/derisk-first-login")
          machine.succeed("systemctl restart display-manager.service")
          machine.wait_until_succeeds(
              "loginctl list-sessions --no-legend | grep -q alice", timeout=300
          )
          machine.wait_until_succeeds("pgrep -u alice -f 'derisk session'", timeout=120)
          # Taken once: the next round shows the login screen.
          machine.succeed("test ! -e /run/derisk-first-login")

      with subtest("the privacy display's face counter runs"):
          # The VM has no camera: the counter says so and exits, which is
          # how the session learns there is nothing to watch with.
          machine.succeed("derisk-faces watch | grep -qx 'camera none'")

      with subtest("sysupdate sees the installed version"):
          # The test VM has no route to the release feed, and without
          # --offline sysupdate fails on the download before listing what is
          # installed.
          machine.succeed(
              "${nodes.machine.systemd.package}/lib/systemd/systemd-sysupdate --offline list | grep -q '1'"
          )

      with subtest("pm is installed as the system manager"):
          machine.succeed("pm --help")
          machine.succeed("pm source-path https://example.org/x-1.0.tar.xz | grep -q x-1.0.tar.xz")

      with subtest("pm administers the machine as its own user, and nothing else does"):
          machine.fail("command -v sudo")
          machine.fail("test -e /run/wrappers/bin/su")
          # Root is never asked by polkit, so this is the daemon end to end:
          # bus activation, polkit and the unit job. Units and updates are all
          # pm does: there is no command to run one as root, and the debug
          # shell, a root shell by another name, is not a unit pm may start.
          machine.succeed("pm system restart systemd-hostnamed")
          machine.fail("pm system run id")
          machine.fail("pm system start debug-shell")
          machine.fail("systemctl is-active debug-shell.service")
          machine.succeed("ps -o user= -C pmd | grep -qx pm")
          # pkcheck: 0 authorized, 1 refused, 2 needs authentication. pm may
          # manage units unasked; alice, an administrator, may not manage them
          # herself, which is what kept run0 and `systemctl restart` from her,
          # and is asked to authenticate before pm does it for her.
          machine.succeed(
              "pkcheck --action-id org.freedesktop.systemd1.manage-units"
              " --process $(pgrep -x pmd | head -1)"
          )
          alice = "$(pgrep -u alice -f 'derisk session' | head -1)"
          machine.succeed(
              "pkcheck --action-id org.freedesktop.systemd1.manage-units"
              f" --process {alice}; test $? = 1"
          )
          machine.succeed(
              "pkcheck --action-id org.pm1.system.manage-units"
              f" --detail verb restart --detail unit sshd.service --process {alice}; test $? = 2"
          )
          # Without an agent nobody is there to authenticate, so it is refused.
          machine.fail("runuser -u alice -- pm system restart systemd-hostnamed")

      with subtest("the terminal is derisk-terminal, with a Nerd Font, and foot is gone"):
          machine.succeed("command -v derisk-terminal")
          machine.succeed("test -e /run/current-system/sw/share/applications/org.derisk.terminal.desktop")
          machine.fail("command -v foot")
          machine.succeed("fc-list : family | grep -q 'Hack Nerd Font'")
          machine.succeed("fc-list : family | grep -q 'Symbols Nerd Font'")

      # Every command above ran through the system profile, so they already
      # ran uutils; this names which tool answers.
      with subtest("the shell's tools are uutils and Sequoia's gpg"):
          machine.succeed("ls --version | grep -q uutils")
          machine.succeed("find --version | grep -q Rust")
          machine.succeed("gpg --version | grep -q 'Sequoia Chameleon'")
          machine.succeed("command -v sq")
          # A key, a signature and its check, through GnuPG's agent, as git
          # signs and verifies a commit; loopback because the test has no one
          # to answer pinentry. The check runs after the signing, not piped
          # from it: the chameleon's --verify answers BAD to a signature
          # still being made by a second gpg, though the same bytes check
          # out from a file.
          machine.succeed(
              "install -d -m 700 /tmp/gnupg && export GNUPGHOME=/tmp/gnupg"
              " && gpg --batch --pinentry-mode loopback --passphrase test"
              " --quick-gen-key 'Test <test@example.org>' ed25519 sign never"
              " && echo hello > /tmp/msg"
              " && gpg --batch --pinentry-mode loopback --passphrase test"
              " --armor --detach-sign -o /tmp/msg.asc /tmp/msg"
              " && gpg --verify /tmp/msg.asc - < /tmp/msg"
          )

      with subtest("the security report answers on the system bus"):
          machine.succeed(
              "busctl call io.losos.Security1 /io/losos/Security1 "
              "org.freedesktop.DBus.Introspectable Introspect"
          )

      with subtest("An update's GRUB replaces the one on the ESP"):
          # losos-grub-update runs once the bless has, and records what it
          # installed. A GRUB that is not this version's, and the
          # systemd-boot older installs started with, are both replaced,
          # each by shim with the signed GRUB beside it (image.nix gives the
          # node a Secure Boot key), and the reboot below starts the result.
          machine.wait_for_unit("losos-grub-update.service")
          machine.succeed(f"test -f {esp}/EFI/losos/installed")
          grub = "${nodes.machine.system.build.grub.efi}/grub.efi"
          shim = "${nodes.machine.system.build.secureBoot.shimFile}"
          machine.succeed(f"cmp {shim} {esp}/EFI/BOOT/BOOTX64.EFI")
          machine.succeed(f"cmp {grub} {esp}/EFI/BOOT/grubx64.efi")
          machine.succeed(f"test -f {esp}/EFI/BOOT/mmx64.efi")
          machine.succeed(f"test -f {esp}/ENROLL_THIS_KEY_IN_MOKMANAGER.cer")
          machine.succeed(f"printf older >> {esp}/EFI/BOOT/grubx64.efi")
          machine.succeed(f"mkdir -p {esp}/EFI/systemd")
          machine.succeed(
              f"cp ${nodes.machine.systemd.package}/lib/systemd/boot/efi/systemd-bootx64.efi {esp}/EFI/systemd/"
          )
          machine.succeed(f"rm {esp}/EFI/losos/installed")
          machine.succeed("systemctl restart losos-grub-update.service")
          machine.succeed(f"cmp {shim} {esp}/EFI/BOOT/BOOTX64.EFI")
          machine.succeed(f"cmp {grub} {esp}/EFI/BOOT/grubx64.efi")
          machine.succeed(f"cmp {shim} {esp}/EFI/systemd/systemd-bootx64.efi")
          machine.succeed(f"cmp {grub} {esp}/EFI/systemd/grubx64.efi")
          machine.succeed(f"test -f {esp}/EFI/losos/installed")

      with subtest("Settings can ask shim to enroll LosOS's key at the next start"):
          # What derisk's System page reads and starts (secure-boot.nix).
          # Taken back at the end, or MokManager would wait at the
          # reboots below for someone to type the code.
          import json
          status = json.loads(machine.succeed("losos-secure-boot"))
          assert status == {"secure_boot": False, "key": "missing", "code": None}, status
          machine.succeed("systemctl start losos-secure-boot-enroll.service")
          status = json.loads(machine.succeed("losos-secure-boot"))
          assert status["key"] == "requested", status
          assert len(status["code"]) == 8 and status["code"].isdigit(), status
          shim_guid = "605dab50-e046-4300-abb6-3dd810dd8b23"
          machine.succeed(f"test -f /sys/firmware/efi/efivars/MokNew-{shim_guid}")
          machine.succeed(f"test -f /sys/firmware/efi/efivars/MokAuth-{shim_guid}")
          machine.succeed("systemctl start losos-secure-boot-cancel.service")
          status = json.loads(machine.succeed("losos-secure-boot"))
          assert status == {"secure_boot": False, "key": "missing", "code": None}, status
          machine.fail(f"test -f /sys/firmware/efi/efivars/MokNew-{shim_guid}")

      with subtest("LosOS's firmware entry goes back to the front of the boot order"):
          # The entry the Windows installer adds, and Windows putting its
          # own back in front of it (grub.nix, losos-boot-order).
          efibootmgr = "${nodes.machine.system.build.grub.efibootmgr}/bin/efibootmgr"
          machine.wait_for_unit("losos-boot-order.service")
          part = machine.succeed(
              "lsblk --noheadings --output PARTN "
              f"$(findmnt --noheadings --output SOURCE --types vfat --mountpoint {esp})"
          ).strip()
          machine.succeed(
              f"{efibootmgr} --quiet --create --disk /dev/vda --part {part} "
              "--label LosOS --loader '\\EFI\\losos\\grubx64.efi'"
          )
          ours = machine.succeed(
              f"{efibootmgr} | sed -n 's/^Boot\\([0-9A-F]\\{{4\\}}\\)\\* LosOS\\([[:space:]].*\\)\\?$/\\1/p'"
          ).strip()
          order = machine.succeed(f"{efibootmgr} | sed -n 's/^BootOrder: //p'").strip().split(",")
          others = [entry for entry in order if entry != ours]
          assert others, order
          machine.succeed(f"{efibootmgr} --quiet --bootorder {','.join(others + [ours])}")
          machine.succeed("systemctl restart losos-boot-order.service")
          machine.succeed(f"{efibootmgr} | grep -qx 'BootOrder: {ours},.*'")

      with subtest("A counted UKI is blessed once it boots under UEFI GRUB"):
          # As boot-bios.nix checks it under a BIOS: the name sysupdate gives
          # a new UKI (update.nix), counted down in grubenv and settled by
          # losos-grub-bless.
          machine.succeed(
              f"mv {esp}/EFI/Linux/losos-desktop_1.efi '{esp}/EFI/Linux/losos-desktop_1+3-0.efi'"
          )
          machine.shutdown()
          machine.start()
          machine.wait_for_unit("multi-user.target")
          machine.wait_for_unit("losos-grub-bless.service")
          machine.succeed(f"test -f {esp}/EFI/Linux/losos-desktop_1.efi")
          machine.fail(f"ls {esp}/EFI/Linux/ | grep -q '+'")
          machine.fail(f"grep -q '^losos_' {esp}/EFI/losos/grubenv")
    '';
}
