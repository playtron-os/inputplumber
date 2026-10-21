//! InputPlumber's own settings, read from [SETTINGS_PATH].

use std::{fs, io, path::Path};

use serde::Deserialize;

/// Where an administrator sets InputPlumber's own settings.
pub const SETTINGS_PATH: &str = "/etc/inputplumber/inputplumber.yaml";

/// InputPlumber's own settings. A missing file means the defaults.
///
/// ```yaml
/// # Only the workspace registry may add, remove or activate target groups.
/// target_group_writers:
///   - kora-workspaces.service
/// ```
#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    /// The systemd units whose processes may add, remove or activate target
    /// groups, such as `kora-workspaces.service`: a unit of the caller's own
    /// user manager or of the system. Root may always. Empty, any caller that
    /// polkit allows and that owns the group may.
    #[serde(default)]
    pub target_group_writers: Vec<String>,
}

impl Settings {
    /// Read the settings at `path`: the defaults when there is no file. A file
    /// that cannot be read or parsed is an error, never the defaults, so a
    /// mistake in it does not lift a restriction.
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        // A file of comments only is an empty document.
        serde_yaml::from_str::<Option<Self>>(&text)
            .map(Option::unwrap_or_default)
            .map_err(|e| format!("{}: {e}", path.display()))
    }
}

#[cfg(test)]
#[path = "settings_test.rs"]
mod tests;
