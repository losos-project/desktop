# derisk, mcsapi and pm, at the commits flake.lock pins for them (flake.nix,
# `derisk`, `mcsapi` and `pm`).
#
# This reads the lock rather than taking the flake's inputs through the
# overlay, because the overlay is imported standalone — by the OS
# (nixos/modules/base.nix) and by every VM test (nixos/tests/*.nix) — and
# threading three arguments through all of those would buy the store path the
# lock already names. fetchGit with a pinned revision is a pure fetch: there
# is nothing to guess, so a clone needs no flags and `nix flake update <name>`
# is the whole move.
let
  lock = builtins.fromJSON (builtins.readFile ../../flake.lock);

  # The lock node the root flake's input `name` points at, and its fetchGit.
  # A git+https input's locked.url is the plain https form, which is what
  # fetchGit takes; when the flake fetches the same input, both resolve to
  # the same store path.
  source =
    name:
    let
      locked = lock.nodes.${lock.nodes.${lock.root}.inputs.${name}}.locked;
    in
    assert locked.type or "" == "git";
    builtins.fetchGit {
      url = locked.url;
      rev = locked.rev;
    };
in
{
  derisk = source "derisk";
  mcsapi = source "mcsapi";
  pm = source "pm";
}
