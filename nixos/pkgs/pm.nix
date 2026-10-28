# pm, the package manager this OS keeps as its system manager: it builds signed
# recipes into .cpkg archives and runs their entrypoints in a sandbox, and
# pmd serves it over D-Bus.
#
# The image takes the OS from nixpkgs and keeps pm as the tool a user of the
# running system builds and runs software with. It is pinned by the
# components/pm submodule and moved by hand, because pm's plugin contract has
# changed under this repository before.
{
  lib,
  rustPlatform,
  pkg-config,
  lld,
  llvmPackages,
}:

let
  # The components/pm submodule, at the commit this tree records for it
  # (flake.nix, `self.submodules`). cleanSource drops the submodule's .git
  # file, so the source hash covers only the tree.
  src = lib.cleanSourceWith {
    name = "pm-source";
    src = lib.cleanSource ../../components/pm;
  };

  # pm's build.rs compiles its bundled plugins from plugins/, a workspace of
  # its own with its own lock, in a nested cargo run; this is that lock's
  # crates, as cargoDeps is pm's. pm-plugins.nix builds from the same
  # workspace and takes them from here.
  pluginsDeps = rustPlatform.fetchCargoVendor {
    name = "pm-plugins";
    inherit src;
    sourceRoot = "${src.name}/plugins";
    hash = "sha256-CuGoJFunrBoZjfy4jEvEmKVBsgE00LyAoWpeMMVW29M=";
  };
in
rustPlatform.buildRustPackage {
  pname = "pm";
  version = "0.1.0-unstable-2026-10-28";

  inherit src;

  cargoHash = "sha256-H+8+J/FxvIE95g0bWBHX1zKlqgUSXESmJ791xqr+atc=";

  # build.rs builds the plugins for wasm32-unknown-unknown. nixpkgs' rustc
  # carries that target's std but links it with `lld` from PATH rather than
  # a bundled rust-lld, and the tree-sitter grammars among them are C, which
  # takes a clang that targets wasm32: the unwrapped one, since the cc
  # wrapper adds flags for the host.
  nativeBuildInputs = [
    pkg-config
    lld
    llvmPackages.clang-unwrapped
    llvmPackages.llvm
  ];
  env = {
    CC_wasm32_unknown_unknown = "clang";
    AR_wasm32_unknown_unknown = "llvm-ar";
  };

  # nixpkgs' rustc has no std for wasm32-wasip2, which build.rs wants for
  # one test fixture: a plugin that asks for WASI and must be refused. pm
  # itself never loads it, so it is left out here, and the one test that
  # reads it is skipped below.
  postPatch = ''
        substituteInPlace build.rs \
          --replace-fail '    compile_wasi_fixture(&plugins, &target_dir);
        copy_if_changed(
            &target_dir.join("wasm32-wasip2/release/wasi.wasm"),
            &test_components.join("wasi.wasm"),
        );
    ' ""
  '';

  # The nested cargo build in plugins/ reads this before the vendored pm
  # crates cargoSetupHook configures, which do not hold the plugins' crates.
  # fetchCargoVendor keeps crates.io crates one level down.
  preBuild = ''
    mkdir -p plugins/.cargo
    cat > plugins/.cargo/config.toml <<EOF
    [source.crates-io]
    replace-with = "pm-plugins-vendored"
    [source.pm-plugins-vendored]
    directory = "${pluginsDeps}/source-registry-0"
    EOF
  '';

  # What `pmd --system` needs installed beside it: the D-Bus policy that lets
  # only the pm user own org.pm1.System, the file that has the bus start it
  # through pm-system.service, and the polkit actions, which name pm as their
  # owner. nixos/modules/pm.nix adds the user, the unit and the rules.
  postInstall = ''
    install -Dm644 -t $out/share/dbus-1/system.d data/dbus-1/system.d/org.pm1.System.conf
    install -Dm644 -t $out/share/dbus-1/system-services \
      data/dbus-1/system-services/org.pm1.System.service
    install -Dm644 -t $out/share/polkit-1/actions data/polkit-1/actions/org.pm1.system.policy
  '';

  passthru = { inherit pluginsDeps; };

  # pm's own suite, less what assumes a conventional Linux host. context,
  # perms, pm_trace, sandbox, steps, symbols and worker run /bin/cat,
  # /bin/true and friends under ptrace or in pm's jail, which mirrors the
  # host's /bin and /usr; the nix build sandbox has neither, so they fail here
  # on the missing file and not on pm. The pm build in ci.yml runs pm on a
  # host that has both. The same
  # assumption holds for pm at run time on this OS, whose /bin and /usr hold
  # almost nothing; docs/nixos.md tracks that.
  cargoTestFlags = [
    "--lib"
    "--bins"
  ]
  ++ map (t: "--test=${t}") [
    "build_file"
    "cli_build"
    "command_output"
    "config_file"
    "download"
    "example_plugins"
    "graph"
    "landlock"
    "metadata"
    "packaging"
    "plugins"
    "progress"
    "wire"
  ];
  # The two tests in command_output that spawn a literal /bin/true, and the
  # one that loads the WASI fixture postPatch leaves out.
  checkFlags = [
    "--skip=a_finished_command_leaves_no_line_behind"
    "--skip=a_sandbox_without_progress_still_runs_commands"
    "--skip=a_plugin_that_wants_more_of_the_host_than_log_does_not_instantiate"
  ];

  meta = {
    description = "Build signed recipes into packages and run them in a Linux sandbox";
    homepage = "https://github.com/dichhead/pm";
    license = lib.licenses.gpl3Only;
    platforms = lib.platforms.linux;
    mainProgram = "pm";
  };
}
