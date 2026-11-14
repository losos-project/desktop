# Refresh the installer's own derisk and losos-installer from the channel's
# newest GHCR bundle while the live medium is running, Subiquity-style
# (docs/installer.md).
{
  lib,
  pkgs,
  writeShellApplication,
  configJson,
}:

writeShellApplication {
  name = "losos-installer-livepatch";
  runtimeInputs = [
    pkgs.oras
    pkgs.nix
    pkgs.zstd
    pkgs.jq
    pkgs.coreutils
  ];
  text = ''
    set -euo pipefail

    offer=/run/losos-installer/livepatch-offer.json
    session=/run/losos-installer/session.env
    work=/run/losos-installer/livepatch-work
    config=${configJson}

    if grep -qw 'losos.installer.livepatch=0' /proc/cmdline 2>/dev/null; then
      exit 0
    fi

    read_config() {
      channel=$(jq -er .channel "$config")
      arch=$(jq -er .arch "$config")
      repository=$(jq -er .repository "$config")
      version=$(jq -er .version "$config")
    }

    newer() {
      remote=$1
      local=$2
      [ "$(printf '%s\n' "$local" "$remote" | sort -V | tail -1)" = "$remote" ] \
        && [ "$remote" != "$local" ]
    }

    remote_version() {
      ref="$repository:$channel-$arch"
      oras manifest fetch "$ref" 2>/dev/null \
        | jq -er '.annotations["org.opencontainers.image.version"] // empty'
    }

    write_offer() {
      remote=$1
      mkdir -p /run/losos-installer
      jq -n \
        --arg current "$version" \
        --arg new "$remote" \
        --arg channel "$channel" \
        '{current: $current, version: $new, channel: $channel}' > "$offer"
    }

    check() {
      read_config
      rm -f "$offer"
      if remote=$(remote_version); then
        if newer "$remote" "$version"; then
          write_offer "$remote"
        fi
      fi
    }

    apply() {
      read_config
      ref="$repository:$channel-$arch"
      mkdir -p "$work"
      rm -rf "${"work:?"}"/*
      # CI for losos-project/desktop may be failing, so the bundle is not
      # published. Fall back to building the installer-live bundle locally
      # from the flake when nix and a checkout are available, and otherwise
      # keep the installer's own bundled closure (build it itself).
      if ! oras pull "$ref" -o "$work" 2>/dev/null; then
        if command -v nix >/dev/null 2>&1 && [ -f flake.nix ]; then
          echo "No published bundle; building installer-live-bundle locally."
          nix build .#installer-live-bundle --out-link "$work/bundle" || {
            rm -f "$offer"
            echo "Build failed; keeping the bundled installer at $version."
            exit 0
          }
          work="$work/bundle"
        else
          rm -f "$offer"
          echo "No published bundle and no local build; keeping the bundled installer at $version."
          exit 0
        fi
      fi
      remote=$(jq -er .version "$work/manifest.json")
      if ! newer "$remote" "$version"; then
        rm -f "$offer"
        echo "The installer is already at $version."
        exit 0
      fi
      echo "Updating the installer from $version to $remote"
      ${lib.getExe pkgs.zstd} -d -c "$work/export.nar.zst" | ${pkgs.nix}/bin/nix-store --import /nix/store
      jq -r '"DERISK=" + .derisk, "BACKEND=" + .backend' "$work/manifest.json" > "$session"
      rm -f "$offer"
      rm -rf "$work"
      echo "The installer now runs $remote."
    }

    case ''${1-} in
      check) check ;;
      apply) apply ;;
      *)
        echo "usage: losos-installer-livepatch check|apply" >&2
        exit 2
        ;;
    esac
  '';
}
