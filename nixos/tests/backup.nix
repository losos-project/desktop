# Backups to a LosOS box (backup.nix, docs/backup.md).
#
# `box` is a LosOS box as far as the backups can tell: Nextcloud at
# /nextcloud and Forgejo at /forgejo/ behind one nginx with a self-signed
# certificate for testbox.local, made the way the box makes its own
# (modules/tls.nix in the LosOS repository), and the certificate with its
# fingerprint under /setup/. `desktop` is a person's session with the
# backups module, which signs in to both, backs up a home with files and a
# repository, and backs up again after changing them. Then it reaches the
# box as a PC with a LosOS boxes machine does, through the address the
# relay's hosts file names, and skips the run once no machine names it.
#
# Not the whole image, only what the backups need, so it boots the test
# driver's kernel quickly and runs without KVM too.
{ lib, ... }:

let
  boxName = "testbox.local";
  cloudPassword = "Cloud-test-password-1";
  gitPassword = "Git-test-password-1";
  # The box's certificate as tls.nix makes it, with state.json's
  # fingerprint, made once at build time.
  certificate =
    pkgs:
    pkgs.runCommand "testbox-certificate" { nativeBuildInputs = [ pkgs.openssl ]; } ''
      mkdir -p $out/setup
      openssl req -x509 -newkey rsa:2048 -sha256 -days 3650 -nodes \
        -keyout $out/box.key -out $out/box.crt \
        -subj "/CN=${boxName}/O=losos" \
        -addext "subjectAltName=DNS:${boxName},DNS:testbox,DNS:localhost,IP:127.0.0.1" \
        -addext "basicConstraints=critical,CA:TRUE" \
        -addext "keyUsage=critical,digitalSignature,keyCertSign"
      cp $out/box.crt $out/setup/losos-ca.crt
      hex=$(openssl x509 -in $out/box.crt -noout -fingerprint -sha256 | cut -d= -f2 | tr -d : | tr A-F a-f)
      printf '{"hostName":"testbox","fqdn":"${boxName}","fingerprint":"sha256:%s"}' "$hex" > $out/setup/state.json
    '';
