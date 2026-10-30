# What Secure Boot needs on an ESP (docs/secure-boot.md): Fedora's
# Microsoft-signed shim at the path firmware starts, GRUB signed with
# LosOS's key beside it, MokManager, and LosOS's certificate for MokManager
# to enroll. Plain data, called by secure-boot.nix for the image and by the
# installer's iso.nix for its medium.
#
#   firmware (Microsoft's keys) -> shim -> GRUB (LosOS's key, in MokList)
#     -> the UKI (LosOS's key), which GRUB loads through shim's LoadImage
#
# shim 16 replaces the firmware's LoadImage with its own, which also trusts
# the MOK list, and GRUB 2.14 loads through it, so GRUB's chainloader
# starts a UKI the firmware alone would refuse. systemd-stub then starts the
# kernel inside the UKI, which shim accepts as part of an image it already
# checked.
#
# Without a private key this is the layout from before: GRUB alone at the
# removable-media path, nothing signed.
{
  pkgs,
  # A path the build can read the key at (options.nix), or null.
  privateKey,
  # Its certificate, in PEM.
  certificate,
}:

let
  inherit (pkgs) lib;
  efi = pkgs.stdenv.hostPlatform.efiArch;
  EFI = lib.toUpper efi;
  shim = pkgs.shim-signed;
in
rec {
  # secure-boot.nix asserts the same of the image; the installer's medium is
  # a system of its own and would otherwise fail on a null certificate.
  enable =
    privateKey != null
    && (
      certificate != null
      || throw "losos.secureBoot.privateKey is set, but there is no certificate to go with it (nixos/keys/secure-boot.crt, docs/secure-boot.md)."
    );

  # Signs an EFI program with systemd's signing tool. A key outside the
  # store is read with no sandbox around this one step (__noChroot), so the
  # builder has to allow that (sandbox = relaxed) and every other build
  # still runs without sight of it: a key in extra-sandbox-paths would be
  # in every build's sandbox, fetchers with network access included. Only
  # the signature reaches $out. A key that is not the certificate's signs
  # fine and fails at boot, so it is checked here instead.
  sign =
    name: file:
    pkgs.runCommand name
      (
        {
          nativeBuildInputs = [ pkgs.buildPackages.openssl ];
        }
        // lib.optionalAttrs (!lib.hasPrefix builtins.storeDir privateKey) {
          __noChroot = true;
        }
      )
      ''
        key=${privateKey}
        if [ ! -r "$key" ]; then
          echo "The Secure Boot key $key cannot be read by the build user." >&2
          echo "It has to be readable by the nixbld group, with sandbox = relaxed (docs/secure-boot.md)." >&2
          exit 1
        fi
        if [ "$(openssl pkey -in "$key" -pubout)" != "$(openssl x509 -in ${certificate} -noout -pubkey)" ]; then
          echo "The Secure Boot key $key is not the key of ${certificate}." >&2
          exit 1
        fi
        ${pkgs.buildPackages.systemdUkify}/lib/systemd/systemd-sbsign sign \
          --private-key="$key" --certificate=${certificate} --output=$out ${file}
      '';

  # MokManager's "Enroll key from disk" reads DER, and lists the ESP's top
  # directory first, so the file says what it is for where a person looks.
  certificateDer = pkgs.runCommand "losos-secure-boot.cer" {
    nativeBuildInputs = [ pkgs.buildPackages.openssl ];
  } "openssl x509 -in ${certificate} -outform der -out $out";
  enrolPath = "/ENROLL_THIS_KEY_IN_MOKMANAGER.cer";

  shimFile = "${shim}/shim${efi}.efi";
  mokManagerFile = "${shim}/mm${efi}.efi";

  # The files that start GRUB from one directory of an ESP under the name
  # firmware starts (BOOT<ARCH>.EFI, or shim<arch>.efi where a boot entry
  # names it), keyed by path. shim looks for grub<arch>.efi and
  # mm<arch>.efi beside itself.
  starting =
    dir: name: grub:
    if enable then
      {
        "${dir}/${name}" = shimFile;
        "${dir}/grub${efi}.efi" = grub;
        "${dir}/mm${efi}.efi" = mokManagerFile;
      }
    else
      { "${dir}/${name}" = grub; };

  # The removable-media path, which firmware starts with no boot entry, and
  # the certificate.
  removable =
    grub:
    starting "/EFI/BOOT" "BOOT${EFI}.EFI" grub
    // lib.optionalAttrs enable { ${enrolPath} = certificateDer; };
}
