# Dev shell for losos-desktop. Standalone devenv, not flake.devShells: same
# reason as dasmatus/losos — devenv.lib.mkShell needs an impure project root.
# `direnv allow` runs `use devenv` (.envrc). enterShell starts the local GHCR
# Nix cache (docs/binary-cache.md).
{ pkgs, ... }:

{
  name = "losos-desktop";

  packages = with pkgs; [
    nixfmt
    qemu
    jq
    xz
    systemd
    systemdUkify
    curl
  ];

  languages.javascript = {
    enable = true;
    package = pkgs.nodejs;
  };

  enterShell = ''
    "$DEVENV_ROOT/tools/enter-local-cache.sh"
  '';
}
