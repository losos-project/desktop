# Secure Boot on a PC (docs/secure-boot.md): with LosOS's key, GRUB and every
# UKI are signed, and shim, signed by Microsoft, starts GRUB once a person
# has enrolled LosOS's certificate in MokManager. The ESP's files are
# shim.nix's, put there by grub.nix; this signs the UKI and checks the key
# and certificate belong together.
#
# The key comes from outside the Nix store (options.nix) because the store
# is world-readable and the binary cache publishes it. A derivation carries
# only its path, which shim.nix's signing step reads outside the sandbox on
# a builder that has it.
{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.losos.secureBoot;
  shim = import ./shim.nix {
    inherit pkgs;
    inherit (cfg) privateKey certificate;
  };
in
{
  system.build.secureBoot = shim;

  # A key without its certificate fails in shim.nix, where the installer's
  # medium meets it too.
  assertions = [
    {
      # A key in the store is in the binary cache. The tests sign with one
      # that is public on purpose, which they say by putting it there.
      assertion =
        cfg.privateKey != null && lib.hasPrefix builtins.storeDir cfg.privateKey
        -> lib.hasInfix "losos-secure-boot-test-key" cfg.privateKey;
      message = "losos.secureBoot.privateKey is in the Nix store, where anyone can read it. Give the build a path outside it (docs/secure-boot.md).";
    }
  ];

  # nixpkgs' UKI with usrhash= on its command line (repart-verity-store.nix),
  # built the same way and then signed. ukify can sign as it builds, but
  # then the step that reads the key unsandboxed would be the one that
  # reads the kernel, the initrd and the rest too; the signing step reads
  # one file (shim.nix). The kernel inside is left unsigned: shim accepts it
  # as part of the UKI it has already checked.
  system.build.uki = lib.mkIf shim.enable (
    lib.mkOverride 98 (
      let
        inherit (config.system.boot.loader) ukiFile;
        cmdline = "init=${config.system.build.toplevel}/init ${toString config.boot.kernelParams}";
        unsigned =
          pkgs.runCommand "${ukiFile}-unsigned"
            {
              nativeBuildInputs = [
                pkgs.buildPackages.jq
                pkgs.buildPackages.systemdUkify
              ];
            }
            ''
              usrhash=$(jq -r '.[] | select(.type=="${config.losos.arch.usrVerity}") | .roothash' \
                ${config.system.build.intermediateImage}/repart-output.json)
              ukify build --config=${config.boot.uki.configFile} \
                --cmdline="${cmdline} usrhash=$usrhash" --output=$out
            '';
      in
      pkgs.runCommand ukiFile { } ''
        mkdir $out
        cp ${shim.sign ukiFile unsigned} $out/${ukiFile}
      ''
    )
  );
}
