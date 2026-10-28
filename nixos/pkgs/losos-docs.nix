# This documentation, the Docusaurus site in website/ with docs/ as its
# pages, built to be read offline (docs/troubleshooting.md, the docs.nix
# module). DOCS_OFFLINE gives it hash routes, so every page loads from one
# index.html opened as a file:// URL with no server, and adds
# sections/<page>/<heading>.html, the plain files derisk's error alerts open
# (website/offline-sections.js).
#
# Only docs/ and website/ are its source, so a change anywhere else in the
# repository leaves it as it was. npmDepsHash changes with
# website/package-lock.json: `prefetch-npm-deps website/package-lock.json`.
{
  lib,
  buildNpmPackage,
  fetchNpmDeps,
  nodejs,
}:

let
  src = lib.fileset.toSource {
    root = ../..;
    fileset = lib.fileset.unions [
      ../../docs
      (lib.fileset.difference ../../website (lib.fileset.maybeMissing ../../website/node_modules))
    ];
  };
in
buildNpmPackage {
  pname = "losos-docs";
  version = "0";
  inherit src nodejs;

  # The npm project is website/, beside the pages it builds from.
  npmRoot = "website";
  npmDeps = fetchNpmDeps {
    src = "${src}/website";
    hash = "sha256-Kp1k80QDntfJ3CasfdzhmxiRAXD24Q0ROuL8lZZdQxo=";
  };

  env.DOCS_OFFLINE = "1";

  buildPhase = ''
    runHook preBuild
    # Docusaurus keeps a cache under the home directory.
    export HOME=$TMPDIR
    (cd website && npm run build -- --out-dir "$out/share/doc/losos")
    runHook postBuild
  '';
  # The site is the output; there is no npm package to install.
  dontNpmInstall = true;
  installPhase = ''
    runHook preInstall
    runHook postInstall
  '';

  meta = {
    description = "The LosOS Desktop documentation, for reading offline";
    license = lib.licenses.agpl3Plus;
    platforms = lib.platforms.all;
  };
}
