# Company accounts (company.nix, docs/company.md).
#
# `box` is a LosOS box as far as the bridge can tell: Authelia, which is
# what LosOS Login is, under /auth/ behind one nginx with a self-signed
# certificate for testbox.local, made and stated under /setup/ the way the
# box does it (modules/tls.nix in the LosOS repository), and a `GET
# /api/edge` behind Authelia that names a gateway of the secondary cluster
# `office`, as lososd says when the box goes through one. `desktop` has
# homed, the bridge and the console's PAM stack. It is joined to the box,
# then people log in through PAM: the first login makes each a homed record
# in the realm `office`, an administrator of the box becomes one here, a
# wrong password and a person disabled at the box are refused, the box out
# of reach falls back to homed for those who logged in before and to
# nobody else, and a password changed at the box re-keys the home. Between
# logins the box's people list locks those it removed, and the company's
# Mesh CI jobs (a stand-in for the box's, as lososd relays them) run their
# fixed steps here and refuse anything else.
#
# Not the whole image, only what the bridge needs, so it boots the test
# driver's kernel quickly and runs without KVM too.
{ lib, ... }:

let
  boxName = "testbox.local";
  adaPassword = "Ada-test-password-1";
  bobPassword = "Bob-test-password-1";
  bobNewPassword = "Bob-test-password-2";
  carolPassword = "Carol-test-password-1";
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
  # LosOS Login's people file as lososd writes it: argon2id hashes in
  # Authelia's format. Three versions: as joined, after bob changed his
  # password, and with bob disabled.
  people =
    pkgs:
    pkgs.runCommand "testbox-people" { nativeBuildInputs = [ pkgs.authelia ]; } ''
      # Cheap parameters: the default argon2id costs a TCG guest longer
      # than the bridge waits for an answer.
      hash() {
        authelia crypto hash generate argon2 --password "$1" -i 1 -m 8192 -p 1 \
          | sed -n 's/^Digest: //p'
      }
      ada=$(hash ${adaPassword}); bob=$(hash ${bobPassword}); bob2=$(hash ${bobNewPassword})
      carol=$(hash ${carolPassword})
      write() {
        cat > $out/$1 <<EOF
      users:
        ada:
          displayname: Ada Lovelace
          email: ada@example.com
          password: "$ada"
          groups: [losos-admins]
        bob:
          displayname: Bob Builder
          password: "$2"
          disabled: $3
          groups: [staff]
        carol:
          displayname: Carol
          password: "$carol"
          groups: [staff]
      EOF
      }
      mkdir -p $out
      write joined.yml "$bob" false
      write changed.yml "$bob2" false
      write disabled.yml "$bob2" true
    '';
  # The token lososd's `POST /api/devices/directory` gives the joined
  # computer, and its people list (`GET /api/directory/people`) as the box
  # has it when bob is disabled.
  directoryToken = "losos_dir_0123abcd_${lib.fixedWidthString 40 "0" "f"}";
  directoryPeople = builtins.toJSON {
    people = [
      {
        name = "ada";
        displayName = "Ada Lovelace";
        email = "ada@example.com";
        groups = [ "losos-admins" ];
        disabled = false;
      }
      {
        name = "bob";
        displayName = "Bob Builder";
        email = "";
        groups = [ "staff" ];
        disabled = true;
      }
      {
        name = "carol";
        displayName = "Carol";
        email = "";
        groups = [ "staff" ];
        disabled = false;
      }
    ];
  };
  # What lososd's `GET /api/edge` says on a box that goes through a gateway
  # of the secondary cluster `office`.
  edge = builtins.toJSON {
    path = {
      name = "office-1";
      url = "http://gateway.local:8443";
      tunnel = "gateway.local:7000";
      source = "lan";
      tier = "secondary";
      cluster = "office";
      member = "office-1";
    };
  };
  # A stand-in for the box's Mesh CI as lososd relays it to a joined
  # computer: a job the test puts in /var/lib/fake-ci/queued.json is handed
  # out once, and every report is kept, one JSON line each.
  fakeCi =
    pkgs:
    pkgs.writers.writePython3 "fake-mesh-ci" { flakeIgnore = [ "E501" ]; } ''
      import json
      import os
      import time
      from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

      DIR = "/var/lib/fake-ci"
      TOKEN = "Bearer ${directoryToken}"


      class Handler(BaseHTTPRequestHandler):
          def answer(self, code, body=None):
              data = json.dumps(body).encode() if body is not None else b""
              self.send_response(code)
              self.send_header("Content-Type", "application/json")
              self.send_header("Content-Length", str(len(data)))
              self.end_headers()
              self.wfile.write(data)

          def do_POST(self):
              length = int(self.headers.get("Content-Length") or 0)
              body = self.rfile.read(length)
              if self.headers.get("Authorization") != TOKEN:
                  return self.answer(401, {"error": "nobody"})
              if self.path == "/api/directory/jobs/claim":
                  queued = os.path.join(DIR, "queued.json")
                  for _ in range(4):
                      if os.path.exists(queued):
                          with open(queued) as f:
                              job = json.load(f)
                          os.remove(queued)
                          return self.answer(200, job)
                      time.sleep(0.5)
                  return self.answer(204)
              if self.path == "/api/directory/jobs/report":
                  with open(os.path.join(DIR, "reports.jsonl"), "a") as f:
                      f.write(body.decode() + "\n")
                  return self.answer(200, {"stop": False})
              self.answer(404, {"error": "not found"})


      ThreadingHTTPServer(("127.0.0.1", 8099), Handler).serve_forever()
    '';
