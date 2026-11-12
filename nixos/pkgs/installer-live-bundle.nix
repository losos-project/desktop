# What the installer ISO livepatches from: the installer's Nix store closure,
# plus a manifest that names derisk and the backend script the session runs.
# CI pushes it to GHCR as installer-live:<channel>-<version>-<arch> and moves
# installer-live:<channel>-<arch> on publish (docs/installer.md).
{
  lib,
  pkgs,
  version,
  channel,
  installerToplevel,
  derisk,
  backend,
}:

let
  arch = pkgs.stdenv.hostPlatform.parsed.cpu.name;
in
pkgs.runCommand "installer-live_${version}_${arch}"
  {
    nativeBuildInputs = [
      pkgs.nix
      pkgs.zstd
      pkgs.jq
    ];
  }
  ''
    mkdir -p $out
    jq -n \
      --arg version ${lib.escapeShellArg version} \
      --arg channel ${lib.escapeShellArg channel} \
      --arg arch ${arch} \
      --arg toplevel ${installerToplevel} \
      --arg derisk ${derisk}/bin/derisk \
      --arg backend ${backend} \
      '{version, channel, arch, toplevel, derisk, backend}' > $out/manifest.json
    nix-store --export $(nix-store -qR ${installerToplevel}) \
      | ${lib.getExe pkgs.zstd} -T0 -19 -c > $out/export.nar.zst
  ''
