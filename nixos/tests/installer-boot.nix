# Boot the installer from its actual ISO under UEFI, or with `bios` under a
# legacy BIOS through GRUB's El Torito image (iso.nix), including the initrd
# mounts and the service that owns tty1.
{
  self,
  bios ? false,
}:

let
  # The test driver talks to a guest only through the backdoor shell that
  # nixpkgs' test-instrumentation.nix starts on hvc0. A test node gets that
  # module for free, but the ISO is a system of its own, evaluated in
  # installer.nix, so without it every machine.succeed waits for a shell that
  # never answers until the driver's global timeout kills the run. The module
  # is added to that system here, not to the medium that ships.
  installerSystem =
    self.nixosConfigurations."losos-desktop-x86_64".config.system.build.installerSystem.extendModules
      {
        modules = [
          (
            { modulesPath, ... }:
            {
              imports = [ "${modulesPath}/testing/test-instrumentation.nix" ];
            }
          )
        ];
      };
  installerIso = installerSystem.config.system.build.isoImage;

  # The installer's own command line, `derisk installer -- <backend> serve
  # ...`, shell-quoted by escapeShellArgs, with derisk's three words shifted
  # off so the backend runs alone and takes one request on stdin.
  install = disk: ''
    printf '%s\n' '{"method":"install","disk":"${disk}"}' \
      | (set -- ${installerSystem.config.systemd.services.derisk-installer.serviceConfig.ExecStart}; shift 3; "$@") \
      > /tmp/install.log 2>&1
  '';

  # The install's last step on its own, `losos-installer encrypt`, with the
  # root definitions and losos-swap the backend was given and the PIN on
  # stdin: there is no release here to download, so an install never gets
  # that far. The recovery key is what it prints.
  encrypt = disk: pin: ''
    set -- ${installerSystem.config.systemd.services.derisk-installer.serviceConfig.ExecStart}
    shift 3
    backend=$1
    while [ $# -gt 0 ]; do
      case $1 in
        --root-definitions) definitions=$2 ;;
        --swap) swap=$2 ;;
      esac
      shift
    done
    printf '%s\n' '${pin}' \
      | "$backend" encrypt --root-definitions "$definitions" --swap "$swap" --work /run/losos-encrypt ${disk} \
      2> /tmp/encrypt.log
  '';
in
{
  name = "losos-desktop-installer-boot${if bios then "-bios" else ""}";

  nodes.machine =
    { ... }:
    {
      virtualisation = {
        directBoot.enable = false;
        mountHostNixStore = false;
        useEFIBoot = !bios;
        qemu.options = [
          "-cdrom ${installerIso}"
          "-boot order=d"
        ];
        memorySize = 3072;
        # A disk to install onto, as large as repart's layout needs with
        # root, /home and swap: both /usr slots, 8G of root, 4G of /home
        # and swap as large as RAM.
        emptyDiskImages = [ 40960 ];
        # A TPM 2.0, which encrypting root with a PIN needs; legacy BIOS
        # PCs are not offered it.
        tpm.enable = !bios;
      };
    };

  testScript = ''
    machine.${if bios then "fail" else "succeed"}("test -d /sys/firmware/efi")
    machine.succeed("findmnt --kernel /iso")
    machine.succeed("findmnt --kernel /nix/store")
    machine.wait_for_unit("derisk-installer.service")
    machine.succeed(
        "systemctl show derisk-installer.service --property=TTYPath --value "
        "| grep -qx /dev/tty1"
    )

    # A disk that already has a partition table, as any used disk does and
    # as a failed install leaves one. repart's ESP format used to fail here
    # with "Failed to create new partition '/dev/vdb2': Device or resource
    # busy", since the kernel still had the old partition 2. With nowhere to
    # download from, the install then stops at the download, and that it got
    # there is what is checked.
    machine.succeed("printf 'label: gpt\\n,1M\\n,512M\\n,128M\\n' | sfdisk /dev/vdb")
    machine.succeed("test -b /dev/vdb2")
    machine.execute(${builtins.toJSON (install "/dev/vdb")}, timeout=900)
    print(machine.succeed("cat /tmp/install.log"))
    machine.succeed("grep -q '\"label\":\"Downloading and writing the system\"' /tmp/install.log")
    machine.fail("grep -q 'resource busy' /tmp/install.log")
  ''
  + (
    if bios then
      ''
        # No UEFI, so no PIN at startup is offered.
        machine.fail("grep -q '\"encryption\":true' /tmp/install.log")
      ''
    else
      ''
        import re

        with subtest("a PC with a TPM is offered a PIN at startup"):
            machine.succeed("test -e /dev/tpmrm0")
            machine.succeed("grep -q '\"encryption\":true' /tmp/install.log")

        with subtest("root, /home and swap are made, root encrypted with the PIN"):
            key = machine.succeed(${builtins.toJSON (encrypt "/dev/vdb" "246813")}).strip()
            print(machine.succeed("cat /tmp/encrypt.log"))
            assert re.fullmatch(r"([cbdefghijklnrtuv]{8}-){7}[cbdefghijklnrtuv]{8}", key), key
            # Neither the PIN nor the key in what the installer logs.
            machine.fail(f"grep -q -e 246813 -e {key} /tmp/encrypt.log")
            root = machine.succeed("blkid -t TYPE=crypto_LUKS -o device /dev/vdb*").strip()
            machine.succeed("blkid -t TYPE=swap -o device /dev/vdb* | grep -q .")
            machine.succeed("lsblk -no PARTTYPENAME /dev/vdb | grep -qi 'linux home'")
            dump = machine.succeed(f"cryptsetup luksDump {root}")
            print(dump)
            assert dump.count("systemd-tpm2") == 1, "repart's slot without a PIN is still there"
            assert re.search(r"tpm2-pin:\s+true", dump), dump
            assert "systemd-recovery" in dump, dump

        cryptsetup = "/run/current-system/systemd/lib/systemd/systemd-cryptsetup"

        with subtest("the TPM unlocks root with the PIN and not without it"):
            machine.fail(f"PIN=000000 {cryptsetup} attach probe {root} - tpm2-device=auto,headless=true")
            machine.fail(f"{cryptsetup} attach probe {root} - tpm2-device=auto,headless=true")
            machine.succeed(f"PIN=246813 {cryptsetup} attach probe {root} - tpm2-device=auto,headless=true")
            machine.succeed("blkid -o value -s TYPE /dev/mapper/probe | grep -qx ext4")
            machine.succeed(f"{cryptsetup} detach probe")

        with subtest("the recovery key opens root too"):
            machine.succeed(f"printf %s {key} | cryptsetup open --test-passphrase {root} --key-file=-")
      ''
  );
}
