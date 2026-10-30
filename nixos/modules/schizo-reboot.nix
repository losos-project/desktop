# Schizo mode reboots the computer every four hours, to wipe its RAM.
#
# Schizo mode (schizo.nix) is for a threat where the machine itself may be
# seized. Keys, decrypted data and onion-service material sit in RAM for as
# long as the machine is up, and DRAM holds its contents for a while after
# power is cut (a cold-boot attack). So while schizo mode is on, a timer
# reboots the machine every four hours, and the kernel zeroes every page as
# it is freed (`init_on_free=1`), so a reboot leaves little behind and an
# idle machine is never more than four hours from a clean one.
#
# The reboot rides the same `losos-schizo.target` and flag as the rest of
# schizo mode (schizo.nix): it runs only while schizo mode is on, and stops
# the moment it is turned off. `init_on_free` is a boot parameter, so it is
# always set; it costs a little memory bandwidth and nothing else, and is
# the kind of zeroing a locked-down machine wants in any case.
{
  lib,
  pkgs,
  ...
}:

let
  # schizo.nix's own flag and target. Kept in step with it by hand; both
  # are stable parts of that module's contract.
  flag = "/var/lib/losos/schizo";
  # How often to reboot while schizo mode is on.
  every = "4h";
in
{
  config = {
    # Zero freed pages, so a reboot (or a crash) leaves no stale secrets in
    # memory for a cold-boot attack to read. Always on: it is cheap and
    # wanted regardless, and a kernel parameter cannot follow a runtime flag.
    boot.kernelParams = [ "init_on_free=1" ];

    # The reboot itself: a plain `systemctl reboot`. DefaultDependencies stay
    # on, so it queues behind a clean shutdown rather than cutting units off.
    systemd.services.losos-schizo-reboot = {
      description = "Schizo mode: reboot to wipe RAM";
      unitConfig.ConditionPathExists = flag;
      serviceConfig = {
        Type = "oneshot";
        ExecStart = "${pkgs.systemd}/bin/systemctl reboot";
      };
    };

    # Every four hours while schizo mode is on. OnActiveSec gives the first
    # window after the timer starts (boot, or turning schizo mode on), and
    # OnUnitActiveSec the ones after. It is part of the target, so turning
    # schizo mode off stops it and the clock starts over next time.
    systemd.timers.losos-schizo-reboot = {
      description = "Schizo mode: reboot every ${every} to wipe RAM";
      partOf = [ "losos-schizo.target" ];
      wantedBy = [ "losos-schizo.target" ];
      unitConfig.ConditionPathExists = flag;
      timerConfig = {
        OnActiveSec = every;
        OnUnitActiveSec = every;
      };
    };
  };
}
