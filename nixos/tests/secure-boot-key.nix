# A Secure Boot key and certificate for the VM tests, made when they are
# built. Public on purpose: the key is in the Nix store, which
# secure-boot.nix refuses for any key whose path does not say it is this one.
{ pkgs }:

pkgs.runCommand "losos-secure-boot-test-key" { nativeBuildInputs = [ pkgs.buildPackages.openssl ]; }
  ''
    mkdir $out
    openssl req -x509 -newkey rsa:2048 -sha256 -nodes -days 36500 \
      -subj "/CN=LosOS Desktop Secure Boot (test)/" \
      -keyout $out/key.pem -out $out/cert.pem
  ''
