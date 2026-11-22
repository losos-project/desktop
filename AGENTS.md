# What this is?

A based NixOS config made as an appliance aimed to be very secure.

# Where are the files?

```bash
tree .
```

or

```bash
find .
```

whichever is better. `flake.nix` and the modules in `nixos/` are the OS,
`src/` holds this repository's own programs, `docs/` says how each piece is
built, and `website/` turns `docs/` into the documentation site.

# Where can I learn about the OS?

The docs are in `docs/`, starting at `docs/index.md`. Building is in
`docs/building.md` and the comment at the top of `flake.nix`.

# How can I start working?

Make sure you have installed Nix with flakes enabled. Run `nix develop` for
the dev shell (or let direnv load it: `.envrc` is `use flake`). Then:

```bash
nix build --impure      # the disk image (just build)
nix flake check         # both architectures, the tests, and with KVM the VM boot tests (just check)
nix run .#vm --impure   # boot the image in QEMU with UEFI firmware (just vm)
nix fmt                 # format, as CI's format check expects (just fmt)
```

`docs/building.md` has the rest, including why `--impure` is needed.

Please also read the PR template at `.github/pull_request_template.md` to learn more.

[CONTRIBUTING.md](CONTRIBUTING.md) is one line on purpose: read the docs
and/or ask your agent about this codebase before deciding to do anything.

I've automated (with the help of agents) the most annoying parts.

We use Conventional Commits in order for changes to be easily findable and sortable.

Run `nix fmt` and `nix flake check` before pushing. CI runs the same checks,
and they are not to be skipped.

Do not put anything under this line.
