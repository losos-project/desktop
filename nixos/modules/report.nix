# Reporting a problem on the desktop as a GitHub issue (docs/reporting.md).
#
# derisk offers "Report the problem" when a window stops answering, a service
# fails or an action in an app never finishes, and runs losos-report, which
# writes the report, shows it through derisk, signs in with GitHub's device
# flow and files it. It reads where to from /etc/losos/report.json, written
# here only when the image was built with a GitHub App to sign in with
# (losos.report.clientId, from the LOSOS_REPORT_CLIENT_ID repository
# variable); without it losos-report says so and derisk offers no report.
{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.losos.report;
in
{
  environment.systemPackages = [ pkgs.losos-report ];

  environment.etc."losos/report.json" = lib.mkIf (cfg.clientId != null && cfg.repository != null) {
    text = builtins.toJSON {
      repository = cfg.repository;
      client_id = cfg.clientId;
    };
  };
}
