# Routes to a GitHub release bundle or a source build from components/derisk.
args @ {
  lib,
  bundlePin ? import ./derisk-bundle.pin.nix,
  deriskReleaseTag ? (
    let
      fromEnv = builtins.getEnv "LOSOS_DERISK_TAG";
    in
    if fromEnv != "" then fromEnv else null
  ),
  ...
}:

let
  fromSource =
    (builtins.getEnv "LOSOS_DERISK_FROM_SOURCE") == "1" || !(bundlePin.enable or false);
in
if fromSource then
  import ./derisk-from-source.nix (builtins.removeAttrs args [
    "bundlePin"
    "deriskReleaseTag"
  ])
else
  import ./derisk-from-bundle.nix {
    inherit lib bundlePin deriskReleaseTag;
    inherit (args) stdenv fetchurl runCommand xz;
  }
