# Rewrite limine.conf from the UKIs on a disk.
#
#   losos-limine-menu --esp /efi
#   losos-limine-menu --disk /dev/vda
#
# Limine has no script and no glob, so the menu is this file. The newest UKI
# with tries left is first. A BIOS entry starts the kernel and initrd beside
# the UKI; a UEFI entry chainloads the UKI. With LOSOS_BIOS=1, a disk that
# has a BIOS boot partition also gets Limine's boot code.
set -eu

id=${LOSOS_ID:?}
name=${LOSOS_NAME:?}
bios=${LOSOS_BIOS:-0}
limine=${LOSOS_LIMINE:?}
wallpaper_src=${LOSOS_WALLPAPER:?}

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
: > "$work/ukilist"

# version, tries left (empty when uncounted), file name, partition uuid, on-esp
add_uki() {
  local base=$1 uuid=$2 on_esp=$3
  local version left
  if [[ $base =~ ^${id}_([^/+_]+)(\+([0-9]+)(-[0-9]+)?)?\.efi$ ]]; then
    version=${BASH_REMATCH[1]}
    left=${BASH_REMATCH[3]}
  else
    return 0
  fi
  # '|' rather than a tab: bash read collapses empty IFS-whitespace fields,
  # and an uncounted UKI has an empty try count. A collapsed field shifts
  # the partition UUID into the file name and the entry does not load.
  printf '%s|%s|%s|%s|%s\n' "$version" "$left" "$base" "$uuid" "$on_esp" >> "$work/ukilist"
}

scan_tree() {
  local dir=$1 uuid=$2 on_esp=$3
  local file base version
  [ -d "$dir" ] || return 0
  for file in "$dir/${id}_"*.efi; do
    [ -f "$file" ] || continue
    base=$(basename "$file")
    [[ $base =~ ^${id}_([^/+_]+)(\+([0-9]+)(-[0-9]+)?)?\.efi$ ]] || continue
    version=${BASH_REMATCH[1]}
    add_uki "$base" "$uuid" "$on_esp"
    if [ -f "$dir/${id}_$version.cmdline" ]; then
      cp "$dir/${id}_$version.cmdline" "$work/${id}_$version.cmdline"
    fi
    if [ -f "$dir/${id}_$version.linux" ]; then
      touch "$work/${id}_$version.linux"
    fi
  done
}

copy_out() {
  local disk=$1 offset=$2 dest=$3
  export MTOOLS_SKIP_CHECK=1
  mkdir -p "$dest"
  mcopy -n -i "$disk@@$offset" -s ::/EFI "$dest/" 2>/dev/null || true
}

pull_disk() {
  local disk=$1
  sfdisk --json "$disk" > "$work/table.json"
  jq -r '
    .partitiontable as $t
    | $t.partitions[]
    | select((.type | ascii_downcase) == "c12a7328-f81f-11d2-ba4b-00a0c93ec93b"
          or (.type | ascii_downcase) == "bc13c2ff-59e6-4262-a352-b275fd6f7172")
    | "\(.start * $t.sectorsize)\t\(.uuid | ascii_downcase)\t\(.type | ascii_downcase)"
  ' "$work/table.json" > "$work/parts"
  local offset uuid type dest on_esp
  while IFS=$'\t' read -r offset uuid type; do
    [ -n "$offset" ] || continue
    dest=$work/part-$uuid
    copy_out "$disk" "$offset" "$dest"
    on_esp=0
    if [ "$type" = c12a7328-f81f-11d2-ba4b-00a0c93ec93b ]; then
      on_esp=1
      printf '%s\n' "$offset" > "$work/esp-offset"
      printf '%s\n' "$uuid" > "$work/esp-uuid"
    fi
    if [ -d "$dest/EFI/Linux" ]; then
      scan_tree "$dest/EFI/Linux" "$uuid" "$on_esp"
    fi
    if [ -f "$dest/EFI/Microsoft/Boot/bootmgfw.efi" ]; then
      printf 1 > "$work/windows"
    fi
  done < "$work/parts"
}

