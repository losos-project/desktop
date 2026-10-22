//! The guest's apps as the host's own: a desktop entry and an icon for
//! each, in the person's applications and icons folders, so the app grid
//! and the command palette list them beside everything else.
//!
//! Each entry's name is `<prefix>.<app id>.desktop`, and its windows carry
//! the same `<prefix>.<app id>` as their app id, which is how the desktop
//! pairs a window with its entry. Entries with the prefix that the guest no
//! longer lists are removed, and nothing else is touched.

use std::path::{Path, PathBuf};

use app_windows_protocol::AppInfo;
use base64::Engine;

pub struct Places {
    pub applications: PathBuf,
    pub icons: PathBuf,
}

impl Places {
    /// `$XDG_DATA_HOME/applications` and the hicolor theme's 128 px apps.
    pub fn of_this_user() -> Result<Self, String> {
        let data = match std::env::var_os("XDG_DATA_HOME") {
            Some(d) if !d.is_empty() => PathBuf::from(d),
            _ => {
                Path::new(&std::env::var_os("HOME").ok_or("HOME is not set")?).join(".local/share")
            }
        };
        Ok(Places {
            applications: data.join("applications"),
            icons: data.join("icons/hicolor/128x128/apps"),
        })
    }
}

/// The app id a guest app's windows and entry share.
pub fn app_id(prefix: &str, app: &str) -> String {
    format!("{prefix}.{app}")
}

/// A desktop entry's value with its reserved characters escaped.
fn escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('\t', "\\t")
}

/// One argument of `Exec=`, quoted as the specification asks.
fn quote(arg: &str) -> String {
    if arg
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "-_./=:".contains(c))
    {
        return arg.to_owned();
    }
    let mut out = String::from("\"");
    for c in arg.chars() {
        if matches!(c, '"' | '`' | '$' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out.replace('%', "%%")
}

pub fn entry(prefix: &str, guest: &str, exec: &[String], app: &AppInfo) -> String {
    let id = app_id(prefix, &app.id);
    let mut command: Vec<String> = exec.iter().map(|a| quote(a)).collect();
    command.push("open".into());
    command.push(quote(&app.id));
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name={name}\n\
         Comment={guest} app\n\
         Exec={exec}\n\
         Icon={id}\n\
         StartupWMClass={id}\n\
         Categories=Utility;\n\
         X-App-Windows-Guest={guest}\n",
        name = escape(&app.name),
        guest = escape(guest),
        exec = command.join(" "),
    )
}

/// Writes an entry and icon for every app, and removes the ones for apps
/// that are gone. Returns how many entries there are.
pub fn sync(
    places: &Places,
    prefix: &str,
    guest: &str,
    exec: &[String],
    apps: &[AppInfo],
) -> Result<usize, String> {
    std::fs::create_dir_all(&places.applications).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&places.icons).map_err(|e| e.to_string())?;
    let mut keep = std::collections::HashSet::new();
    for app in apps {
        // Bundle identifiers are reverse DNS; anything that would leave the
        // folder or confuse a desktop file name is not one.
        if app.id.is_empty()
            || !app
                .id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
        {
            continue;
        }
        let id = app_id(prefix, &app.id);
        write_if_changed(
            &places.applications.join(format!("{id}.desktop")),
            entry(prefix, guest, exec, app).as_bytes(),
        )?;
        if let Some(icon) = app
            .icon
            .as_deref()
            .and_then(|i| base64::engine::general_purpose::STANDARD.decode(i).ok())
        {
            write_if_changed(&places.icons.join(format!("{id}.png")), &icon)?;
        }
        keep.insert(id);
    }
    for (folder, extension) in [(&places.applications, "desktop"), (&places.icons, "png")] {
        let Ok(entries) = std::fs::read_dir(folder) else {
            continue;
        };
        for file in entries.flatten() {
            let name = file.file_name().to_string_lossy().into_owned();
            let Some(id) = name.strip_suffix(&format!(".{extension}")) else {
                continue;
            };
            if id.starts_with(&format!("{prefix}.")) && !keep.contains(id) {
                let _ = std::fs::remove_file(file.path());
            }
        }
    }
    Ok(keep.len())
}

/// Writes a file only when its contents change, so the desktop's file
/// watchers do not reload every entry each time the guest is asked.
fn write_if_changed(path: &Path, contents: &[u8]) -> Result<(), String> {
    if std::fs::read(path).is_ok_and(|old| old == contents) {
        return Ok(());
    }
    let temporary = path.with_extension("new");
    std::fs::write(&temporary, contents).map_err(|e| format!("{}: {e}", temporary.display()))?;
    std::fs::rename(&temporary, path).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(id: &str, name: &str) -> AppInfo {
        AppInfo {
            id: id.into(),
            name: name.into(),
            icon: Some(base64::engine::general_purpose::STANDARD.encode(b"png")),
        }
    }

    #[test]
    fn entries_match_windows_and_come_and_go() {
        let root = std::env::temp_dir().join(format!("app-windows-entries-{}", std::process::id()));
        let places = Places {
            applications: root.join("applications"),
            icons: root.join("icons"),
        };
        std::fs::create_dir_all(&places.applications).unwrap();
        std::fs::write(places.applications.join("org.other.desktop"), "keep").unwrap();
        let exec = vec!["macos-app-support".to_owned()];
        let apps = [
            app("com.apple.TextEdit", "TextEdit"),
            app("bad/../id", "Nope"),
        ];
        assert_eq!(sync(&places, "macos", "macOS", &exec, &apps).unwrap(), 1);
        let text =
            std::fs::read_to_string(places.applications.join("macos.com.apple.TextEdit.desktop"))
                .unwrap();
        assert!(text.contains("Exec=macos-app-support open com.apple.TextEdit\n"));
        assert!(text.contains("StartupWMClass=macos.com.apple.TextEdit\n"));
        assert_eq!(
            std::fs::read(places.icons.join("macos.com.apple.TextEdit.png")).unwrap(),
            b"png"
        );
        sync(&places, "macos", "macOS", &exec, &[]).unwrap();
        assert!(!places
            .applications
            .join("macos.com.apple.TextEdit.desktop")
            .exists());
        assert!(places.applications.join("org.other.desktop").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn quoting() {
        assert_eq!(quote("plain"), "plain");
        assert_eq!(quote("two words"), "\"two words\"");
        assert_eq!(quote("100%$"), "\"100%%\\$\"");
    }
}
