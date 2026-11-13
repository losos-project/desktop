# Installs derisk from a GitHub release bundle instead of compiling the
# submodule. The tarball matches derisk's `desktop-bundle` flake output.
{
  lib,
  stdenv,
  fetchurl,
  runCommand,
  xz,
  bundlePin,
  deriskReleaseTag ? null,
}:

let
  repository = bundlePin.repository;
  tag =
    if deriskReleaseTag != null && deriskReleaseTag != "" then
      deriskReleaseTag
    else
      bundlePin.tag;
  arch = stdenv.hostPlatform.config;
  version = bundlePin.version;
  fileName = "derisk-desktop-${arch}-${version}.tar.xz";
  bundleHash = bundlePin.hashes.${arch} or (throw "derisk-bundle.pin.nix has no hash for ${arch}");

  tarball = fetchurl {
    url = "https://github.com/${repository}/releases/download/${tag}/${fileName}";
    hash = bundleHash;
  };

  pkg = runCommand "derisk-${version}" {
    nativeBuildInputs = [ xz ];
    passthru = {
      inherit version;
    };
  } ''
    mkdir -p "$out"
    tar -xJf ${tarball} -C "$out"
  '';
in
pkg.overrideAttrs (_: {
  passthru = {
    inherit version;
    terminal = pkg;
    index = pkg;
    agent = pkg;
    gpui = pkg;
  };
  meta = {
    description = "derisk desktop bundle from ${repository} release ${tag}";
    homepage = "https://github.com/${repository}";
    license = lib.licenses.gpl3Only;
    platforms = lib.platforms.linux;
    mainProgram = "derisk";
  };
})
