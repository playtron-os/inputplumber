//! Target groups: named sets of gamepad target devices.
//!
//! Every composite device keeps one set of gamepad targets per group, of the
//! same types it would otherwise create once, and only the active group's
//! gamepads receive input. The others stay connected and at rest, so whatever
//! reads an inactive group's gamepad (a game in a background session or
//! sandbox) sees it idle instead of unplugged. Keyboard, mouse, touch and DBus
//! targets are shared by all groups: the compositor already sends those to
//! whatever is in front.
//!
//! Each grouped gamepad target carries the `phys` [phys_for] gives it, and
//! [GroupViews] keeps one directory per group under [VIEWS_PATH] holding hard
//! links of every `/dev/input` node except other groups' gamepads, for a
//! sandbox or container to bind as its own `/dev/input`. Binding the directory
//! rather than its nodes is what lets gamepads plugged in later show up.

use std::{
    collections::BTreeSet,
    fs, io,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::{Path, PathBuf},
};

use crate::input::target::TargetDeviceTypeId;

/// Prefix of the `phys` of a grouped gamepad target; the group name follows.
pub const PHYS_PREFIX: &str = "inputplumber/group/";
/// Longest group name, so the `phys` fits UHID's 64 bytes.
pub const MAX_NAME_LEN: usize = 40;
/// Where the per-group views of `/dev/input` live.
pub const VIEWS_PATH: &str = "/dev/inputplumber/groups";
/// Where the groups and the active one are kept across daemon restarts.
pub const STATE_PATH: &str = "/run/inputplumber/target-groups";

/// Check that a group name is usable as a directory name and in a `phys`.
pub fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > MAX_NAME_LEN {
        return Err(format!(
            "Target group name must be 1 to {MAX_NAME_LEN} characters: '{name}'"
        ));
    }
    if name == "." || name == ".." {
        return Err(format!("Invalid target group name: '{name}'"));
    }
    let valid = name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if !valid {
        return Err(format!(
            "Target group name may only hold ASCII letters, digits, '.', '_' and '-': '{name}'"
        ));
    }
    Ok(())
}

/// The `phys` given to a gamepad target of the given group.
pub fn phys_for(group: &str) -> String {
    format!("{PHYS_PREFIX}{group}")
}

/// The group a device belongs to, from its `phys`.
pub fn group_of_phys(phys: &str) -> Option<&str> {
    phys.strip_prefix(PHYS_PREFIX)
        .filter(|name| validate_name(name).is_ok())
}

/// The persistent id of a group's gamepad: distinct per group, so targets
/// that derive a serial or MAC address from it never collide (the kernel's
/// DualSense driver refuses a second device with the same address).
pub fn persistent_id_for(persistent_id: &str, group: &str) -> String {
    format!("{persistent_id}/{group}")
}

/// The groups that exist and the one whose gamepads receive input.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TargetGroups {
    names: Vec<String>,
    active: String,
}

impl TargetGroups {
    /// Group names, in the order they were added
    pub fn names(&self) -> &[String] {
        &self.names
    }

    /// The group whose gamepads receive input, or "" when there are no groups
    pub fn active(&self) -> &str {
        &self.active
    }

    /// The targets the given types make: one of each non-gamepad type, and one
    /// gamepad of each gamepad type per group, or one ungrouped gamepad when
    /// there are no groups.
    pub fn targets_for(
        &self,
        types: &[TargetDeviceTypeId],
    ) -> Vec<(TargetDeviceTypeId, Option<String>)> {
        let mut targets = Vec::new();
        for kind in types {
            if kind.is_gamepad() && !self.names.is_empty() {
                targets.extend(self.names.iter().map(|group| (*kind, Some(group.clone()))));
            } else {
                targets.push((*kind, None));
            }
        }
        targets
    }

    /// Whether a target of the given group receives input: every target but
    /// the gamepads of inactive groups. Shared targets have no group.
    pub fn receives_input(&self, group: Option<&str>) -> bool {
        group.is_none_or(|group| group == self.active)
    }

    /// Add a group. Returns whether anything changed.
    pub fn add(&mut self, name: &str) -> Result<bool, String> {
        validate_name(name)?;
        if self.names.iter().any(|n| n == name) {
            return Ok(false);
        }
        self.names.push(name.to_string());
        if self.active.is_empty() {
            self.active = name.to_string();
        }
        Ok(true)
    }

    /// Remove a group. The active group can only go when it is the last one.
    pub fn remove(&mut self, name: &str) -> Result<bool, String> {
        let Some(index) = self.names.iter().position(|n| n == name) else {
            return Ok(false);
        };
        if self.active == name && self.names.len() > 1 {
            return Err(format!(
                "Target group '{name}' is active; activate another one first"
            ));
        }
        self.names.remove(index);
        if self.names.is_empty() {
            self.active.clear();
        }
        Ok(true)
    }

    /// Make a group the active one, adding it first if needed. Returns whether
    /// anything changed.
    pub fn set_active(&mut self, name: &str) -> Result<bool, String> {
        if name.is_empty() {
            return if self.names.is_empty() {
                Ok(false)
            } else {
                Err("An active target group is required while groups exist".to_string())
            };
        }
        let added = self.add(name)?;
        if self.active == name {
            return Ok(added);
        }
        self.active = name.to_string();
        Ok(true)
    }

