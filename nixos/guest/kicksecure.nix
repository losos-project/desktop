# Kicksecure for every NixOS service machine (service.nix imports this): its
# hardening (kicksecure-settings.nix) on the image's own NixOS rather than
# Kicksecure's Debian, so a service machine stays in the verified /usr and
# updates with the OS. Whonix's Gateway is Kicksecure with Tor, and so is
# schizo mode's (tor-gateway.nix).
#
# Kicksecure's other half is about people's sessions, which few service
# machines have: no SUID programs but PAM's (its permission-hardener), a locked
# root and a 027 umask. Each is set below as far as a NixOS machine has it.
#
# A sysctl gives way to a machine's own module, which sets it plainly. The
# rest are lists, which merge rather than give way: a machine adds kernel
# parameters after these and takes one away only with mkForce, names a
# disabled kernel module it needs in allowModules, and a SUID program it
# needs in wrappers.
{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.losos.service.kicksecure;
  settings = import ./kicksecure-settings.nix {
    inherit (pkgs.stdenv.hostPlatform) isx86;
    falseProgram = "${pkgs.coreutils}/bin/false";
  };
  disabled = lib.subtractLists cfg.allowModules (
    lib.concatLists (lib.attrValues settings.disabledModules)
  );
in
{
  options.losos.service.kicksecure = {
    enable = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = "Whether this service machine has Kicksecure's hardening.";
    };
    allowModules = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ ];
      example = [ "udf" ];
      description = "Kernel modules Kicksecure disables that this machine needs.";
    };
    wrappers = lib.mkOption {
      type = lib.types.attrsOf lib.types.attrs;
      default = { };
      example = lib.literalExpression ''
        { fusermount3 = { setuid = true; owner = "root"; group = "root"; source = "''${pkgs.fuse3}/bin/fusermount3"; }; }
      '';
      description = ''
        SUID or SGID programs this machine needs besides unix_chkpwd, in
        security.wrappers' form; every other wrapper NixOS would make is
        left out.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    boot.kernelParams = settings.kernelParams;
    # Above NixOS's own defaults (mkDefault), below any machine's setting.
    boot.kernel.sysctl = lib.mapAttrs (_: lib.mkOverride 900) settings.sysctl // {
      # Forced over the "core" NixOS sets when systemd-coredump is off, as
      # it is here, since it would otherwise put its own handler back.
      "kernel.core_pattern" = lib.mkForce settings.sysctl."kernel.core_pattern";
    };
    systemd.coredump.enable = false;
    systemd.settings.Manager.DefaultLimitCORE = "0";

    # `install <module> false`, as security-misc does it, so loading one by
    # name fails rather than silently doing nothing, and the journal says so.
    boot.extraModprobeConfig = lib.concatMapStrings (
      module: "install ${module} ${pkgs.coreutils}/bin/false\n"
    ) disabled;
    boot.blacklistedKernelModules = lib.subtractLists cfg.allowModules settings.blacklistedModules;

    # No SUID or SGID program but unix_chkpwd, as Kicksecure's
    # permission-hardener strips the bit from everything outside its list.
    # unix_chkpwd stays, as on Kicksecure, because PAM's account check
    # needs it to start a lingering user's systemd, which a machine with a
    # person's session (agent.nix) has. Forced, so the wrappers other
    # NixOS modules add are dropped; a machine adds its own in `wrappers`.
    security.wrappers = lib.mkForce (
      {
        unix_chkpwd = {
          setuid = true;
          owner = "root";
          group = "root";
          source = "${config.security.pam.package}/bin/unix_chkpwd";
        };
      }
      // cfg.wrappers
    );
    security.sudo.enable = lib.mkDefault false;
    # Root has no password already: sysusers (service.nix) makes it locked.
    security.loginDefs.settings.UMASK = lib.mkDefault "027";
  };
}
