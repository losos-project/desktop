# The Slovak eID klient in a machine (docs/tokens.md, "The Slovak eID").
# Its licence lets nobody pass it on, so the image does not carry it: the
# first start downloads it from the Ministry of the Interior into the
# user's data, and each later start fetches it again only when the server
# has another one. Without the network, the copy here runs.

url=https://eidas.minv.sk/download/files/linux/eID_klient_x86_64.tar.gz
dir=${XDG_DATA_HOME:-$HOME/.local/share}/losos-eid
mkdir -p "$dir"
cd "$dir"

# The server says neither when the file changed nor a tag for it, only
# its size, so a copy of another size is another version. Asking costs a
# request, and the 60 MB come down only when they changed.
size=$(curl --fail --silent --head --location --proto '=https' --tlsv1.2 "$url" |
  awk 'tolower($1) == "content-length:" { size = $2 + 0 } END { print size + 0 }') || size=0
have=$(stat -c %s eID_klient.tar.gz 2>/dev/null || echo 0)
if [ "$size" = 0 ]; then
  echo "losos-eid: cannot reach the eID klient's download; starting the copy here, if there is one" >&2
elif [ "$size" != "$have" ]; then
  if curl --fail --silent --show-error --location --proto '=https' --tlsv1.2 \
    --output eID_klient.tar.gz.part "$url"; then
    mv eID_klient.tar.gz.part eID_klient.tar.gz
    rm -rf app
  else
    echo "losos-eid: cannot download the eID klient; starting the copy here, if there is one" >&2
  fi
fi
rm -f eID_klient.tar.gz.part

if [ ! -x app/AppRun ]; then
  if [ ! -f eID_klient.tar.gz ]; then
    echo "losos-eid: there is no eID klient here yet, and it cannot be downloaded" >&2
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
  mv "$work/app" app
  # exec below would leave the trap unrun.
  rm -rf "$work"
  trap - EXIT
fi

# AppRun sets the library path to the libraries the eID klient carries and
# starts it on X11, the only platform it is built for. The rest of what it
# links comes from losos-eid-env (eid.nix).
exec losos-eid-env "$dir/app/AppRun" "$@"
