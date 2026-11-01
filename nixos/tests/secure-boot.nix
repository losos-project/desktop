# Boot the real image with Secure Boot on, as a PC that trusts Microsoft's
# keys does: the firmware starts shim, which starts the signed GRUB, which
# chainloads the signed UKI through shim (docs/secure-boot.md). LosOS's key,
# image.nix's test key here, is in the MOK list as MokManager leaves it once
# a person has enrolled it, and Settings' Security page counts Secure Boot
# as passed.
{ self }:

{ lib, pkgs, ... }:

let
  image = import ./image.nix { inherit self; };
  key = import ./secure-boot-key.nix { inherit pkgs; };

  # OVMF's variables with Microsoft's KEK and db, Secure Boot on, and the
  # test key in MokList under shim's GUID: what a person's "Enroll key from
  # disk" in MokManager writes. The SATA disk entry the template carries is
  # a disk this VM does not have.
  variables =
    pkgs.runCommand "OVMF_VARS.ms.losos.fd"
      { nativeBuildInputs = [ pkgs.python3Packages.virt-firmware ]; }
      ''
        virt-fw-vars --input ${pkgs.OVMFFull.fd}/FV/OVMF_VARS.ms.fd --output $out \
          --delete Boot0002 \
          --add-mok 605dab50-e046-4300-abb6-3dd810dd8b23 ${key}/cert.pem
      '';
in
{
  name = "losos-desktop-secure-boot";

  # The module adds its own overlay, which the test framework's shared,
  # read-only package set would refuse.
  node.pkgsReadOnly = false;

  # Merged beside the shared node, as boot-bios.nix is, so this boots
  # boot.nix's disk image: only the firmware differs.
  nodes.machine = lib.mkMerge [
    image.node
    {
      virtualisation.efi = {
        firmware = "${pkgs.OVMFFull.fd}/FV/OVMF_CODE.ms.fd";
        inherit variables;
      };
    }
  ];

  testScript =
    { nodes, ... }:
    image.disk nodes.machine
    + ''
      import json

      machine.wait_for_unit("multi-user.target")

      with subtest("the firmware enforced Secure Boot and shim trusted LosOS's key"):
          machine.succeed(
              "test \"$(od -An -tu1 -j4 -N1 /sys/firmware/efi/efivars/SecureBoot-8be4df61-93ca-11d2-aa0d-00e098032b8c | tr -d ' ')\" = 1"
          )
          # shim hands the MOK list to the kernel, which loads it into its
          # platform keyring.
          machine.succeed("journalctl -k -b | grep -q \"Loaded X.509 cert 'LosOS Desktop Secure Boot (test)\"")
          machine.succeed("journalctl -k -b | grep -q 'Secure boot enabled'")

      with subtest("GRUB started the UKI under Secure Boot"):
          loader = machine.succeed(
              "tail -c +5 /sys/firmware/efi/efivars/LoaderInfo-4a67b082-0a4c-41cf-b6c7-440b29bb8c4f | tr -d '\\0'"
          )
          assert loader.startswith("GRUB"), loader
          machine.succeed("grep -q 'usrhash=' /proc/cmdline")
          assert "ACTIVE" in machine.succeed("dmsetup info --target verity usr")

      with subtest("Settings' Security page counts Secure Boot as passed"):
          security = "${lib.head (lib.splitString " " nodes.machine.systemd.services.losos-security.serviceConfig.ExecStart)}"
          report = json.loads(machine.succeed(f"{security} --json"))
          check = next(a for a in report if a["appstream_id"] == "io.losos.SecurityAttr.SecureBoot")
          assert check["success"], check
          # What the page's Secure Boot section reads: nothing left to do.
          status = json.loads(machine.succeed("losos-secure-boot"))
          assert status == {"secure_boot": True, "key": "enrolled", "code": None}, status
    '';
}
