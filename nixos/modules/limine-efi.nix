# Limine's EFI program for this architecture, with an .sbat section.
#
# Limine 12 ships no .sbat section. Shim 15.3 and later refuses a second
# stage without one, so the section is added here, after the image's own
# sections (limine-sbat.py). The signature, when there is a key, is over
# this file. limine.losos is LosOS's own generation, the way a
# distributor's line is, so a later revocation can name this build without
# naming every Limine.
#
# Called by limine.nix for the installed system and by the installer's
# iso.nix for the medium. Both sign it with the same key.
{
  pkgs,
  efiArch,
}:

let
  limineEfi = "${pkgs.limine}/share/limine/BOOT${pkgs.lib.toUpper efiArch}.EFI";
  sbatCsv = pkgs.writeText "limine.sbat.csv" ''
    sbat,1,SBAT Version,sbat,1,https://github.com/rhboot/shim/blob/main/SBAT.md
    limine,1,Limine,limine,${pkgs.limine.version},https://github.com/limine-bootloader/limine
    limine.losos,1,LosOS Desktop,limine,${pkgs.limine.version},https://github.com/losos-project/desktop
  '';
in
pkgs.runCommand "limine-sbat-${efiArch}.efi"
  {
    nativeBuildInputs = [
      pkgs.buildPackages.binutils
      pkgs.buildPackages.python3
    ];
  }
  ''
    objcopy \
      --add-section .sbat=${sbatCsv} \
      --set-section-flags .sbat=contents,alloc,load,readonly,data \
      ${limineEfi} $out
    python3 ${./limine-sbat.py} $out
    grep -aq 'sbat,1,SBAT Version' $out
    grep -aq 'limine.losos,' $out
  ''
