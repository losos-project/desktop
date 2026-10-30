//! What `losos-focus` decides without running anything: which system unit
//! holds a person's focus, and which of the units it would stop are running.

/// The system unit that holds `user`'s focus, or `None` for a name systemd
/// would have to escape. Account names on this OS never need it (homed and
/// accounts.nix take `[a-z_][a-z0-9_-]*`), and polkit's rule in focus.nix
/// compares the unit with the caller's name as it is.
pub fn instance(user: &str) -> Option<String> {
    let mut chars = user.chars();
    let first = chars.next()?;
    let ok = (first.is_ascii_lowercase() || first == '_')
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-');
    ok.then(|| format!("losos-focus@{user}.service"))
}

/// The units in `systemctl show -p Id,ActiveState <units>` output that are
/// running or starting, in the order systemd printed them. Blocks are
/// separated by an empty line; a unit that does not exist prints
/// `ActiveState=inactive` and so is left out.
pub fn running(show: &str) -> Vec<String> {
    show.split("\n\n")
        .filter_map(|block| {
            let mut id = None;
            let mut state = None;
            for line in block.lines() {
                match line.split_once('=') {
                    Some(("Id", v)) => id = Some(v),
                    Some(("ActiveState", v)) => state = Some(v),
                    _ => {}
                }
            }
            match (id, state) {
                (Some(id), Some("active" | "activating" | "reloading")) => Some(id.to_owned()),
                _ => None,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_account_name_names_its_unit() {
        assert_eq!(
            instance("matus").as_deref(),
            Some("losos-focus@matus.service")
        );
        assert_eq!(
            instance("_build-2").as_deref(),
            Some("losos-focus@_build-2.service")
        );
    }

    #[test]
    fn a_name_systemd_would_escape_or_polkit_could_misread_is_refused() {
        for name in ["", "Matus", "2fast", "a.b", "a/b", "a b", "ä", "x\\x2d"] {
            assert_eq!(instance(name), None, "{name:?}");
        }
    }

    #[test]
    fn only_running_or_starting_units_are_stopped() {
        let show = "Id=ollama.service\nActiveState=active\n\n\
                    Id=absent.service\nActiveState=inactive\n\n\
                    Id=starting.service\nActiveState=activating\n\n\
                    Id=broken.service\nActiveState=failed\n";
        assert_eq!(running(show), ["ollama.service", "starting.service"]);
    }

    #[test]
    fn nothing_listed_is_nothing_running() {
        assert!(running("").is_empty());
    }
}
