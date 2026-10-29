# The edge the PC's traffic leaves through (docs/network-machine.md,
# "Through an edge"), the host's half. The network machine does the work
# (losos-netd's edge thread): it picks the edge the way a LosOS box does,
# enrols itself at the exit with the person's account, and sends the host's
# and its machines' traffic through a WireGuard tunnel there, or nowhere
# while the tunnel is down. In schizo mode Tor carries everything instead,
# and the edge is where it leaves Tor: the machine hands this host the
# edge's onion address and SOCKS account, and the host gives them to the
# Tor gateway (schizo.nix, docs/schizo-mode.md, "Through an edge").
#
# The host keeps only the account, root's alone, and the machine's word on
# how things stand:
#
#   losos-edge                 the status, as JSON
#   losos-edge set <id> [url]  the account, its token on stdin; the URL is
#                              the edge to use when no gateway is in reach
#   losos-edge off             back to the network's own way out
{
  config,
  lib,
  pkgs,
  ...
}:

let
  addresses = import ./network-addresses.nix;
  schizoAddresses = import ./schizo-addresses.nix;
  inherit (schizoAddresses) edge;
  # The account, and the copy the network machine reads from the host's
  # share (network-machine.nix), which only libvirt's QEMU user may read.
  stateDir = "/var/lib/losos-edge";
  account = "${stateDir}/account";
  share = "/run/losos-net";
  schizoFlag = "/var/lib/losos/schizo";
  chainFile = "${edge.hostDir}/${edge.file}";

  # Root's half of `losos-edge set|off`, run through systemd so that polkit
  # asks an administrator, as for schizo mode.
  store = pkgs.writeShellApplication {
    name = "losos-edge-store";
    runtimeInputs = [ pkgs.coreutils ];
    text = ''
      umask 077
      mkdir -p ${stateDir}
      case "$1" in
        set)
          id=$2 url=''${3:-}
          IFS= read -r token || [ -n "$token" ]
          case "$id$token$url" in
            *[[:space:]]*) echo "the id, token and URL take no spaces" >&2; exit 2 ;;
          esac
          [ -n "$id" ] && [ -n "$token" ] || { echo "an id and a token" >&2; exit 2; }
          {
            echo "id $id"
            echo "token $token"
            [ -z "$url" ] || echo "url $url"
          } > ${account}.new
          mv ${account}.new ${account}
          ;;
        off) rm -f ${account} ${chainFile} ;;
      esac
      ${lib.getExe shareAccount}
    '';
  };

  # The machine's copy, made again from the account at every boot.
  shareAccount = pkgs.writeShellApplication {
    name = "losos-edge-share";
    runtimeInputs = [ pkgs.coreutils ];
    text = ''
      if [ -e ${account} ]; then
        install -m 0600 -o qemu-libvirtd ${account} ${share}/edge.new
        mv ${share}/edge.new ${share}/edge
      else
        rm -f ${share}/edge
      fi
    '';
  };

  command = pkgs.writeShellApplication {
    name = "losos-edge";
    runtimeInputs = [
      pkgs.losos-vm-call
      pkgs.systemd
    ];
    text = ''
      root() {
        if [ "$(id -u)" = 0 ]; then
          exec ${lib.getExe store} "$@"
        fi
        exec systemd-run --quiet --pipe --wait --collect --service-type=exec \
          -p ProtectHome=yes ${lib.getExe store} "$@"
      }
      case ''${1:-status} in
        status) exec losos-vm-call call edge-status ;;
        set)
          [ $# -ge 2 ] && [ $# -le 3 ] || { echo "usage: losos-edge set <id> [<url>] (token on stdin)" >&2; exit 2; }
          root "$@"
          ;;
        off) root off ;;
        *)
          echo "usage: losos-edge [status|set <id> [<url>]|off]" >&2
          exit 2
          ;;
      esac
    '';
  };

  # The Tor gateway's file follows the machine's word while schizo mode is
  # on and an edge is set. Until the machine has enrolled, and whenever it
  # cannot say, the file is a placeholder or the last good one: the
  # gateway's chain takes the file's presence as "leave through the edge",
  # refuses what it cannot carry there, and so never falls back to Tor's
  # own exits. Only an edge turned off, or schizo mode, takes the file
  # away. 9p shows the gateway no change as it happens, and it looks for
  # the file every two seconds; so does this.
  pending = pkgs.writeShellApplication {
    name = "losos-edge-chain-pending";
    runtimeInputs = [ pkgs.coreutils ];
    text = ''
      if [ -e ${account} ] && [ -e ${schizoFlag} ]; then
        if [ ! -e ${chainFile} ]; then
          umask 077
          mkdir -p ${edge.hostDir}
          # Not a file losos-edge-chain can use, so it refuses everything.
          echo '{"pending":true}' > ${chainFile}
        fi
      else
        rm -f ${chainFile}
      fi
    '';
  };

  chain = pkgs.writeShellApplication {
    name = "losos-edge-chain-host";
    runtimeInputs = [
      pkgs.losos-vm-call
      pkgs.coreutils
      pkgs.diffutils
    ];
    text = ''
      while :; do
        ${lib.getExe pending}
        if [ -e ${chainFile} ] && json=$(losos-vm-call call edge-chain 2>/dev/null) &&
          [ -n "$json" ] && ! printf '%s\n' "$json" | cmp -s - ${chainFile}; then
          umask 077
          printf '%s\n' "$json" > ${chainFile}.new
          mv ${chainFile}.new ${chainFile}
        fi
        sleep 2
      done
    '';
  };
in
{
  config = lib.mkIf config.losos.machines.network.enable {
    environment.systemPackages = [ command ];

    systemd.tmpfiles.rules = [ "d ${stateDir} 0700 root root -" ];

    systemd.services.losos-edge-share = {
      description = "Hand the network machine the edge account";
      wantedBy = [ "multi-user.target" ];
      before = [ "losos-service-machine-sys-net.service" ];
      after = [ "systemd-tmpfiles-setup.service" ];
      serviceConfig = {
        Type = "oneshot";
        RemainAfterExit = true;
        ExecStart = lib.getExe shareAccount;
      };
    };

    systemd.services.losos-edge-chain = {
      description = "Hand the Tor gateway the edge to leave Tor through";
      bindsTo = [ "losos-service-machine-sys-net.service" ];
      after = [
        "losos-service-machine-sys-net.service"
        "losos-vm-call-relay.service"
      ];
      wantedBy = [ "losos-service-machine-sys-net.service" ];
      serviceConfig = {
        ExecStart = lib.getExe chain;
        Restart = "always";
        RestartSec = 2;
      };
    };

    # The placeholder is there before the gateway's first look, which runs
    # before Tor starts.
    systemd.services.losos-edge-chain-pending = {
      description = "Hold the Tor gateway to the edge until it is enrolled";
      wantedBy = [ "losos-service-machine-losos-tor-gateway.service" ];
      before = [ "losos-service-machine-losos-tor-gateway.service" ];
      serviceConfig = {
        Type = "oneshot";
        ExecStart = lib.getExe pending;
      };
    };

    losos.vmCalls = {
      # Settings and `losos-edge` ask how things stand.
      edge-status = {
        inherit (addresses) cid;
        port = addresses.ports.edgeStatus;
        allow = [
          "group:wheel"
          "active-session"
        ];
      };
      # Root's alone: it holds the edge's SOCKS password.
      edge-chain = {
        inherit (addresses) cid;
        port = addresses.ports.edgeChain;
      };
    };
  };
}
