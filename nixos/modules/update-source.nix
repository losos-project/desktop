# The update channel, served on 127.0.0.1 from GitHub Releases.
#
# systemd-sysupdate wants one URL with a SHA256SUMS and whole files under it.
# The proxy (proxy/) was that URL, out of GHCR, and a hosted function is a
# thing whose bill or limits can stop every machine updating. A GitHub release
# holds the same files, but not under those names: each architecture's
# manifest is SHA256SUMS-<arch>, and a file of 2 GiB or more is .part-NN
# assets (docs/releases.md). GitHub Pages, which holds the documentation, can
# only serve files, so it carries an index instead: per channel and
# architecture, which assets make up which file (website/release-index.js).
#
# This socket answers sysupdate for that index. Each connection is one
# instance of a short script: it reads the request line, looks the name up in
# the index and streams the asset URLs one after the other with curl, so a
# split file reaches sysupdate whole and nothing is staged on disk. The index
# and the assets are not trusted: sysupdate checks SHA256SUMS.gpg against the
# image's key and every file against SHA256SUMS, as it would from any source.
# The installer reads the same index, but fetches the files with aria2 over
# many connections into a scratch partition on the target disk first
# (src/losos-installer/src/stage.rs). That is not done here: aria2 writes
# each connection's range wherever it lands, so a file cannot reach
# sysupdate as it arrives, and an installed system's free space is not a
# partition the size of a /usr to spare.
#
# The index is fetched again for every request, and a release published in
# the middle of an update can move it between SHA256SUMS and the files it
# names. Their names carry the version, so the old ones are then simply not
# found, sysupdate gives up on that attempt, and the next timer run starts
# over from the new manifest.
#
# Which channel's index it reads is the machine's to choose, not the image's:
# the image names the cadence it was built for, and Settings, System can
# pick another (nightly, weekly, monthly, yearly), which
# losos-update-channel writes to /var/lib/losos/update-channel as root. The
# script reads that file for each request too, so a new choice holds from
# the next update on. The files of every cadence's release are the same
# kind and signed by the same key, so a machine can move between them; it
# only never moves back in version, since sysupdate installs nothing older
# than what it runs.
{
  config,
  lib,
  pkgs,
  ...
}:

