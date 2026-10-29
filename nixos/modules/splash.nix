# The boot screen: Plymouth, with a theme (nixos/branding/splash.script)
# that draws the salmon LosOS boots behind swimming on the dark ground, the
# system's name under it and a line that grows as the boot gets on, and
# fades each of them in and out. Imported by the
# PC image (default.nix) and by the installer's live system, which then
# reads "LosOS Desktop installer is starting" (docs/boot-loaders.md).
#
# The whole start is one picture: GRUB's menu on the same ground with the
# same salmon (grub.nix), then the UKI's own splash, which systemd-stub
# shows while the kernel and initrd load, then Plymouth from the initrd on,
# until the login screen or the installer takes the display. Plymouth comes
# back at shutdown and says the machine is restarting or shutting down.
#
# Plymouth draws on the display it finds in its first seconds, which on a
# UEFI PC is the firmware's framebuffer, in the mode GRUB left. A BIOS
# machine hands GRUB's graphics mode on to the kernel for the same reason
# (grub.cfg). Without either, Plymouth runs in text mode. Esc shows the boot
# log, and Esc again hides it.
#
# Halium phones boot through Android's boot loader and draw nothing until
# the compositor starts, so nixos/halium does not import this.
{
  config,
  lib,
  pkgs,
  ...
}:

let
  plymouth = config.boot.plymouth.package;
  name = config.system.nixos.distroName;

  art = import ../branding { inherit pkgs name; };
  inherit (art.palette)
    ground
    ink
    muted
    accent
    ;

  # The theme's colours as the 0..1 fractions Plymouth's script wants.
  colour =
    var: hex:
    lib.concatMapStrings
      (c: "${var}.${c.n} = ${toString ((lib.fromHexString (builtins.substring c.i 2 hex)) / 255.0)};\n")
      [
        {
          n = "r";
          i = 1;
        }
        {
          n = "g";
          i = 3;
        }
        {
          n = "b";
          i = 5;
        }
      ];

  theme =
    pkgs.runCommand "losos-plymouth-theme" { nativeBuildInputs = [ pkgs.buildPackages.imagemagick ]; }
      ''
        dir=$out/share/plymouth/themes/losos
        mkdir -p $dir
        for f in ${art}/swim/*.png; do
          cp $f $dir/swim-$(basename $f)
        done
        magick -size 8x8 xc:'${accent}' PNG24:$dir/accent.png
        cat > $dir/losos.script <<'EOF'
        ${colour "ground" ground}${colour "ink" ink}${colour "muted" muted}${colour "accent" accent}name = "${name}";
        swim_frames = ${toString art.swimFrames};
        EOF
        cat ${../branding/splash.script} >> $dir/losos.script
        cat > $dir/losos.plymouth <<EOF
        [Plymouth Theme]
        Name=LosOS
        Description=The salmon swimming, the system's name and how far the boot has got
        ModuleName=script

        [script]
        ImageDir=$dir
        ScriptFile=$dir/losos.script
        EOF
      '';

  # The theme asks for DejaVu Sans and DejaVu Sans Bold by name. The NixOS
  # module copies one font file into the initrd; this folder has both, and
  # the running system carries it at the same path, because plymouthd moves
  # into the real root with the rest of the boot and draws from there until
  # the login screen.
  fonts = pkgs.runCommand "losos-plymouth-fonts" { } ''
    mkdir -p $out
    cp ${pkgs.dejavu_fonts}/share/fonts/truetype/DejaVuSans{,-Bold}.ttf $out
  '';
in
{
  boot.plymouth = {
    enable = true;
    theme = "losos";
    themePackages = [ theme ];
    logo = "${art}/logo.png";
  };

  boot.initrd.systemd.contents."/etc/plymouth/fonts".source = lib.mkForce fonts;
  environment.etc."plymouth/fonts".source = fonts;

  # The salmon again while the kernel loads, from the UKI's .splash section,
  # so the screen does not go black between GRUB and Plymouth.
  boot.uki.settings.UKI.Splash = "${art}/splash.bmp";

  # Errors only, so the kernel and systemd do not write over the splash.
  # Every message is still in the journal and behind Esc.
  boot.consoleLogLevel = lib.mkDefault 3;
  boot.kernelParams = [
    "udev.log_level=3"
    # systemd's "[  OK  ] Started ..." lines only when a unit fails or the
    # boot stalls, so nothing scrolls past before Plymouth takes the screen.
    "rd.systemd.show_status=auto"
    "systemd.show_status=auto"
    # A serial console on the command line (a VM test's, say) would
    # otherwise turn the screen's splash into the text log.
    "plymouth.ignore-serial-consoles"
  ];

  # The splash fades out to the bare ground before Plymouth lets go of the
  # screen: the theme hears losos-fade-out, and the half-second wait covers
  # its 20 refreshes with a little to spare for a slow machine. At boot the
  # progress line fills and stays, for the login screen to start from. That
  # last frame, the ground and the line, then stays on the screen until the
  # login screen or the installer draws its first (desktop.nix and the
  # installer order themselves after this), rather than a moment of black
  # console between the two. Plymouth fades in by itself when it starts, at
  # boot and at shutdown alike.
  systemd.services.plymouth-quit.serviceConfig = {
    # Only when Plymouth is showing anything, so a boot without the splash
    # does not wait for a fade nobody sees.
    ExecStartPre = "-${pkgs.writeShellScript "losos-splash-fade-out" ''
      ${plymouth}/bin/plymouth --ping || exit 0
      ${plymouth}/bin/plymouth update --status=losos-fade-out
      exec ${pkgs.coreutils}/bin/sleep 0.5
    ''}";
    ExecStart = [
      ""
      "-${plymouth}/bin/plymouth quit --retain-splash"
    ];
  };
}
