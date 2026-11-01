# LosOS boxes found over mDNS (losos-boxes.nix, docs/losos-boxes.md).
#
# Two boxes as far as the desktop can tell, each Avahi and nginx as a LosOS
# box runs them: `box` announces `_losos._tcp` with its release, as boxes
# do since that announcement, and `oldbox` only `_workstation._tcp`, as
# boxes did before it, and is told apart by its /setup/state.json. The
# desktop browses through systemd-resolved alone, as the image does: no
# Avahi on it.
#
# Not the whole image, only what finding boxes needs, so it boots the test
# driver's kernel quickly and runs without KVM too.
{ lib, ... }:

let
  # A box's front vhost, with the one route a box serves before setup that
  # tells it apart (setup.nix in the LosOS repository, abridged).
  boxNode =
    { announce }:
    { config, ... }:
    {
      services.avahi = {
        enable = true;
        openFirewall = true;
        publish = {
          enable = true;
          addresses = true;
          workstation = true;
        };
        # configuration.nix in the LosOS repository, as a box announces it.
        extraServiceFiles = lib.optionalAttrs announce {
          losos = ''
            <?xml version="1.0" standalone='no'?>
            <!DOCTYPE service-group SYSTEM "avahi-service.dtd">
            <service-group>
              <name replace-wildcards="yes">%h</name>
              <service>
                <type>_losos._tcp</type>
                <port>80</port>
                <txt-record>txtvers=1</txt-record>
                <txt-record>version=v0.4.0</txt-record>
              </service>
            </service-group>
          '';
        };
      };
      services.nginx = {
        enable = true;
        virtualHosts.front = {
          default = true;
          locations."= /setup/state.json".return = "200 '${
            builtins.toJSON {
              inherit (config.networking) hostName;
              fqdn = "${config.networking.hostName}.local";
              address = "";
              tls = false;
              certificate = null;
            }
          }'";
          locations."/".return = "200 'admin pages'";
        };
      };
      networking.firewall.allowedTCPPorts = [ 80 ];
    };
