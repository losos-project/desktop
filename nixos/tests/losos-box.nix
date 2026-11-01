# A real LosOS box beside the desktop (docs/losos-boxes.md, docs/backup.md).
#
# losos-boxes.nix and backup.nix each stand a fake box up: nginx answering
# the routes the desktop reads, Nextcloud and Forgejo where a box's own
# services used to be. This one boots the box's own modules, from the LosOS
# commit the desktop's `losos` client is built from (nixos/pkgs/losos-pin.nix,
# `losos.box`), so a change on either side that breaks the other shows up
# here: the announcement, the setup routes, the terminal envelope, LosOS
# Cloud and LosOS Git, all as the box serves them.
#
# `box` is that box: lososd, LosOS Login, LosOS Cloud and LosOS Git behind
# the front nginx, announcing itself through resolved as
# modules/configuration.nix makes every box do. It is claimed by `matus`, as
# the setup wizard would. `desktop` is a person's session with the backups
# module, losos-boxes and the losos client, a keychain on its session bus
# and a GPG key the box was given at install, as `losos-ctl install
# --gpg-key` gives one. It:
#
#   * finds the box by its `_losos._tcp` announcement, with its release;
#   * signs the losos client in with a token the box made for that key, and
#     reads the box's state through the signed, encrypted envelope;
#   * pins the box's certificate for the backups and signs in to LosOS
#     Cloud with matus's LosOS Login password, the fallback;
#   * signs the backups in through LosOS Login with a device code, which
#     matus approves on LosOS Login's consent page in a browser, and gets
#     a LosOS Cloud app password and a LosOS Git token of the desktop's;
#   * backs a home up to both: files to LosOS Cloud, a repository to LosOS
#     Git.
#
# Not the whole image, only what the desktop side needs, so it boots the
# test driver's kernel and runs without KVM too, slowly.
{ hostPkgs, ... }:

let
  # The box's tree at the pinned commit. Imported, not only built: its
  # modules are this test's box.
  src = (hostPkgs.callPackage ../pkgs/losos.nix { }).box;
  lososPkgs = import "${src}/nix/packages.nix" { pkgs = hostPkgs; };

  password = "Correct-Horse-Battery-9";

  # The key the box is installed with and the desktop signs with: Ed25519
  # to sign, Curve25519 to decrypt, no passphrase, as `losos key import`
  # takes one. Made once at build time; nothing secret leaves the test.
  key = hostPkgs.runCommand "losos-box-test-key" { nativeBuildInputs = [ hostPkgs.gnupg ]; } ''
    export GNUPGHOME=$PWD/gnupg
    mkdir -m 700 "$GNUPGHOME"
    gpg --batch --passphrase "" --quick-gen-key "Alice <alice@desktop>" ed25519 sign,cert never
    fpr=$(gpg --list-keys --with-colons | awk -F: '/^fpr/ { print $10; exit }')
    gpg --batch --passphrase "" --quick-add-key "$fpr" cv25519 encr never
    mkdir $out
    gpg --armor --export "$fpr" > $out/public.asc
    gpg --armor --export-secret-keys "$fpr" > $out/secret.asc
    echo "$fpr" > $out/fingerprint
  '';

  # The admin pages are not what this test reads, and building them builds
  # the Lab's Bevy canvas for wasm32. LosOS Login only needs them present.
  adminUi = hostPkgs.writeTextDir "index.html" "admin pages";
