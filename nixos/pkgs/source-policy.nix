# The source policy (docs/trust.md, "What may be built from source") as a
# check: `tools/source-policy` reads the tree and fails on a fetch whose
# origin is neither the losos-project namespace nor nixpkgs nor a
# distributor's prebuilt package.
#
# It reads the declarations rather than a built closure, which is why it is
# cheap enough to run on every `nix flake check`: the policy is about where a
# source comes from, and every source here is a literal path, a nixpkgs
# attribute or a fetch in one of these files. Nothing is built, so the check
# is the same on a laptop without the image's RAM as in CI.
{
  lib,
  runCommandLocal,
}:

runCommandLocal "source-policy"
  {
    src = lib.fileset.toSource {
      root = ../..;
      fileset = lib.fileset.unions [
        ../../flake.lock
        ../../nixos
        ../../tools/source-policy
      ];
    };
  }
  ''
    bash ${../../tools/source-policy} $src
    touch $out
  ''
