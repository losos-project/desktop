# The bridge between a LosOS box's LosOS Login and systemd-homed: company
# accounts that log in with their LosOS Login password and become homed
# records here (nixos/modules/company.nix, docs/company.md).
#
# Built from src/losos-login-bridge. One crate makes both halves: the
# command (join, leave, the join service) and the PAM module, a cdylib
# installed where PAM stacks name it. Cargo.lock pins every crate with its
# SHA-256, and importCargoLock turns each pin into a fixed-output fetch.
{
  lib,
  rustPlatform,
  pam,
}:

rustPlatform.buildRustPackage {
  pname = "losos-login-bridge";
  version = "0.1.0";

  src = ../../src/losos-login-bridge;

  cargoLock.lockFile = ../../src/losos-login-bridge/Cargo.lock;

  # build.rs generates the PAM module's bindings from Linux-PAM's headers.
  nativeBuildInputs = [ rustPlatform.bindgenHook ];
  buildInputs = [ pam ];

  # The login's judgement with homed and LosOS Login stood in for, and
  # what LosOS Login's and userdb's answers mean; no box or homed needed.
  # nixos/tests/company.nix logs in against a stand-in LosOS Login.
  doCheck = true;

  postInstall = ''
    install -Dm444 org.losos.login-bridge.policy \
      $out/share/polkit-1/actions/org.losos.login-bridge.policy
    mkdir -p $out/lib/security
    mv $out/lib/liblosos_login_bridge.so $out/lib/security/pam_losos_login.so
    # The rlib half is for the command alone.
    rm -f $out/lib/*.rlib
  '';

  meta = {
    description = "Log in with a LosOS box's LosOS Login account, kept as a systemd-homed record";
    license = lib.licenses.agpl3Plus;
    platforms = lib.platforms.linux;
    mainProgram = "losos-login-bridge";
  };
}
