# Security keys and smart card readers lent to the machines (tokens.nix,
# guest/tokens.nix, docs/tokens.md), both ends of the relay on one machine
# over vsock's loopback: the host's half as the image runs it, and a
# machine's half beside it, where the host takes connections from this
# kernel for a machine called "work".
#
# The key is a FIDO2 key made with uhid that answers CTAPHID's INIT and
# CTAP2's getInfo, plugged in on the host. The machine's half makes its
# copy, and libfido2 in the machine reads the key's AAGUID through it.
# Pulling the key out takes the copy away. The reader is vsmartcard's
# virtual one in the host's pcscd, and the machine lists it and the virtual
# card put in it through its own pcscd socket. A web server on the
# machine's localhost stands in for the eID klient: the host's localhost
# reaches it while it listens, and nothing does once it stops. The
# machine's localhost is 127.0.0.2 here, since the host's end listens on
# 127.0.0.1 of the same kernel.
#
# Not the whole image, only the host's module, so it boots the test
# driver's kernel quickly and runs without KVM too. machines.nix runs the
# same with the guest template in libvirt.
{ lib, ... }:

let
  # A FIDO2 key on uhid. One report per message, which INIT's and
  # getInfo's answers fit in.
  mockKey =
    pkgs:
    pkgs.writers.writePython3Bin "mock-fido-key" { } ''
      import os
      import struct

      EVENT = 4376
      DESCRIPTOR = bytes([
          0x06, 0xd0, 0xf1, 0x09, 0x01, 0xa1, 0x01,
          0x09, 0x20, 0x15, 0x00, 0x26, 0xff, 0x00, 0x75, 0x08, 0x95, 0x40,
          0x81, 0x02,
          0x09, 0x21, 0x15, 0x00, 0x26, 0xff, 0x00, 0x75, 0x08, 0x95, 0x40,
          0x91, 0x02,
          0xc0,
      ])
      AAGUID = bytes(range(16))
      # getInfo: {1: ["FIDO_2_0"], 3: AAGUID}
      INFO = b"\xa2\x01\x81\x68FIDO_2_0\x03\x50" + AAGUID
      CHANNEL = b"\x00\x00\x00\x07"

      fd = os.open("/dev/uhid", os.O_RDWR)


      def event(kind, payload):
          head = struct.pack("<I", kind) + payload
          return head + bytes(EVENT - len(head))


      create = struct.pack(
          "<128s64s64sHHIIII", b"Test FIDO key", b"usb-test", b"",
          len(DESCRIPTOR), 3, 0x1234, 0x5678, 0, 0,
      ) + DESCRIPTOR.ljust(4096, b"\0")
      os.write(fd, event(11, create))


      def send(channel, command, data):
          report = channel + bytes([command]) + struct.pack(">H", len(data))
          report = (report + data).ljust(64, b"\0")
          os.write(fd, event(12, struct.pack("<H", 64) + report))


      while True:
          received = os.read(fd, EVENT)
          if struct.unpack_from("<I", received)[0] != 6:
              continue
          size = struct.unpack_from("<H", received, 4 + 4096)[0]
          data = received[4:4 + size]
          if len(data) == 65:
              data = data[1:]
          channel, command = data[0:4], data[4]
          length = struct.unpack(">H", data[5:7])[0]
          payload = data[7:7 + length]
          if command == 0x86:
              # CTAPHID 2, version 1.0.0, CBOR and no CTAP1 messages.
              send(channel, 0x86, payload[:8] + CHANNEL + b"\x02\x01\x00\x00\x0c")
          elif command == 0x90 and payload[:1] == b"\x04":
              send(channel, 0x90, b"\x00" + INFO)
          else:
              # ERR_INVALID_CMD
              send(channel, 0xbf, b"\x01")
    '';
