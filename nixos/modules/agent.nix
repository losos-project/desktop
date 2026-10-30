# derisk's Agent (docs/agent.md), on the PC image, which is the desktop and
# tablet image alike, and in a machine of each person's own rather than in
# their session (docs/agent.md, "Its own machine"). The Halium image goes
# without: it is one image for every Treble phone, and the Agent's window,
# three columns at least 900 logical pixels wide, does not fit a screen
# under 600 on its short side.
#
# The Agent runs Claude Code, which runs whatever it decides to, with the
# person's say-so or without. In their session that was everything they
# have; in the machine it is the machine's own disk, the files the person
# picks for it in the portal machine's dialogs, the desktop's tools derisk
# offers agents, and the network. Their files reach it the way they reach
# every machine, through the portal, files and network shares machines
# (docs/portal-machine.md), and never by a share of the host's own.
# guest/agent.nix is the machine. Here is the host's half: the machine for
# each person, `agent-<user>`, made the first time they open the Agent and
# started whenever they do; the root end that starts it for them; the end
# of its tools channel in their session; and the Agent's desktop entry,
# which opens it there.
#
# Sonne was here before it: the project's fork of Zed, with this agent
# inside it. It took about half a gigabyte of /usr for an editor the Text
# Editor and the Agent's own code view already cover, so it left the image
# and its agent moved into derisk; it is not coming back.
{
  config,
  lib,
  pkgs,
  modulesPath,
  ...
}:

let
  libvirt = config.virtualisation.libvirtd.package;
  qemu = config.virtualisation.libvirtd.qemu.package;
  channels = lib.getExe pkgs.losos-agent-machine;

  # The machine as a NixOS system, booted the way service-machines.nix
  # boots the OS's own machines: from this image's store, with its kernel,
  # so it is part of the image and an update brings a new one. One system
  # for everyone; what is a person's is on their disk.
  guest =
    (import "${modulesPath}/../lib/eval-config.nix" {
      system = null;
      modules = [
        ../guest/service.nix
        ../guest/agent.nix
        {
          losos.service.name = "agent";
          nixpkgs.pkgs = pkgs;
          boot.kernelPackages = lib.mkForce config.boot.kernelPackages;
        }
      ];
    }).config;

  # losos-agent-machine's CID_BASE: a person's machine is at this plus
  # their uid on vsock.
  cidBase = 1000000;

  # The libvirt secret that holds the key of the disk of the person with
  # $uid while their machine starts, one for each person.
  secret = "printf '1b0e3f5a-6a1d-4c8e-9f2b-%012x' \"$uid\"";

  # One person's machine, filled in by its unit: @NAME@ agent-<user> and
  # @PERSON@ <user>, whom the portal's relay serves it for alone, @CID@
  # its address, @DISK@ and @SECRET@ its disk and that disk's key, and
  # @TYPE@ and @CPU@ KVM's when the host has it and emulation's otherwise,
  # as for a service machine. Hidden from Machines by its pm metadata, its
  # windows framed in black. Its one disk is its home, encrypted with a key
  # kept in the person's home, which homed encrypts in turn: what the Agent
  # keeps, Claude Code's sign-in among it, is as locked as the rest of
  # theirs whenever they are signed out. The balloon hands what the machine
  # frees back to the host.
  domain = pkgs.writeText "agent.xml" ''
    <domain type='@TYPE@'>
      <name>@NAME@</name>
      <title>The Agent's machine</title>
      <metadata>
        <pm:machine xmlns:pm='https://losos-project.github.io/xmlns/pm-machine/1'>
          <pm:label>black</pm:label>
          <pm:service>true</pm:service>
          <pm:person>@PERSON@</pm:person>
        </pm:machine>
      </metadata>
      <memory unit='MiB'>2048</memory>
      <vcpu>2</vcpu>
      <os>
        <type arch='${pkgs.stdenv.hostPlatform.qemuArch}' machine='${
          if pkgs.stdenv.hostPlatform.isx86 then "q35" else "virt"
        }'>hvm</type>
        <kernel>${
          # The image's kernel image, which carries every module's
          # certificate (kernel-hardening.nix), as service-machines.nix boots.
          config.system.build.kernelImage
            or "${guest.system.build.kernel}/${guest.system.boot.loader.kernelFile}"
        }</kernel>
        <initrd>${guest.system.build.initialRamdisk}/${guest.system.boot.loader.initrdFile}</initrd>
        <cmdline>init=${guest.system.build.toplevel}/init ${toString guest.boot.kernelParams}</cmdline>
      </os>
      @CPU@
      <on_poweroff>destroy</on_poweroff>
      <on_reboot>restart</on_reboot>
      <on_crash>destroy</on_crash>
      <devices>
        <filesystem type='mount' accessmode='squash'>
          <source dir='/nix/store'/>
          <target dir='nix-store'/>
          <readonly/>
        </filesystem>
        <disk type='file' device='disk'>
          <driver name='qemu' type='qcow2' discard='unmap'/>
          <source file='@DISK@'/>
          <target dev='vda' bus='virtio'/>
          <encryption format='luks'>
            <secret type='passphrase' uuid='@SECRET@'/>
          </encryption>
        </disk>
        <interface type='network'>
          <source network='default'/>
          <model type='virtio'/>
        </interface>
        <vsock model='virtio'>
          <cid auto='no' address='@CID@'/>
        </vsock>
        <memballoon model='virtio' freePageReporting='on'/>
        <serial type='pty'>
          <target port='0'/>
        </serial>
        <console type='pty'/>
        <rng model='virtio'>
          <backend model='random'>/dev/urandom</backend>
        </rng>
      </devices>
    </domain>
  '';
