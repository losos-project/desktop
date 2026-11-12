# LosOS admin/panes web export, from the LosOS appliance repo.
# Nothing in nixos/modules installs this yet; the overlay entry is for
# follow-up wiring and `nix build .#losos-admin-panes`.
#
# Pinned to channel/daily (latest cut on that branch). Before Expo export,
# channel/daily still runs `bun run build` into dist/. Sparse-checkout
# admin-ui/app only (docs/trust.md, losos.nix).
{
  lib,
  stdenvNoCC,
  fetchgit,
  bun,
}:

let
  lososRev = "2d36b250037b2e97bc1daca3fd5258f465bf3bda";
  src = fetchgit {
    url = "https://github.com/dasmatus/losos";
    rev = lososRev;
    hash = "sha256-jPRu9LY2Wjjb4Gzz3tpyFp1vikoY0u+RpCqenxAp8T0=";
    sparseCheckout = [ "admin-ui/app" ];
  };
in
stdenvNoCC.mkDerivation {
  pname = "losos-admin-panes";
  version = "0-unstable-${builtins.substring 0 12 lososRev}";

  inherit src;

  nativeBuildInputs = [ bun ];

  buildPhase = ''
    runHook preBuild
    export HOME=$TMPDIR
    cd admin-ui/app
    bun install --frozen-lockfile --ignore-scripts || bun install --ignore-scripts
    bun run build
    runHook postBuild
  '';

  installPhase = ''
    runHook preInstall
    mkdir -p $out/share/losos-admin-panes
    cp -r admin-ui/app/dist/* $out/share/losos-admin-panes/
    runHook postInstall
  '';

  meta = {
    description = "LosOS admin/panes web export for LosOS Desktop";
    homepage = "https://github.com/dasmatus/losos/tree/channel/daily";
    license = lib.licenses.eupl12;
    platforms = lib.platforms.linux;
  };
}
