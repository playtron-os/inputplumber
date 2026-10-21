//! DBus client for InputPlumber's Manager interface (`org.shadowblip.InputManager`).
//!
//! Interface definitions only, MIT licensed, so any program can talk to the
//! daemon. See the README for target groups.

use std::path::PathBuf;

/// Well-known name InputPlumber owns on the system bus.
pub const BUS_NAME: &str = "org.shadowblip.InputPlumber";
/// Object path of the Manager.
pub const MANAGER_PATH: &str = "/org/shadowblip/InputPlumber/Manager";
/// Directory holding one view of `/dev/input` per target group: the shared
/// nodes plus that group's own gamepads, for binding as a sandbox's
/// `/dev/input`.
pub const VIEWS_PATH: &str = "/dev/inputplumber/groups";
/// Longest target group name the daemon accepts.
pub const GROUP_NAME_MAX: usize = 40;

/// Whether the daemon accepts this target group name: 1 to
/// [GROUP_NAME_MAX] ASCII letters, digits, `.`, `_` or `-`, and not `.` or `..`.
pub fn is_valid_group_name(name: &str) -> bool {
    let allowed = |b: u8| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-';
    (1..=GROUP_NAME_MAX).contains(&name.len())
        && name != "."
        && name != ".."
        && name.bytes().all(allowed)
}

/// Where the view of `/dev/input` for a target group lives.
pub fn group_view(name: &str) -> PathBuf {
    PathBuf::from(VIEWS_PATH).join(name)
}

/// InputPlumber's Manager.
#[zbus::proxy(
    interface = "org.shadowblip.InputManager",
    default_service = "org.shadowblip.InputPlumber",
    default_path = "/org/shadowblip/InputPlumber/Manager"
)]
pub trait InputManager {
    /// Daemon version
    #[zbus(property)]
    fn version(&self) -> zbus::Result<String>;

    /// Composite device paths in player order
    #[zbus(property)]
    fn gamepad_order(&self) -> zbus::Result<Vec<String>>;
    #[zbus(property)]
    fn set_gamepad_order(&self, order: Vec<String>) -> zbus::Result<()>;

    /// Whether every device with a composite device configuration is managed
    #[zbus(property)]
    fn manage_all_devices(&self) -> zbus::Result<bool>;
    #[zbus(property)]
    fn set_manage_all_devices(&self, value: bool) -> zbus::Result<()>;

    /// Names of the target devices InputPlumber can emulate
    #[zbus(property)]
    fn supported_target_devices(&self) -> zbus::Result<Vec<String>>;

    /// Ids of the target devices InputPlumber can emulate, such as `xb360`
    #[zbus(property)]
    fn supported_target_device_ids(&self) -> zbus::Result<Vec<String>>;

    /// The target groups: every composite device keeps gamepads for each.
    /// Empty when there are none, with one set of targets per device.
    #[zbus(property)]
    fn target_groups(&self) -> zbus::Result<Vec<String>>;

    /// The target group whose gamepads receive input, or "" without groups.
    #[zbus(property)]
    fn active_target_group(&self) -> zbus::Result<String>;
    /// Make a target group the active one, adding it first if needed.
    #[zbus(property)]
    fn set_active_target_group(&self, name: &str) -> zbus::Result<()>;

    /// Add a target group. Adding an existing group does nothing.
    fn add_target_group(&self, name: &str) -> zbus::Result<()>;

    /// Remove a target group and its gamepads. The active group can only be
    /// removed last. Removing a missing group does nothing.
    fn remove_target_group(&self, name: &str) -> zbus::Result<()>;

    /// Create a composite device from the configuration file at the path
    fn create_composite_device(&self, config_path: &str) -> zbus::Result<String>;

    /// Create a target device of the given type, returning its object path
    fn create_target_device(&self, kind: &str) -> zbus::Result<String>;

    /// Stop the target device at the given object path
    fn stop_target_device(&self, path: &str) -> zbus::Result<()>;

    /// Attach a target device to a composite device
    fn attach_target_device(&self, target_path: &str, composite_path: &str) -> zbus::Result<()>;

    /// Prepare for system suspend
    fn hook_sleep(&self) -> zbus::Result<()>;

    /// Prepare for resume from system suspend
    fn hook_wake(&self) -> zbus::Result<()>;
}

#[cfg(test)]
mod tests;
