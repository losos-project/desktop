# Hands a service machine the PCI devices of the classes it asks for, the
# way Qubes gives sys-net the network cards: each is unbound from its
# host driver and bound to vfio-pci, so the host never drives it again
# this boot, and a <hostdev> for it is left in the machine's devices/ for
# service-machines.nix to add to its definition.
#
# Usage: losos-passthrough NAME CLASS...   (CLASS as in sysfs, 0x0280)
#
# With LOSOS_PASSTHROUGH_MANAGED=1 the devices stay on their host drivers
# for now, and the <hostdev>s say managed='yes': libvirt takes each from the
# host when the machine starts and gives it back when it stops, for a
# machine started only when it is needed (the USB machine).
#
# A device goes only when the IOMMU can fence it: its IOMMU group must hold
# nothing but it, other devices this call takes, and PCI bridges, since
# whatever shares a group shares its DMA. Without an IOMMU, or with a group
# that holds more, the device stays on the host and the caller's fallback
# applies. /run/losos-passthrough/keep lists addresses that never go,
# whichever machine asks: the peripherals' USB machine writes the
# controller with the boot disk or the only keyboard there.

name=$1
shift
classes=("$@")
out=/run/losos-service-machines/$name/devices
keep=/run/losos-passthrough/keep
managed=no
[ "${LOSOS_PASSTHROUGH_MANAGED:-0}" = 1 ] && managed=yes
mkdir -p "$out"
rm -f "$out"/pci-*.xml

wanted() {
  local class want
  class=$(cat "$1/class")
  for want in "${classes[@]}"; do
    case $class in "$want"*) return 0 ;; esac
  done
  return 1
}

kept() {
  [ -e "$keep" ] && grep -qxF "$1" "$keep"
}

if [ -z "$(ls -A /sys/kernel/iommu_groups 2>/dev/null)" ]; then
  echo "no IOMMU: $name's devices stay on the host"
  exit 0
fi
modprobe vfio-pci

taken=0
for dev in /sys/bus/pci/devices/*; do
  addr=${dev##*/}
  wanted "$dev" || continue
  if kept "$addr"; then
    echo "$addr: kept on the host"
    continue
  fi
  clean=1
  for member in "$dev"/iommu_group/devices/*; do
    other=${member##*/}
    [ "$other" = "$addr" ] && continue
    case $(cat "$member/class") in 0x0604*) continue ;; esac
    if wanted "$member" && ! kept "$other"; then
      continue
    fi
    clean=0
    echo "$addr: shares its IOMMU group with $other; it stays on the host"
  done
  [ "$clean" = 1 ] || continue

  if [ "$managed" = no ]; then
    echo vfio-pci > "$dev/driver_override"
    if [ -e "$dev/driver" ] && [ "$(basename "$(readlink "$dev/driver")")" != vfio-pci ]; then
      echo "$addr" > "$dev/driver/unbind"
    fi
    [ -e "$dev/driver" ] || echo "$addr" > /sys/bus/pci/drivers_probe
  fi

  IFS=:. read -r domain bus slot function <<< "$addr"
  cat > "$out/pci-$addr.xml" <<XML
<hostdev mode='subsystem' type='pci' managed='$managed'>
  <source>
    <address domain='0x$domain' bus='0x$bus' slot='0x$slot' function='0x$function'/>
  </source>
</hostdev>
XML
  echo "$addr: $(cat "$dev/vendor"):$(cat "$dev/device") goes to $name"
  taken=$((taken + 1))
done
echo "$taken device(s) for $name"