    /// Read the groups saved by [TargetGroups::save]. Anything unreadable
    /// reads as no groups.
    pub fn load(path: &Path) -> Self {
        let Ok(text) = fs::read_to_string(path) else {
            return Self::default();
        };
        let mut lines = text.lines();
        let active = lines.next().unwrap_or_default();
        let mut groups = Self::default();
        for name in lines {
            if let Err(e) = groups.add(name) {
                log::warn!("Ignoring saved target group: {e}");
            }
        }
        if groups.set_active(active).is_err() {
            log::warn!("Ignoring saved active target group '{active}'");
        }
        groups
    }

    /// Save the groups: the active one on the first line, then one per line.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        if self.names.is_empty() {
            return match fs::remove_file(path) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            };
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut text = format!("{}\n", self.active);
        for name in &self.names {
            text.push_str(name);
            text.push('\n');
        }
        let tmp = path.with_extension("tmp");
        fs::write(&tmp, text)?;
        fs::rename(tmp, path)
    }
}

/// One directory per group mirroring `/dev/input` without the other groups'
/// gamepads. Entries are hard links, so they share the original node's owner,
/// mode and ACLs, and follow every change logind makes to them.
#[derive(Debug)]
pub struct GroupViews {
    /// Directory holding one view per group
    root: PathBuf,
    /// The directory the views mirror, normally `/dev/input`
    input: PathBuf,
    /// Groups whose view is kept by this process
    groups: BTreeSet<String>,
}

impl GroupViews {
    pub fn new(root: impl Into<PathBuf>, input: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            input: input.into(),
            groups: BTreeSet::new(),
        }
    }

    /// The directory of a group's view
    pub fn path_of(&self, group: &str) -> PathBuf {
        self.root.join(group)
    }

    /// Keep a view for exactly these groups. A view this process has not kept
    /// yet is emptied of what an earlier run left and filled from the input
    /// directory, `owner` telling which group each node there belongs to.
    /// Directories are kept rather than replaced, so a sandbox that bound one
    /// before a daemon restart keeps seeing it fill.
    pub fn set_groups(
        &mut self,
        groups: &[String],
        owner: impl Fn(&str) -> Option<String>,
    ) -> io::Result<()> {
        let wanted: BTreeSet<String> = groups.iter().cloned().collect();
        if wanted.is_empty() {
            self.groups.clear();
            return match fs::remove_dir_all(&self.root) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            };
        }

        make_dir(&self.root)?;
        if let Ok(entries) = fs::read_dir(&self.root) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if !wanted.contains(&name) {
                    fs::remove_dir_all(entry.path())?;
                    self.groups.remove(&name);
                }
            }
        }

        let nodes = self.input_nodes();
        for group in wanted.difference(&self.groups.clone()) {
            let dir = self.path_of(group);
            make_dir(&dir)?;
            clear_dir(&dir)?;
            for node in &nodes {
                let shared_or_ours = owner(node).is_none_or(|g| &g == group);
                if shared_or_ours {
                    self.link(node, group);
                }
            }
            self.groups.insert(group.clone());
        }
        self.groups.retain(|g| wanted.contains(g));
        Ok(())
    }

    /// A node appeared in the input directory: show it in its own group's
    /// view, or in every view when it belongs to no group.
    pub fn node_added(&mut self, node: &str, owner: Option<&str>) {
        if !is_node_name(node) {
            return;
        }
        // A node of the same name may still be there from a device removed
        // before its removal was seen; it must never point at the new one.
        self.node_removed(node);
        let groups: Vec<String> = match owner {
            Some(group) if self.groups.contains(group) => vec![group.to_string()],
            Some(_) => vec![],
            None => self.groups.iter().cloned().collect(),
        };
        for group in groups {
            self.link(node, &group);
        }
    }

    /// A node left the input directory
    pub fn node_removed(&mut self, node: &str) {
        if !is_node_name(node) {
            return;
        }
        for group in &self.groups {
            let path = self.path_of(group).join(node);
            if let Err(e) = fs::remove_file(&path) {
                if e.kind() != io::ErrorKind::NotFound {
                    log::warn!("Failed to remove {}: {e}", path.display());
                }
            }
        }
    }

    /// Nodes directly in the input directory: no subdirectories (`by-id`,
    /// `by-path`), whose relative links would point outside a view.
    fn input_nodes(&self) -> Vec<String> {
        let Ok(entries) = fs::read_dir(&self.input) else {
            return vec![];
        };
        entries
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| !t.is_dir() && !t.is_symlink()))
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect()
    }

    fn link(&self, node: &str, group: &str) {
        let source = self.input.join(node);
        let target = self.path_of(group).join(node);
        match fs::hard_link(&source, &target) {
            Ok(()) => log::debug!("Showing {} in {}", source.display(), target.display()),
            // Gone again, or moved away by a hide rule: nothing to show.
            Err(e) if e.kind() == io::ErrorKind::NotFound => (),
            Err(e) => log::warn!(
                "Failed to link {} to {}: {e}",
                source.display(),
                target.display()
            ),
        }
    }
}

/// A plain name inside the input directory
fn is_node_name(node: &str) -> bool {
    !node.is_empty() && node != "." && node != ".." && !node.contains('/')
}

fn make_dir(path: &Path) -> io::Result<()> {
    match fs::DirBuilder::new()
        .recursive(true)
        .mode(0o755)
        .create(path)
    {
        Ok(()) => fs::set_permissions(path, fs::Permissions::from_mode(0o755)),
        Err(e) => Err(e),
    }
}

fn clear_dir(path: &Path) -> io::Result<()> {
    for entry in fs::read_dir(path)?.flatten() {
        let path = entry.path();
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            fs::remove_dir_all(path)?;
        } else {
            fs::remove_file(path)?;
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "target_group_test.rs"]
mod tests;
