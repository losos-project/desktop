# The network shares machine, sys-shares (docs/files-machine.md, "Network
# shares"): the SMB, NFS and WebDAV shares a person adds are mounted there
# and not on the host, as drives are in the files machine, and reach Files
# the same way, under /run/media/<user>/<name>. Its one network card is on
# the host's link to the network machine, which routes it as it routes the
# host: through the ad blocker, and through Tor in schizo mode. It keeps
# the shares and their passwords on its own state disk, which only it
# reads.
#
# BusyBox and losos-filesd, as the files machine, with the kernel's CIFS
# and NFS clients and davfs2 for WebDAV. It starts when a share is added,
# and at boot once one has been (the host keeps that fact, not the
# shares), and powers off a minute after the last one is removed.
#
# Backups to a LosOS box do not go through it: losos-backup talks HTTPS to
# the box through the LosOS boxes machine and pins its certificate
# (docs/backup.md).
{
  config,
  lib,
  pkgs,
  ...
}:

let
  name = "sys-shares";
  cid = 4103;
  ports = {
    list = 5200;
    sftp = 5201;
    eject = 5202;
    add = 5203;
    remove = 5204;
  };
  addresses = import ./network-addresses.nix;
  inherit (addresses) up;
  marker = "/var/lib/losos-shares/added";

  filesdConfig = pkgs.writeText "losos-filesd.conf" ''
    sftp-server ${pkgs.openssh}/libexec/sftp-server
    idle 60
    state-disk /dev/vda /var/lib/losos-shares
    shares /var/lib/losos-shares/shares
    link ${addresses.shares.mac} ${addresses.shares.address}/${toString up.prefix} ${up.machine}
    nameserver ${up.machine}
    run losos-vm-call serve ${toString ports.list} losos-filesd list
    run losos-vm-call serve ${toString ports.sftp} losos-filesd sftp
    run losos-vm-call serve ${toString ports.eject} losos-filesd eject
    run losos-vm-call serve ${toString ports.add} losos-filesd add
    run losos-vm-call serve ${toString ports.remove} losos-filesd remove
  '';

  sftp = pkgs.writeShellScript "losos-shares-sftp" ''
    exec ${lib.getExe pkgs.losos-vm-call} call shares-sftp "$LOSOS_FILES_NAME"
  '';

  sharesHost = pkgs.writeShellApplication {
    name = "losos-shares-host";
    runtimeInputs = [
      pkgs.losos-vm-call
      pkgs.sshfs
      pkgs.fuse3
      pkgs.gnugrep
      pkgs.coreutils
      pkgs.systemd
      pkgs.util-linux
    ];
    text =
      builtins.replaceStrings
        [ "@machine@" "@sftp@" "@list@" "@state@" ]
        [ name "${sftp}" "shares-list" "/run/losos-shares" ]
        (builtins.readFile ./losos-files-host.sh);
  };

  # `losos-shares add|remove|list|eject`, for Settings and a terminal. Adding
  # starts the machine, and the host remembers that there are shares so
  # it starts at the next boot too.
  sharesTool = pkgs.writeShellApplication {
    name = "losos-shares";
    runtimeInputs = [
      pkgs.losos-vm-call
      pkgs.systemd
      pkgs.coreutils
    ];
    text = ''
      usage() {
        echo "usage: losos-shares list" >&2
        echo "       losos-shares add <name> <smb://|nfs://|davs://...> (user and password on stdin, one per line)" >&2
        echo "       losos-shares remove|eject <name>" >&2
        exit 2
      }
      case ''${1:-} in
        list) exec losos-vm-call call shares-list ;;
        add)
          [ $# -eq 3 ] || usage
          systemctl start losos-shares-wanted.service
          for _ in $(seq 120); do
            losos-vm-call call shares-list >/dev/null 2>&1 && break
            sleep 1
          done
          exec losos-vm-call call shares-add "$2" "$3"
          ;;
        remove) [ $# -eq 2 ] || usage; exec losos-vm-call call shares-remove "$2" ;;
        eject) [ $# -eq 2 ] || usage; exec losos-vm-call call shares-eject "$2" ;;
        *) usage ;;
      esac
    '';
  };