in
{
  name = "losos-desktop-backup";

  # The desktop's packages come from the OS's overlay; the box's are
  # nixpkgs' own.
  node.pkgsReadOnly = false;

  nodes.box =
    { pkgs, ... }:
    let
      cert = certificate pkgs;
    in
    {
      virtualisation.memorySize = 2048;
      environment.systemPackages = [ pkgs.curl ];
      networking.firewall.allowedTCPPorts = [
        80
        443
      ];
      services.nextcloud = {
        enable = true;
        package = pkgs.nextcloud33;
        # Its own vhost on loopback; the front one below carries it at
        # /nextcloud as the box does.
        hostName = "nextcloud.internal";
        # No admin account: nixpkgs' module stopped making one (an
        # installation makes its users with nextcloud-occ), and the test
        # makes alice that way below.
        config.dbtype = "sqlite";
        settings = {
          trusted_domains = [ boxName ];
          trusted_proxies = [ "127.0.0.1" ];
          overwritehost = boxName;
          overwriteprotocol = "https";
          overwritewebroot = "/nextcloud";
        };
      };
      services.forgejo = {
        enable = true;
        settings.server = {
          ROOT_URL = "https://${boxName}/forgejo/";
          HTTP_ADDR = "127.0.0.1";
          HTTP_PORT = 3000;
        };
      };
      services.nginx = {
        enable = true;
        virtualHosts."nextcloud.internal".listen = [
          {
            addr = "127.0.0.1";
            port = 8080;
          }
        ];
        virtualHosts.${boxName} = {
          default = true;
          addSSL = true;
          sslCertificate = "${cert}/box.crt";
          sslCertificateKey = "${cert}/box.key";
          locations."/setup/".alias = "${cert}/setup/";
          locations."/nextcloud/" = {
            proxyPass = "http://127.0.0.1:8080/";
            extraConfig = ''
              client_max_body_size 512M;
              proxy_request_buffering off;
            '';
          };
          locations."/forgejo/".proxyPass = "http://127.0.0.1:3000/";
        };
      };
    };

  nodes.desktop =
    { nodes, pkgs, ... }:
    {
      imports = [ ../modules/backup.nix ];
      environment.systemPackages = [
        pkgs.curl
        pkgs.openssl
      ];
      nixpkgs.overlays = [ (import ../pkgs) ];
      networking.hosts.${nodes.box.networking.primaryIPAddress} = [ boxName ];
      users.users.alice = {
        isNormalUser = true;
        uid = 1000;
      };
    };

  testScript =
    { nodes, ... }:
    let
      boxAddress = nodes.box.networking.primaryIPAddress;
      forgejo = "${nodes.box.services.forgejo.package}/bin/forgejo";
    in
    ''
      import json

      def alice(command):
          return desktop.succeed(f"su - alice -c {json.dumps(command)}")

      def last():
          return json.loads(alice("cat ~/.local/state/losos-backup/last.json"))["last"]

      def back_up():
          # The unit Settings' "Back up now" starts, in alice's own manager.
          desktop.succeed("systemctl --user -M alice@ start losos-backup-now.service")
          report = last()
          assert report["outcome"] == "done", report
          return report

      def forgejo(path):
          return json.loads(box.succeed(
              f"curl -sf -u alice:${gitPassword} http://127.0.0.1:3000/api/v1/repos/alice/{path}"
          ))

      files = "/var/lib/nextcloud/data/alice/files/Backups/desktop"

      start_all()
      box.wait_for_unit("nextcloud-setup.service")
      box.wait_for_unit("forgejo.service")
      box.wait_for_unit("nginx.service")
      box.wait_for_open_port(3000)
      box.succeed(
          "OC_PASS=${cloudPassword} nextcloud-occ user:add --password-from-env alice",
          "su -s /bin/sh forgejo -c '${forgejo} --work-path /var/lib/forgejo"
          " --config /var/lib/forgejo/custom/conf/app.ini admin user create"
          " --username alice --password ${gitPassword} --email alice@example.com --must-change-password=false'",
      )
      desktop.wait_for_unit("multi-user.target")
      desktop.succeed("loginctl enable-linger alice")
      desktop.wait_for_unit("user@1000.service")

      with subtest("a home with files, configs, a cache and a repository"):
          alice(
              "mkdir -p Documents .cache/thumbs .config/git src/notes && echo hello > Documents/letter.txt"
              " && echo 'alias ll=ls' > .bashrc && printf '[user]\\n\\tname = Alice\\n' > .config/git/config"
              " && head -c 150M /dev/urandom > Documents/big.bin && echo cache > .cache/thumbs/x"
              " && cd src/notes && git init -q -b main && echo one > a.txt && git add a.txt"
              " && git -c user.name=a -c user.email=a@a commit -qm first && echo draft > draft.txt"
          )

      with subtest("choosing the box pins its certificate"):
          fingerprint = alice("losos-backup choose ${boxName}").strip()
          expected = desktop.succeed(
              "curl -s http://${boxName}/setup/losos-ca.crt | openssl x509 -noout -fingerprint -sha256 | cut -d= -f2"
          ).strip()
          assert fingerprint == expected, (fingerprint, expected)

      with subtest("signing in trades each password for a token of this computer's"):
          desktop.fail("su - alice -c 'printf wrong | losos-backup sign-in cloud alice'")
          alice("printf ${cloudPassword} | losos-backup sign-in cloud alice")
          alice("printf ${gitPassword} | losos-backup sign-in git alice")
          alice("losos-backup on")
          status = json.loads(alice("losos-backup status"))
          assert status["cloud"] == "alice" and status["git"] == "alice" and status["enabled"], status
          assert status["routed"] is False, status
          assert "${cloudPassword}" not in alice("cat ~/.config/losos-backup/*")
          assert alice("stat -c %a ~/.config/losos-backup/secrets.json").strip() == "600"

      with subtest("files go to LosOS cloud, the repository to LosOS Git"):
          report = back_up()
          assert report["uploaded"] == 2 and report["repos"] == 1 and report["failed"] == 0, report
          assert box.succeed(f"cat {files}/Documents/letter.txt").strip() == "hello"
          assert box.succeed(f"stat -c %s {files}/Documents/big.bin").strip() == str(150 << 20)
          box.fail(f"test -e {files}/.cache")
          box.fail(f"test -e {files}/src/notes")
          # The configs to a repository of their own, not LosOS cloud.
          assert report["configs"] >= 2 and report["configs_changed"], report
          box.fail(f"test -e {files}/.bashrc")
          box.fail(f"test -e {files}/.config")
          assert forgejo("desktop-config")["private"]
          assert forgejo("desktop-config/contents/.config/git/config")["type"] == "file"
          assert len(forgejo("desktop-config/commits")) == 1
          repo = forgejo("notes")
          assert repo["private"], repo
          branches = {b["name"]: b["commit"]["message"].strip() for b in forgejo("notes/branches")}
          assert branches == {
              "main": "first",
              "losos-backup/uncommitted": "Uncommitted changes in ~/src/notes on desktop",
          }, branches

      with subtest("a second backup sends what changed and removes what went"):
          alice(
              "echo again >> Documents/letter.txt && rm Documents/big.bin && echo 'alias la=ls' >> .bashrc"
              " && cd src/notes && git add -A && git -c user.name=a -c user.email=a@a commit -qm second"
          )
          report = back_up()
          assert report["uploaded"] == 1 and report["deleted"] == 1 and report["unchanged"] == 0, report
          assert "again" in box.succeed(f"cat {files}/Documents/letter.txt")
          box.fail(f"test -e {files}/Documents/big.bin")
          branches = {b["name"]: b["commit"]["message"].strip() for b in forgejo("notes/branches")}
          assert branches == {"main": "second"}, branches
          # One commit more for the changed config, on top of the first.
          assert report["configs_changed"], report
          commits = forgejo("desktop-config/commits")
          assert len(commits) == 2 and commits[0]["parents"][0]["sha"] == commits[1]["sha"], commits

      with subtest("through a LosOS boxes machine, and skipped without one"):
          # The relay's file for a machine named boxes; this desktop has no
          # libvirt, so the address is the box's own.
          desktop.succeed(
              "mkdir -p /run/losos-boxes/hosts",
              "echo '${boxAddress} ${boxName}' > /run/losos-boxes/hosts/boxes",
          )
          status = json.loads(alice("losos-backup status"))
          assert status["routed"] and status["boxes"] == ["${boxName}"], status
          report = back_up()
          assert report["machine"] == "boxes" and report["unchanged"] == 1, report
          assert not report["configs_changed"], report
          desktop.succeed("rm /run/losos-boxes/hosts/boxes")
          desktop.succeed("systemctl --user -M alice@ start losos-backup-now.service")
          report = last()
          assert report["outcome"] == "skipped", report

      with subtest("the timer's run waits for a day to pass"):
          desktop.succeed("echo '${boxAddress} ${boxName}' > /run/losos-boxes/hosts/boxes")
          before = last()
          desktop.succeed("systemctl --user -M alice@ start losos-backup.service")
          assert last() == before
    '';
}
