# Danube in its own machine (docs/hypervisor.md, "The Danube machine").
#
# The browser runs in the machine `danube`, labelled green, where WPE WebKit
# is the distribution's prebuilt one rather than this repository's build
# (nixos/pkgs/wpewebkit.nix): Arch Linux, whose extra repository carries the
# same 2.54.2 release the overlay builds. The template is Arch's cloud
# image, pinned to a dated release; at the machine's first boot cloud-init
# provisions it from the CD this module attaches, doing what the Linux tools
# CD's setup.sh does by hand (docs/hypervisor.md, "Other distributions")
# and, because Arch does not package Danube, building the browser once from
# the sources and the vendored crates the same CD carries, against the
# distribution's WebKit. No patch rides along: web processes run in
# WebKit's own bwrap sandbox, inside the machine's (docs/danube.md,
# "Sandbox and hardening").
#
# Everything else is the guest machinery every machine has, so the desktop
# lists and opens Danube there as it would in the LosOS guest: the QEMU
# guest agent, a waypipe server over vsock, losos-guest-run, and the label's
# nftables.
{
  config,
  lib,
  pkgs,
  ...
}:

let
  machine = "danube";
  template = "losos-danube";

  # The component sources, at the commits flake.lock pins (sources.nix).
  sources = import ../pkgs/sources.nix;

  # Arch's cloud image, pinned to a dated release: "latest" would make every
  # build a different template. It boots UEFI, grows its root with cloud-init
  # over the machine's whole disk, and carries cloud-init itself, which
  # provisions the machine at its first boot from the CD below.
  archImage = pkgs.fetchurl {
    url = "https://geo.mirror.pkgbuild.com/images/v20261115.625457/Arch-Linux-x86_64-cloudimg.qcow2";
    hash = "sha256-29AoMPbrD+B+9uXtrduui1d85CltnnkRiQRwTM4KH2A=";
  };

  # Danube's sources as the machine builds them, laid out as the paths in
  # Cargo.toml expect: the browser at src/danube, the mcsapi crates it
  # reaches by path at components/mcsapi, and the crates of Cargo.lock
  # vendored, so the build needs no network and is the release's own pins.
  danubeSrc = lib.fileset.toSource {
    root = ../..;
    fileset = ../../src/danube;
  };
  vendor = pkgs.rustPlatform.importCargoLock {
    lockFile = ../../src/danube/Cargo.lock;
  };

  # losos-guest-run for this machine: the one script the LosOS guest and the
  # tools CD install, with the user and the waypipe socket named. The socket
  # is wayland-0, as in the LosOS guest, because no desktop of the machine's
  # own runs here (docs/hypervisor.md, "The guest template").
  guestRun = pkgs.writeText "losos-guest-run" (
    builtins.replaceStrings
      [ "@user@" "@display@" ]
      [ "user" "wayland-0" ]
      (builtins.readFile ../guest/losos-guest-run.sh)
  );

  # What cloud-init runs, once, at the first boot. A shell script, so it is
  # one thing to read: the packages, the account, the browser, the guest
  # machinery (docs/hypervisor.md, "Other distributions").
  userData = pkgs.writeText "user-data" ''
    #!/bin/bash
    set -euo pipefail

    cdpath=/run/losos-danube
    mkdir -p "$cdpath"
    mount -L cidata "$cdpath"

    # WebKit is the distribution's prebuilt one, with the WPEPlatform API
    # and the headless platform Danube draws through. The rest is the guest
    # machinery and the toolchain the browser builds with.
    pacman -Syu --noconfirm --needed \
      wpewebkit waypipe qemu-guest-agent \
      mesa libxkbcommon libseccomp aria2 xdg-dbus-proxy glib-networking \
      rust clang gcc pkg-config
    systemctl enable --now qemu-guest-agent.service

    # The account whose apps the desktop shows, whose manager runs from
    # boot, as the LosOS guest's user does.
    user=user
    id -u "$user" >/dev/null 2>&1 || useradd -m -U "$user"

    # The browser, built once against the distribution's WebKit from the
    # CD's vendored sources: minutes, on the machine's own CPU, and never
    # again.
    build=/var/lib/losos-danube
    mkdir -p "$build"
    cp -a "$cdpath/src" "$cdpath/components" "$cdpath/vendor" "$build/"
    mkdir -p "$build/src/danube/.cargo"
    cat > "$build/src/danube/.cargo/config.toml" <<EOF
    [source.crates-io]
    replace-with = "vendored-sources"
    [source.vendored-sources]
    directory = "$build/vendor"
    EOF
    CARGO_TARGET_DIR="$build/target" cargo build --offline --release \
      --manifest-path "$build/src/danube/Cargo.toml"
    install -m 0755 "$build/target/release/danube" /usr/local/bin/danube
    install -Dm 0644 "$cdpath/org.losos.Danube.desktop" \
      /usr/local/share/applications/org.losos.Danube.desktop
    install -Dm 0644 "$cdpath/org.losos.Danube.svg" \
      /usr/local/share/icons/hicolor/scalable/apps/org.losos.Danube.svg

    # The guest machinery, as the tools CD's setup.sh installs it.
    install -m 0755 "$cdpath/losos-guest-run" /usr/local/bin/losos-guest-run
    echo "$user" >/etc/losos-guest-user
    cat >/etc/systemd/user/losos-waypipe.service <<'EOF'
    [Unit]
    Description=Show this machine's apps on the host's desktop

    [Service]
    ExecStart=/usr/bin/waypipe --vsock --socket 2:7350 --no-gpu --display wayland-0 server -- sleep infinity
    Restart=always

    [Install]
    WantedBy=default.target
    EOF
    systemctl --global enable losos-waypipe.service
    loginctl enable-linger "$user"
  '';

  metaData = pkgs.writeText "meta-data" ''
    instance-id: losos-danube-1
    local-hostname: ${machine}
  '';

  # The machine's provisioning CD, labelled cidata for cloud-init: what it
  # runs, the guest-run it installs, the desktop files, and the sources and
  # vendored crates the browser builds from. Attached once, below; cloud-init
  # runs this CD's user-data once per instance-id.
  cdTree = pkgs.runCommand "losos-danube-cd-tree" { } ''
    mkdir -p $out/src $out/components
    install -m 0644 ${userData} $out/user-data
    install -m 0644 ${metaData} $out/meta-data
    install -m 0755 ${guestRun} $out/losos-guest-run
    install -m 0644 ${../pkgs/danube/org.losos.Danube.desktop} $out/org.losos.Danube.desktop
    install -m 0644 ${../pkgs/danube/danube.svg} $out/org.losos.Danube.svg
    cp -r ${danubeSrc}/src/danube $out/src/danube
    cp -r ${sources.mcsapi} $out/components/mcsapi
    cp -r ${vendor} $out/vendor
  '';
  cd = pkgs.runCommand "losos-danube-cd.iso"
    {
      nativeBuildInputs = [ pkgs.buildPackages.xorriso ];
    }
    ''
      xorriso -as mkisofs -quiet -J -R -V cidata -o $out ${cdTree}
    '';
