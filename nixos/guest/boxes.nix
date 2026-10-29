# The LosOS boxes machine (docs/losos-boxes.md): the one machine that talks
# to the LosOS boxes, the project's home servers, so that the host and the
# other machines do not. It finds the boxes, offers each one on the host's
# desktop as an app of this machine, and runs `losos`, the boxes' terminal
# client, with its keys and tokens in this machine's Secret Service. Admin
# pages open in Danube here too.
#
# modules/losos-boxes.nix adds it to losos.machines.guests as losos-boxes,
# so machines.nix ships it as share/losos/machines/losos-boxes.qcow2, and pm
# records that name as the machine's template, which is what the host's
# relay there lets through to its resolved.
{ config, pkgs, ... }:

let
  # Where losos-boxes reaches the host's resolved, below.
  hostResolve = "/run/losos-boxes/resolve";

  # common.nix's one account, whose keyring holds what `losos` signed in to.
  user = "user";

  # What the host's command palette runs here through the guest agent to
  # find text in the boxes' files and Git (docs/losos-boxes.md, "In the
  # command palette"). The agent runs it as root with an empty environment,
  # and `losos` keeps its tokens in the user's keyring, reached over the
  # user's bus: so it runs `losos-boxes search` as the user, on that bus,
  # with the user's PATH, where `losos` and its secret-tool are.
  boxesSearch = pkgs.writeShellApplication {
    name = "losos-boxes-search";
    runtimeInputs = [
      pkgs.coreutils
      pkgs.util-linux
    ];
    text = ''
      uid=$(id -u ${user})
      exec runuser -u ${user} -- env \
        HOME=/home/${user} \
        XDG_RUNTIME_DIR="/run/user/$uid" \
        DBUS_SESSION_BUS_ADDRESS="unix:path=/run/user/$uid/bus" \
        PATH="/etc/profiles/per-user/${user}/bin:/run/current-system/sw/bin" \
        losos-boxes search "$@"
    '';
  };
in
{
  imports = [ ./common.nix ];

  environment.systemPackages = [
    pkgs.losos
    # `losos-boxes list` prints what the machine sees, and the entries run
    # `losos-boxes terminal`.
    pkgs.losos-boxes
    boxesSearch
    # `losos ci setup` puts a CI key and the box's address into a GitHub
    # repository's Actions secrets with gh, so a workflow can reach the box;
    # it signs in with `gh auth login` here, where the keys are.
    pkgs.gh
  ];

  # The boxes are found by mDNS, which this machine cannot do itself: it
  # does none (common.nix), and libvirt's NAT would keep its multicast off
  # the host's LAN anyway. So it browses through the host's resolved,
  # which does mDNS on every network the host is on: this socket, which
  # any account here may use, is carried to the host's relay on vsock port
  # 7351, and the relay passes it to resolved's Varlink socket there.
  systemd.sockets.losos-host-resolve = {
    description = "The host's resolved, for finding LosOS boxes";
    wantedBy = [ "sockets.target" ];
    socketConfig = {
      ListenStream = hostResolve;
      SocketMode = "0666";
    };
  };
  systemd.services.losos-host-resolve = {
    description = "Carry lookups to the host's resolved over vsock";
    serviceConfig = {
      ExecStart = "${config.systemd.package}/lib/systemd/systemd-socket-proxyd vsock:2:7351";
      DynamicUser = true;
    };
  };

  # The other machines reach a box through this one (docs/losos-boxes.md,
  # "From the other machines"): the host's DNS for machines answers a box's
  # name with this machine's address, and `losos-boxes route` passes each
  # connection on ports 80 and 443 to the box its Host header or TLS server
  # name asks for, as it is, TLS and all, so a box's certificate is what the
  # other machine sees and pins. It tells the host's relay which names are
  # boxes, and refuses connections for any other name.
  systemd.services.losos-boxes-route = {
    description = "Pass the other machines' connections to LosOS boxes on";
    wantedBy = [ "multi-user.target" ];
    requires = [ "losos-host-resolve.socket" ];
    after = [ "losos-host-resolve.socket" ];
    environment.LOSOS_BOXES_RESOLVE = hostResolve;
    serviceConfig = {
      ExecStart = "${pkgs.losos-boxes}/bin/losos-boxes route";
      DynamicUser = true;
      AmbientCapabilities = [ "CAP_NET_BIND_SERVICE" ];
      CapabilityBoundingSet = [ "CAP_NET_BIND_SERVICE" ];
      Restart = "always";
      RestartSec = "5s";
    };
  };
  networking.firewall.allowedTCPPorts = [
    80
    443
  ];

  # One entry per box in the user's applications, which losos-guest-run
  # lists for the host's desktop: opening one opens the box in `losos` in
  # derisk-terminal, as this machine is where boxes are managed, and its
  # action opens the admin pages. The user's manager runs from boot, so the
  # entries are there whenever the machine is.
  systemd.user.services.losos-boxes = {
    description = "Offer the LosOS boxes on the host's networks as applications";
    wantedBy = [ "default.target" ];
    environment.LOSOS_BOXES_RESOLVE = hostResolve;
    serviceConfig = {
      ExecStart = "${pkgs.losos-boxes}/bin/losos-boxes watch --terminal";
      ExecStopPost = "${pkgs.losos-boxes}/bin/losos-boxes clear";
      # The host's resolved restarting, or the relay, ends a browse; the
      # next run starts over.
      Restart = "always";
      RestartSec = "10s";
    };
  };

  # losos keeps every box's key and token in the Secret Service through
  # secret-tool, never in a file.
  services.gnome.gnome-keyring.enable = true;

  # Nobody signs in here, so no sign-in password unlocks the keyring: the
  # host keeps one for this machine instead (`losos-boxes keyring-password`,
  # which asks the host's relay on vsock port 7353), and this starts the
  # keyring with it at every boot, making the login keyring with it the
  # first time. --replace takes over from a keyring D-Bus started first.
  # A keyring made before this, with a password its owner chose, does not
  # open with the host's and keeps asking for the owner's in a window of
  # this machine; so does every keyring if the host cannot be asked, as
  # D-Bus then starts one locked.
  systemd.user.services.losos-boxes-keyring = {
    description = "Unlock the keyring with the password the host keeps for this machine";
    wantedBy = [ "default.target" ];
    before = [ "losos-boxes.service" ];
    script = ''
      password=$(${pkgs.losos-boxes}/bin/losos-boxes keyring-password)
      # Every byte read is the password, so no newline goes with it.
      exec /run/wrappers/bin/gnome-keyring-daemon --replace --foreground \
        --components=secrets --unlock < <(printf '%s' "$password")
    '';
    serviceConfig = {
      # Until the host's relay answers, which it does from its boot.
      Restart = "on-failure";
      RestartSec = "5s";
    };
  };
}
