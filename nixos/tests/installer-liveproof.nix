# Boot the installer ISO and prove the live-medium changes are present:
# livepatch wiring, driver rules, and disk release before repart.
{
  self,
}:

let
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
  sessionExec = installerSystem.config.systemd.services.derisk-installer.serviceConfig.ExecStart;

  disksLine = builtins.toJSON ''
    printf '%s\n' '{"method":"disks"}' | (set -- ${sessionExec}; shift 1; "$@") | head -1
  '';

  installBusy = builtins.toJSON ''
    printf '%s\n' '{"method":"install","disk":"/dev/vdb"}' \
      | (set -- ${sessionExec}; shift 1; "$@") \
      > /tmp/install-liveproof.log 2>&1
  '';
in
{
  name = "losos-desktop-installer-liveproof";

  nodes.machine =
    { ... }:
    {
      virtualisation = {
        directBoot.enable = false;
        mountHostNixStore = false;
        useEFIBoot = true;
        qemu.options = [
          "-cdrom ${installerIso}"
          "-boot order=d"
        ];
        memorySize = 4096;
        emptyDiskImages = [ 65536 ];
        tpm.enable = true;
      };
    };

  testScript = ''
    import json

    machine.succeed("findmnt --kernel /iso")
    machine.succeed("findmnt --kernel /nix/store")
    machine.wait_for_unit("derisk-installer.service")

    with subtest("livepatch channel wiring is on the medium"):
        machine.succeed("command -v losos-installer-livepatch")
        machine.succeed("test -f /etc/losos/installer-live.json")
        machine.succeed("jq -e .repository /etc/losos/installer-live.json")
        machine.succeed("test -f /etc/losos/installer-backend")
        machine.succeed(
            "systemctl show derisk-installer.service --property=ExecStart --value "
            "| grep -q losos-installer-session"
        )
        machine.succeed("systemctl cat losos-installer-livepatch-check.service | grep -q check")

    with subtest("losos-hardware rules and NVIDIA stack are on the medium"):
        machine.succeed("test -f /etc/udev/rules.d/99-losos-hardware.rules")
        machine.succeed("command -v losos-hardware")
        machine.succeed("test -f /etc/systemd/system/losos-hardware.service")
        machine.succeed("grep -rq nvidia_drm /etc/modprobe.d/")

    with subtest("a livepatch offer is merged into the backend hello"):
        offer = {"current": "20000101.000000", "version": "20991231.235959", "channel": "nightly"}
        machine.succeed(
            "mkdir -p /run/losos-installer && "
            + f"printf '%s' {json.dumps(json.dumps(offer))} "
            + "> /run/losos-installer/livepatch-offer.json"
        )
        line = machine.succeed(${disksLine})
        print(line)
        assert "livepatch" in line, line

    with subtest("releasing a busy partition no longer blocks repart"):
        machine.succeed("printf 'label: gpt\\n,1M\\n,512M\\n,128M\\n' | sfdisk /dev/vdb")
        machine.succeed("dmsetup create busy --table '0 2097152 linear /dev/vdb2 0'")
        machine.execute(${installBusy}, timeout=180)
        log = machine.succeed("cat /tmp/install-liveproof.log")
        print(log)
        machine.succeed("dmsetup remove busy || true")
        machine.fail("grep -qi 'resource busy' /tmp/install-liveproof.log")
        machine.succeed("grep -q '\"label\":\"Partitioning the disk\"' /tmp/install-liveproof.log")
  '';
}
