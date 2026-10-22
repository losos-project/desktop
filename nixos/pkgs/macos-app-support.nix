# macOS app support: macOS apps as windows of their own on the desktop
# (docs/macos-apps.md). macOS runs in dockur/macos, a container whose QEMU
# downloads macOS's recovery from Apple's own servers and installs it, with
# Reims vGPU drawing its Metal on this PC's GPU through Vulkan. Inside it,
# macos-app-server sends each window to app-windows here
# (docs/app-windows.md), as WinBoat's guest server and FreeRDP do for
# Windows (windows-app-support.nix).
#
# Nothing of macOS is in the image or this repository: dockur fetches it
# from Apple when a person first opens the app. Apple's licence lets macOS
# run only on Apple hardware, in a virtual machine or not, so the app runs
# only on a PC whose firmware says Apple made it, such as an Intel Mac
# running LosOS.
#
# - `macos-app-support` is what the app grid opens, and what each macOS
#   app's entry runs (`macos-app-support open ID`): macOS's container,
#   made the first time, then app-windows against it.
# - `macos-app-support-setup` makes the container and the folder macOS
#   shares with it, and waits for macOS's app server to answer.
# - `/Volumes/shared/install` inside macOS builds and installs
#   macos-app-server from this image's own source, as the person's login
#   item, with the token that app-windows here holds.
{
  lib,
  pm,
  pm-plugins,
  app-windows,
  writeShellApplication,
  writeText,
  runCommand,
  makeDesktopItem,
  symlinkJoin,
  coreutils,
  gawk,
  jq,
  libnotify,
  podman-compose,
  systemd,
  util-linux,
  xdg-utils,
}:

