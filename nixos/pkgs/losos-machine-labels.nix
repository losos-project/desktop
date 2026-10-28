# Keeps the machines on libvirt's default network apart by their colour
# label: libvirt runs it as a hook when a machine starts or stops, and it
# puts the machine's ports into an nftables table that filters on the
# bridge (modules/machine-labels.nix, docs/hypervisor.md, "Labels on the
# network").
#
# Built from src/losos-machine-labels, pinned by its Cargo.lock.
{
  lib,
  rustPlatform,
  makeWrapper,
  nftables,
  iproute2,
  libvirt,
}:

rustPlatform.buildRustPackage {
  pname = "losos-machine-labels";
  version = "0.1.0";

  src = lib.fileset.toSource {
    root = ../../src/losos-machine-labels;
    # A local `cargo build` leaves target/ beside the sources; it is
    # gitignored, but a path source would still copy all of it into the store.
    fileset = lib.fileset.difference ../../src/losos-machine-labels (
      lib.fileset.maybeMissing ../../src/losos-machine-labels/target
    );
  };

  cargoLock.lockFile = ../../src/losos-machine-labels/Cargo.lock;

  # libvirt runs its hooks with a bare environment, so what it runs is on a
  # PATH of its own: nft for the table, ip for the bridge's addresses and
  # virsh for `sync`.
  nativeBuildInputs = [ makeWrapper ];
  postInstall = ''
    wrapProgram $out/bin/losos-machine-labels --prefix PATH : ${
      lib.makeBinPath [
        nftables
        iproute2
        libvirt
      ]
    }
  '';

  doCheck = true;

  meta = {
    description = "Keeps libvirt's machines apart on the network by their colour label";
    license = lib.licenses.agpl3Plus;
    platforms = lib.platforms.linux;
    mainProgram = "losos-machine-labels";
  };
}
