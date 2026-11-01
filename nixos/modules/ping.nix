# The active-user count, and the choice screens it turns on.
#
# Once a day, and a few minutes into each login, every signed-in user's
# systemd user manager sends the proxy's /ping two lines: an id and the
# architecture. The id is SHA-256 of a random secret that never leaves the
# user's home and the current month, so the proxy can count a user once per
# month and cannot follow anyone from one month to the next. No account, no
# hardware id, no version, nothing about the machine but its architecture.
# The proxy (proxy/src/lib.rs) adds the id to the month's HyperLogLog, and
# to the EU's when the edge network places the request in the EU, and
# answers with a policy: whether the browser and search engine choice
# screens are on. Those are the EU's Digital Markets Act rules, so they
# come on for users in the EEA once the EU's monthly count passes the DMA's
# 45 million, or its daily pings pass a threshold set on the proxy. The
# proxy decides; the OS never learns the numbers.
#
# The policy lands where derisk reads it, $XDG_STATE_HOME/derisk/policy.conf
# (derisk_settings::choice). derisk shows the screens at the next login and
# under Settings, Default apps; until the policy says so they stay hidden.
{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.losos.ping;
  arch = pkgs.stdenv.hostPlatform.parsed.cpu.name;

  losos-ping = pkgs.writeShellApplication {
    name = "losos-ping";
    runtimeInputs = with pkgs; [
      coreutils
      curl
      gnugrep
      jq
    ];
    text = ''
      config="''${XDG_CONFIG_HOME:-$HOME/.config}"
      state="''${XDG_STATE_HOME:-$HOME/.local/state}"

      # The Privacy page's switch. Off sends nothing and leaves the last
      # policy where it is, so a screen already shown does not vanish.
      if grep -Eqs '^[[:space:]]*privacy\.usage_ping[[:space:]]*=[[:space:]]*false[[:space:]]*$' \
          "$config/derisk/settings.conf"; then
        exit 0
      fi

      mkdir -p "$state/losos" "$state/derisk"
      secret="$state/losos/ping-secret"
      if [ ! -s "$secret" ]; then
        (umask 077 && head -c 32 /dev/urandom | od -An -vtx1 | tr -d ' \n' >"$secret.new")
        mv "$secret.new" "$secret"
      fi

      # UTC, as the proxy files it: a ping in the first hours of a month
      # by local time then lands in the month the proxy is counting.
      month=$(date -u +%Y-%m)
      id=$(printf '%s %s' "$(cat "$secret")" "$month" | sha256sum | cut -c1-64)

      answer=$(curl --fail --silent --show-error --max-time 30 \
        --retry 3 --retry-connrefused --retry-delay 20 \
        --header 'content-type: text/plain' \
        --data-binary "$(printf 'id=%s\narch=%s\n' "$id" ${arch})" \
        ${lib.escapeShellArg cfg.url})

      # Only the two flags derisk reads, as true or false, whatever else the
      # proxy may add later.
      jq -r '.choice_screens as $c
        | "choice_screens.browser = \($c.browser == true)",
          "choice_screens.search = \($c.search == true)",
          "region = \(.region // "other" | ascii_downcase | gsub("[^a-z]"; ""))"' \
        <<<"$answer" >"$state/derisk/policy.conf.new"
      mv "$state/derisk/policy.conf.new" "$state/derisk/policy.conf"
    '';
  };
in
{
  config = lib.mkIf (cfg.url != null) {
    systemd.user.services.losos-ping = {
      description = "Count this user as active and fetch the choice screen policy";
      # The login screen's own user, and any other system account that
      # gets a user manager, is not a person.
      unitConfig.ConditionUser = "!@system";
      serviceConfig = {
        Type = "oneshot";
        ExecStart = lib.getExe losos-ping;
        # Waits while focus mode is on (focus.nix).
        Slice = "background.slice";
      };
    };

    systemd.user.timers.losos-ping = {
      description = "Count this user as active once a day";
      wantedBy = [ "timers.target" ];
      timerConfig = {
        # A few minutes into the session, when the network is likely up,
        # and a day after each run. A failed run is retried at the next.
        OnStartupSec = "5min";
        OnUnitActiveSec = "1d";
        RandomizedDelaySec = "10min";
      };
    };
  };
}
