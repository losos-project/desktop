# losos-eid-fetch DIR: the Slovak eID klient, unpacked into DIR/app
# (docs/tokens.md, "The Slovak eID"). Its licence lets nobody pass it on,
# so the image does not carry it: the first run downloads it from the
# Ministry of the Interior, and each later one fetches it again only when
# the server has another one. Without the network, the copy in DIR stays.
# A machine's `losos-eid` runs it in the person's data before it starts
# the klient, and the host's losos-eid-fetch.service in the eID machine's
# share (eid-signin.nix).

url=https://eidas.minv.sk/download/files/linux/eID_klient_x86_64.tar.gz
dir=${1:?usage: losos-eid-fetch DIR}
mkdir -p "$dir"
cd "$dir"

# The server says neither when the file changed nor a tag for it, only
# its size, so a copy of another size is another version. Asking costs a
# request, and the 60 MB come down only when they changed.
size=$(curl --fail --silent --head --location --proto '=https' --tlsv1.2 "$url" |
  awk 'tolower($1) == "content-length:" { size = $2 + 0 } END { print size + 0 }') || size=0
have=$(stat -c %s eID_klient.tar.gz 2>/dev/null || echo 0)
if [ "$size" = 0 ]; then
  echo "losos-eid-fetch: cannot reach the eID klient's download; keeping the copy here, if there is one" >&2
elif [ "$size" != "$have" ]; then
  if curl --fail --silent --show-error --location --proto '=https' --tlsv1.2 \
    --output eID_klient.tar.gz.part "$url"; then
    mv eID_klient.tar.gz.part eID_klient.tar.gz
  else
    echo "losos-eid-fetch: cannot download the eID klient; keeping the copy here, if there is one" >&2
  fi
fi
rm -f eID_klient.tar.gz.part

# Unpacked again when the download is newer than the copy. The new copy
# takes the old one's place in one rename, so the eID machine, which may be
# loading the old one at that moment, never finds half of either.
if [ ! -x app/AppRun ] || [ eID_klient.tar.gz -nt app ]; then
  if [ ! -f eID_klient.tar.gz ]; then
    echo "losos-eid-fetch: there is no eID klient here yet, and it cannot be downloaded" >&2
    exit 1
  fi
  work=$(mktemp -d "$dir/unpack.XXXXXX")
  trap 'rm -rf "$work"' EXIT
  tar -xzf eID_klient.tar.gz -C "$work"
  appimage=$(find "$work" -name 'eID_klient*.AppImage' -print -quit)
  # An AppImage is a small ELF program with a squashfs after it, which
  # starts where the ELF's section headers end. Unpacking it here runs
  # nothing from the download; its own runner would need FUSE as well.
  offset=$(readelf -h "$appimage" | awk -F: '
    /Start of section headers/ { start = $2 + 0 }
    /Size of section headers/ { size = $2 + 0 }
    /Number of section headers/ { count = $2 + 0 }
    END { print start + size * count }')
  unsquashfs -quiet -no-progress -offset "$offset" -dest "$work/app" "$appimage" >/dev/null
  touch "$work/app"
  # A file system without renameat2's exchange has a moment with neither.
  if [ ! -e app ]; then
    mv "$work/app" app
  elif ! mv -T --exchange "$work/app" app 2>/dev/null; then
    rm -rf app
    mv "$work/app" app
  fi
  rm -rf "$work"
  trap - EXIT
fi