in
{
  options.losos.machines.shares.enable = lib.mkOption {
    type = lib.types.bool;
    default = config.losos.machines.network.enable;
    description = ''
      Whether network shares are mounted in the network shares machine
      rather than on the host.
    '';
  };

  config = lib.mkIf config.losos.machines.shares.enable {
    losos.busyboxGuests.${name} = {
      init = "${lib.getExe pkgs.losos-filesd} init";
      hostKernelTree = true;
      packages = [
        pkgs.losos-filesd
        pkgs.losos-vm-call
        pkgs.davfs2
      ];
      modules = [
        "virtio_net"
        "virtio_blk"
        "virtio_balloon"
        "ext4"
        "cifs"
        "nfs"
        "fuse"
      ];
      # Kicksecure keeps network file systems out; mounting them is this
      # machine's whole job. The servers (nfsd, ksmbd) stay out.
      kicksecure.allowModules = [
        "cifs"
        "cifs_arc4"
        "cifs_md4"
        "nfs"
        "nfs_acl"
        "nfsv3"
        "nfsv4"
        "nfs_layout_nfsv41_files"
        "nfs_layout_flexfiles"
      ];
      files."/etc/losos-filesd.conf" = filesdConfig;
    };

    losos.serviceMachines.${name} = {
      busybox = name;
      title = "The network shares machine";
      memory = 256;
      stateSize = "16M";
      inherit cid;
      interfaces = [
        {
          network = up.libvirt;
          inherit (addresses.shares) mac;
        }
      ];
      whenStopped = "stop";
    };

    systemd.services."losos-service-machine-${name}" = {
      requires = [ "losos-network-links.service" ];
      after = [ "losos-network-links.service" ];
    };

    # Starts the machine, and keeps the fact that shares were added, so it
    # starts at boot from then on; with none left it powers off a minute
    # after, at every boot.
    systemd.services.losos-shares-wanted = {
      description = "Start the network shares machine";
      serviceConfig = {
        Type = "oneshot";
        ExecStart = [
          "${pkgs.coreutils}/bin/mkdir -p /var/lib/losos-shares"
          "${pkgs.coreutils}/bin/touch ${marker}"
          "${pkgs.systemd}/bin/systemctl start --no-block losos-service-machine-${name}.service"
        ];
      };
    };
    systemd.services.losos-shares-boot = {
      description = "Start the network shares machine for its shares";
      wantedBy = [ "multi-user.target" ];
      after = [ "losos-network-links.service" ];
      unitConfig.ConditionPathExists = marker;
      serviceConfig = {
        Type = "oneshot";
        ExecStart = "${pkgs.systemd}/bin/systemctl start --no-block losos-service-machine-${name}.service";
      };
    };

    systemd.services.losos-shares-host = {
      description = "Mount the network shares for the person at the screen";
      bindsTo = [ "losos-service-machine-${name}.service" ];
      after = [
        "losos-service-machine-${name}.service"
        "losos-vm-call-relay.service"
      ];
      wantedBy = [ "losos-service-machine-${name}.service" ];
      serviceConfig = {
        ExecStart = lib.getExe sharesHost;
        Restart = "on-failure";
        RuntimeDirectory = "losos-shares";
      };
    };

    losos.vmCalls =
      let
        call = port: allow: { inherit cid port allow; };
        person = [ "active-session" ];
      in
      {
        shares-list = call ports.list person;
        shares-sftp = call ports.sftp [ ];
        shares-eject = call ports.eject person;
        shares-add = call ports.add person;
        shares-remove = call ports.remove person;
      };

    environment.systemPackages = [ sharesTool ];
  };
}