pull_esp() {
  local esp=$1 boot buuid
  local uuid
  uuid=$(findmnt --noheadings --output PARTUUID --target "$esp" | tr A-F a-f)
  printf '%s\n' "$uuid" > "$work/esp-uuid"
  printf '%s\n' "$esp" > "$work/esp-mount"
  scan_tree "$esp/EFI/Linux" "$uuid" 1
  boot=$(bootctl --print-boot-path 2>/dev/null || printf '%s' "$esp")
  if [ "$boot" != "$esp" ] && [ -d "$boot/EFI/Linux" ]; then
    buuid=$(findmnt --noheadings --output PARTUUID --target "$boot" | tr A-F a-f)
    scan_tree "$boot/EFI/Linux" "$buuid" 0
  fi
}

# image.nix and the installer pass the disk alone.
if [ $# -eq 1 ] && [ "$1" != --esp ] && [ "$1" != --disk ]; then
  set -- --disk "$1"
fi
mode=${1:?--esp or --disk}
target=${2:?a path}
case $mode in
  --esp) pull_esp "$target" ;;
  --disk) pull_disk "$target" ;;
  *) echo "usage: losos-limine-menu --esp DIR | --disk DEVICE" >&2; exit 2 ;;
esac

esp_uuid=$(cat "$work/esp-uuid" 2>/dev/null || true)
[ -n "$esp_uuid" ] || { echo "losos-limine-menu: no ESP on $target" >&2; exit 1; }

# Spent (tries left 0) after everything else. Newest version first in each.
awk -F '|' '
  {
    spent = ($2 == "0")
    print spent "|" $0
  }
' "$work/ukilist" | sort -t '|' -k1,1n -k2,2Vr > "$work/sorted"

wallpaper_hash=
if [ -f "$wallpaper_src" ] && command -v b2sum >/dev/null; then
  wallpaper_hash=$(b2sum "$wallpaper_src" | awk '{print $1}')
fi

{
  count=0
  while IFS='|' read -r spent version left file uuid on_esp; do
    [ -n "$file" ] || continue
    [ -n "$spent" ] || continue
    count=$((count + 1))
    if [ "$left" = 0 ]; then
      title="$name $version (failed to start)"
    else
      title="$name $version"
    fi
    # The visible name is the comment people read. LoaderEntrySelected is
    # derived from it and is not the UKI's file name; trial and bless find
    # the file by this system's version instead.
    if [ "$on_esp" = 1 ]; then
      path="boot():/EFI/Linux/$file"
    else
      path="guid($uuid):/EFI/Linux/$file"
    fi
    printf '/%s\n' "$title"
    printf '    comment: %s\n' "$title"
    printf '    protocol: efi\n'
    printf '    path: %s\n' "$path"
    printf '    if_fw_type: UEFI\n'
    printf '\n'

    # One line, the UKI's command line with root already named for a BIOS
    # boot (uki-files.nix). losos.esp is this disk's ESP, which the file
    # cannot know.
    cmdfile=$work/${id}_$version.cmdline
    if [ "$bios" = 1 ] && [ -f "$cmdfile" ] && [ -f "$work/${id}_$version.linux" ]; then
      cmdline=$(head -n 1 "$cmdfile")
      cmdline="$cmdline losos.esp=$esp_uuid losos.uki=$file"
      if [ "$on_esp" = 1 ]; then
        kpath="boot():/EFI/Linux/${id}_$version.linux"
        ipath="boot():/EFI/Linux/${id}_$version.initrd"
      else
        kpath="guid($uuid):/EFI/Linux/${id}_$version.linux"
        ipath="guid($uuid):/EFI/Linux/${id}_$version.initrd"
      fi
      printf '/%s\n' "$title"
      printf '    comment: %s\n' "$title"
      printf '    protocol: linux\n'
      printf '    path: %s\n' "$kpath"
      printf '    module_path: %s\n' "$ipath"
      printf '    cmdline: %s\n' "$cmdline"
      printf '    if_fw_type: BIOS\n'
      printf '\n'
    fi
  done < "$work/sorted"
  printf '%s\n' "$count" > "$work/count"
} > "$work/entries"

