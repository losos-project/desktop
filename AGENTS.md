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

whichever is better.

# Where can I learn about the OS?

There is a handbook at `handbook/` where you can learn more about the OS.

# How can I start working?

Make sure you have installed Nix and `devenv`. After you've installed it, run `devenv shell` or `devenv test` depending on situation

Please also read the PR template at `.github/pull_request_template.md` to learn more.

[CONTRIBUTING.md](CONTRIBUTING.md) is the long version of this: getting a
shell, the gates and what each one catches, and the starter tasks.

I've automated (with the help of agents) the most annoying parts.

We use Conventional Commits in order for changes to be easily findable and sortable.

There are also some pre-push changes THAT YOU SHOULD NEVER EVER SKIP UNDER ANY CIRCUMSTANCES.

# Source policy

Only projects in the `losos-project` GitHub namespace are built from source:
this repository, and derisk, mcsapi and pm from `flake.lock`. Everything else
comes from nixpkgs, which substitutes from cache.nixos.org, or as a
distributor's prebuilt package; behaviour that would otherwise need a patch on
someone else's code is set at run time (sysctl, kernel cmdline, env, config).
`tools/source-policy` enforces this and runs in `nix flake check`; read
`docs/trust.md`, "What may be built from source", before adding a fetch.

# Bend

Use Bend primarily for validation alongside existing scripts. Set it up using `curl -fsSL https://bend-lang.com/install.sh | sh` and then adding the `bend` binary to `$PATH`.

- run `bend guide` to learn it
- use `LAWS.bend` to keep important rules
- run `bend PROOF.bend` before committing
- parallelize the code whenever possible, with hard bans on build times since we don't have enough RAM right now (optimize within code and find redundant/compile-time-prolonging stuff that should be removed).

Do not put anything under this line.
