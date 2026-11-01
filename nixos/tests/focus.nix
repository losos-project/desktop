# Focus mode (focus.nix, docs/focus-mode.md), the way a game turns it on:
# `gamemoderun` in a person's session asks GameMode, whose start script is
# `losos-focus start`. Checked in the kernel's own cgroup files, not in what
# systemd reports: the system's and the person's background slices are
# frozen while the game runs, the timers and the service focus stops are
# stopped, and all of it carries on when the game exits or the person logs
# out. polkit lets the person on the seat start their own focus unit and
# nobody else's.
{ lib, ... }:

{
  name = "losos-desktop-focus";

  # losos-focus from the OS's overlay: the derivation the image builds.
  node.pkgsReadOnly = false;

  nodes.machine =
    { pkgs, ... }:
    let
      # Writes the time five times a second, so a frozen one is plain.
      ticker = file: {
        script = "while true; do date +%s%N > ${file}; sleep 0.2; done";
      };
    in
    {
      imports = [ ../modules/focus.nix ];
      nixpkgs.overlays = [ (import ../pkgs) ];

      users.users.alice = {
        isNormalUser = true;
        uid = 1000;
      };
      users.users.bob = {
        isNormalUser = true;
        uid = 1001;
      };
      # An active session on the seat, as signing in at the desktop gives.
      services.getty.autologinUser = "alice";

      systemd.services.ticker = ticker "/run/ticker" // {
        wantedBy = [ "multi-user.target" ];
        serviceConfig.Slice = "losos-background.slice";
      };
      # The timer of some background work, as the updates' is.
      systemd.timers.late = {
        wantedBy = [ "timers.target" ];
        timerConfig.OnUnitActiveSec = "1d";
      };
      systemd.services.late = {
        serviceConfig = {
          Type = "oneshot";
          Slice = "losos-background.slice";
          ExecStart = "${pkgs.coreutils}/bin/true";
        };
      };
      # Ollama's part: stopped in focus, started again after.
      systemd.services.hog = {
        wantedBy = [ "multi-user.target" ];
        serviceConfig.ExecStart = "${pkgs.coreutils}/bin/sleep infinity";
      };
      systemd.services."losos-focus@".serviceConfig.ExecStartPre =
        lib.mkForce "${lib.getExe pkgs.losos-focus} system pause hog.service late.timer";
      systemd.user.services.ticker = ticker "$XDG_RUNTIME_DIR/ticker" // {
        wantedBy = [ "default.target" ];
        serviceConfig.Slice = "background.slice";
      };
    };

  testScript = ''
    import time

    # A dash in a slice's name nests it: losos-background.slice is in
    # losos.slice.
    system = "/sys/fs/cgroup/losos.slice/losos-background.slice/cgroup.events"
    person = "/sys/fs/cgroup/user.slice/user-1000.slice/user@1000.service/background.slice/cgroup.events"

    def frozen(events):
        return machine.succeed(f"grep '^frozen ' {events}").split()[1] == "1"

    def user_active(unit):
        return machine.execute(
            f"su alice -c 'XDG_RUNTIME_DIR=/run/user/1000 systemctl --user is-active {unit}'"
        )[0] == 0

    def moving(file):
        before = machine.succeed(f"cat {file}")
        time.sleep(1.5)
        return machine.succeed(f"cat {file}") != before

    machine.wait_for_unit("multi-user.target")
    machine.wait_for_unit("ticker.service")
    machine.wait_for_unit("hog.service")
    machine.wait_for_unit("late.timer")
    machine.wait_until_succeeds("pgrep -u alice -t tty1 bash")
    machine.wait_for_unit("ticker.service", "alice")
    shell = machine.succeed("pgrep -u alice -t tty1 bash").split()[0]

    with subtest("polkit lets the person on the seat start their own focus and nothing else"):
        def may(unit, verb="start", pid=shell, action="org.freedesktop.systemd1.manage-units"):
            return machine.execute(
                f"pkcheck --action-id {action} --process {pid} --detail unit {unit} --detail verb {verb}"
            )[0] == 0
        assert may("losos-focus@alice.service")
        assert may("losos-focus@alice.service", "stop")
        assert not may("losos-focus@alice.service", "restart")
        assert not may("losos-focus@bob.service")
        assert not may("hog.service", "stop")
        assert may("-", "-", action="com.feralinteractive.GameMode.governor-helper")
        assert not may("-", "-", action="com.feralinteractive.GameMode.gpu-helper")
        # bob has no session on the seat.
        machine.fail("su bob -c 'systemctl --no-ask-password start losos-focus@bob.service'")

    with subtest("a game turns focus on"):
        assert moving("/run/ticker") and moving("/run/user/1000/ticker")
        assert user_active("systemd-tmpfiles-clean.timer")
        # What the person sees, for the pull request's pictures, without
        # kernel messages landing in the middle of them.
        machine.succeed("dmesg -n 1")
        machine.send_chars("clear; losos-focus status | tee /tmp/off\n")
        machine.wait_until_succeeds("test -s /tmp/off")
        time.sleep(1)
        machine.screenshot("focus-off")
        machine.send_chars("gamemoderun sleep 600 &\n")
        machine.wait_until_succeeds("systemctl is-active losos-focus@alice.service")
        machine.wait_until_succeeds(f"grep -qx 'frozen 1' {system}")
        machine.wait_until_succeeds(f"grep -qx 'frozen 1' {person}")
        machine.send_chars("clear; losos-focus status | tee /tmp/on\n")
        machine.wait_until_succeeds("test -s /tmp/on")
        time.sleep(1)
        machine.screenshot("focus-on")
        assert not moving("/run/ticker") and not moving("/run/user/1000/ticker")
        machine.fail("systemctl is-active hog.service")
        machine.fail("systemctl is-active late.timer")
        assert not user_active("systemd-tmpfiles-clean.timer")

    with subtest("the game exiting turns it off"):
        machine.send_chars("kill %1\n")
        # GameMode's reaper notices the exit within five seconds.
        machine.wait_until_fails("systemctl is-active losos-focus@alice.service", timeout=60)
        machine.wait_until_succeeds(f"grep -qx 'frozen 0' {system}")
        machine.wait_until_succeeds(f"grep -qx 'frozen 0' {person}")
        machine.wait_for_unit("hog.service")
        machine.wait_for_unit("late.timer")
        assert user_active("systemd-tmpfiles-clean.timer")
        assert moving("/run/ticker") and moving("/run/user/1000/ticker")

    with subtest("logging out turns it off too"):
        machine.send_chars("gamemoderun sleep 600 &\n")
        machine.wait_until_succeeds("systemctl is-active losos-focus@alice.service")
        machine.wait_until_succeeds(f"grep -qx 'frozen 1' {system}")
        machine.succeed("systemctl stop getty@tty1.service")
        machine.succeed("loginctl terminate-user alice")
        machine.wait_until_fails("systemctl is-active losos-focus@alice.service", timeout=60)
        machine.wait_until_succeeds(f"grep -qx 'frozen 0' {system}")
        machine.wait_for_unit("hog.service")
        assert moving("/run/ticker")
  '';
}
