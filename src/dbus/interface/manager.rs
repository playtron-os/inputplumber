use std::{path::PathBuf, time::Duration};

use tokio::sync::mpsc;
use zbus::{fdo, message::Header, Connection, DBusError, Message};
use zbus_macros::interface;

use crate::{
    config::{
        settings::{Settings, SETTINGS_PATH},
        CompositeDeviceConfig, LoadError,
    },
    constants::BUS_PREFIX,
    dbus::{caller, interface::Unregisterable, polkit::check_polkit},
    input::{
        manager::ManagerCommand,
        target::TargetDeviceTypeId,
        target_group::{self, TargetGroups},
    },
};

/// The [ManagerInterface] provides a DBus interface that can be exposed for managing
/// a [Manager]. It works by sending command messages to a channel that the
/// [Manager] is listening on.
pub struct ManagerInterface {
    tx: mpsc::Sender<ManagerCommand>,
    gamepad_order: Vec<String>,
    target_groups: TargetGroups,
    /// InputPlumber's own settings, read at each target group change.
    settings: PathBuf,
    /// Where callers' processes are looked up: `/proc`.
    proc: PathBuf,
}

impl ManagerInterface {
    pub fn new(tx: mpsc::Sender<ManagerCommand>, target_groups: TargetGroups) -> ManagerInterface {
        ManagerInterface {
            tx,
            gamepad_order: Default::default(),
            target_groups,
            settings: PathBuf::from(SETTINGS_PATH),
            proc: PathBuf::from("/proc"),
        }
    }

    /// Refuse a caller a change to the target group `name` unless it is root,
    /// or the group is theirs ([target_group::may_change]) and they run in one
    /// of the units the settings name ([Settings::target_group_writers]).
    /// Without a header the call comes from the daemon itself, not the bus.
    async fn check_group_change(
        &self,
        conn: &Connection,
        hdr: Option<&Header<'_>>,
        name: &str,
    ) -> fdo::Result<()> {
        target_group::validate_name(name).map_err(fdo::Error::InvalidArgs)?;
        let Some(hdr) = hdr else {
            return Ok(());
        };
        let creds = caller::credentials(conn, hdr).await?;
        let uid = creds.unix_user_id().ok_or_else(|| {
            fdo::Error::AccessDenied("The bus gave no user for the caller".into())
        })?;
        if !target_group::may_change(uid, name) {
            return Err(fdo::Error::AccessDenied(format!(
                "Target group '{name}' is not yours: user {uid} may only change groups named '{uid}-…'"
            )));
        }
        if uid == 0 {
            return Ok(());
        }
        let writers = Settings::load(&self.settings)
            .map_err(|e| {
                log::error!("Refusing target group changes: unreadable settings: {e}");
                fdo::Error::AccessDenied(format!(
                    "Target groups cannot be changed: InputPlumber's settings are unreadable: {e}"
                ))
            })?
            .target_group_writers;
        if writers.is_empty() {
            return Ok(());
        }
        let unit = caller::cgroup(&self.proc, &creds)
            .ok()
            .and_then(|cgroup| caller::unit_of(&cgroup, uid));
        if caller::is_writer(&writers, unit.as_deref()) {
            return Ok(());
        }
        let from = unit.map_or_else(|| "no unit".to_string(), |unit| format!("unit '{unit}'"));
        log::info!("Refused a change to target group '{name}' from {from}");
        Err(fdo::Error::AccessDenied(format!(
            "Target groups are changed only by {}: the caller runs in {from}",
            writers.join(", ")
        )))
    }

    /// The interface with its settings and `/proc` elsewhere, for a test.
    #[cfg(test)]
    pub fn with_files(mut self, settings: impl Into<PathBuf>, proc: impl Into<PathBuf>) -> Self {
        self.settings = settings.into();
        self.proc = proc.into();
        self
    }

