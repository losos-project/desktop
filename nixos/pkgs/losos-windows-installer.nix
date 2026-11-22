# LosOS's installer for a machine that runs Windows (src/losos-windows-installer,
# docs/windows-installer.md). Called twice:
#
# - from the overlay, for Linux, with nothing compiled in: its test suite
#   runs, and `--image` installs into a disk image laid out like a Windows
#   disk, which is how the install is checked without Windows;
# - from windows-installer.nix, through pkgsCross.mingwW64, with the OS's own
#   channel, release key, layout and Limine compiled in: the .exe a
#   release carries.
#
# The Windows build links with mingw's GNU ld, not mold: mold writes only
# ELF, and every other Rust program here goes through callWithMold for that
# reason alone.
{
  lib,
  stdenv,
  rustPlatform,
  # build.rs's inputs, each a value or a file; null leaves it out.
  updateUrl ? null,
  updateIndex ? null,
  pubring ? null,
  imageId ? null,
  arch ? null,
  usrSize ? null,
  loader ? null,
}:

let
  set = name: value: lib.optionalAttrs (value != null) { ${name} = toString value; };
in
rustPlatform.buildRustPackage (
  {
    pname = "losos-windows-installer";
    version = "0.1.0";

    src = lib.cleanSourceWith {
      src = ../../src/losos-windows-installer;
      # A local `cargo build` leaves target/ beside the sources.
      filter = path: _type: baseNameOf path != "target";
    };

    cargoLock.lockFile = ../../src/losos-windows-installer/Cargo.lock;

    # The suite works on files: the release's signature with the real key and
    # manifest, the layout, a FAT filesystem and an xz image written into a
    # disk image, the firmware boot entry's bytes. A Windows build cannot run
    # it here, and the Linux build is the same code but for windows.rs.
    doCheck = stdenv.buildPlatform.canExecute stdenv.hostPlatform;

    meta = {
      description = "Installs LosOS Desktop beside Windows, on space shrunk from C:";
      license = lib.licenses.agpl3Plus;
      platforms = lib.platforms.linux ++ lib.platforms.windows;
      mainProgram = "losos-windows-installer";
    };
  }
  // set "LOSOS_UPDATE_URL" updateUrl
  // set "LOSOS_UPDATE_INDEX" updateIndex
  // set "LOSOS_PUBRING" pubring
  // set "LOSOS_IMAGE_ID" imageId
  // set "LOSOS_ARCH" arch
  // set "LOSOS_USR_SIZE" usrSize
  // set "LOSOS_LOADER" loader
)
