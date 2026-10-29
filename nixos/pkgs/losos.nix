# losos, the terminal client for LosOS boxes: the same panes as a box's admin
# pages, over a signed and encrypted envelope, with no SSH
# (docs/losos-boxes.md). It is written in the LosOS repository
# (github.com/dasmatus/losos, admin-ui/tui), which builds the same crate as
# its `.#losos`; this is that build, from a pinned commit.
#
# fetchgit with a sparse checkout rather than a components/ submodule: the
# LosOS tree is about 140 MB, handbook pictures and the admin UI included,
# which the flake would copy on every evaluation (docs/trust.md), and the
# client needs only these directories. Its build script reads the admin UI's
# catalogue, palette, pane keywords and the salmon, and the crate links the
# envelope crate in backend/access by path, which is why those are here.
#
# The pin is losos-pin.nix, which the LosOS repository rewrites in a pull
# request here when it tags a release (.github/workflows/desktop-pin.yml
# there).
{
  lib,
  rustPlatform,
  fetchgit,
  makeWrapper,
  libsecret,
  gnupg,
}:

let
  pin = import ./losos-pin.nix;
in
rustPlatform.buildRustPackage (finalAttrs: {
  pname = "losos";
  # The release, or the release this commit follows plus its date.
  inherit (pin) version;

  src = fetchgit {
    url = "https://github.com/dasmatus/losos";
    inherit (pin) rev hash;
    sparseCheckout = [
      "admin-ui/tui"
      "admin-ui/app/src/i18n"
      "admin-ui/app/src/styles"
      "admin-ui/app/src/screens/settings"
      "admin-ui/themes/brand"
      "backend/access"
    ];
  };

  cargoRoot = "admin-ui/tui";
  buildAndTestSubdir = "admin-ui/tui";
  inherit (pin) cargoHash;

  # `losos --version` and its About pane show it.
  env.LOSOS_VERSION = pin.release;

  nativeBuildInputs = [ makeWrapper ];

  # Its suites start a mock box and edge over TLS and drive a pseudo
  # terminal; the LosOS repository's CI runs them on every change.
  doCheck = false;

  # It keeps keys and tokens in the Secret Service through libsecret's
  # secret-tool (gnome-keyring answers it, desktop.nix), and `losos key
  # import --gpg` exports the key from gpg. Suffixed, so a gpg the user
  # installed with pm comes first.
  postInstall = ''
    wrapProgram $out/bin/losos \
      --suffix PATH : ${
        lib.makeBinPath [
          libsecret
          gnupg
        ]
      }
  '';

  # The box at the same commit, for the test that boots one beside the
  # desktop (nixos/tests/losos-box.nix): its modules, the crates lososd is
  # built from, and the colours and themes LosOS Login's pages are drawn
  # with. Not the handbook, the admin pages or the screenshots, which are
  # most of the tree.
  passthru.box = fetchgit {
    url = "https://github.com/dasmatus/losos";
    inherit (pin) rev;
    sparseCheckout = [
      "modules"
      "nix"
      "backend"
      "admin-ui/themes"
      "admin-ui/app/src/styles"
    ];
    hash = pin.boxHash;
  };

  meta = {
    description = "Manage LosOS boxes and edges from a terminal, without SSH";
    homepage = "https://losos.dasmat.us/manual/terminal/";
    license = lib.licenses.agpl3Plus;
    mainProgram = "losos";
  };
})
