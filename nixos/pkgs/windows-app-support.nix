# Windows app support: Windows apps as windows of their own on the desktop
# (docs/windows-apps.md). It is WinBoat
# ([TibixDev/winboat](https://github.com/TibixDev/winboat)), MIT, under the
# name the desktop gives it. WinBoat runs Windows in dockur/windows, a
# container whose QEMU installs Windows from Microsoft's own download with
# an answer file, and opens each app as a FreeRDP RemoteApp window.
#
# Three things here are this OS's, not WinBoat's:
#
# - WinBoat runs its container through pm (`pm container`, whose podman
#   plugin runs it with rootless Podman and refuses root), never Podman or
#   Docker itself, and offers Podman only
#   (patches/winboat/0001-run-containers-through-pm.patch).
# - `windows-app-support-setup` does what WinBoat's setup wizard does once
#   it has its answers, with answers of its own: the person's name as the
#   Windows account, a random password nobody types, half the PC's memory
#   and processors. So Windows sets itself up the first time the app opens,
#   and nobody is asked anything.
# - `windows-app-support` is what the app grid opens: the setup first, when
#   this person has no Windows yet, then WinBoat.
#
# `winboat` is nixpkgs' own package from its unpatched set: it runs on
# Electron, which nixpkgs builds from source and which would link the
# overlay's patched GTK. Patching WinBoat's source only rebuilds its own
# small Node build; Electron and the Windows guest server substitute.
{
  lib,
  winboat,
  pm,
  pm-plugins,
  writeShellApplication,
  runCommand,
  makeDesktopItem,
  symlinkJoin,
  coreutils,
  curl,
  diffutils,
  gawk,
  jq,
  libargon2,
  libnotify,
  podman,
  podman-compose,
  systemd,
  util-linux,
}:

