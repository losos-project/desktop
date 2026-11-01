# NVIDIA's driver, for the machines whose card it supports.
#
# The image is one build for every PC, so this does what NixOS's own
# hardware.nvidia module does, minus the two things that module assumes about
# the machine: it blacklists nouveau and loads nvidia_uvm at every boot, which
# would leave a machine with an older NVIDIA card without a driver and fail
# systemd-modules-load on every machine with none. Here both drivers are in
# the image, and the rule at the bottom decides at boot from nixos-facter's
# report: a card the driver supports goes to NVIDIA's open kernel module, and
# everywhere else NVIDIA's modules are kept off and nouveau keeps every card
# (hardware.nix, docs/drivers.md).
#
# The open module, not the closed one: NVIDIA's current branch supports only
# Turing (GTX 16xx, RTX 20xx) and newer, all of which the open module drives,
# and it is the module NVIDIA develops. Maxwell, Pascal and Volta cards are in
# NVIDIA's 580 legacy branch, so they stay on nouveau, with Mesa's NVK for
# Vulkan.
{
  config,
  lib,
  pkgs,
  ...
}:

let
  base = config.boot.kernelPackages.nvidiaPackages.production;

  vramDir = "/run/nvidia-suspend";

  # The userspace half, less what nothing in a desktop session loads. Every
  # PC downloads this with each update, so the parts for compute and for
  # other display servers come out: OpenCL and its ICD (an app that wants it
  # ships it in its Flatpak runtime, as Flathub's NVIDIA GL extension does),
  # OptiX, the CUDA debugger, Vulkan SC (the safety-critical variant, which
  # no ICD file even names), the DLSS DLLs for Wine, and the X server's GLX
  # module (Xorg is not in the image). That is about 210 MB of 784. GL, EGL,
  # GLES, Vulkan with ray tracing, GBM, CUDA and NVDEC/NVENC stay: VA-API on
  # NVIDIA decodes through CUDA. Nothing kept links against what is removed
  # (readelf -d over every library).
  nvidia = base.overrideAttrs (old: {
    postFixup = (old.postFixup or "") + ''
      rm -f $out/lib/libnvidia-opencl.so* $out/etc/OpenCL/vendors/nvidia.icd
      rm -f $out/lib/libnvoptix.so* $out/lib/nvoptix.bin
      rm -f $out/lib/libcudadebugger.so*
      rm -f $out/lib/libnvidia-vksc-core.so*
      rm -rf $out/lib/nvidia/wine
      rm -f $out/lib/libglxserver_nvidia.so* $out/lib/xorg/modules/extensions/libglxserver_nvidia.so*
    '';
  });

  # The device IDs of every GPU the driver supports, from the table NVIDIA
  # ships inside the installer. Its current branch lists only Turing and
  # newer; the older chips it still names carry a "legacybranch" and are
  # left out. Reading NVIDIA's table rather than a range of IDs means a card
  # newer than this driver, which nouveau might drive and this driver
  # cannot, is not handed to it.
  supportedGpus =
    pkgs.runCommand "nvidia-${base.version}-supported-gpus.json"
      {
        nativeBuildInputs = [
          pkgs.jq
          pkgs.libarchive
          pkgs.zstd
        ];
      }
      ''
        # The installer extracts itself with a zstd binary it carries, which
        # cannot run in the build sandbox; the archive starts at the line its
        # header names, as nixpkgs' own unpacking of it does.
        skip=$(sed 's/^skip=//; t; d' ${base.src})
        tail -n +$skip ${base.src} | bsdtar -xf - supported-gpus/supported-gpus.json
        jq '[.chips[] | select(.legacybranch == null) | .devid] | unique' \
          supported-gpus/supported-gpus.json > $out
        # An empty list would quietly leave every card on nouveau.
        test "$(jq length $out)" -gt 0
      '';

  # The EGL external platforms: how NVIDIA's EGL reaches Wayland (egl-wayland
  # and its successor egl-wayland2, which does explicit sync) and GBM, which
  # derisk's compositor scans out through. Not egl-x11: derisk has no
  # XWayland.
  eglPlatforms = pkgs.symlinkJoin {
    name = "nvidia-egl-external-platforms";
    paths = with pkgs; [
      egl-wayland
      egl-wayland2
      egl-gbm
    ];
  };