in
{
  name = "losos-desktop-losos-boxes";

  # The desktop's packages come from the OS's overlay.
  node.pkgsReadOnly = false;

  nodes = {
    box = boxNode { announce = true; };
    oldbox = boxNode { announce = false; };

    desktop =
      { pkgs, ... }:
      {
        nixpkgs.overlays = [ (import ../pkgs) ];
        environment.systemPackages = [
          pkgs.losos-boxes
          pkgs.curl
          (pkgs.writeShellApplication {
            name = "losos-guest-run-test";
            runtimeInputs = with pkgs; [
              coreutils
              gawk
              gnused
              systemd
            ];
            text = builtins.replaceStrings [ "@user@" "@display@" ] [ "user" "wayland-0" ] (
              builtins.readFile ../guest/losos-guest-run.sh
            );
          })
        ];

        # network.nix's mDNS: resolved with MulticastDNS on the link and in
        # resolved, 5353 open. networkd stands in for NetworkManager here,
        # because it keeps the test network's static address as it is; the
        # machines test checks that NetworkManager turns mDNS on per link.
        networking.useNetworkd = true;
        systemd.network.networks."40-eth1".networkConfig.MulticastDNS = true;
        services.resolved = {
          enable = true;
          settings.Resolve.MulticastDNS = true;
        };
        networking.firewall.allowedUDPPorts = [ 5353 ];
      };
  };

  testScript = ''
    start_all()
    for node in [box, oldbox]:
        node.wait_for_unit("avahi-daemon.service")
        node.wait_for_unit("nginx.service")
    desktop.wait_for_unit("systemd-resolved.service")
    desktop.wait_for_unit("systemd-networkd.service")

    with subtest("list finds the announced box and the one from before"):
        found = desktop.wait_until_succeeds(
            "out=$(losos-boxes list) && echo \"$out\" | grep -qxP 'box\\thttp://box.local\\tv0.4.0' "
            "&& echo \"$out\" | grep -qxP 'oldbox\\thttp://oldbox.local\\t' && echo \"$out\"",
            timeout=180,
        )
        print(found)
        assert len(found.strip().splitlines()) == 2, "a machine that is not a box was listed"

    with subtest("watch writes an entry per box"):
        box.succeed("systemctl stop avahi-daemon.service")
        desktop.succeed("""
            mkdir -p /tmp/data /tmp/mock-bin
            printf '%s\\n' '#!/bin/sh' 'echo poll >>/tmp/keyring-polls' \\
              "if [ -e /tmp/keyring-locked ]; then echo 'b true'; else echo 'b false'; fi" \\
              >/tmp/mock-bin/busctl
            printf '%s\\n' '#!/bin/sh' 'cat /tmp/losos-hosts' >/tmp/mock-bin/losos
            chmod +x /tmp/mock-bin/busctl /tmp/mock-bin/losos
            printf 'http://box.local  box\\n  away from home: https://box.losos.cfd\\n' >/tmp/losos-hosts
            PATH=/tmp/mock-bin:/run/current-system/sw/bin \\
              LOSOS_BOXES_REMEMBER_EVERY_SECS=1 XDG_DATA_HOME=/tmp/data \\
              losos-boxes watch >/tmp/watch.log 2>&1 &
        """)
        apps = "/tmp/data/applications"
        desktop.wait_for_file(f"{apps}/losos-box-box.local.desktop", timeout=60)
        desktop.wait_for_file(f"{apps}/losos-box-oldbox.local.desktop", timeout=60)
        entry = desktop.succeed(f"cat {apps}/losos-box-box.local.desktop")
        print(entry)
        assert "\nComment=LosOS box box.local, away from home, through box.losos.cfd\n" in entry
        assert "losos-boxes terminal http://box.local\n" in entry
        cloud = f"{apps}/losos-box-box.local@cloud.desktop"
        desktop.wait_for_file(cloud, timeout=60)
        assert "Exec=xdg-open https://box.losos.cfd/cloud/" in desktop.succeed(
            f"cat {cloud}"
        )
        listed = desktop.succeed(
            f"LOSOS_GUEST_APPLICATIONS={apps} losos-guest-run-test --list"
        )
        assert "losos-box-box.local@cloud\tLosOS Cloud — box\n" in listed

        with subtest("a locked keyring preserves away entries"):
            desktop.succeed(
                "touch /tmp/keyring-locked; : >/tmp/losos-hosts; "
                "wc -l </tmp/keyring-polls >/tmp/polls-before"
            )
            desktop.wait_until_succeeds(
                "test $(wc -l </tmp/keyring-polls) -gt $(cat /tmp/polls-before)",
                timeout=10,
            )
            desktop.succeed(f"test -e {apps}/losos-box-box.local.desktop && test -e {cloud}")

        with subtest("away entries become a LAN entry, then return when the box leaves"):
            desktop.succeed(
                "rm /tmp/keyring-locked; "
                "printf 'http://box.local  box\\n  away from home: https://box.losos.cfd\\n' "
                ">/tmp/losos-hosts"
            )
            box.succeed("systemctl start avahi-daemon.service")
            desktop.wait_until_succeeds(
                f"grep -q 'Comment=LosOS v0.4.0 box at box.local' {apps}/losos-box-box.local.desktop",
                timeout=60,
            )
            lan_entry = desktop.succeed(f"cat {apps}/losos-box-box.local.desktop")
            assert "\nExec=xdg-open http://box.local/\n" in lan_entry
            assert "losos-boxes terminal http://box.local\n" in lan_entry
            desktop.succeed("curl -sf http://box.local/ | grep -qx 'admin pages'")
            desktop.wait_until_fails(f"test -e {cloud}", timeout=10)
            box.succeed("systemctl stop avahi-daemon.service")
            desktop.wait_until_succeeds(
                f"grep -q 'away from home' {apps}/losos-box-box.local.desktop",
                timeout=60,
            )
            desktop.wait_for_file(cloud, timeout=10)

        with subtest("logging out removes the away entries"):
            desktop.succeed(": >/tmp/losos-hosts")
            desktop.wait_until_fails(
                f"test -e {apps}/losos-box-box.local.desktop || test -e {cloud}",
                timeout=10,
            )

    with subtest("clear removes the remaining LAN entry"):
        desktop.succeed(f"test -e {apps}/losos-box-oldbox.local.desktop")
        desktop.succeed("pkill -f 'losos-boxes watch'")
        desktop.succeed("XDG_DATA_HOME=/tmp/data losos-boxes clear")
        desktop.succeed(f"test -z \"$(ls -A {apps})\"")
        print(desktop.succeed("cat /tmp/watch.log"))
  '';
}