let
  # dockur/macos 3.12, which takes VERSION, GPU and USER_PORTS
  # (docs/environment.md in dockur/macos).
  image = "ghcr.io/dockur/macos:3.12";

  # The server's port, inside macOS and here. app-windows uses it unless
  # told otherwise, so the entries it writes need no address.
  port = "7180";

  # pm with the podman plugin, under a configuration of its own, as
  # windows-app-support.nix explains for WinBoat's.
  containerPm = writeShellApplication {
    name = "pm";
    runtimeInputs = [
      coreutils
      util-linux
      podman-compose
    ];
    text = ''
      state=''${XDG_STATE_HOME:-$HOME/.local/state}/macos-app-support
      export XDG_CONFIG_HOME=$state/config
      plugins=$XDG_CONFIG_HOME/pm/plugins
      plugin=${pm-plugins}/share/pm/plugins/podman.wasm
      if ! cmp -s "$plugin" "$plugins/podman.wasm"; then
        mkdir -p "$plugins"
        chmod 0700 "$state"
        (
          flock 9
          [ -e "$XDG_CONFIG_HOME/pm/signing.key" ] || ${lib.getExe pm} keygen >&2
          if ! cmp -s "$plugin" "$plugins/podman.wasm"; then
            install -m 0644 "$plugin" "$plugins/podman.wasm.new"
            mv "$plugins/podman.wasm.new" "$plugins/podman.wasm"
            ${lib.getExe pm} sign "$plugins/podman.wasm" >&2
          fi
        ) 9> "$state/lock"
      fi
      exec ${lib.getExe pm} "$@"
    '';
  };
  containerPmPath = runCommand "macos-app-support-pm" { } ''
    mkdir -p $out/bin
    ln -s ${lib.getExe containerPm} $out/bin/pm
  '';

  # The container, less what the setup fills in per person. The server's
  # port goes into macOS over QEMU's user network (USER_PORTS) and is bound
  # here on 127.0.0.1 only; the web viewer, which shows macOS's own screen
  # for its Setup Assistant, goes to any free port, read back from Podman.
  compose = builtins.toFile "macos-compose.json" (
    builtins.toJSON {
      name = "macos-app-support";
      services.macos = {
        inherit image;
        container_name = "macOS";
        environment = {
          VERSION = "15";
          DISK_SIZE = "64G";
          NETWORK = "user";
          USER_PORTS = port;
        };
        cap_add = [ "NET_ADMIN" ];
        ports = [
          "127.0.0.1::8006"
          "127.0.0.1:${port}:${port}"
        ];
        stop_grace_period = "120s";
        restart = "on-failure";
        devices = [
          "/dev/kvm"
          "/dev/net/tun"
        ];
      };
    }
  );

  # What runs inside macOS, from the shared folder, once its Setup
  # Assistant is done: the Xcode command line tools and Rust if they are
  # missing, then macos-app-server built from the source beside it and
  # started at every login, with the token beside it. macOS asks for Screen
  # Recording and Accessibility as the server first starts.
  install = writeText "install" ''
    #!/bin/sh
    # Installs LosOS's app server in this macOS (docs/macos-apps.md).
    set -eu
    here=$(cd "$(dirname "$0")" && pwd)
    dir="$HOME/Library/Application Support/LosOS App Server"
    mkdir -p "$dir"
    until xcode-select -p > /dev/null 2>&1; do
      echo "Installing the command line developer tools; finish macOS's dialog."
      xcode-select --install 2> /dev/null || true
      sleep 30
    done
    if ! [ -x "$HOME/.cargo/bin/cargo" ]; then
      curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs |
        sh -s -- -y --profile minimal --no-modify-path
    fi
    rm -rf "$dir/src"
    cp -R "$here/app-windows" "$dir/src"
    chmod -R u+w "$dir/src"
    (cd "$dir/src" && "$HOME/.cargo/bin/cargo" build --release --locked -p macos-app-server)
    install -m 0755 "$dir/src/target/release/macos-app-server" "$dir/macos-app-server"
    install -m 0600 "$here/token" "$dir/token"
    agent="$HOME/Library/LaunchAgents/os.losos.app-server.plist"
    mkdir -p "$(dirname "$agent")"
    cat > "$agent" <<EOF
    <?xml version="1.0" encoding="UTF-8"?>
    <!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
    <plist version="1.0">
    <dict>
      <key>Label</key><string>os.losos.app-server</string>
      <key>ProgramArguments</key><array><string>$dir/macos-app-server</string></array>
      <key>RunAtLoad</key><true/>
      <key>KeepAlive</key><true/>
      <key>StandardErrorPath</key><string>$dir/server.log</string>
    </dict>
    </plist>
    EOF
    launchctl bootout "gui/$(id -u)/os.losos.app-server" 2> /dev/null || true
    launchctl bootstrap "gui/$(id -u)" "$agent"
    "$dir/macos-app-server" permissions || true
    echo "Done. Allow Screen Recording and Accessibility for macos-app-server if macOS asks."
  '';

  setup = writeShellApplication {
    name = "macos-app-support-setup";
    runtimeInputs = [
      coreutils
      gawk
      jq
      libnotify
      xdg-utils
      containerPmPath
    ];
    text = ''
      state=''${XDG_STATE_HOME:-$HOME/.local/state}/macos-app-support
      compose=$state/compose.yml
      shared=$state/shared
      # macOS's disk.
      storage=''${XDG_DATA_HOME:-$HOME/.local/share}/macos-app-support
      token=''${XDG_CONFIG_HOME:-$HOME/.config}/app-windows/macos/token

      notify() {
        notify-send --app-name="macOS app support" --icon=computer "$1" "$2" || true
      }
      trap 'notify "macOS could not be set up" "See journalctl --user -u macos-app-support-setup."' ERR

      # Apple's licence: macOS on Apple hardware only.
      vendor=$(cat /sys/class/dmi/id/sys_vendor 2> /dev/null || true)
      if [ "$vendor" != "Apple Inc." ]; then
        notify "macOS needs a Mac" "Apple lets macOS run only on Apple hardware, and this PC is not a Mac."
        exit 1
      fi

      exists() {
        pm container list | grep -x macOS > /dev/null
      }

      # The token app-windows here and the server in macOS share.
      if ! [ -s "$token" ]; then
        mkdir -p "$(dirname "$token")"
        (umask 077 && head -c 32 /dev/urandom | base64 > "$token")
      fi

      if ! exists; then
        if ! [ -r /dev/kvm ] || ! [ -w /dev/kvm ]; then
          notify "macOS could not be set up" "This Mac has no hardware virtualisation (KVM) for macOS to run on."
          exit 1
        fi
        mkdir -p "$state" "$storage"

        # The folder macOS mounts with mount_9p: the installer, the token
        # and the app server's source, new from this image each time.
        rm -rf "$shared"
        mkdir -p "$shared"
        cp -r --no-preserve=mode ${app-windows.src} "$shared/app-windows"
        install -m 0755 ${install} "$shared/install"
        install -m 0600 "$token" "$shared/token"

        # Half the Mac's memory and processors, within what macOS needs.
        memory=$(awk '/^MemTotal:/ { print int($2 / 1048576 / 2) }' /proc/meminfo)
        memory=$(( memory < 4 ? 4 : memory > 16 ? 16 : memory ))
        cores=$(( $(nproc) / 2 ))
        cores=$(( cores < 2 ? 2 : cores > 8 ? 8 : cores ))

        # Reims vGPU where this Mac has a render node for it: Metal inside
        # macOS drawn by this GPU through Vulkan. Without one, QEMU's
        # VMware adapter, drawn in software.
        gpu=N
        devices='[]'
        if [ -e /dev/dri ]; then
          gpu=Y
          devices='["/dev/dri"]'
        fi

        jq \
          --arg memory "''${memory}G" --arg cores "$cores" --arg gpu "$gpu" \
          --argjson gpu_devices "$devices" \
          --arg storage "$storage:/storage" --arg shared "$shared:/shared" \
          '.services.macos.environment += { RAM_SIZE: $memory, CPU_CORES: $cores, GPU: $gpu }
           | .services.macos.devices += $gpu_devices
           | .services.macos.volumes = [$storage, $shared]' \
          ${compose} > "$compose.new"
        mv "$compose.new" "$compose"

        notify "Setting up macOS" "macOS is downloading from Apple. Its own screen opens in the browser for its Setup Assistant."
        # Podman pulls dockur's image the first time.
        pm container up "$compose"
        fresh=1
      fi

      # The first time, macOS's own screen, for its Setup Assistant and the
      # one command that installs the app server.
      if [ -n "''${fresh:-}" ]; then
        until viewer=$(pm container ports macOS 2> /dev/null | awk '$1 == "8006/tcp" { print $3 }') && [ -n "$viewer" ]; do
          sleep 2
        done
        xdg-open "http://$viewer/" || true
        notify "Finish setting up macOS" "When macOS's Setup Assistant is done, open Terminal there and run: sudo mount_9p shared && /Volumes/shared/install"
      fi

      # macOS is ready when its app server answers.
      until (exec 3<> /dev/tcp/127.0.0.1/${port}) 2> /dev/null; do
        if [ "$(pm container status macOS 2> /dev/null)" != running ]; then
          notify "macOS stopped" "Open macOS app support to start it again."
          exit 1
        fi
        sleep 10
      done
      if [ -n "''${fresh:-}" ]; then
        notify "macOS is ready" "Its apps are in the app grid."
      fi
    '';
  };

  launcher = writeShellApplication {
    name = "macos-app-support";
    runtimeInputs = [
      systemd
      app-windows
      containerPmPath
    ];
    text = ''
      # macOS first, its container started if it is stopped, through the
      # setup unit so it carries on while nothing here is open; then
      # app-windows, whose entries for macOS's apps run this again.
      if ! (exec 3<> /dev/tcp/127.0.0.1/${port}) 2> /dev/null; then
        # Podman's own word for its state: "exited" once it has stopped.
        status=$(pm container status macOS 2> /dev/null || true)
        if [ -n "$status" ] && [ "$status" != running ]; then
          pm container start macOS
        fi
        systemctl --user reset-failed macos-app-support-setup.service 2> /dev/null || true
        systemctl --user start --wait macos-app-support-setup.service
      fi
      [ $# -gt 0 ] || set -- show
      exec app-windows --exec macos-app-support "$@"
    '';
  };

  desktopItem = makeDesktopItem {
    name = "macos-app-support";
    desktopName = "macOS app support";
    comment = "macOS apps as windows of their own";
    exec = "macos-app-support";
    icon = "computer";
    categories = [ "Utility" ];
  };

in
symlinkJoin {
  pname = "macos-app-support";
  version = "0.1.0";
  paths = [
    launcher
    setup
    desktopItem
  ];
  passthru = {
    inherit setup install;
    pm = containerPm;
  };
  meta = {
    description = "macOS apps as windows of their own, from macOS in a container";
    license = lib.licenses.agpl3Plus;
    platforms = [ "x86_64-linux" ];
    mainProgram = "macos-app-support";
  };
}
