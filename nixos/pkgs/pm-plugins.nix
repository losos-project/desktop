# pm's plugins as WebAssembly components, and what `just plugins` builds: this
# repository's three and the five from pm's own tree that this OS needs
# (plugins/README.md, "Building", says which and why).
#
# Installed under share/pm/plugins and deliberately nowhere pm loads from. pm
# reads only <config>/pm/plugins and runs a plugin only when it is signed by a
# key the user trusts, so shipping them in the image offers them without
# trusting them on anyone's behalf: a user who wants them copies them into
# their own config and signs them there (docs/pm.md, "pm's plugins"), by hand
# or through the Home Manager module (nixos/home/pm.nix).
{
  lib,
  stdenv,
  rustPlatform,
  rustc,
  cargo,
  lld,
  pm,
}:

let
  # This repository's plugin workspace. Its lock pins crates.io only, so
  # importCargoLock needs no hash.
  ours = lib.fileset.toSource {
    root = ../../plugins;
    fileset = lib.fileset.unions [
      ../../plugins/Cargo.toml
      ../../plugins/Cargo.lock
      ../../plugins/wit
      ../../plugins/losos-image
      ../../plugins/losos-nix
      ../../plugins/losos-systemd
    ];
  };
  oursDeps = rustPlatform.importCargoLock { lockFile = ../../plugins/Cargo.lock; };

  # pm's plugin workspace, from the same commit as pm itself, whose build
  # vendors its crates already (pm.nix).
  pmDeps = pm.pluginsDeps;

  local = [
    "losos-image"
    "losos-nix"
    "losos-systemd"
  ];
  fromPm = [
    "libvirt"
    # Each person's containers, rootless, for Windows app support
    # (windows-app-support.nix).
    "podman"
    "sysext"
    "sysupdate"
    "systemd"
  ];
  packages = crates: lib.concatMapStringsSep " " (c: "-p ${c}") crates;
in
stdenv.mkDerivation {
  pname = "pm-plugins";
  inherit (pm) version;

  srcs = [
    ours
    pm.src
  ];
  sourceRoot = ".";
  unpackPhase = ''
    runHook preUnpack
    cp -r ${ours} ours
    cp -r ${pm.src} pm
    chmod -R u+w ours pm
    runHook postUnpack
  '';

  # nixpkgs' rustc links wasm32 with `lld` from PATH rather than a bundled
  # rust-lld.
  nativeBuildInputs = [
    rustc
    cargo
    lld
  ];

  # Two workspaces, each pointed at its own vendored crates; no network.
  configurePhase = ''
    runHook preConfigure
    export CARGO_HOME=$PWD/cargo-home
    vendor() {
      mkdir -p "$1/.cargo"
      cat > "$1/.cargo/config.toml" <<EOF
    [source.crates-io]
    replace-with = "vendored"
    [source.vendored]
    directory = "$2"
    EOF
    }
    vendor ours ${oursDeps}
    # fetchCargoVendor keeps crates.io crates one level down.
    vendor pm/plugins ${pmDeps}/source-registry-0
    runHook postConfigure
  '';

  buildPhase = ''
    runHook preBuild
    export CARGO_BUILD_JOBS=1
    (cd ours && cargo build --offline --release --target wasm32-unknown-unknown ${packages local})
    (cd pm/plugins && cargo build --offline --release --target wasm32-unknown-unknown ${packages fromPm})
    # The encoder wraps a core module into a component; it runs here, on the
    # build machine, so it is the one native build.
    (cd pm/plugins && cargo build --offline --release -p encoder)
    runHook postBuild
  '';

  installPhase = ''
    runHook preInstall
    mkdir -p $out/share/pm/plugins
    encoder=pm/plugins/target/release/encoder
    for crate in ${lib.concatStringsSep " " local}; do
      $encoder ours/target/wasm32-unknown-unknown/release/''${crate//-/_}.wasm \
        $out/share/pm/plugins/$crate.wasm
    done
    for crate in ${lib.concatStringsSep " " fromPm}; do
      $encoder pm/plugins/target/wasm32-unknown-unknown/release/''${crate//-/_}.wasm \
        $out/share/pm/plugins/$crate.wasm
    done
    runHook postInstall
  '';

  # The names under share/pm/plugins, for the Home Manager module
  # (nixos/home/pm.nix) to check a user's list against without building this.
  passthru.plugins = local ++ fromPm;

  meta = {
    description = "pm's plugin components for LosOS Desktop, unsigned";
    license = [
      lib.licenses.agpl3Plus
      lib.licenses.gpl3Only
    ];
    platforms = lib.platforms.linux;
  };
}