    /// An error from a `&self` property setter, with its DBus name kept: zbus
    /// answers a plain [fdo::Error] from one as `org.freedesktop.zbus.Error`.
    fn setter_error(hdr: Option<&Header<'_>>, error: fdo::Error) -> zbus::Error {
        let name = DBusError::name(&error);
        let description = DBusError::description(&error).map(str::to_string);
        let reply = hdr.map(|hdr| {
            Message::error(hdr, name.clone())?.build(&(description.as_deref().unwrap_or(""),))
        });
        match reply {
            Some(Ok(reply)) => zbus::Error::MethodError(name.into(), description, reply),
            _ => error.into(),
        }
    }

    /// Send a target group request to the manager and wait for its answer
    async fn ask_about_group(
        &self,
        command: impl FnOnce(mpsc::Sender<Result<(), String>>) -> ManagerCommand,
    ) -> fdo::Result<()> {
        let (sender, mut receiver) = mpsc::channel(1);
        self.tx
            .send_timeout(command(sender), Duration::from_millis(500))
            .await
            .map_err(|err| fdo::Error::Failed(err.to_string()))?;
        match receiver.recv().await {
            Some(Ok(())) => Ok(()),
            Some(Err(e)) => Err(fdo::Error::InvalidArgs(e)),
            None => Err(fdo::Error::Failed("No response from manager".to_string())),
        }
    }
}