in
{
  losos.machines.foreignTemplates.${template} = {
    file = archImage;
    apps = "org.losos.Danube.desktop\tDanube";
  };

  losos.machines.system.${machine} = {
    inherit template;
    label = "green";
    # WebKit in the machine and its one build at the first boot.
    memory = 4096;
    # The OS's: out of the Machines app, its apps in the palette.
    service = true;
  };

  # The machine's provisioning CD, attached to its definition once. It rides
  # no boot but the first (cloud-init runs user-data once per instance-id),
  # but it stays attached, the way an installer machine keeps its CD.
  systemd.services.losos-danube-cd = {
    description = "Give the Danube machine its provisioning CD";
    wantedBy = [ "multi-user.target" ];
    requires = [ "losos-system-machines.service" ];
    after = [ "losos-system-machines.service" ];
    path = [
      config.virtualisation.libvirtd.package
      pkgs.coreutils
      pkgs.gnugrep
    ];
    environment.LIBVIRT_DEFAULT_URI = "qemu:///system";
    serviceConfig = {
      Type = "oneshot";
      RemainAfterExit = true;
    };
    script = ''
      virsh -q dominfo ${machine} >/dev/null 2>&1 || exit 0
      if virsh -q dumpxml ${machine} 2>/dev/null | grep -q losos-danube-cd; then
        exit 0
      fi
      virsh attach-disk ${machine} ${cd} sdb --type cdrom --sourcetype file \
        --mode readonly --config
    '';
  };

  # Buildable on its own, for checking the CD: its sources and vendored
  # crates are what the machine builds the browser from.
  system.build.lososDanubeCd = cd;
}
