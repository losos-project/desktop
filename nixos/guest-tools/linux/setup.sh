#!/bin/sh
# The LosOS Linux tools CD's setup (docs/hypervisor.md, "Other distributions"):
# what a distribution installed in a machine from its own CD needs to fit the
# host's desktop the way the LosOS guest template does. Run it once, as root,
# from the installed system:
#
#   sudo sh /run/media/$USER/LOSOS_TOOLS/setup.sh [user]
#
# It
#   - keeps virtio's drivers in the initramfs, so the system still boots its
#     virtio disk if the distribution's initramfs is ever rebuilt for other
#     hardware than it was installed on;
#   - installs and starts the QEMU guest agent, which is how the desktop
#     lists the machine's apps and opens one;
#   - installs waypipe and runs a waypipe server for `user` (the one who ran
#     sudo, unless named), whose apps' windows then go to the host's desktop
#     over vsock, framed in the machine's colour;
#   - installs losos-guest-run, which the desktop runs through the agent;
#   - installs the SPICE agent, so the machine's own screen shares the
#     desktop's clipboard while its viewer has the keyboard.
#
# Debian, Ubuntu and their derivatives (apt), Fedora and RHEL's family (dnf),
# openSUSE (zypper) and Arch (pacman). Anything else gets the files and is
# told which packages to install itself.

# $modules and $packages below are lists, split into words where they are
# used on purpose.
# shellcheck disable=SC2086
set -eu

here=$(cd "$(dirname "$0")" && pwd)
user=${1:-${SUDO_USER:-}}

if [ "$(id -u)" -ne 0 ]; then
  echo "run this as root: sudo sh $0" >&2
  exit 1
fi
if [ -z "$user" ] || ! id "$user" >/dev/null 2>&1; then
  echo "name the user whose apps the desktop shows: sudo sh $0 <user>" >&2
  exit 1
fi

# virtio's block, SCSI, network, PCI and vsock drivers, by each initramfs
# generator's own configuration, then the initramfs again.
modules="virtio_pci virtio_blk virtio_scsi virtio_net vmw_vsock_virtio_transport"
if command -v dracut >/dev/null 2>&1; then
  mkdir -p /etc/dracut.conf.d
  echo "add_drivers+=\" $modules \"" >/etc/dracut.conf.d/losos-virtio.conf
  dracut --regenerate-all --force
elif command -v update-initramfs >/dev/null 2>&1; then
  for module in $modules; do
    grep -qx "$module" /etc/initramfs-tools/modules || echo "$module" >>/etc/initramfs-tools/modules
  done
  update-initramfs -u -k all
elif command -v mkinitcpio >/dev/null 2>&1; then
  mkdir -p /etc/mkinitcpio.conf.d
  echo "MODULES+=($modules)" >/etc/mkinitcpio.conf.d/losos-virtio.conf
  mkinitcpio -P
else
  echo "no dracut, initramfs-tools or mkinitcpio here; virtio's drivers are left as the distribution has them" >&2
fi

# The guest agent, waypipe and the SPICE agent. Every one of these
# distributions packages them under these names, and its spice-vdagent
# starts with the graphical session.
packages="qemu-guest-agent waypipe spice-vdagent"
if command -v apt-get >/dev/null 2>&1; then
  apt-get update
  DEBIAN_FRONTEND=noninteractive apt-get install -y $packages
elif command -v dnf >/dev/null 2>&1; then
  dnf install -y $packages
elif command -v zypper >/dev/null 2>&1; then
  zypper --non-interactive install $packages
elif command -v pacman >/dev/null 2>&1; then
  pacman -S --needed --noconfirm $packages
else
  echo "install $packages with this distribution's package manager, then run this again" >&2
  exit 1
fi
systemctl enable --now qemu-guest-agent.service

install -m 0755 "$here/losos-guest-run" /usr/local/bin/losos-guest-run

# The waypipe server, as the LosOS guest runs it (nixos/guest/default.nix):
# a socket apps connect to as if it were a compositor, each connection a
# vsock stream to the host (address 2, port 7350), where the desktop's gate
# takes it. The socket is losos-wayland rather than wayland-0, so a desktop
# session the distribution runs on the machine's own screen keeps its own.
cat >/etc/systemd/user/losos-waypipe.service <<'EOF'
[Unit]
Description=Show this machine's apps on the host's desktop

[Service]
ExecStart=/usr/bin/waypipe --vsock --socket 2:7350 --no-gpu --display losos-wayland server -- sleep infinity
Restart=always

[Install]
WantedBy=default.target
EOF
systemctl --global enable losos-waypipe.service
# The user's manager runs from boot, without anyone signing in, which is
# where the waypipe server and the apps losos-guest-run starts run.
loginctl enable-linger "$user"
systemctl --machine="$user@.host" --user daemon-reload || true
systemctl --machine="$user@.host" --user start losos-waypipe.service || true
echo "$user" >/etc/losos-guest-user

echo "done: the host's desktop can now list and open $user's apps on this machine"