#[interface(
    name = "org.shadowblip.InputManager",
    proxy(
        default_service = "org.shadowblip.InputPlumber",
        default_path = "/org/shadowblip/InputPlumber/Manager"
    )
)]
impl ManagerInterface {
    #[zbus(property)]
    async fn version(
        &self,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] hdr: Option<Header<'_>>,
    ) -> fdo::Result<String> {
        check_polkit(conn, hdr, "org.shadowblip.InputPlumber.Version").await?;
        const VERSION: &str = env!("CARGO_PKG_VERSION");
        Ok(VERSION.to_string())
    }

    #[zbus(property)]
    async fn gamepad_order(
        &self,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] hdr: Option<Header<'_>>,
    ) -> fdo::Result<Vec<String>> {
        check_polkit(conn, hdr, "org.shadowblip.InputPlumber.GamepadOrder").await?;
        Ok(self.gamepad_order.clone())
    }

    #[zbus(property)]
    async fn set_gamepad_order(
        &mut self,
        order: Vec<String>,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] hdr: Option<Header<'_>>,
    ) -> fdo::Result<()> {
        check_polkit(conn, hdr, "org.shadowblip.InputPlumber.SetGamepadOrder").await?;
        self.tx
            .send_timeout(
                ManagerCommand::SetGamepadOrder {
                    dbus_paths: order.clone(),
                },
                Duration::from_millis(500),
            )
            .await
            .map_err(|err| fdo::Error::Failed(err.to_string()))?;
        self.gamepad_order = order;

        Ok(())
    }

    /// Target groups: named sets of gamepad target devices. Every composite
    /// device keeps one gamepad of each gamepad target type per group, and
    /// only the active group's gamepads receive input; the others stay
    /// connected and at rest. Empty means no groups, one set of targets.
    #[zbus(property)]
    async fn target_groups(
        &self,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] hdr: Option<Header<'_>>,
    ) -> fdo::Result<Vec<String>> {
        check_polkit(conn, hdr, "org.shadowblip.InputPlumber.TargetGroups").await?;
        Ok(self.target_groups.names().to_vec())
    }

    /// The target group whose gamepads receive input, or "" when there are
    /// no groups. Setting a group that does not exist adds it first. Only
    /// root may set a group not named after the caller's uid (`<uid>-…`);
    /// with `target_group_writers` set, only callers in those units may.
    #[zbus(property)]
    async fn active_target_group(
        &self,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] hdr: Option<Header<'_>>,
    ) -> fdo::Result<String> {
        check_polkit(conn, hdr, "org.shadowblip.InputPlumber.ActiveTargetGroup").await?;
        Ok(self.target_groups.active().to_string())
    }

    #[zbus(property)]
    async fn set_active_target_group(
        &self,
        name: String,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] hdr: Option<Header<'_>>,
    ) -> zbus::Result<()> {
        let set = async {
            self.check_group_change(conn, hdr.as_ref(), &name).await?;
            check_polkit(
                conn,
                hdr.clone(),
                "org.shadowblip.InputPlumber.SetActiveTargetGroup",
            )
            .await?;
            self.ask_about_group(|sender| ManagerCommand::SetActiveTargetGroup { name, sender })
                .await
        };
        set.await
            .map_err(|error| Self::setter_error(hdr.as_ref(), error))
    }

    /// Add a target group: every composite device creates its gamepads, at
    /// rest unless the group is active. Adding an existing group does nothing.
    /// Only root may add a group not named after the caller's uid (`<uid>-…`);
    /// with `target_group_writers` set, only callers in those units may.
    async fn add_target_group(
        &self,
        name: String,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] hdr: Header<'_>,
    ) -> fdo::Result<()> {
        self.check_group_change(conn, Some(&hdr), &name).await?;
        check_polkit(
            conn,
            Some(hdr),
            "org.shadowblip.InputPlumber.AddTargetGroup",
        )
        .await?;
        self.ask_about_group(|sender| ManagerCommand::AddTargetGroup { name, sender })
            .await
    }

    /// Remove a target group and its gamepads. The active group can only be
    /// removed last. Removing a group that does not exist does nothing. Only
    /// root may remove a group not named after the caller's uid (`<uid>-…`);
    /// with `target_group_writers` set, only callers in those units may.
    async fn remove_target_group(
        &self,
        name: String,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] hdr: Header<'_>,
    ) -> fdo::Result<()> {
        self.check_group_change(conn, Some(&hdr), &name).await?;
        check_polkit(
            conn,
            Some(hdr),
            "org.shadowblip.InputPlumber.RemoveTargetGroup",
        )
        .await?;
        self.ask_about_group(|sender| ManagerCommand::RemoveTargetGroup { name, sender })
            .await
    }

    /// If set to 'true', InputPlumber will try to manage all input devices
    /// on the system that have a Composite Device configuration.
    #[zbus(property)]
    async fn manage_all_devices(
        &self,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] hdr: Option<Header<'_>>,
    ) -> fdo::Result<bool> {
        check_polkit(conn, hdr, "org.shadowblip.InputPlumber.ManageAllDevices").await?;
        let (sender, mut receiver) = mpsc::channel(1);
        self.tx
            .send_timeout(
                ManagerCommand::GetManageAllDevices { sender },
                Duration::from_millis(500),
            )
            .await
            .map_err(|err| fdo::Error::Failed(err.to_string()))?;

        // Read the response from the manager
        let Some(response) = receiver.recv().await else {
            return Err(fdo::Error::Failed("No response from manager".to_string()));
        };
        Ok(response)
    }
    #[zbus(property)]
    async fn set_manage_all_devices(
        &self,
        value: bool,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] hdr: Option<Header<'_>>,
    ) -> zbus::Result<()> {
        check_polkit(conn, hdr, "org.shadowblip.InputPlumber.SetManageAllDevices").await?;
        self.tx
            .send_timeout(
                ManagerCommand::SetManageAllDevices(value),
                Duration::from_millis(500),
            )
            .await
            .map_err(|err| zbus::Error::Failure(err.to_string()))?;
        Ok(())
    }

    /// Returns a list of supported target device names. E.g. ["InputPlumber Mouse", "Microsoft
    /// XBox 360 Gamepad"]
    #[zbus(property)]
    async fn supported_target_devices(
        &self,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] hdr: Option<Header<'_>>,
    ) -> fdo::Result<Vec<String>> {
        check_polkit(
            conn,
            hdr,
            "org.shadowblip.InputPlumber.SupportedTargetDevices",
        )
        .await?;
        let supported = TargetDeviceTypeId::supported_types();
        Ok(supported.iter().map(|id| id.name().to_string()).collect())
    }

    /// Returns a list of supported target device ids. E.g. ["xb360", "deck"]
    #[zbus(property)]
    async fn supported_target_device_ids(
        &self,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] hdr: Option<Header<'_>>,
    ) -> fdo::Result<Vec<String>> {
        check_polkit(
            conn,
            hdr,
            "org.shadowblip.InputPlumber.SupportedTargetDeviceIds",
        )
        .await?;
        let supported = TargetDeviceTypeId::supported_types();
        Ok(supported.iter().map(|id| id.to_string()).collect())
    }

    /// Create a composite device using the given composite device config. The
    /// path should be the absolute path to a composite device configuration file.
    async fn create_composite_device(
        &self,
        config_path: String,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] hdr: Header<'_>,
    ) -> fdo::Result<String> {
        check_polkit(
            conn,
            Some(hdr),
            "org.shadowblip.InputPlumber.CreateCompositeDevice",
        )
        .await?;
        let device = CompositeDeviceConfig::from_yaml_file(config_path).map_err(|e| match e {
            LoadError::IoError(error) => fdo::Error::Failed(error.to_string()),
            LoadError::MaximumSizeReached(error) => fdo::Error::Failed(error.to_string()),
            LoadError::DeserializeError(_) => {
                fdo::Error::InvalidFileContent("Failed to parse file".to_string())
            }
        })?;
        self.tx
            .send_timeout(
                ManagerCommand::CreateCompositeDevice { config: device },
                Duration::from_millis(500),
            )
            .await
            .map_err(|err| fdo::Error::Failed(err.to_string()))?;
        Ok("".to_string())
    }

    /// Create a target device of the given type. Returns the DBus path to
    /// the created target device.
    async fn create_target_device(
        &self,
        kind: String,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] hdr: Header<'_>,
    ) -> fdo::Result<String> {
        check_polkit(
            conn,
            Some(hdr),
            "org.shadowblip.InputPlumber.CreateTargetDevice",
        )
        .await?;
        let Ok(kind) = TargetDeviceTypeId::try_from(kind.as_str()) else {
            return Err(fdo::Error::InvalidArgs(format!(
                "Invalid target device type: {kind}."
            )));
        };
        let (sender, mut receiver) = mpsc::channel(1);
        self.tx
            .send_timeout(
                ManagerCommand::CreateTargetDevice {
                    kind,
                    persistent_id: None,
                    group: None,
                    sender,
                },
                Duration::from_millis(500),
            )
            .await
            .map_err(|err| fdo::Error::Failed(err.to_string()))?;

        // Read the response from the manager
        let Some(response) = receiver.recv().await else {
            return Err(fdo::Error::Failed("No response from manager".to_string()));
        };
        let device_path = match response {
            Ok(path) => path,
            Err(e) => {
                let err = format!("Failed to create target device: {e:?}");
                return Err(fdo::Error::Failed(err));
            }
        };

        Ok(device_path)
    }

    /// Stop the given target device
    async fn stop_target_device(
        &self,
        path: String,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] hdr: Header<'_>,
    ) -> fdo::Result<()> {
        check_polkit(
            conn,
            Some(hdr),
            "org.shadowblip.InputPlumber.StopTargetDevice",
        )
        .await?;
        self.tx
            .send_timeout(
                ManagerCommand::StopTargetDevice { path },
                Duration::from_millis(500),
            )
            .await
            .map_err(|err| fdo::Error::Failed(err.to_string()))?;
        Ok(())
    }

    /// Attach the given target device to the given composite device
    async fn attach_target_device(
        &self,
        target_path: String,
        composite_path: String,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] hdr: Header<'_>,
    ) -> fdo::Result<()> {
        check_polkit(
            conn,
            Some(hdr),
            "org.shadowblip.InputPlumber.AttachTargetDevice",
        )
        .await?;
        let (sender, mut receiver) = mpsc::channel(1);
        self.tx
            .send_timeout(
                ManagerCommand::AttachTargetDevice {
                    target_path: target_path.clone(),
                    composite_path: composite_path.clone(),
                    sender,
                },
                Duration::from_millis(500),
            )
            .await
            .map_err(|err| fdo::Error::Failed(err.to_string()))?;

        // Read the response from the manager
        let Some(response) = receiver.recv().await else {
            return Err(fdo::Error::Failed("No response from manager".to_string()));
        };
        if let Err(e) = response {
            let err = format!("Failed to attach target device {target_path} to composite device {composite_path}: {e:?}");
            return Err(fdo::Error::Failed(err));
        }

        Ok(())
    }

    /// Used to prepare InputPlumber for system suspend
    async fn hook_sleep(
        &self,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] hdr: Header<'_>,
    ) -> fdo::Result<()> {
        check_polkit(conn, Some(hdr), "org.shadowblip.InputPlumber.HookSleep").await?;
        let (sender, mut receiver) = mpsc::channel(1);
        self.tx
            .send_timeout(
                ManagerCommand::SystemSleep { sender },
                Duration::from_secs(5),
            )
            .await
            .map_err(|err| fdo::Error::Failed(err.to_string()))?;

        // Read the response from the manager
        if receiver.recv().await.is_none() {
            return Err(fdo::Error::Failed("No response from manager".to_string()));
        }

        Ok(())
    }

    /// Used to prepare InputPlumber for resume from system suspend
    async fn hook_wake(
        &self,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] hdr: Header<'_>,
    ) -> fdo::Result<()> {
        check_polkit(conn, Some(hdr), "org.shadowblip.InputPlumber.HookWake").await?;
        let (sender, mut receiver) = mpsc::channel(1);
        self.tx
            .send_timeout(
                ManagerCommand::SystemWake { sender },
                Duration::from_secs(5),
            )
            .await
            .map_err(|err| fdo::Error::Failed(err.to_string()))?;

        // Read the response from the manager
        if receiver.recv().await.is_none() {
            return Err(fdo::Error::Failed("No response from manager".to_string()));
        }

        Ok(())
    }
}