in
{
  # The machine's own closure, for `nix build` and the checks.
  system.build.agentMachine = guest.system.build.toplevel;

  # The Agent's desktop entry, as derisk ships it, opening the Agent in the
  # person's machine: the app grid's Agent, and the derisk-agent: links of
  # the palette's "Ask the agent".
  environment.systemPackages = [
    pkgs.losos-agent-machine
    (pkgs.runCommand "derisk-agent-entry" { } ''
      install -Dm644 -t $out/share/applications \
        ${pkgs.derisk.agent}/share/applications/org.derisk.agent.desktop
      substituteInPlace $out/share/applications/org.derisk.agent.desktop \
        --replace-fail "Exec=${lib.getExe pkgs.derisk.agent}" "Exec=${channels} open"
    '')
  ];

  # Where a person's session asks for their machine. Anyone may connect;
  # the machine started is the one of whoever the kernel says connected.
  systemd.sockets.losos-agent-machine-open = {
    description = "Open the Agent in its machine";
    wantedBy = [ "sockets.target" ];
    socketConfig = {
      ListenStream = "/run/losos-agent-machine/open.sock";
      SocketMode = "0666";
      Accept = true;
    };
  };
  systemd.services."losos-agent-machine-open@" = {
    description = "Open the Agent in its machine";
    path = [ config.systemd.package ];
    serviceConfig = {
      ExecStart = "${channels} serve-open";
      StandardInput = "socket";
      StandardError = "journal";
    };
  };

  # The machine of the person with this uid. Bound to their own systemd,
  # so it runs only while they are signed in, which is while their home and
  # the disk's key in it are open, and stops before homed locks it again.
  # Its main process watches the machine, so the unit ends when the machine
  # powers itself off.
  systemd.services."losos-agent-machine@" = {
    description = "The Agent's machine for uid %i";
    bindsTo = [ "user@%i.service" ];
    requires = [ "libvirtd.service" ];
    after = [
      "user@%i.service"
      "libvirtd.service"
    ];
    path = [
      libvirt
      qemu
      pkgs.coreutils
      pkgs.gnused
      pkgs.getent
      pkgs.util-linux
    ];
    environment = {
      LIBVIRT_DEFAULT_URI = "qemu:///system";
      # The instance, for every script below: only ExecStart gets arguments.
      uid = "%i";
    };
    # Defined again at every start, as a service machine is, so an update's
    # machine is the one that starts.
    preStart = ''
      IFS=: read -r user _ _ gid _ home _ < <(getent passwd "$uid")
      name=agent-$user
      run=/run/losos-agent-machine/$uid
      disk=/var/lib/losos-agent-machine/$uid/home.qcow2
      mkdir -p -m 0700 "$run"
      # QEMU, as libvirt's own user, opens the disk inside.
      mkdir -p -m 0711 "''${disk%/*}"
      as() { setpriv --reuid="$uid" --regid="$gid" --clear-groups -- "$@"; }

      # The disk's key, made by the person themselves the first time and
      # read as them every time, so nothing here reads or writes in a home
      # as root, and a link in it reaches only what they can read. Only
      # with their home open: a key not found in a home that is not there
      # would be made again, over a disk that still has one.
      as test -d "$home"
      key=$home/.local/share/losos/agent-machine.key
      if ! as test -e "$key"; then
        as sh -c 'umask 077 && mkdir -p "''${1%/*}" && head -c 32 /dev/urandom | base64 -w0 > "$1"' sh "$key"
        # A disk whose key is gone opens for no one.
        rm -f "$disk"
      fi
      (umask 077 && as cat "$key" > "$run/key")
      [ -s "$run/key" ]
      [ -e "$disk" ] || qemu-img create -q -f qcow2 \
        --object secret,id=key,file="$run/key" \
        -o encrypt.format=luks,encrypt.key-secret=key "$disk" 32G

      # The key, to libvirt for this start alone: kept in its memory, never
      # shown by it, and forgotten once QEMU has opened the disk.
      secret=$(${secret})
      cat > "$run/secret.xml" <<EOF
      <secret ephemeral='yes' private='yes'><uuid>$secret</uuid></secret>
      EOF
      virsh -q secret-define "$run/secret.xml" >/dev/null
      virsh -q secret-set-value "$secret" --file "$run/key" --plain >/dev/null
      rm -f "$run/key"

      if [ -e /dev/kvm ]; then
        type=kvm cpu="<cpu mode='host-passthrough'/>"
      else
        type=qemu cpu="<cpu mode='maximum'/>"
      fi
      sed -e "s|@TYPE@|$type|" -e "s|@CPU@|$cpu|" -e "s|@NAME@|$name|" \
        -e "s|@PERSON@|$user|" \
        -e "s|@CID@|$((${toString cidBase} + uid))|" -e "s|@DISK@|$disk|" \
        -e "s|@SECRET@|$secret|" ${domain} > "$run/domain.xml"
      virsh -q define "$run/domain.xml" >/dev/null
      [ "$(virsh -q domstate "$name")" = running ] || virsh -q start "$name"
      virsh -q secret-undefine "$secret" >/dev/null
    '';
    script = ''
      name=agent-$(getent passwd "$uid" | cut -d: -f1)
      while [ "$(virsh -q domstate "$name")" = running ]; do
        sleep 5
      done
    '';
    # Asked to shut down, then stopped if it has not within half a minute.
    preStop = ''
      name=agent-$(getent passwd "$uid" | cut -d: -f1)
      virsh -q shutdown "$name" 2>/dev/null || true
      for _ in $(seq 30); do
        [ "$(virsh -q domstate "$name")" = running ] || break
        sleep 1
      done
      virsh -q destroy "$name" 2>/dev/null || true
    '';
    # What a start that failed halfway leaves of the key.
    postStop = ''
      rm -f /run/losos-agent-machine/$uid/key
      virsh -q secret-undefine "$(${secret})" >/dev/null 2>&1 || true
    '';
    serviceConfig.Type = "simple";
  };

  # The person's end of their machine's tools channel, for their session:
  # derisk's desktop tools and web links, from their machine alone.
  systemd.user.services.losos-agent-tools = {
    description = "Let the Agent's machine use the desktop's tools";
    wantedBy = [ "graphical-session.target" ];
    partOf = [ "graphical-session.target" ];
    after = [ "graphical-session.target" ];
    serviceConfig = {
      ExecStart = "${channels} tools";
      Restart = "on-failure";
      RestartSec = 5;
    };
  };

  # Text and source files have no default again, as before Sonne claimed
  # them: derisk's Text Editor cannot take one, its desktop entry living
  # inside the derisk binary with no file argument and no types
  # (docs/not-done.md).
}