let
  # The dockur/windows WinBoat 0.9.2 starts, which its own Podman compose
  # file names (src/renderer/data/podman.ts). The setup below writes that
  # file itself, so it has to name the same image; the build stops if a
  # newer WinBoat names another.
  image = "ghcr.io/dockur/windows:6.05";

  # pm for WinBoat and the setup, with the podman plugin. pm runs only
  # plugins its user signed, from their own configuration, so this keeps a
  # configuration of its own, as the machines the system makes for each
  # person do (modules/machines.nix): its own key, and the plugin signed
  # with it, renewed when the image brings another. The containers are the
  # person's as ever, in their Podman's store; Podman reads its settings
  # from this configuration too, and finds none.
  containerPm = writeShellApplication {
    name = "pm";
    runtimeInputs = [
      coreutils
      diffutils
      podman
      util-linux
      # What the plugin's steps run besides Podman, which the person's PATH
      # has, beside the setuid newuidmap it needs.
      podman-compose
    ];
    text = ''
      state=''${XDG_STATE_HOME:-$HOME/.local/state}/windows-app-support
      export XDG_CONFIG_HOME=$state/config
      plugins=$XDG_CONFIG_HOME/pm/plugins
      plugin=${pm-plugins}/share/pm/plugins/podman.wasm
      if ! cmp -s "$plugin" "$plugins/podman.wasm"; then
        mkdir -p "$plugins"
        chmod 0700 "$state"
        # WinBoat asks several things at once; one of them sets this up.
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
  # On a PATH of its own, ahead of the system's, for WinBoat and the setup
  # only: as `pm`, which is what the patch has WinBoat run.
  containerPmPath = runCommand "windows-app-support-pm" { } ''
    mkdir -p $out/bin
    ln -s ${lib.getExe containerPm} $out/bin/pm
  '';

  app = winboat.overrideAttrs (old: {
    patches = (old.patches or [ ]) ++ [ ./patches/winboat/0001-run-containers-through-pm.patch ];

    postPatch = old.postPatch + ''
      grep -qF '"${image}"' src/renderer/data/podman.ts

      # The name the desktop gives it, wherever WinBoat shows its own:
      # its window's title, the sidebar and the About page. WinBoat stays
      # named where it is credited and where Windows names its service.
      substituteInPlace src/renderer/index.html \
        --replace-fail '<title>WinBoat</title>' '<title>Windows app support</title>'
      substituteInPlace src/main/main.ts \
        --replace-fail 'title: "WinBoat",' 'title: "Windows app support",'
      substituteInPlace src/renderer/App.vue \
        --replace-fail '<x-label>WinBoat</x-label>' '<x-label>Windows app support</x-label>' \
        --replace-fail '>WinBoat</h1>' '>Windows app support</h1>' \
        --replace-fail 'WinBoat Beta v{{' 'Built on WinBoat v{{'
      substituteInPlace src/renderer/views/About.vue \
        --replace-fail '<h2 class="mb-0">WinBoat</h2>' '<h2 class="mb-0">Windows app support</h2>' \
        --replace-fail 'WinBoat Beta v{{' 'Built on WinBoat v{{'
      substituteInPlace src/renderer/views/Config.vue \
        --replace-fail '<x-label class="mb-4 text-neutral-300">WinBoat</x-label>' \
          '<x-label class="mb-4 text-neutral-300">Windows app support</x-label>'

      # Each app's window over RDP's graphics pipeline as H.264 in full
      # colour (AVC444): a video stream instead of the bitmaps RDP sends
      # otherwise, less to carry and to draw for every frame that changes.
      # nixpkgs' FreeRDP decodes it with FFmpeg.
      substituteInPlace src/renderer/lib/winboat.ts \
        --replace-fail '"/compression",' '"/compression", "/gfx:AVC444",'

      # And Windows' side of it: the policy that has its Remote Desktop
      # prefer AVC444, among the registry changes WinBoat's guest setup
      # imports at Windows' first sign-in. The file ends without a newline.
      printf '\n\n%s\n%s\n' \
        '[HKEY_LOCAL_MACHINE\SOFTWARE\Policies\Microsoft\Windows NT\Terminal Services]' \
        '"AVC444ModePreferred"=dword:00000001' >> guest_server/RDPApps.reg
    '';

    # The app grid's entry is below, named for the desktop and opening
    # through the setup. Its file keeps WinBoat's name, winboat.desktop,
    # which is the app_id Electron gives the window, so the desktop matches
    # one to the other.
    desktopItems = [ ];
  });

  # WinBoat's Podman compose file, as its setup wizard writes it
  # (src/renderer/data/podman.ts and lib/install.ts), less what the setup
  # fills in per person. Ports are bound on 127.0.0.1 only, each to any
  # free port, which WinBoat reads back from Podman.
  compose = builtins.toFile "winboat-compose.json" (
    builtins.toJSON {
      name = "winboat";
      volumes.data = null;
      services.windows = {
        inherit image;
        container_name = "WinBoat";
        environment = {
          VERSION = "11";
          DISK_SIZE = "64G";
          HOME = "\${HOME}";
          NETWORK = "user";
          USER_PORTS = "7148";
          HOST_PORTS = "7149";
          ARGUMENTS = "-qmp tcp:0.0.0.0:7149,server,wait=off";
        };
        cap_add = [ "NET_ADMIN" ];
        ports = [
          "127.0.0.1::8006"
          "127.0.0.1::7148"
          "127.0.0.1::7149"
          "127.0.0.1::3389/tcp"
          "127.0.0.1::3389/udp"
        ];
        stop_grace_period = "120s";
        restart = "on-failure";
        privileged = true;
        volumes = [ "./oem:/oem" ];
        devices = [
          "/dev/kvm"
          "/dev/bus/usb"
        ];
      };
    }
  );

  setup = writeShellApplication {
    name = "windows-app-support-setup";
    runtimeInputs = [
      coreutils
      curl
      gawk
      jq
      libargon2
      libnotify
      containerPmPath
    ];
    text = ''
      dir=$HOME/.winboat
      compose=$dir/podman-compose.yml
      # Windows' disk, which WinBoat's wizard would put in ~/winboat.
      storage=''${XDG_DATA_HOME:-$HOME/.local/share}/winboat

      notify() {
        notify-send --app-name="Windows app support" --icon=winboat "$1" "$2" || true
      }
      trap 'notify "Windows could not be set up" "See journalctl --user -u windows-app-support-setup."' ERR

      exists() {
        pm container list | grep -x WinBoat > /dev/null
      }

      if ! exists; then
        if ! [ -r /dev/kvm ] || ! [ -w /dev/kvm ]; then
          notify "Windows could not be set up" "This PC has no hardware virtualisation (KVM) for Windows to run on."
          exit 1
        fi

        # A compose file without its container is a Windows set up before
        # and its container removed: WinBoat makes it again from the file,
        # disk and all, and so does this.
        if ! [ -e "$compose" ]; then
          mkdir -p "$dir" "$storage"

          # WinBoat's Windows half, which dockur copies to C:\OEM and runs
          # at Windows' first sign-in: its guest server, as a service, and
          # the registry changes that let every program be a RemoteApp.
          rm -rf "$dir/oem"
          cp -r --no-preserve=mode ${app}/share/winboat/resources/guest_server "$dir/oem"

          # The Windows account. Its name is the person's, as far as
          # Windows takes one (letters and digits, at most 20); its
          # password is random, and WinBoat reads it back from the compose
          # file for every RDP session, so nobody types it. The guest
          # server checks the host by the password's Argon2id hash, with
          # the parameters WinBoat's own hash uses.
          user=$(printf %s "$USER" | tr -dc 'A-Za-z0-9' | cut -c1-20)
          [ "''${#user}" -ge 2 ] || user=winboat
          password=$(head -c 32 /dev/urandom | base64 | tr -d '+/=\n' | cut -c1-24)
          salt=$(head -c 16 /dev/urandom | base64 | tr -d '+/=\n')
          printf %s "$password" | argon2 "$salt" -id -t 3 -m 16 -p 4 -l 32 -e > "$dir/oem/auth.hash"

          # Half the PC's memory and processors, within what Windows 11
          # needs and what it makes use of.
          memory=$(awk '/^MemTotal:/ { print int($2 / 1048576 / 2) }' /proc/meminfo)
          memory=$(( memory < 4 ? 4 : memory > 16 ? 16 : memory ))
          cores=$(( $(nproc) / 2 ))
          cores=$(( cores < 2 ? 2 : cores > 8 ? 8 : cores ))

          # Windows in the person's own language where dockur has it.
          case "''${LANG%%[_.@]*}" in
            ar) language=Arabic ;; bg) language=Bulgarian ;; zh) language=Chinese ;;
            hr) language=Croatian ;; cs) language=Czech ;; da) language=Danish ;;
            nl) language=Dutch ;; et) language=Estonian ;; fi) language=Finnish ;;
            fr) language=French ;; de) language=German ;; el) language=Greek ;;
            he) language=Hebrew ;; hu) language=Hungarian ;; it) language=Italian ;;
            ja) language=Japanese ;; ko) language=Korean ;; lv) language=Latvian ;;
            lt) language=Lithuanian ;; nb | nn | no) language=Norwegian ;;
            pl) language=Polish ;; pt) language=Portuguese ;; ro) language=Romanian ;;
            ru) language=Russian ;; sr) language=Serbian ;; sk) language=Slovak ;;
            sl) language=Slovenian ;; es) language=Spanish ;; sv) language=Swedish ;;
            th) language=Thai ;; tr) language=Turkish ;; uk) language=Ukrainian ;;
            *) language=English ;;
          esac

          # JSON is YAML, which is what WinBoat reads the file as. The
          # person's home is shared with Windows, as the wizard offers.
          jq \
            --arg user "$user" --arg password "$password" \
            --arg memory "''${memory}G" --arg cores "$cores" --arg language "$language" \
            --arg storage "$storage:/storage" --arg home "$HOME:/shared" \
            '.services.windows.environment += {
               USERNAME: $user, PASSWORD: $password,
               RAM_SIZE: $memory, CPU_CORES: $cores, LANGUAGE: $language
             }
             | .services.windows.volumes += [$storage, $home]' \
            ${compose} > "$compose.new"
          chmod 600 "$compose.new"
          mv "$compose.new" "$compose"

          # WinBoat's settings with Podman as its container runtime, which
          # is the one thing the wizard asks that it does not default to.
          # WinBoat fills in every setting missing here from its defaults.
          jq -n --arg version ${lib.escapeShellArg winboat.version} \
            '{ containerRuntime: "Podman", versionData: { previous: $version, current: $version } }' \
            > "$dir/winboat.config.json"
        fi

        notify "Setting up Windows" "Windows is downloading and installing itself. This takes a while; you will be told when it is ready."
        # Podman pulls dockur's image the first time.
        pm container up "$compose"
        fresh=1
      fi

      # Windows is ready when WinBoat's guest server answers, which it
      # does once Windows has installed itself and run the setup above.
      until address=$(pm container ports WinBoat 2> /dev/null | awk '$1 == "7148/tcp" { print $3 }') &&
        [ -n "$address" ] && curl -fs -m 5 "http://$address/health" > /dev/null; do
        if [ "$(pm container status WinBoat 2> /dev/null)" != running ]; then
          notify "Windows stopped while setting up" "Open Windows app support to start it again."
          exit 1
        fi
        sleep 10
      done
      if [ -n "''${fresh:-}" ]; then
        notify "Windows is ready" "Its apps are in Windows app support."
      fi
    '';
  };

  launcher = writeShellApplication {
    name = "windows-app-support";
    runtimeInputs = [
      systemd
      containerPmPath
    ];
    text = ''
      # A person without Windows yet: the setup runs as a unit of their
      # own, so it carries on while WinBoat is closed, and WinBoat opens
      # once Windows' container exists, showing it install. WinBoat's own
      # wizard never shows, since it only does for a person with no
      # container.
      exists() {
        pm container list 2> /dev/null | grep -x WinBoat > /dev/null
      }
      if ! exists; then
        systemctl --user reset-failed windows-app-support-setup.service 2> /dev/null || true
        systemctl --user start windows-app-support-setup.service
        until exists; do
          systemctl --user is-active --quiet windows-app-support-setup.service || exit 1
          sleep 2
        done
      fi
      exec ${app}/bin/winboat "$@"
    '';
  };

  desktopItem = makeDesktopItem {
    name = "winboat";
    desktopName = "Windows app support";
    comment = "Windows apps as windows of their own";
    exec = "windows-app-support %U";
    icon = "winboat";
    startupWMClass = "winboat";
    categories = [ "Utility" ];
  };

in
symlinkJoin {
  pname = "windows-app-support";
  inherit (winboat) version;
  paths = [
    launcher
    setup
    desktopItem
    app
  ];
  passthru = {
    inherit app setup;
    pm = containerPm;
  };
  meta = winboat.meta // {
    description = "Windows apps as windows of their own, through WinBoat";
    mainProgram = "windows-app-support";
  };
}