impl ManagerInterface {
    /// Update the target gamepad order property and emit property changed signal
    pub async fn update_target_gamepad_order(
        conn: &Connection,
        order: Vec<String>,
    ) -> Result<(), zbus::Error> {
        let path = format!("{BUS_PREFIX}/Manager");
        let iface_ref = conn.object_server().interface::<_, Self>(path).await?;

        let mut iface = iface_ref.get_mut().await;
        iface.gamepad_order = order;
        iface
            .gamepad_order_changed(iface_ref.signal_emitter())
            .await?;

        Ok(())
    }

    /// Update the target group properties and emit their changed signals
    pub async fn update_target_groups(
        conn: &Connection,
        groups: TargetGroups,
    ) -> Result<(), zbus::Error> {
        let path = format!("{BUS_PREFIX}/Manager");
        let iface_ref = conn.object_server().interface::<_, Self>(path).await?;

        let mut iface = iface_ref.get_mut().await;
        let names_changed = iface.target_groups.names() != groups.names();
        let active_changed = iface.target_groups.active() != groups.active();
        iface.target_groups = groups;
        if names_changed {
            iface
                .target_groups_changed(iface_ref.signal_emitter())
                .await?;
        }
        if active_changed {
            iface
                .active_target_group_changed(iface_ref.signal_emitter())
                .await?;
        }

        Ok(())
    }
}

impl Unregisterable for ManagerInterface {}

#[cfg(test)]
#[path = "manager_test.rs"]
mod tests;