in
{
  name = "losos-desktop-tokens";

  node.pkgsReadOnly = false;

  nodes.host =
    { config, pkgs, ... }:
    {
      imports = [ ../modules/tokens.nix ];
      nixpkgs.overlays = [ (import ../pkgs) ];

      # The image has polkit for its own agent, so pcscd asks it.
      security.polkit.enable = true;
      # vsock between programs on this one kernel.
      boot.kernelModules = [
        "vsock_loopback"
        "uhid"
        "hid_generic"
      ];

      # The relay as the module runs it, taking this kernel for "work".
      systemd.services.losos-tokens.serviceConfig.ExecStart = lib.mkForce (
        lib.escapeShellArgs [
          (lib.getExe pkgs.losos-tokens)
          "host"
          "--loopback-machine"
          "work"
          "--forward"
          "8124"
        ]
      );
      # The machine's half, as guest/tokens.nix runs it, with its pcscd
      # socket beside the host's.
      systemd.services.losos-tokens-guest = {
        wantedBy = [ "multi-user.target" ];
        serviceConfig.ExecStart = lib.escapeShellArgs [
          (lib.getExe pkgs.losos-tokens)
          "guest"
          "--host-cid"
          "1"
          "--pcsc-socket"
          "/run/machine/pcscd.comm"
          "--serve"
          "8124"
          "--localhost"
          "127.0.0.2"
        ];
      };
      systemd.services.mock-fido-key.serviceConfig.ExecStart = lib.getExe (mockKey pkgs);

      # vsmartcard's reader, which pcscd lists with no card in it, and
      # its virtual card, which vicc puts in.
      services.pcscd.readerConfigs = [
        ''
          FRIENDLYNAME "Virtual PCD"
          DEVICENAME   /dev/null:0x8C7B
          LIBPATH      ${pkgs.vsmartcard-vpcd}/var/lib/pcsc/drivers/serial/libifdvpcd.so
          CHANNELID    0x8C7B
        ''
      ];
      systemd.services.virtual-card.serviceConfig.ExecStart =
        "${pkgs.vsmartcard-vpcd}/bin/vicc --type iso7816";

      # The app on the machine's localhost that a page hands work to.
      systemd.services.machine-app.serviceConfig.ExecStart = lib.escapeShellArgs [
        "${pkgs.python3}/bin/python3"
        "-m"
        "http.server"
        "--bind"
        "127.0.0.2"
        "--directory"
        (pkgs.writeTextDir "hello.txt" "signed in")
        "8124"
      ];

      environment.systemPackages = [
        pkgs.libfido2
        pkgs.pcsc-tools
        pkgs.curl
      ];
    };

  testScript = ''
    import re

    def copies():
        """The hidraw devices that are the relay's copies."""
        return host.succeed(
            "grep -l '^HID_PHYS=losos-tokens$' /sys/class/hidraw/*/device/uevent"
            " | cut -d/ -f5 || true"
        ).split()

    host.wait_for_unit("losos-tokens.service")
    host.wait_for_unit("losos-tokens-guest.service")

    with subtest("a key plugged into the host shows up in the machine"):
        host.succeed("systemctl start mock-fido-key")
        host.wait_until_succeeds(
            "grep -q '^HID_PHYS=losos-tokens$' /sys/class/hidraw/*/device/uevent"
        )
        [copy] = copies()
        # The host relays its key once, and never the machine's copy.
        host.wait_until_succeeds(
            "journalctl -u losos-tokens | grep -q 'security key plugged in'"
        )
        assert host.succeed(
            "journalctl -u losos-tokens | grep -c 'security key plugged in'"
        ).strip() == "1"

    with subtest("libfido2 in the machine talks to the key through the copy"):
        listed = host.succeed("fido2-token -L")
        assert f"/dev/{copy}" in listed, listed
        info = host.succeed(f"fido2-token -I /dev/{copy}")
        assert "FIDO_2_0" in info, info
        assert "000102030405060708090a0b0c0d0e0f" in info, info
        host.succeed(
            "journalctl -u losos-tokens | grep -q 'using the security key machine=work'"
        )

    with subtest("pulling the key out takes the copy away"):
        host.succeed("systemctl stop mock-fido-key")
        host.wait_until_succeeds(
            "! grep -q '^HID_PHYS=losos-tokens$' /sys/class/hidraw/*/device/uevent"
        )
        host.wait_until_succeeds(
            "journalctl -u losos-tokens | grep -q 'security key pulled out'"
        )

    with subtest("the machine lists the host's smart card readers"):
        readers = host.wait_until_succeeds(
            "PCSCLITE_CSOCK_NAME=/run/machine/pcscd.comm timeout 10 pcsc_scan -r"
            " | grep 'Virtual PCD'"
        )
        assert re.search(r"Virtual PCD 00 0[01]", readers), readers

    with subtest("and the card in one"):
        host.succeed("systemctl start virtual-card")
        host.wait_until_succeeds(
            "PCSCLITE_CSOCK_NAME=/run/machine/pcscd.comm timeout 10 pcsc_scan -c"
            " | grep -q 'ATR'"
        )
        # The one machine held the readers each time it asked.
        host.fail("journalctl -u losos-tokens | grep -q 'refused the smart card readers'")

    with subtest("the host's localhost reaches an app on the machine's"):
        # Nothing on the machine listens yet: the host takes the connection
        # and closes it.
        host.fail("curl -sf --max-time 10 http://127.0.0.1:8124/hello.txt")
        host.succeed("systemctl start machine-app")
        host.wait_until_succeeds(
            "journalctl -u losos-tokens | grep -q 'serves a localhost port machine=work port=8124'"
        )
        reply = host.succeed("curl -sf --max-time 10 http://127.0.0.1:8124/hello.txt")
        assert reply == "signed in", reply
        host.succeed("systemctl stop machine-app")
        host.wait_until_succeeds(
            "journalctl -u losos-tokens | grep -q 'no longer serves a localhost port machine=work port=8124'"
        )
        host.fail("curl -sf --max-time 10 http://127.0.0.1:8124/hello.txt")
  '';
}