in
{
  # The kernel refuses a module without a signature it can check
  # (kernel-hardening.nix). nixpkgs' nvidia-open signs its modules as it
  # builds them, each with a certificate of its own, and the kernel image
  # this OS boots carries every installed module's certificate, so NVIDIA's
  # loads like the kernel's own.
  imports = [ ./kernel-hardening.nix ];

  # The only unfree package in the image. NVIDIA's licence lets it be
  # redistributed unmodified, which is what the image and the binary cache do
  # (docs/trust.md); the open kernel module is MIT/GPL and not covered.
  nixpkgs.config.allowUnfreePredicate =
    pkg:
    builtins.elem (lib.getName pkg) [
      "nvidia-x11"
    ];

  # NVIDIA's open module, for the same driver version as the userspace below.
  #
  # It used to be compiled inside the kernel's own build, when the kernel
  # signed its modules with a key it generated and threw away, so nothing
  # built afterwards could be signed. nixpkgs' kernel signs with kmodigest
  # now, which any module build can run, so the module is nixpkgs' package
  # again.
  boot.extraModulePackages = [ base.open ];

  boot.extraModprobeConfig = ''
    # KMS, and a framebuffer console on it, which the compositor's GBM scanout
    # needs. The driver defaults to both since 570; set for clarity.
    options nvidia_drm modeset=1 fbdev=1
    # Suspend through the kernel's own notifiers (open module,
    # 595 and later) instead of nvidia-sleep.sh units around
    # systemd-suspend.service, keeping video memory across them in a file in
    # the tmpfs below.
    options nvidia NVreg_UseKernelSuspendNotifiers=1 NVreg_PreserveVideoMemoryAllocations=1 NVreg_TemporaryFilePath=${vramDir}
  '';

  # Where the driver saves video memory over a suspend: the session's
  # framebuffers and textures. Not /tmp or /var/tmp, which are on the root
  # partition, unencrypted ext4 (docs/layout.md): this tmpfs keeps them in
  # RAM, which suspend keeps powered. Its size is a ceiling,
  # not a reservation, and as large as memory, since the copy has to fit
  # all of the card's used memory; tmpfs pages can go to that swap too.
  # Mounted only on machines the NVIDIA rule matched.
  systemd.mounts = [
    {
      what = "tmpfs";
      where = vramDir;
      type = "tmpfs";
      options = "mode=0700,size=100%";
      wantedBy = [ "sysinit.target" ];
      before = [ "systemd-modules-load.service" ];
      after = [ "losos-hardware.service" ];
      unitConfig = {
        DefaultDependencies = false;
        ConditionPathExists = "/run/losos/hardware/flags/nvidia";
      };
    }
  ];

  # GSP firmware: the open module runs the GPU's resource manager on the
  # card's GSP, from this firmware. It is NVIDIA's build for this driver
  # version; nouveau's copy in linux-firmware is a different, older one.
  hardware.firmware = [ nvidia.firmware ];

  hardware.graphics.extraPackages = [
    nvidia.out
    eglPlatforms
    # VA-API through NVDEC, so browsers and GStreamer decode video on the
    # card. libva picks it by the DRM driver's name, so it stays idle on
    # every other GPU.
    pkgs.nvidia-vaapi-driver
  ];

  # libglvnd's EGL looks for external platforms here, not in
  # /run/opengl-driver, where hardware.graphics puts them.
  environment.etc."egl/egl_external_platform.d".source =
    "/run/opengl-driver/share/egl/egl_external_platform.d/";

  # nvidia-smi, which is the first thing anyone asks for when the card
  # misbehaves.
  environment.systemPackages = [ nvidia.bin ];

  services.udev.extraRules = ''
    # The device nodes CUDA, Vulkan and EGL open, which nvidia-modprobe makes
    # elsewhere; NixOS's module makes them the same way.
    KERNEL=="nvidia", RUN+="${pkgs.runtimeShell} -c 'mknod -m 666 /dev/nvidiactl c 195 255'"
    KERNEL=="nvidia", RUN+="${pkgs.runtimeShell} -c 'for i in $$(cat /proc/driver/nvidia/gpus/*/information | grep Minor | cut -d \  -f 4); do mknod -m 666 /dev/nvidia$${i} c 195 $${i}; done'"
    KERNEL=="nvidia_modeset", RUN+="${pkgs.runtimeShell} -c 'mknod -m 666 /dev/nvidia-modeset c 195 254'"
    KERNEL=="nvidia_uvm", RUN+="${pkgs.runtimeShell} -c 'mknod -m 666 /dev/nvidia-uvm c $$(grep nvidia-uvm /proc/devices | cut -d \  -f 1) 0'"
    KERNEL=="nvidia_uvm", RUN+="${pkgs.runtimeShell} -c 'mknod -m 666 /dev/nvidia-uvm-tools c $$(grep nvidia-uvm /proc/devices | cut -d \  -f 1) 1'"
    # Let a laptop's NVIDIA GPU power down while nothing uses it. The driver
    # does runtime power management on Turing and newer, but the kernel only
    # suspends a PCI device whose power/control says auto.
    ACTION=="bind", SUBSYSTEM=="pci", ATTR{vendor}=="0x10de", ATTR{class}=="0x030000", TEST=="power/control", ATTR{power/control}="auto"
    ACTION=="bind", SUBSYSTEM=="pci", ATTR{vendor}=="0x10de", ATTR{class}=="0x030200", TEST=="power/control", ATTR{power/control}="auto"
  '';

  losos.hardware.rules.nvidia-open = {
    classes = [ "graphics_card" ];
    vendor = 4318; # 0x10de, NVIDIA
    devicesFile = supportedGpus;
    load = [
      "nvidia"
      "nvidia_modeset"
      "nvidia_drm"
      "nvidia_uvm"
    ];
    # nova_core is the kernel's own Rust driver for the same GPUs. nixpkgs
    # builds it off today; NixOS's NVIDIA module blacklists it as well, for
    # when that changes.
    blacklist = [
      "nouveau"
      "nova_core"
    ];
    # If NVIDIA's module refuses the card, nouveau still drives it.
    fallback = [ "nouveau" ];
    flags = [ "nvidia" ];
    # Their aliases claim every NVIDIA display controller, older ones too, so
    # on every machine the rule does not match, udev must not load them.
    reserve = [
      "nvidia"
      "nvidia_drm"
      "nvidia_modeset"
      "nvidia_uvm"
    ];
  };
}