# Windows Boot Manager, when it shares the ESP.
if [ -f "$work/windows" ] || [ -f "$(cat "$work/esp-mount" 2>/dev/null || echo /none)/EFI/Microsoft/Boot/bootmgfw.efi" ]; then
  cat >> "$work/entries" <<'EOF'
/Windows
    comment: Windows
    protocol: efi
    path: boot():/EFI/Microsoft/Boot/bootmgfw.efi
    if_fw_type: UEFI

EOF
fi

# serial: yes makes Limine's UEFI build skip the graphical terminal, so a
# machine with a serial port (QEMU's OVMF among them) would draw text and
# no salmon. UEFI reads the config beside its EFI program and ignores
# /limine.conf. BIOS does not look there; it reads /limine.conf, where
# serial stays on and the graphical menu still starts.
header() {
  local serial=$1
  printf 'timeout: 3\n'
  if [ "$serial" = 1 ]; then
    printf 'serial: yes\n'
    printf 'serial_baudrate: 115200\n'
  fi
  printf 'editor_enabled: yes\n'
  if [ -n "$wallpaper_hash" ]; then
    printf '# wallpaper %s\n' "$wallpaper_hash"
  fi
  printf 'wallpaper: boot():/limine/salmon.png\n'
  printf 'wallpaper_style: centered\n'
  printf 'backdrop: 0a0e12\n'
  printf 'term_background: 800a0e12\n'
  printf 'term_foreground: e2e8ee\n'
  printf 'term_background_bright: 48b3c0\n'
  printf 'term_foreground_bright: 0a0e12\n'
  printf 'interface_branding: %s\n' "$name"
  printf 'interface_branding_colour: 48b3c0\n'
  printf 'interface_help_colour: 94a2b0\n'
  printf 'interface_help_colour_bright: 48b3c0\n'
  printf '\n'
  cat "$work/entries"
}
header 1 > "$work/limine-bios.conf"
header 0 > "$work/limine-uefi.conf"

install_wallpaper() {
  local dest=$1
  mkdir -p "$dest/limine" "$dest/EFI/BOOT" "$dest/EFI/losos"
  cp "$wallpaper_src" "$dest/limine/salmon.png"
  cp "$work/limine-bios.conf" "$dest/limine.conf"
  cp "$work/limine-uefi.conf" "$dest/EFI/BOOT/limine.conf"
  cp "$work/limine-uefi.conf" "$dest/EFI/losos/limine.conf"
}

if [ "$mode" = --esp ]; then
  esp=$(cat "$work/esp-mount")
  install_wallpaper "$esp"
else
  offset=$(cat "$work/esp-offset")
  export MTOOLS_SKIP_CHECK=1
  for dir in ::/limine ::/EFI ::/EFI/BOOT ::/EFI/losos; do
    mmd -i "$target@@$offset" "$dir" 2>/dev/null || true
  done
  mcopy -o -i "$target@@$offset" "$wallpaper_src" ::/limine/salmon.png
  mcopy -o -i "$target@@$offset" "$work/limine-bios.conf" ::/limine.conf
  mcopy -o -i "$target@@$offset" "$work/limine-uefi.conf" ::/EFI/BOOT/limine.conf
  mcopy -o -i "$target@@$offset" "$work/limine-uefi.conf" ::/EFI/losos/limine.conf
  if [ "$bios" = 1 ] && [ -f "$limine/share/limine/limine-bios.sys" ]; then
    mcopy -o -i "$target@@$offset" "$limine/share/limine/limine-bios.sys" ::/limine-bios.sys
    idx=$(jq -r '
      .partitiontable.partitions
      | to_entries[]
      | select(.value.type | ascii_downcase == "21686148-6449-6e6f-744e-656564454649")
      | .key + 1
    ' "$work/table.json" | head -n 1)
    if [ -n "$idx" ]; then
      "$limine/bin/limine" bios-install "$target" "$idx"
    fi
  fi
fi
