# Short names for the commands CI runs. Each recipe is one nix command; running
# nix directly does the same thing.
set shell := ["bash", "-euo", "pipefail", "-c"]

default:
  @just --list

# The disk image, or any other flake output (`just build installer`).
build target="image":
  nix build --impure -L ".#{{target}}"

# Evaluates both architectures, runs losos-security's tests, and on a builder
# with KVM boots the image in a VM.
check:
  nix flake check --impure -L

fmt:
  nix fmt

# Boot the image in QEMU with UEFI firmware.
vm:
  nix run .#vm --impure

# pm's plugins as WebAssembly components, for a pm installed outside the image:
# the package the image carries (nixos/pkgs/pm-plugins.nix), so pm's own are
# built from the pm input's pin in flake.lock rather than a checkout that can
# drift. plugins/dist/share/pm/plugins holds them.
plugins:
  nix build -L .#pm-plugins -o plugins/dist