let
  inherit (lib) mkOption types;
  cfg = config.services.losos-update-source;

  # The channel the machine follows: the one losos-update-channel wrote, when
  # it is still one this image knows, and the image's own otherwise. Shared
  # by the server and the command, so they cannot disagree.
  current = ''
    channel=${lib.escapeShellArg cfg.channel}
    channels=${lib.escapeShellArg (toString cfg.channels)}
    if read -r chosen 2>/dev/null <${cfg.stateFile}; then
      case " $channels " in
        *" $chosen "*) channel=$chosen ;;
      esac
    fi
  '';

  # `losos-update-channel` says which channel this machine updates from;
  # `losos-update-channel set <channel>`, as root, changes it. Settings runs
  # the second through losos-update-channel@<channel>.service, so polkit
  # asks an administrator, as it does for any unit started by hand.
  command = pkgs.writeShellApplication {
    name = "losos-update-channel";
    text = ''
      ${current}
      case ''${1-} in
        "")
          echo "$channel"
          ;;
        set)
          new=''${2-}
          case " $channels " in
            *" $new "*) ;;
            *)
              echo "losos-update-channel: '$new' is not a channel; one of: $channels" >&2
              exit 2
              ;;
          esac
          mkdir -p "$(dirname ${cfg.stateFile})"
          # Written beside and renamed, so a reader never sees half a name.
          echo "$new" > ${cfg.stateFile}.new
          mv ${cfg.stateFile}.new ${cfg.stateFile}
          echo "$new"
          ;;
        *)
          echo "usage: losos-update-channel [set ${lib.concatStringsSep "|" cfg.channels}]" >&2
          exit 2
          ;;
      esac
    '';
  };

  serve = pkgs.writeShellApplication {
    name = "losos-update-source";
    runtimeInputs = [ pkgs.curl ];
    text = ''
      answer() {
        printf 'HTTP/1.1 %s\r\nContent-Length: 0\r\nConnection: close\r\n\r\n' "$1"
        exit 0
      }

      IFS=' ' read -r method target _ || exit 0
      # The rest of the header says nothing this needs; it is read so the
      # client is not cut off while still sending it.
      while IFS= read -r line; do
        [ -z "''${line%$'\r'}" ] && break
      done
      case $method in
        GET | HEAD) ;;
        *) answer "405 Method Not Allowed" ;;
      esac
      # A file name as a release asset can have one, and nothing else: no
      # slash, no leading dot, no query.
      name=''${target#/}
      case $name in
        "" | .* | */* | *[!A-Za-z0-9._+-]*) answer "404 Not Found" ;;
      esac

      ${current}
      url=${lib.escapeShellArg cfg.index}
      index=$(curl --silent --show-error --fail --location --proto '=https' \
        --retry 3 --max-time 60 "''${url//\{channel\}/$channel}") ||
        answer "502 Bad Gateway"
      size="" urls=""
      while read -r file length rest; do
        if [ "$file" = "$name" ]; then
          size=$length urls=$rest
          break
        fi
      done <<<"$index"
      [ -n "$urls" ] || answer "404 Not Found"
      case $size in
        "" | *[!0-9]*) answer "502 Bad Gateway" ;;
      esac

      if [ "$method" = HEAD ]; then
        printf 'HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: %s\r\nConnection: close\r\n\r\n' "$size"
        exit 0
      fi

      # Do not promise a complete response until every asset is reachable.
      # Otherwise an unavailable release asset makes curl exit after we've
      # sent 200, leaving sysupdate with a truncated body (reported as a
      # connection reset instead of a useful HTTP error).
      # HEAD follows GitHub's redirect without downloading the potentially
      # multi-GiB asset. If any part is unavailable, answer before streaming.
      for url in $urls; do
        curl --silent --show-error --fail --head --location --proto '=https' \
          --retry 3 --max-time 60 "$url" >/dev/null || answer "502 Bad Gateway"
      done

      printf 'HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: %s\r\nConnection: close\r\n\r\n' "$size"

      # A connection that breaks off is picked up where it stopped, from the
      # github.com URL again, which redirects to a fresh signed address. A
      # /usr of 2 GiB is a long download, and one dropped connection made
      # sysupdate throw it all away and the timer start over hours later.
      # --continue-at fails, rather than send the file again from its start,
      # when the server ignores the range. A part that still cannot be had
      # ends the file there, so it comes up short and sysupdate refuses it.
      # shellcheck disable=SC2086 # the URLs are words of the index line
      for url in $urls; do
        sent=0 tries=0
        until
          rc=0
          curl --silent --show-error --fail --location --proto '=https' \
            --retry 3 --continue-at "$sent" \
            --write-out '%output{/tmp/received}%{size_download}' "$url" || rc=$?
          [ "$rc" = 0 ]
        do
          tries=$((tries + 1))
          # 23: sysupdate hung up, and nothing is reading any more.
          [ "$rc" != 23 ] && [ "$tries" -lt 10 ] || exit 1
          sent=$((sent + $(cat /tmp/received)))
        done
      done
    '';
  };
in
{
  options.services.losos-update-source = {
    index = mkOption {
      type = types.nullOr types.str;
      default = null;
      example = "https://losos-project.github.io/losos-desktop/updates/{channel}/x86_64/index";
      description = ''
        Each channel's index on the documentation site, which names the
        GitHub release assets each file is made of, with `{channel}` where
        the channel's name goes. `null` serves nothing.
      '';
    };

    channel = mkOption {
      type = types.str;
      default = "nightly";
      description = "The channel served until someone picks another.";
    };

    channels = mkOption {
      type = types.listOf (types.strMatching "[a-z][a-z0-9-]*");
      default = [ cfg.channel ];
      description = "The channels `losos-update-channel set` accepts.";
    };

    stateFile = mkOption {
      type = types.path;
      default = "/var/lib/losos/update-channel";
      description = "Where the channel picked on this machine is kept.";
    };

    defaultIndex = mkOption {
      type = types.nullOr types.str;
      readOnly = true;
      default =
        if cfg.index == null then null else lib.replaceStrings [ "{channel}" ] [ cfg.channel ] cfg.index;
      defaultText = lib.literalMD "`index` with `channel` filled in";
      description = ''
        The index of the image's own channel, for the installers, which
        install from it before any machine has chosen one.
      '';
    };

    port = mkOption {
      type = types.port;
      default = 8485;
      description = "The port on 127.0.0.1 sysupdate fetches the channel from.";
    };
  };

  config = lib.mkIf (cfg.index != null) {
    environment.systemPackages = [ command ];

    systemd.services."losos-update-channel@" = {
      description = "Follow the %i update channel";
      serviceConfig = {
        Type = "oneshot";
        ExecStart = "${lib.getExe command} set %i";
      };
    };

    systemd.sockets.losos-update-source = {
      description = "LosOS update channel from GitHub Releases";
      wantedBy = [ "sockets.target" ];
      # Loopback only: it serves public files, but to this machine.
      listenStreams = [ "127.0.0.1:${toString cfg.port}" ];
      socketConfig.Accept = true;
    };

    systemd.services."losos-update-source@" = {
      description = "LosOS update channel request";
      serviceConfig = {
        ExecStart = lib.getExe serve;
        StandardInput = "socket";
        StandardOutput = "socket";
        StandardError = "journal";
        DynamicUser = true;
        ProtectSystem = "strict";
        ProtectHome = true;
        PrivateTmp = true;
        PrivateDevices = true;
        NoNewPrivileges = true;
        RestrictAddressFamilies = [
          "AF_INET"
          "AF_INET6"
          "AF_UNIX"
        ];
        CapabilityBoundingSet = "";
        SystemCallFilter = [ "@system-service" ];
        # A file of several GiB at a slow connection's pace.
        RuntimeMaxSec = "12h";
      };
    };
  };
}
