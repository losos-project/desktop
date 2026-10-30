# The gaming machine (gaming.nix, docs/gaming-machine.md), as far as a test
# VM goes: it has one emulated card and no IOMMU, so nothing can be moved
# to vfio-pci, and no KVM, so libvirt will not define a KVM machine. What
# is checked is that the card is listed and refused for the right reasons,
# that a boot with a card reserved carries on when it cannot go, and that
# the machine losos-gaming would define is one libvirt's schema accepts,
# with its firmware there and nothing on it that says virtual machine.
{ ... }:

{
  name = "losos-desktop-gaming";

  # losos-gaming from the OS's overlay: the derivation the image builds.
  node.pkgsReadOnly = false;

  nodes.machine =
    { pkgs, ... }:
    {
      imports = [
        ../modules/libvirt.nix
        ../modules/gaming.nix
      ];
      nixpkgs.overlays = [ (import ../pkgs) ];
      # virt-xml-validate checks with xmllint.
      environment.systemPackages = [ pkgs.libxml2 ];
      virtualisation.memorySize = 2048;
    };

  testScript = ''
    import json

    machine.wait_for_unit("multi-user.target")

    with subtest("the one card is listed and stays with the desktop"):
        gpus = json.loads(machine.succeed("losos-gaming gpus --json"))
        assert len(gpus["gpus"]) == 1, gpus
        card = gpus["gpus"][0]
        assert card["boot_vga"], card
        assert "IOMMU is off" in card["refused"], card
        assert gpus["suggested"] is None, gpus
        address = card["address"]
        machine.fail(f"systemctl start losos-gaming-reserve@{address}.service")
        machine.fail("test -e /var/lib/losos-gaming/gpu")

    with subtest("a reserved card that cannot go leaves the boot alone"):
        machine.succeed(f"mkdir -p /var/lib/losos-gaming && echo {address} > /var/lib/losos-gaming/gpu")
        machine.succeed("systemctl restart losos-gaming-bind.service")
        machine.succeed("systemctl is-active losos-gaming-bind.service")
        machine.fail("test -e /run/losos-gaming/functions")
        out = machine.succeed("journalctl -u losos-gaming-bind.service")
        assert "stays on the host" in out, out
        machine.succeed("systemctl start losos-gaming-reserve@none.service")
        machine.fail("test -e /var/lib/losos-gaming/gpu")

    with subtest("the machine's definition is one libvirt takes"):
        machine.succeed("test -s /etc/losos-gaming/OVMF_CODE.fd")
        machine.succeed("test -s /etc/losos-gaming/OVMF_VARS.ms.fd")
        machine.succeed("stat -c %U:%G:%a /dev/shm/looking-glass | grep -qx qemu-libvirtd:wheel:660")
        # As bind would have left it with a card: its two functions.
        machine.succeed("mkdir -p /run/losos-gaming && printf '0000:01:00.0\\n0000:01:00.1\\n' > /run/losos-gaming/functions")
        for screen, installing in [
            ("looking-glass", ""),
            ("looking-glass", " --installing"),
            ("monitor", ""),
        ]:
            machine.succeed(f"losos-gaming xml {screen}{installing} > /tmp/gaming.xml")
            machine.succeed("virt-xml-validate /tmp/gaming.xml domain")
            xml = machine.succeed("cat /tmp/gaming.xml")
            for expected in [
                "<hidden state='on'/>",
                "<vendor_id state='on' value='LososGaming'/>",
                "<smbios mode='host'/>",
                "mode='host-passthrough'",
                "/etc/losos-gaming/OVMF_VARS.ms.fd",
                "<tpm model='tpm-crb'>",
                "managed='no'",
                "bus='0x01' slot='0x00' function='0x1'",
            ]:
                assert expected in xml, f"{expected} is not in {xml}"
            assert "virtio" not in xml, xml
            assert ("<model type='vga'/>" in xml) == bool(installing), xml
            assert ("<shmem name='looking-glass'>" in xml) == (screen == "looking-glass"), xml
  '';
}
