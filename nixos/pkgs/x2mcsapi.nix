# x2mcsapi, mcsapi's adapter that styles apps not built on mcsapi so they
# match derisk: a GTK 3 and GTK 4 theme, a Qt style sheet and web CSS, all
# generated from the same mcsapi theme derisk draws itself with. Nothing in
# the image runs it since Danube, an mcsapi window itself, replaced
# Uranium, whose launcher styled Chromium's interface with it; it stays a
# package for apps outside the image.
#
# The same mcsapi commit derisk's Cargo.lock pins, so both read a theme the
# same way; move the two together.
{
  lib,
  rustPlatform,
  libxkbcommon,
  # mcsapi's source, at the commit flake.lock pins (sources.nix).
  source,
}:

rustPlatform.buildRustPackage {
  pname = "x2mcsapi";
  version = "0.1.0-unstable-2026-10-22";

  # cleanSourceWith gives the source its name for the build tree. The fetched
  # source is already clean — a fetchGit tree has no .git — so this only
  # names it.
  src = lib.cleanSourceWith {
    name = "mcsapi-source";
    src = source;
  };

  cargoHash = "sha256-PdxnfWdAGSgN1XXeQ76iJ3wdut3KWIv+1f7cmoxjB/c=";

  cargoBuildFlags = [
    "-p"
    "x2mcsapi"
  ];
  # The mcsapi crate it builds on links xkbcommon for keyboard handling.
  buildInputs = [ libxkbcommon ];

  cargoTestFlags = [
    "-p"
    "x2mcsapi"
  ];

  meta = {
    description = "Styles GTK, Qt and web apps to match an mcsapi desktop such as derisk";
    homepage = "https://github.com/dasmatus/mcsapi";
    license = lib.licenses.gpl3Only;
    platforms = lib.platforms.linux;
    mainProgram = "x2mcsapi";
  };
}
