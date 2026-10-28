# derisk's Agent (docs/agent.md), on the PC image, which is the desktop and
# tablet image alike, and in a machine of each person's own rather than in
# their session (docs/agent.md, "Its own machine"). The Halium image goes
# without: it is one image for every Treble phone, and the Agent's window,
# three columns at least 900 logical pixels wide, does not fit a screen
# under 600 on its short side.
#
# The Agent runs Claude Code, which runs whatever it decides to, with the
# person's say-so or without. In their session that was everything they
# have; in the machine it is the Agent folder in their home, the desktop's
# tools derisk offers agents, and the network. guest/agent.nix is the
# machine. Here is the host's half: the machine for each person,
# `agent-<user>`, made the first time they open the Agent and started
# whenever they do; the root end that starts it for them; the end of its
# tools channel in their session; and the Agent's desktop entry, which
# opens it there.
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
  channels = lib.getExe pkgs.losos-agent-machine;

  # The machine as a NixOS system, booted the way service-machines.nix
  # boots the OS's own machines: from this image's store, with its kernel,
  # so it is part of the image and an update brings a new one. One system
  # for everyone; what is a person's is their Agent folder.
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

  # One person's machine, filled in by its unit: @NAME@ agent-<user>, @CID@
  # its address, @RUN@ its run directory, and @TYPE@ and @CPU@ KVM's when
  # the host has it and emulation's otherwise, as for a service machine.
  # Hidden from Machines by its pm metadata, its windows framed in black.
  # The person's Agent folder is its one disk, through virtiofs, which needs
  # the machine's memory shared with virtiofsd; the balloon hands what it
  # frees back to the host.
  domain = pkgs.writeText "agent.xml" ''
    <domain type='@TYPE@'>
      <name>@NAME@</name>
      <title>The Agent's machine</title>
      <metadata>
        <pm:machine xmlns:pm='https://losos-project.github.io/xmlns/pm-machine/1'>
          <pm:label>black</pm:label>
          <pm:service>true</pm:service>
        </pm:machine>
      </metadata>
      <memory unit='MiB'>2048</memory>
      <vcpu>2</vcpu>
      <memoryBacking>
        <source type='memfd'/>
        <access mode='shared'/>
      </memoryBacking>
      <os>
        <type arch='${pkgs.stdenv.hostPlatform.qemuArch}' machine='${
          if pkgs.stdenv.hostPlatform.isx86 then "q35" else "virt"
        }'>hvm</type>
        <kernel>${guest.system.build.kernel}/${guest.system.boot.loader.kernelFile}</kernel>
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
        <filesystem type='mount' accessmode='passthrough'>
          <driver type='virtiofs'/>
          <binary path='@RUN@/virtiofsd'/>
          <source dir='@RUN@/home'/>
          <target dir='home'/>
        </filesystem>
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
  # so it runs only while they are signed in, which is while their home is
  # open, and stops before homed locks it again. Its main process watches
  # the machine, so the unit ends when the machine powers itself off.
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
      mkdir -p -m 0700 "$run/home"

      # The Agent folder, made by the person themselves the first time, so
      # nothing here writes into a home as root. It must be a directory of
      # theirs: a link or someone else's would hand the machine whatever it
      # points at, and virtiofsd runs as root.
      folder=$home/Agent
      setpriv --reuid="$uid" --regid="$gid" --clear-groups mkdir -p -m 0700 "$folder"
      if [ "$(realpath -e "$folder")" != "$folder" ] || [ "$(stat -c %u "$folder")" != "$uid" ]; then
        echo "$folder is not a folder of $user's own" >&2
        exit 1
      fi
      mountpoint -q "$run/home" || mount --bind "$folder" "$run/home"
      # Checked again once mounted, against a swap since the check above.
      if [ ! -d "$run/home" ] || [ -L "$folder" ] || [ "$(stat -c %u "$run/home")" != "$uid" ]; then
        umount "$run/home"
        echo "$folder changed while it was being shared" >&2
        exit 1
      fi

      # virtiofsd as libvirt starts it, with every file the machine makes
      # the person's and every file of theirs its user's, and no device
      # nodes made at all; homed mounts homes nodev and nosuid besides.
      cat > "$run/virtiofsd" <<EOF
      #!${pkgs.runtimeShell}
      exec ${pkgs.virtiofsd}/bin/virtiofsd \
        --translate-uid squash-guest:0:$uid:4294967295 \
        --translate-uid squash-host:0:1000:4294967295 \
        --translate-gid squash-guest:0:$gid:4294967295 \
        --translate-gid squash-host:0:100:4294967295 \
        --modcaps=-mknod \
        "\$@"
      EOF
      chmod 0700 "$run/virtiofsd"

      if [ -e /dev/kvm ]; then
        type=kvm cpu="<cpu mode='host-passthrough'/>"
      else
        type=qemu cpu="<cpu mode='maximum'/>"
      fi
      sed -e "s|@TYPE@|$type|" -e "s|@CPU@|$cpu|" -e "s|@NAME@|$name|" \
        -e "s|@CID@|$((${toString cidBase} + uid))|" -e "s|@RUN@|$run|g" \
        ${domain} > "$run/domain.xml"
      virsh -q define "$run/domain.xml" >/dev/null
      [ "$(virsh -q domstate "$name")" = running ] || virsh -q start "$name"
    '';
    script = ''
      name=agent-$(getent passwd "$uid" | cut -d: -f1)
      while [ "$(virsh -q domstate "$name")" = running ]; do
        sleep 5
      done
    '';
    # Asked to shut down, then stopped if it has not within half a minute,
    # and the folder let go of before the home is locked.
    preStop = ''
      name=agent-$(getent passwd "$uid" | cut -d: -f1)
      virsh -q shutdown "$name" 2>/dev/null || true
      for _ in $(seq 30); do
        [ "$(virsh -q domstate "$name")" = running ] || break
        sleep 1
      done
      virsh -q destroy "$name" 2>/dev/null || true
    '';
    postStop = ''
      run=/run/losos-agent-machine/$uid
      ! mountpoint -q "$run/home" || umount "$run/home"
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
