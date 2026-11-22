# One icon theme

Every app draws its icons from the icon theme that derisk's theme names.
The default is Papirus-Dark or Papirus, and Settings, Appearance changes
it. derisk tells every toolkit the name through the setting that toolkit
already reads. An application can still set another name afterwards. The
toolkits are nixpkgs' builds.

![derisk's command palette listing apps with Papirus-Dark icons, from Files and Settings to the terminal](images/icon-theme-palette.png)

derisk hands the name to every toolkit through the channel each one already
reads, in `src/theme.rs`:

- **GTK 3 and 4** read `gtk-icon-theme-name` from `settings.ini` on the XDG
  config path, which derisk publishes, but prefer GSettings'
  `org.gnome.desktop.interface icon-theme` whenever the GNOME schemas are
  installed, as they are for any app nixpkgs wraps. So derisk also writes
  that key with `dconf`, which `programs.dconf` in
  `nixos/modules/desktop.nix` provides. Without it GSettings answers with its
  default, Adwaita, and GTK apps used to ignore derisk's theme. Running apps
  follow a change.
- **Qt 5 and 6** ask their platform theme. In a session that is neither
  GNOME nor Plasma, the platform theme names no icon theme, so Qt apps used
  to have only hicolor's few icons. Both versions read
  `QT_QPA_SYSTEM_ICON_THEME` first. derisk sets it for every app it launches,
  and in the environment of the user manager and of D-Bus activation. A
  running Qt app keeps the theme it started with.
- **Flatpak apps** see the host's icon themes at `/run/host/share/icons`,
  where nixpkgs' flatpak binds `/run/current-system/sw/share/icons`. Their
  GTK asks the settings portal for the name, and the GTK backend answers
  from the same GSettings key.

An app that loads an image file or resource by path gets that image.
Chromium and Electron draw their own UI icons that way and use GTK for
file icons and the file chooser. Flatpak apps run on their runtime's GTK
and Qt. KDE runtime apps, which carry Breeze and never read
`QT_QPA_SYSTEM_ICON_THEME` from the host, keep Breeze.
