# The Agent's machine (agent.nix, guest/agent.nix, docs/agent.md, "Its own
# machine"): a person opens the Agent, the host's root end starts their
# machine, `agent-<user>`, hidden from Machines and at the address their
# uid gives it, and the Agent's window reaches the host from that address,
# which is how the desktop frames it as the machine's. The machine's home
# is the person's Agent folder, and what it writes there is theirs. Its
# tools channel answers it in the person's session. Root and the system's
# accounts have no machine, and the machine stops with its unit.
#
# The desktop's half, derisk's gate and the compositor, is derisk's and is
# tested there; here a listener on the gate's port stands in for it and
# records who connected, as in machines.nix. The machine runs under TCG,
# with the test VM's KVM modules kept out, so it does not depend on nested
# virtualisation the runner may not give.
{ lib, ... }:

{
  name = "losos-desktop-agent-machine";

  node.pkgsReadOnly = false;

  nodes.machine =
    { pkgs, ... }:
    {
      imports = [
        ../modules/libvirt.nix
        ../modules/agent.nix
      ];
      nixpkgs.overlays = [ (import ../pkgs) ];

      boot.blacklistedKernelModules = [
        "kvm_intel"
        "kvm_amd"
      ];

      virtualisation = {
        cores = 4;
        memorySize = 4096;
      };

      # A person, signed in as far as the machine's unit cares: their own
      # systemd runs, which is what it binds itself to.
      users.users.alex = {
        isNormalUser = true;
        uid = 1000;
        linger = true;
      };

      environment.etc."vsock-listen.py".text = ''
        import socket, sys
        listener = socket.socket(socket.AF_VSOCK, socket.SOCK_STREAM)
        listener.bind((socket.VMADDR_CID_ANY, 7350))
        listener.listen()
        connection, (cid, port) = listener.accept()
        with open(sys.argv[1], "w") as peer:
            peer.write(str(cid))
      '';

      environment.systemPackages = [ pkgs.python3 ];
    };

  testScript = ''
    import re

    virsh = "virsh -c qemu:///system"
    cid = 1_000_000 + 1000
    user = "systemctl --user -M alex@"

    start_all()
    machine.wait_for_unit("libvirtd.service")
    machine.wait_for_unit("user@1000.service")
    machine.wait_for_unit("losos-agent-machine-open.socket")

    with subtest("root has no Agent machine"):
        out = machine.fail("losos-agent-machine open 2>&1")
        assert "uid 0 has no Agent machine" in out, out

    with subtest("the person's session answers their machine's tools"):
        machine.succeed(
            "mkdir -p /run/user/1000/derisk",
            "echo '{\"accent\":\"#3584e4\"}' > /run/user/1000/derisk/theme.json",
            "chown -R alex /run/user/1000/derisk",
        )
        machine.succeed(f"{user} start losos-agent-tools.service")
        machine.wait_until_succeeds(f"ss -l --vsock | grep -q {cid}")

    with subtest("opening the Agent starts the person's machine and its window reaches the desktop"):
        machine.succeed("systemd-run --unit=vsock-listen python3 /etc/vsock-listen.py /tmp/peer")
        machine.wait_until_succeeds("ss -l --vsock | grep -q 7350")
        machine.succeed(
            "systemd-run --unit=open-agent --uid=alex -p StandardOutput=file:/tmp/open.log"
            " -p StandardError=file:/tmp/open.log"
            " losos-agent-machine open 'derisk-agent:ask?prompt=hello'"
        )
        machine.wait_for_unit("losos-agent-machine@1000.service")
        xml = machine.succeed(f"{virsh} dumpxml agent-alex")
        assert f"<cid auto='no' address='{cid}'/>" in xml, xml
        metadata = machine.succeed(
            f"{virsh} metadata agent-alex https://losos-project.github.io/xmlns/pm-machine/1"
        )
        assert re.search(r"<\w*:?service>true</", metadata), metadata
        machine.wait_until_succeeds("test -s /tmp/peer", timeout=1500)
        peer = machine.succeed("cat /tmp/peer").strip()
        assert peer == str(cid), f"the window came from {peer}, the machine is {cid}"
        machine.wait_until_succeeds("! systemctl is-active open-agent.service", timeout=120)
        print(machine.succeed("cat /tmp/open.log"))
        machine.succeed("systemctl show -p Result open-agent.service | grep -qx Result=success")

    with subtest("the machine read the desktop's theme through the tools channel"):
        machine.succeed(
            "journalctl --no-pager _SYSTEMD_USER_UNIT=losos-agent-tools.service"
            " | grep -q \"read the desktop's theme\""
        )

    with subtest("the machine's home is the person's Agent folder, and its files are theirs"):
        machine.succeed("test \"$(stat -c %U /home/alex/Agent)\" = alex")
        machine.wait_until_succeeds("test -d /home/alex/Agent/.local/share/derisk-agent/chats", timeout=600)
        owners = machine.succeed("find /home/alex/Agent -printf '%u\\n' | sort -u").split()
        assert owners == ["alex"], f"the Agent folder holds files of {owners}"

    with subtest("the machine stops with its unit and lets the folder go"):
        machine.succeed("systemctl stop losos-agent-machine@1000.service")
        machine.succeed(f"{virsh} domstate agent-alex | grep -q 'shut off'")
        machine.fail("mountpoint -q /run/losos-agent-machine/1000/home")
  '';
}
