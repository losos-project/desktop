# shim and MokManager as Fedora ships them, signed by Microsoft's UEFI CA,
# which is what lets a PC with Secure Boot on and only the keys it came with
# start LosOS (docs/secure-boot.md). shim starts GRUB once GRUB's signature
# checks against a certificate in the machine's MOK list, and MokManager is
# where a person puts LosOS's certificate on that list.
#
# Fedora's rather than a build of nixpkgs' shim-unsigned: only Microsoft can
# make a shim the firmware trusts, and it signs a distribution's shim after
# that distribution's build has passed rhboot/shim-review. LosOS has not
# been through that review (docs/not-done.md), so it uses a shim that has.
# The x86_64 one is signed by both Microsoft UEFI CAs, 2011's and 2023's;
# the arm64 one by 2023's alone, which PCs sold before it may not trust.
#
# Pinned by hash from Fedora's build system, which keeps every build it has
# made, and only the two EFI programs are kept.
{
  lib,
  stdenvNoCC,
  fetchurl,
  rpmextract,
}:

let
  # Fedora's architecture names, and shim's for the EFI programs.
  arch =
    {
      x86_64 = {
        rpm = "x64";
        machine = "x86_64";
        efi = "x64";
        hash = "sha256-UMqK0PCJOptp0leyMsw4/lctfyyBS8muxN9eSSgAzcc=";
      };
      aarch64 = {
        rpm = "aa64";
        machine = "aarch64";
        efi = "aa64";
        hash = "sha256-AexSjsTwobJS6dP8N0ph6f8+ohmWwZPLGyR9IbQxhrU=";
      };
    }
    .${stdenvNoCC.hostPlatform.parsed.cpu.name};
in
stdenvNoCC.mkDerivation (finalAttrs: {
  pname = "shim-signed";
  version = "16.1-10";

  src = fetchurl {
    url = "https://kojipkgs.fedoraproject.org/packages/shim/16.1/10/${arch.machine}/shim-${arch.rpm}-${finalAttrs.version}.${arch.machine}.rpm";
    inherit (arch) hash;
  };

  nativeBuildInputs = [ rpmextract ];

  unpackPhase = ''
    rpmextract $src
  '';

  installPhase = ''
    dir=usr/lib/efi/shim/${finalAttrs.version}/EFI/fedora
    install -D -m 0644 $dir/shim${arch.efi}.efi $out/shim${arch.efi}.efi
    install -D -m 0644 $dir/mm${arch.efi}.efi $out/mm${arch.efi}.efi
  '';

  # Signed EFI programs: changing a byte breaks the signature.
  dontFixup = true;

  passthru = { inherit (arch) efi; };

  meta = {
    description = "Fedora's Microsoft-signed shim and MokManager";
    homepage = "https://github.com/rhboot/shim";
    license = lib.licenses.bsd2;
    platforms = [
      "x86_64-linux"
      "aarch64-linux"
    ];
    sourceProvenance = [ lib.sourceTypes.binaryNativeCode ];
  };
})