in
{
  name = "losos-desktop-company";

  # The desktop's packages come from the OS's overlay; the box's are
  # nixpkgs' own.
  node.pkgsReadOnly = false;

  nodes.box =
    { pkgs, ... }:
    let
      cert = certificate pkgs;
      authz = ''
        internal;
        proxy_pass http://127.0.0.1:9091/auth/api/authz/auth-request;
        proxy_pass_request_body off;
        proxy_set_header Content-Length "";
        proxy_set_header X-Original-Method $request_method;
        proxy_set_header X-Original-URL https://$host$request_uri;
        proxy_set_header X-Forwarded-Proto https;
        proxy_set_header X-Forwarded-Host $host;
      '';
    in
    {
      environment.systemPackages = [ pkgs.curl ];
      # The box asks itself by its own name, as nothing here answers mDNS.
      networking.hosts."127.0.0.1" = [ boxName ];
      networking.firewall.allowedTCPPorts = [
        80
        443
      ];
      services.authelia.instances.losos = {
        enable = true;
        secrets = {
          storageEncryptionKeyFile = "/etc/authelia/storage";
          jwtSecretFile = "/etc/authelia/jwt";
          sessionSecretFile = "/etc/authelia/session";
        };
        # As modules/identity.nix in the LosOS repository has it, where it
        # matters here: under /auth, the file backend read on every request,
        # everyone with an account let through at one factor.
        settings = {
          server.address = "tcp://127.0.0.1:9091/auth";
          ntp.disable_startup_check = true;
          authentication_backend = {
            password_reset.disable = true;
            password_change.disable = true;
            refresh_interval = "always";
            file = {
              path = "/var/lib/losos-identity/users.yml";
              watch = true;
            };
          };
          access_control = {
            default_policy = "deny";
            rules = [
              {
                domain = boxName;
                policy = "one_factor";
              }
            ];
          };
          # A test of wrong passwords must not get its people banned.
          regulation.max_retries = 0;
          session.cookies = [
            {
              domain = boxName;
              authelia_url = "https://${boxName}/auth/";
            }
          ];
          storage.local.path = "/var/lib/authelia-losos/db.sqlite3";
          notifier.filesystem.filename = "/var/lib/authelia-losos/notifications.txt";
        };
      };
      environment.etc =
        lib.genAttrs [ "authelia/storage" "authelia/jwt" "authelia/session" ] (_: {
          mode = "0400";
          user = "authelia-losos";
          text = "a-test-secret-of-more-than-twenty-characters";
        })
        // {
          # Each version of the people file, for the test to put in place.
          "people".source = people pkgs;
        };
      systemd.tmpfiles.settings."10-people" = {
        "/var/lib/losos-identity".d.mode = "0755";
        "/var/lib/losos-identity/users.yml".C = {
          argument = "${people pkgs}/joined.yml";
          mode = "0644";
        };
      };
      systemd.services.fake-mesh-ci = {
        wantedBy = [ "multi-user.target" ];
        serviceConfig = {
          ExecStart = fakeCi pkgs;
          StateDirectory = "fake-ci";
        };
      };
      services.nginx = {
        enable = true;
        virtualHosts.${boxName} = {
          default = true;
          addSSL = true;
          sslCertificate = "${cert}/box.crt";
          sslCertificateKey = "${cert}/box.key";
          locations."/setup/".alias = "${cert}/setup/";
          locations."/auth/" = {
            proxyPass = "http://127.0.0.1:9091";
            extraConfig = ''
              proxy_set_header Host $host;
              proxy_set_header X-Forwarded-Proto https;
              proxy_set_header X-Forwarded-Host $host;
              proxy_set_header X-Forwarded-For $remote_addr;
            '';
          };
          locations."= /internal/authz".extraConfig = authz;
          locations."= /api/edge".extraConfig = ''
            auth_request /internal/authz;
            default_type application/json;
            return 200 '${edge}';
          '';
          locations."= /api/devices/directory".extraConfig = ''
            auth_request /internal/authz;
            default_type application/json;
            return 200 '{"token":"${directoryToken}","computer":{"id":"0123abcd","device":"desktop"}}';
          '';
          locations."/api/directory/jobs/".proxyPass = "http://127.0.0.1:8099";
          locations."= /api/directory/people".extraConfig = ''
            if ($http_authorization != "Bearer ${directoryToken}") {
              return 401;
            }
            default_type application/json;
            return 200 '${directoryPeople}';
          '';
        };
      };
    };

  nodes.desktop =
    { nodes, pkgs, ... }:
    {
      imports = [
        ../modules/accounts.nix
        ../modules/company.nix
      ];
      nixpkgs.overlays = [ (import ../pkgs) ];
      security.polkit.enable = true;
      environment.systemPackages = [
        pkgs.curl
        pkgs.openssl
        pkgs.pamtester
      ];
      networking.hosts.${nodes.box.networking.primaryIPAddress} = [ boxName ];
      # Someone who may not join the computer: a plain account, no polkit
      # agent to ask an administrator.
      users.users.eve = {
        isNormalUser = true;
        uid = 1500;
      };
      # Plain directories in place of the image's LUKS homes: formatting a
      # LUKS volume takes a guest without KVM longer than homectl waits.
      # The password is the record's either way, so the bridge cannot tell.
      services.homed.settings.Home.DefaultStorage = lib.mkForce "directory";
      # The people list goes out of step only when a subtest says so, so
      # the sync runs when that subtest starts it, not on its timer.
      systemd.timers.losos-login-bridge-sync.wantedBy = lib.mkForce [ ];
      virtualisation.diskSize = 6144;
      virtualisation.memorySize = 2048;
    };

  testScript = ''
    import json
    import shlex

    def pam(user, *answers):
        """Authenticates through the console's `login` stack, as the
        console and derisk's own stack do, and returns (ok, output)."""
        stdin = "".join(a + "\n" for a in answers)
        status, out = desktop.execute(
            f"printf %s {shlex.quote(stdin)} | pamtester login {user} authenticate 2>&1"
        )
        return status == 0, out

    def record(user):
        return json.loads(desktop.succeed(f"userdbctl user {user} --json=short"))

    def box_people(version):
        box.succeed(f"install -m 0644 /etc/people/{version}.yml /var/lib/losos-identity/users.yml")
        # Authelia watches the file and reads it again on its own.
        box.sleep(3)

    # One after the other: two machines booting at once without KVM can
    # take longer than the driver waits for a machine's shell.
    box.start()
    box.wait_for_unit("nginx.service")
    box.wait_for_unit("authelia-losos.service")
    box.wait_until_succeeds("curl -skf https://${boxName}/auth/api/health")
    desktop.start()
    desktop.wait_for_unit("multi-user.target")
    desktop.wait_for_unit("systemd-homed.service")
    desktop.wait_for_unit("losos-login-bridge.socket")

    with subtest("a computer nobody joined has no company accounts"):
        ok, out = pam("ada", "${adaPassword}")
        assert not ok, out
        assert json.loads(desktop.succeed("losos-login-bridge status")) == {"joined": None}

    fingerprint = desktop.succeed("losos-login-bridge fingerprint ${boxName}").strip()
    expected = desktop.succeed(
        "curl -s http://${boxName}/setup/losos-ca.crt | openssl x509 -noout -fingerprint -sha256 | cut -d= -f2"
    ).strip()
    assert fingerprint == expected, (fingerprint, expected)

    with subtest("only an administrator here, as one of the box's, joins it"):
        out = desktop.fail(
            f"printf '%s\\n' ${adaPassword} | runuser -u eve -- losos-login-bridge join ${boxName} {fingerprint} ada"
        )
        assert "Only an administrator" in out, out
        out = desktop.fail(
            f"printf '%s\\n' ${bobPassword} | losos-login-bridge join ${boxName} {fingerprint} bob"
        )
        assert "does not administer" in out, out
        out = desktop.fail(
            "printf '%s\\n' ${adaPassword} | losos-login-bridge join ${boxName} 00:11 ada"
        )
        assert "not the one you compared" in out, out
        joined = json.loads(desktop.succeed(
            f"printf '%s\\n' ${adaPassword} | losos-login-bridge join ${boxName} {fingerprint} ada"
        ))["joined"]
        assert joined["realm"] == "office", joined
        assert joined["gateway"]["url"] == "http://gateway.local:8443", joined
        assert joined["joinedBy"] == "ada", joined
        desktop.succeed("test $(stat -c %a /var/lib/losos-login-bridge/joined.json) = 644")
        # The box's token for its people list: root's alone.
        desktop.succeed("test $(stat -c %a /var/lib/losos-login-bridge/directory-token) = 600")
        desktop.succeed("grep -qx '${directoryToken}' /var/lib/losos-login-bridge/directory-token")

    with subtest("the first login makes the person's home, in the company's realm"):
        ok, out = pam("bob", "${bobPassword}")
        assert ok, out
        assert "Setting up this computer for Bob Builder" in out, out
        bob = record("bob")
        assert bob["realm"] == "office" and bob["realName"] == "Bob Builder", bob
        assert "wheel" not in bob.get("memberOf", []), bob
        ok, out = pam("ada", "${adaPassword}")
        assert ok, out
        ada = record("ada")
        assert "wheel" in ada["memberOf"] and ada["emailAddress"] == "ada@example.com", ada

    with subtest("LosOS Login refusing keeps them out"):
        ok, out = pam("bob", "not-the-password")
        assert not ok and "did not accept" in out, out
        ok, out = pam("nobody", "whatever")
        assert not ok, out

    with subtest("out of reach, homed decides for those who logged in before"):
        box.block()
        ok, out = pam("bob", "${bobPassword}")
        assert ok, out
        ok, out = pam("bob", "not-the-password")
        assert not ok, out
        ok, out = pam("carol", "${carolPassword}")
        assert not ok and "cannot reach LosOS Login" in out, out
        box.unblock()

    with subtest("a password changed at the box re-keys the home"):
        box_people("changed")
        ok, out = pam("bob", "${bobNewPassword}", "${bobPassword}")
        assert ok, out
        assert "previous password" in out, out
        desktop.succeed("PASSWORD=${bobNewPassword} homectl authenticate --no-ask-password bob")
        # homectl asks again on its terminal after a refused password, even
        # with --no-ask-password, and that prompt would land in the test's
        # own shell; so the old one runs with no terminal and a time limit.
        desktop.fail(
            "PASSWORD=${bobPassword} setsid -w timeout 60"
            " homectl authenticate --no-ask-password bob </dev/null >/dev/null 2>&1"
        )
        # And the next login goes straight through.
        ok, out = pam("bob", "${bobNewPassword}")
        assert ok and "previous password" not in out, out

    with subtest("someone the box disabled cannot log in while it can be asked"):
        box_people("disabled")
        ok, out = pam("bob", "${bobNewPassword}")
        assert not ok, out

    with subtest("between logins, the box's people list locks them out offline too"):
        desktop.succeed("systemctl start losos-login-bridge-sync.service")
        assert record("bob").get("locked") is True, record("bob")
        assert record("ada").get("locked") is not True, record("ada")
        box.block()
        ok, out = pam("bob", "${bobNewPassword}")
        assert not ok, out
        ok, out = pam("ada", "${adaPassword}")
        assert ok, out
        box.unblock()

    def ci_job(job_id, *steps):
        """Hands the desktop a Mesh CI job and returns its reports once it
        reported how it ended."""
        job = {
            "id": job_id,
            "name": f"Fleet / {job_id}",
            "timeoutSecs": 600,
            "payload": {"desktop": "desktop", "steps": list(steps)},
        }
        box.succeed(f"echo {shlex.quote(json.dumps(job))} > /var/lib/fake-ci/queued.json.new")
        box.succeed("mv /var/lib/fake-ci/queued.json.new /var/lib/fake-ci/queued.json")
        box.wait_until_succeeds(
            f"grep -h '\"id\": *\"{job_id}\"' /var/lib/fake-ci/reports.jsonl | grep -q state",
            timeout=180,
        )
        lines = box.succeed("cat /var/lib/fake-ci/reports.jsonl").splitlines()
        return [r for r in map(json.loads, lines) if r["id"] == job_id]

    with subtest("the company's Mesh CI jobs run fixed steps here, and nothing else"):
        desktop.wait_for_unit("losos-login-bridge-manage.service")
        reports = ci_job(
            "1",
            {"uses": "losos/desktop-status", "with": {}, "name": "Status"},
            {"uses": "losos/desktop-lock", "with": {"user": "ada"}, "name": "Lock ada"},
        )
        assert reports[-1]["state"] == "success", reports
        log = "".join(r["log"] for r in reports)
        assert "Company account ada" in log, log
        assert "ada is locked" in log, log
        assert record("ada").get("locked") is True, record("ada")

        # A step outside the list: nothing of the job runs.
        reports = ci_job(
            "2",
            {"uses": "losos/desktop-unlock", "with": {"user": "ada"}},
            {"uses": "losos/desktop-shell", "with": {}},
        )
        assert reports[-1]["state"] == "error", reports
        assert record("ada").get("locked") is True, record("ada")

        # Nor does it touch an account that is not the company's.
        desktop.succeed(
            "NEWPASSWORD=Local-test-password-1 homectl create local --enforce-password-policy=no --no-ask-password"
        )
        reports = ci_job("3", {"uses": "losos/desktop-lock", "with": {"user": "local"}})
        assert reports[-1]["state"] == "failure", reports
        assert record("local").get("locked") is not True, record("local")

        reports = ci_job("4", {"uses": "losos/desktop-unlock", "with": {"user": "ada"}})
        assert reports[-1]["state"] == "success", reports
        assert record("ada").get("locked") is not True, record("ada")

    with subtest("local accounts are not the bridge's"):
        box.block()
        ok, out = pam("local", "Local-test-password-1")
        assert ok, out
        box.unblock()

    with subtest("leaving the company keeps the accounts, logged in by homed"):
        desktop.succeed("losos-login-bridge leave")
        desktop.fail("test -e /var/lib/losos-login-bridge/directory-token")
        ok, out = pam("ada", "${adaPassword}")
        assert ok, out
        ok, out = pam("carol", "${carolPassword}")
        assert not ok, out
  '';
}
