# Kicksecure for every NixOS service machine (service.nix imports this): its
# hardening (kicksecure-settings.nix) on the image's own NixOS rather than
# Kicksecure's Debian, so a service machine stays in the verified /usr and
# updates with the OS. Whonix's Gateway is Kicksecure with Tor, and so is
# schizo mode's (tor-gateway.nix).
#
# Kicksecure's other half is about people signing in, which nobody does
# here: no SUID programs (its permission-hardener), a locked root and a
# 027 umask. Each is set below as far as a NixOS machine has it.
#
# Every setting gives way to a machine's own module, which sets it plainly;
# one that needs a disabled kernel module back lists it in allowModules.
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
  };

  config = lib.mkIf cfg.enable {
    boot.kernelParams = settings.kernelParams;
    # Above NixOS's own defaults (mkDefault), below any machine's setting.
    boot.kernel.sysctl = lib.mapAttrs (_: lib.mkOverride 900) settings.sysctl // {
      # The store's false, which /bin does not have. Forced over the
      # "core" NixOS sets when systemd-coredump is off, as it is here, since
      # it would otherwise put its own handler back.
      "kernel.core_pattern" = lib.mkForce "|${pkgs.coreutils}/bin/false";
    };
    systemd.coredump.enable = false;
    systemd.settings.Manager.DefaultLimitCORE = "0";

    # `install <module> false`, as security-misc does it, so loading one by
    # name fails rather than silently doing nothing, and the journal says so.
    boot.extraModprobeConfig = lib.concatMapStrings (
      module: "install ${module} ${pkgs.coreutils}/bin/false\n"
    ) disabled;
    boot.blacklistedKernelModules = lib.subtractLists cfg.allowModules settings.blacklistedModules;

    # No SUID or SGID program at all, where Kicksecure's permission-hardener
    # strips the bit from a list of them: nothing here runs as a person who
    # would need one.
    security.enableWrappers = lib.mkDefault false;
    security.sudo.enable = lib.mkDefault false;
    # Root has no password already: sysusers (service.nix) makes it locked.
    security.loginDefs.settings.UMASK = lib.mkDefault "027";
  };
}
