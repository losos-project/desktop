# Signing in with the eID card (eid-signin.nix, guest/eid-pkcs11.nix,
# docs/tokens.md), both ends on one machine over vsock's loopback: the
# host's socket, relay and p11-kit configuration as the image has them,
# and the eID machine's half beside them, serving SoftHSM in place of the
# eID klient's closed module. The token holds what an eID card holds that
# matters here: a certificate whose key may decrypt and a signing one.
#
# losos-eid-signin picks the first, homed enrols it for a person, and the
# person then logs in on the console with the token's PIN and no password.
# Programs on the host that are neither root nor at the seat get nothing,
# and nobody but the relay reaches the machine's half.
{ lib, ... }:

let
  pin = "123456";
in
{
  name = "losos-desktop-eid-signin";

  node.pkgsReadOnly = false;

  nodes.host =
    { config, pkgs, ... }:
    let
      softhsm = "${pkgs.softhsm}/lib/softhsm/libsofthsm2.so";
      softhsmConf = pkgs.writeText "softhsm2.conf" ''
        directories.tokendir = /var/lib/softhsm
        objectstore.backend = file
      '';
    in
    {
      imports = [
        ../modules/eid-signin.nix
        # For the option eid-signin.nix declares the eID machine in; this
        # test has none.
        ../modules/service-machines.nix
        ../guest/eid-pkcs11.nix
      ];
      nixpkgs.overlays = [ (import ../pkgs) ];

      boot.kernelModules = [ "vsock_loopback" ];
      services.homed.enable = true;

      # The host's half, reaching the machine's on this kernel.
      losos.eidSignin = {
        enable = true;
        machine = false;
        machineCid = 1;
      };

      # The machine's half, with SoftHSM for the eID klient's module. Root
      # rather than DynamicUser, for the token the test script makes.
      losos.eidPkcs11 = {
        module = softhsm;
        hostCid = 1;
      };
      systemd.services."losos-eid-module@" = {
        environment.SOFTHSM2_CONF = softhsmConf;
        serviceConfig = {
          DynamicUser = lib.mkForce false;
          ReadWritePaths = [ "/var/lib/softhsm" ];
        };
      };
      systemd.tmpfiles.rules = [ "d /var/lib/softhsm 0700 root root -" ];
      environment.variables.SOFTHSM2_CONF = "${softhsmConf}";

      environment.systemPackages = [
        pkgs.softhsm
        pkgs.opensc
        pkgs.openssl
        pkgs.socat
      ];
      environment.etc."eid-test/softhsm".text = softhsm;
      environment.etc."eid-test/client".text = "${pkgs.p11-kit}/lib/pkcs11/p11-kit-client.so";
    };

  testScript = ''
    softhsm = host.succeed("cat /etc/eid-test/softhsm").strip()
    client = host.succeed("cat /etc/eid-test/client").strip()

    host.wait_for_unit("multi-user.target")
    host.wait_for_unit("losos-eid-pkcs11.socket")
    host.wait_for_unit("losos-eid-module.socket")

    with subtest("the token holds an encryption and a signing certificate"):
        host.succeed(
            "softhsm2-util --init-token --free --label eID --pin ${pin} --so-pin 12345678",
            "cd /tmp",
        )
        for name, number, usage in [
            ("encryption", "01", "keyEncipherment"),
            ("signing", "02", "digitalSignature,nonRepudiation"),
        ]:
            host.succeed(
                f"openssl req -x509 -newkey rsa:2048 -nodes -keyout /tmp/{name}.key"
                f" -out /tmp/{name}.pem -days 30 -subj '/CN=Test Person {name}'"
                f" -addext 'keyUsage=critical,{usage}' -addext 'basicConstraints=CA:FALSE'",
                f"openssl pkcs8 -topk8 -nocrypt -in /tmp/{name}.key -out /tmp/{name}.p8",
                f"softhsm2-util --import /tmp/{name}.p8 --token eID --label {name}"
                f" --id {number} --pin ${pin}",
                f"openssl x509 -in /tmp/{name}.pem -outform der -out /tmp/{name}.der",
                f"pkcs11-tool --module {softhsm} --login --pin ${pin}"
                f" --write-object /tmp/{name}.der --type cert --id {number} --label {name}",
            )

    with subtest("losos-eid-signin picks the certificate that may decrypt"):
        uri = host.succeed("losos-eid-signin --print").strip()
        # The encryption certificate's ID, which the key beside it shares.
        assert uri.endswith(";id=%01"), uri
        host.succeed("journalctl -u 'losos-eid-module@*' | grep -q 'serving the eID card'")

    with subtest("homed enrols it, and the PIN alone logs the person in"):
        # homectl logs in to the token to find the key beside the
        # certificate, so it needs the PIN as well as a password.
        host.succeed(
            "NEWPASSWORD=correct-horse-battery PIN=${pin} timeout 300 homectl create alice --no-ask-password"
            f" --storage=directory --pkcs11-token-uri='{uri}' </dev/null"
        )
        host.succeed("homectl inspect alice --json=short | grep -q pkcs11EncryptedKey")
        host.succeed("PIN=${pin} timeout 120 homectl authenticate alice --no-ask-password </dev/null")
        host.fail("PIN=000000 PASSWORD=wrong timeout 120 homectl authenticate alice --no-ask-password </dev/null")
        # The console's login, as a person at the computer uses it:
        # pam_systemd_home finds the token wants its PIN and asks for that
        # rather than the password.
        host.wait_for_unit("getty@tty1.service")
        host.wait_until_tty_matches("1", "login: ")
        host.send_chars("alice\n")
        host.wait_until_tty_matches("1", "PIN")
        host.send_chars("${pin}\n")
        host.wait_until_succeeds("loginctl list-sessions | grep -q alice")
        host.send_chars("clear; echo signed in as $(whoami) with the eID token\n")
        host.wait_until_tty_matches("1", "signed in as alice with the eID token")
        host.screenshot("signed-in")

    with subtest("nobody but root and the person at the seat reaches the card"):
        tokens = host.succeed(
            "su -s /bin/sh nobody -c"
            " 'P11_KIT_SERVER_ADDRESS=unix:path=/run/losos-eid/pkcs11.sock"
            f" pkcs11-tool --module {client} -L' 2>&1 || true"
        )
        assert "eID" not in tokens, tokens
        host.wait_until_succeeds(
            "journalctl -u 'losos-eid-pkcs11@*' | grep -q 'refused the eID card: not root and not at the seat'"
        )

    with subtest("and nothing but the relay reaches the machine's half"):
        host.succeed("su -s /bin/sh nobody -c 'socat -u /dev/null VSOCK-CONNECT:1:7373' || true")
        host.wait_until_succeeds(
            "journalctl -u 'losos-eid-module@*' | grep -q 'not the host.s relay'"
        )
  '';
}