in
{
  name = "losos-desktop-losos-box";

  # The desktop's packages come from the OS's overlay.
  node.pkgsReadOnly = false;

  nodes.box =
    { pkgs, ... }:
    {
      imports = [
        "${src}/modules/options.nix"
        "${src}/modules/configuration.nix"
        "${src}/modules/containers.nix"
        "${src}/modules/tls.nix"
        "${src}/modules/setup.nix"
        "${src}/modules/daemon.nix"
        "${src}/modules/identity.nix"
        "${src}/modules/cloud.nix"
        "${src}/modules/git.nix"
        "${src}/modules/config-repo.nix"
      ];
      losos = {
        hostName = "mattbox";
        backend.package = lososPkgs.losos-ctl;
        backend.plugins = lososPkgs.losos-plugins;
        admin.ui = adminUi;
        cloud.enable = true;
        cloud.photos.faces = false;
        git.enable = true;
        # The key `losos-ctl install --gpg-key` would have written into
        # install-target.nix.
        access.gpgKeys = [ (builtins.readFile "${key}/public.asc") ];
      };
      # The test network's address is static, as NetworkManager would not
      # keep it without DHCP; every other link stays NetworkManager's.
      networking.networkmanager.unmanaged = [ "eth1" ];
      environment.systemPackages = [ pkgs.jq ];
      virtualisation = {
        memorySize = 3072;
        cores = 2;
        diskSize = 4096;
      };
    };

  nodes.desktop =
    { pkgs, lib, ... }:
    {
      imports = [
        ../modules/backup.nix
        # The box's own browser for VM tests, which signs in to LosOS
        # Login and presses Accept on the consent page as matus would.
        (import "${src}/nix/tests/screens" { inherit pkgs; }).browser
      ];
      # The programs this test needs from the OS's overlay, not the overlay whole: its
      # GTK and Qt would rebuild the keychain for nothing this test reads.
      nixpkgs.overlays = [
        (final: prev: {
          inherit (import ../pkgs final prev)
            losos
            losos-boxes
            losos-backup
            pm
            ;
        })
      ];
      environment.systemPackages = [
        pkgs.losos
        pkgs.losos-boxes
        pkgs.curl
        pkgs.jq
        pkgs.libsecret
      ];
      environment.etc."losos-test/secret.asc".source = "${key}/secret.asc";
      users.users.alice = {
        isNormalUser = true;
        uid = 1000;
        # Its session bus and keychain are up without anyone logging in.
        linger = true;
      };
      # The keychain losos keeps its key and tokens in (desktop.nix).
      services.gnome.gnome-keyring.enable = true;

      # network.nix's mDNS, as in losos-boxes.nix: resolved with MulticastDNS
      # on the link, networkd keeping the test network's static address.
      networking.useNetworkd = true;
      systemd.network.networks."40-eth1".networkConfig.MulticastDNS = true;
      services.resolved = {
        enable = true;
        settings.Resolve.MulticastDNS = true;
      };
      networking.firewall.allowedUDPPorts = [ 5353 ];
      # Chromium under TCG beside the backups.
      virtualisation.memorySize = lib.mkForce 2560;
    };

  testScript = ''
    import json
    import shlex

    BOX = "mattbox.local"
    URL = f"https://{BOX}"
    PASSWORD = "${password}"
    BUS = "unix:path=/run/user/1000/bus"

    def alice(command, check=True):
        """A command as alice, on her session bus, as her terminal runs it."""
        script = f"export DBUS_SESSION_BUS_ADDRESS={BUS}; cd; {command}"
        full = f"su - alice -c {shlex.quote(script)}"
        return desktop.succeed(full) if check else desktop.execute(full)

    start_all()
    box.wait_for_unit("lososd.service")
    box.wait_for_unit("nginx.service")
    box.wait_for_unit("systemd-resolved.service")
    box.wait_for_open_port(443)
    desktop.wait_for_unit("systemd-resolved.service")
    desktop.wait_for_unit("user@1000.service")

    with subtest("the box is claimed as the setup wizard claims it"):
        claim = json.dumps({"name": "matus", "password": PASSWORD})
        out = box.succeed(
            f"curl -sk -w '\\n%{{http_code}}' -X POST https://127.0.0.1/api/setup/claim"
            f" -H 'Host: {BOX}' -H 'Content-Type: application/json' -d {shlex.quote(claim)}"
        )
        body, code = out.rsplit("\n", 1)
        assert code == "200", (code, body)
        spare = json.loads(body)["token"]
        box.wait_for_unit("authelia-losos.service")
        box.wait_for_unit("losos-cloud.service")
        box.wait_for_unit("losos-git.service")

    with subtest("losos-boxes finds the box by its announcement"):
        found = desktop.wait_until_succeeds(
            "out=$(losos-boxes list) && echo \"$out\" | grep -P '^mattbox\\t' && echo \"$out\"",
            timeout=180,
        )
        print(found)
        name, url, version = found.splitlines()[0].split("\t")
        assert url == f"http://{BOX}", url
        # The release the box's tree says it is, which its TXT record carries.
        assert version == "${import "${src}/nix/version.nix"}", version

    with subtest("losos signs in with a token for the installed key"):
        # A keychain on alice's session bus, unlocked, as a desktop has.
        alice("printf hunter2 | gnome-keyring-daemon --unlock --replace --daemonize --components=secrets")
        alice("losos key import /etc/losos-test/secret.asc")
        fingerprint = "${builtins.readFile "${key}/fingerprint"}".strip()
        host = json.loads(desktop.succeed(f"curl -sk {URL}/api/access/host-key"))
        # The Access tokens pane's request, with the spare key.
        made = json.loads(desktop.succeed(
            f"curl -sk -X POST {URL}/api/access/tokens -H 'Authorization: Bearer {spare}'"
            " -H 'Content-Type: application/json'"
            f" -d {shlex.quote(json.dumps({'name': 'desktop', 'fingerprint': fingerprint, 'scope': 'read', 'expiresInDays': None}))}"
        ))
        alice(f"printf '%s\\n' {made['token']} | losos login {URL} --fingerprint {host['fingerprint']}")
        hosts = alice("losos hosts")
        assert URL in hosts, hosts
        state = json.loads(alice("losos call GET /api/state"))
        print(state)
        assert state.get("hostName", state.get("hostname")) == "mattbox", state

    with subtest("backups pin the box and sign in to LosOS Cloud"):
        alice(
            "mkdir -p Documents src/notes && echo hello > Documents/letter.txt"
            " && head -c 130M /dev/urandom > Documents/big.bin"
            " && cd src/notes && git init -q -b main && echo one > a.txt && git add a.txt"
            " && git -c user.name=a -c user.email=a@a commit -qm first"
        )
        pinned = alice(f"losos-backup choose {BOX}").strip()
        # What the box's own wizard shows for its certificate.
        served = box.succeed("curl -s http://127.0.0.1/setup/state.json | jq -r .fingerprint").strip()
        assert "sha256:" + pinned.replace(":", "").lower() == served.lower(), (pinned, served)
        desktop.fail(f"su - alice -c 'printf wrong | losos-backup sign-in cloud matus'")
        # The fallback: a password, for LosOS Cloud only.
        alice(f"printf %s {PASSWORD} | losos-backup sign-in cloud matus")
        status = json.loads(alice("losos-backup status"))
        assert status["cloud"], status

    with subtest("backups sign in through LosOS Login, approved in a browser"):
        alice("setsid nohup losos-backup sign-in box >/tmp/sign-in.out 2>/tmp/sign-in.err &")
        desktop.wait_until_succeeds("test -s /tmp/sign-in.out", timeout=300)
        prompt = json.loads(desktop.succeed("head -n1 /tmp/sign-in.out"))
        print(prompt)
        link = prompt["link"]
        assert link.startswith(f"{URL}/auth/consent/openid/device-authorization?user_code="), link
        assert link.endswith(prompt["code"]), prompt
        plan = {
            "width": 1280,
            "height": 800,
            "shots": [
                {
                    "name": "backups-consent",
                    "path": link[len(URL):],
                    "waitFor": {"text": "LosOS Desktop backups"},
                    "steps": [
                        {"click": {"selector": "button:has-text('Accept')"}},
                        {"waitText": "Consent has been accepted and processed"},
                    ],
                },
            ],
        }
        desktop.succeed(f"echo {shlex.quote(json.dumps(plan))} > /tmp/plan.json")
        _, said = desktop.execute(
            f"losos-capture --base {URL} --plan /tmp/plan.json"
            f" --out /tmp/screens --user matus --password '{PASSWORD}' 2>&1",
            timeout=1800,
        )
        print(said)
        desktop.copy_from_machine("/tmp/screens", "")
        # The token is traded for this computer's credentials, and the
        # command prints which accounts it signed in to.
        desktop.wait_until_succeeds("test $(wc -l < /tmp/sign-in.out) -ge 2", timeout=300)
        print(desktop.succeed("cat /tmp/sign-in.err"))
        made = json.loads(desktop.succeed("tail -n1 /tmp/sign-in.out"))
        print(made)
        assert made["cloud"] and made["git"] and not made["problems"], made
        status = json.loads(alice("losos-backup status"))
        assert status["cloud"] and status["git"], status
        # The token is LosOS Git's own, named for this computer.
        token = alice("cat ~/.config/losos-backup/secrets.json | jq -r .git").strip()
        who = json.loads(desktop.succeed(
            f"curl -sk -H 'Authorization: token {token}' {URL}/git/api/v1/user"
        ))
        assert who["login"] == made["git"], (who, made)
        # And the LosOS Login token was good for nothing else.
        alice("losos-backup on")

    with subtest("a home reaches LosOS Cloud"):
        desktop.succeed("systemctl --user -M alice@ start losos-backup-now.service")
        report = json.loads(alice("cat ~/.local/state/losos-backup/last.json"))["last"]
        print(report)
        assert report["outcome"] == "done" and report["failed"] == 0, report
        # Under the account LosOS Cloud named at sign-in: the owner's own.
        user = json.loads(alice("cat ~/.config/losos-backup/config.json"))["cloud"]["user"]
        listing = desktop.succeed(
            f"curl -sk -u matus:{PASSWORD} -X PROPFIND -H 'Depth: infinity'"
            f" {URL}/nextcloud/remote.php/dav/files/{user}/Backups/desktop/"
        )
        assert "letter.txt" in listing and "big.bin" in listing, listing
        # The repository went to LosOS Git, not to LosOS Cloud as files.
        assert report["repos"] >= 1, report
        repos = json.loads(desktop.succeed(
            f"curl -sk -H 'Authorization: token {token}' {URL}/git/api/v1/user/repos"
        ))
        print([r["name"] for r in repos])
        assert any("notes" in r["name"] for r in repos), repos
  '';
}
