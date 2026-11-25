# The installer's X11 fallback, for virtual machines whose video device has no
# usable OpenGL/GBM stack. derisk normally runs its compositor on the bare
# seat, through DRM/KMS and GBM. Virt-manager's default QXL device, and
# virtio-vga with 3D acceleration off, cannot give Mesa a drawing context
# there, so the screen stays black instead of showing the installer.
#
# The fallback starts Xorg on the machine's video device and runs the same
# derisk installer as a winit client on it. Winit's renderer uses Mesa's
# software driver over shared memory; the pages, input and backend are the
# same, and the window is sized to the display instead of the compositor's
# 1280x800 nested default.
{
  lib,
  pkgs,
}:

let
  xorg = pkgs.xorg-server;
  xrandr = pkgs.xrandr;
in
pkgs.writeShellApplication {
  name = "losos-installer-x11";
  runtimeInputs = [
    pkgs.coreutils
    pkgs.gawk
    xorg
    xrandr
  ];
  text = ''
    set -euo pipefail

    derisk=$1
    driver=$2
    shift 2

    display=:0
    session=/run/losos-installer
    mkdir -p "$session"
    rm -f /tmp/.X0-lock /tmp/.X11-unix/X0

    cat > "$session/xorg.conf" <<EOF
    Section "Files"
      ModulePath "${xorg}/lib/xorg/modules"
      ModulePath "${pkgs.xf86-video-qxl}/lib/xorg/modules"
      ModulePath "${pkgs.xf86-video-fbdev}/lib/xorg/modules"
      ModulePath "${pkgs.xf86-input-libinput}/lib/xorg/modules"
    EndSection

    Section "ServerFlags"
      Option "AllowMouseOpenFail" "true"
      Option "DontZap" "true"
    EndSection

    Section "InputClass"
      Identifier "libinput"
      Driver "libinput"
    EndSection

    Section "Device"
      Identifier "Display"
      Driver "$driver"
    EndSection

    Section "Monitor"
      Identifier "Monitor"
    EndSection

    Section "Screen"
      Identifier "Screen"
      Device "Display"
      Monitor "Monitor"
    EndSection

    Section "ServerLayout"
      Identifier "Layout"
      Screen "Screen"
    EndSection
    EOF

    export DISPLAY="$display"
    export XDG_SESSION_TYPE=x11
    export MCSAPI_BACKEND=winit
    unset WAYLAND_DISPLAY
    # QXL and virtio-vga without virgl have no GPU Mesa can use. Drawing in
    # software over X shared memory works; asking Mesa to use the render
    # node instead fails to allocate the dumb buffers it needs.
    export LIBGL_ALWAYS_SOFTWARE=1
    export MESA_LOADER_DRIVER_OVERRIDE=llvmpipe
    export EGL_PLATFORM=x11

    "${lib.getExe' xorg "Xorg"}" -config "$session/xorg.conf" \
      -logfile "$session/Xorg.log" -noreset -nolisten tcp "$display" &
    xorg_pid=$!

    # Xorg's first mode comes from the virtual device's EDID. Wait for xrandr
    # to answer before asking it for that mode.
    for _ in $(seq 1 100); do
      if "${lib.getExe' xrandr "xrandr"}" --current >/dev/null 2>&1; then
        break
      fi
      if ! kill -0 "$xorg_pid" 2>/dev/null; then
        cat "$session/Xorg.log" >&2 || true
        exit 1
      fi
      sleep 0.1
    done

    size=$(
      "${lib.getExe' xrandr "xrandr"}" --current 2>/dev/null |
        ${lib.getExe' pkgs.gawk "awk"} '/ connected/ && /\*/ { split($4, mode, "+"); print mode[1]; exit }'
    )
    # A device with no EDID still gets a usable mode from the X driver; keep
    # derisk's nested default if xrandr cannot name one.
    size=''${size:-1280x800}

    exec "$derisk" installer --size "$size" -- "$@"
  '';
}
