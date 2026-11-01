# Downloads and unpacks the Slovak eID klient, which nobody may pass on and
# so no image carries (losos-eid-fetch.sh, docs/tokens.md). A machine's
# `losos-eid` and the host's eID sign-in both start from it.
{
  writeShellApplication,
  curl,
  gnutar,
  gzip,
  squashfsTools,
  binutils,
  gawk,
  findutils,
  coreutils,
}:

writeShellApplication {
  name = "losos-eid-fetch";
  runtimeInputs = [
    curl
    gnutar
    gzip
    squashfsTools
    # readelf, for where the AppImage's squashfs starts.
    binutils
    gawk
    findutils
    coreutils
  ];
  text = builtins.readFile ./losos-eid-fetch.sh;
}
