# One icon theme

Every app draws its icons from the icon theme that derisk's theme names,
not from one the app ships or names in code. The default is Papirus-Dark or
Papirus, and Settings, Appearance changes it. Two things make that work:
derisk tells every toolkit the name, and patched toolkits refuse to let an
app override it.

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

The overlay's patches then make the toolkits refuse an app's attempt to
replace the theme. They are `gtk3/0034`, `gtk4/0005`, `qtbase/0003` and
`qtbase5/0001` in `nixos/pkgs/patches`:

- GTK ignores `gtk-icon-theme-name` set by the application on
  `GtkSettings`, or by a GTK theme's own `settings.ini`. Only the desktop's
  settings and the user's `~/.config/gtk-*/settings.ini` choose it.
  `gtk_icon_theme_set_custom_theme()` (GTK 3) and
  `gtk_icon_theme_set_theme_name()` (GTK 4) already refused the display's
  icon theme.
- Qt ignores `QIcon::setThemeName()` while the system names a theme. The
  name the app asked for becomes its fallback theme instead, unless it
  already set one, so an icon that only its own theme has still shows. KDE
  apps call this to ask for Breeze on every desktop except Plasma.
- In both toolkits, the system's icon directories always come first in the
  search path, whatever the app sets or prepends. An app cannot shadow the
  system theme with a directory of the same name, or drop the directories
  the theme lives in. App directories still follow, and icons in an app's
  own resources stay hicolor-level fallbacks, as the icon theme
  specification says.

> [!NOTE]
> What is left: an app that loads an image file or resource by path instead of
> asking for an icon by name gets that image, since there is no name to look
> up. Chromium and Electron draw their own UI icons that way and use GTK only
> for file icons and the file chooser, which follow the theme. Flatpak apps
> run on their runtime's unpatched GTK and Qt, so one can still name its own
> theme in code; KDE runtime apps, which carry Breeze and never read
> `QT_QPA_SYSTEM_ICON_THEME` from the host, keep Breeze.
