//! The Manager's target group calls on a bus of the test's own: who may
//! change the groups, by user and by systemd unit. The bus says which process
//! calls; its cgroup is read from a stand-in `/proc`.

use std::{
    collections::HashMap,
    fs,
    io::{BufRead, BufReader},
    path::Path,
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
};

use zbus::{connection::Builder, zvariant::OwnedValue};

use super::*;

const NAME: &str = "org.shadowblip.InputPlumber";
const PATH: &str = "/org/shadowblip/InputPlumber/Manager";
const INTERFACE: &str = "org.shadowblip.InputManager";
const ACCESS_DENIED: &str = "org.freedesktop.DBus.Error.AccessDenied";

/// dbus-daemon on a socket in a scratch directory, with none of the
/// machine's configuration. Stopped when dropped.
struct PrivateBus {
    daemon: Child,
    address: String,
}

impl PrivateBus {
    fn start(dir: &Path) -> Self {
        let config = dir.join("bus.conf");
        fs::write(
            &config,
            format!(
                r#"<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <listen>unix:path={}</listen>
  <auth>EXTERNAL</auth>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>
"#,
                dir.join("bus").display()
            ),
        )
        .unwrap();
        let mut daemon = Command::new("dbus-daemon")
            .arg(format!("--config-file={}", config.display()))
            .args(["--nofork", "--print-address"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .spawn()
            .expect("this test needs dbus-daemon on PATH");
        let mut address = String::new();
        BufReader::new(daemon.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        assert!(!address.trim().is_empty(), "dbus-daemon gave no address");
        Self {
            daemon,
            address: address.trim().to_string(),
        }
    }

    fn connect(&self) -> Builder<'static> {
        Builder::address(self.address.as_str()).unwrap()
    }
}

impl Drop for PrivateBus {
    fn drop(&mut self) {
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
    }
}

/// polkit, allowing everything: what is tested here comes before it.
struct Polkit;

#[interface(name = "org.freedesktop.PolicyKit1.Authority")]
impl Polkit {
    fn check_authorization(
        &self,
        _subject: (String, HashMap<String, OwnedValue>),
        _action_id: String,
        _details: HashMap<String, String>,
        _flags: u32,
        _cancellation_id: String,
    ) -> (bool, bool, HashMap<String, String>) {
        (true, false, HashMap::new())
    }
}

/// The manager's side: every target group change it is asked for, in order.
fn answer(mut rx: mpsc::Receiver<ManagerCommand>) -> Arc<Mutex<Vec<String>>> {
    let changes = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&changes);
    tokio::spawn(async move {
        while let Some(command) = rx.recv().await {
            let (change, sender) = match command {
                ManagerCommand::AddTargetGroup { name, sender } => (format!("add {name}"), sender),
                ManagerCommand::RemoveTargetGroup { name, sender } => {
                    (format!("remove {name}"), sender)
                }
                ManagerCommand::SetActiveTargetGroup { name, sender } => {
                    (format!("activate {name}"), sender)
                }
                _ => continue,
            };
            seen.lock().unwrap().push(change);
            let _ = sender.send(Ok(())).await;
        }
    });
    changes
}

/// Add, remove or activate `group`, as the bus answers: the error's name and
/// message when refused.
async fn change(client: &Connection, how: &str, group: &str) -> Result<(), (String, String)> {
    let reply = match how {
        "add" => {
            client
                .call_method(
                    Some(NAME),
                    PATH,
                    Some(INTERFACE),
                    "AddTargetGroup",
                    &(group,),
                )
                .await
        }
        "remove" => {
            client
                .call_method(
                    Some(NAME),
                    PATH,
                    Some(INTERFACE),
                    "RemoveTargetGroup",
                    &(group,),
                )
                .await
        }
        "activate" => {
            client
                .call_method(
                    Some(NAME),
                    PATH,
                    Some("org.freedesktop.DBus.Properties"),
                    "Set",
                    &(
                        INTERFACE,
                        "ActiveTargetGroup",
                        zbus::zvariant::Value::from(group),
                    ),
                )
                .await
        }
        _ => unreachable!("{how}"),
    };
    match reply {
        Ok(_) => Ok(()),
        Err(zbus::Error::MethodError(name, message, _)) => {
            Err((name.to_string(), message.unwrap_or_default()))
        }
        Err(e) => panic!("{how}: {e}"),
    }
}

/// Every one of `group`'s changes refused with AccessDenied, with `why` in it.
async fn refused(client: &Connection, group: &str, why: &str) {
    for how in ["add", "activate", "remove"] {
        let (name, message) = change(client, how, group).await.expect_err(how);
        assert_eq!(name, ACCESS_DENIED, "{how}: {message}");
        assert!(message.contains(why), "{how}: {message}");
    }
}

/// Every one of `group`'s changes allowed.
async fn allowed(client: &Connection, group: &str) {
    for how in ["add", "activate", "remove"] {
        change(client, how, group)
            .await
            .unwrap_or_else(|e| panic!("{how}: {e:?}"));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_the_listed_units_change_target_groups() {
    // SAFETY: getuid cannot fail.
    let uid = unsafe { libc::getuid() };
    assert_ne!(
        uid, 0,
        "run as a user: root may always change target groups"
    );
    let dir = tempfile::tempdir().unwrap();
    let bus = PrivateBus::start(dir.path());
    let settings = dir.path().join("inputplumber.yaml");
    let proc = dir.path().join("proc");
    let cgroup = proc.join(std::process::id().to_string()).join("cgroup");
    fs::create_dir_all(cgroup.parent().unwrap()).unwrap();
    let in_unit = |path: &str| {
        fs::write(
            &cgroup,
            format!("0::/user.slice/user-{uid}.slice/user@{uid}.service/{path}\n"),
        )
        .unwrap();
    };

    let (tx, rx) = mpsc::channel(8);
    let changes = answer(rx);
    let manager = ManagerInterface::new(tx, TargetGroups::default()).with_files(&settings, &proc);
    let _inputplumber = bus
        .connect()
        .name(NAME)
        .unwrap()
        .serve_at(PATH, manager)
        .unwrap()
        .build()
        .await
        .unwrap();
    let _polkit = bus
        .connect()
        .name("org.freedesktop.PolicyKit1")
        .unwrap()
        .serve_at("/org/freedesktop/PolicyKit1/Authority", Polkit)
        .unwrap()
        .build()
        .await
        .unwrap();
    let client = bus.connect().build().await.unwrap();
    let mine = format!("{uid}-desktop");

    // No settings: as before, any unit may change the user's own groups.
    in_unit("app.slice/kora-ws-session-4242.scope");
    allowed(&client, &mine).await;

    // Only the registry, now. A host command in a scope of its own is
    // refused, even one naming a cgroup inside its scope after the registry.
    fs::write(
        &settings,
        "target_group_writers:\n  - kora-workspaces.service\n",
    )
    .unwrap();
    refused(&client, &mine, "unit 'kora-ws-session-4242.scope'").await;
    in_unit("app.slice/run-u7.scope/kora-workspaces.service");
    refused(&client, &mine, "unit 'run-u7.scope'").await;
    // So is a process in no unit at all.
    fs::write(&cgroup, "0::/\n").unwrap();
    refused(&client, &mine, "no unit").await;

    // The registry changes its user's groups, and only those.
    in_unit("app.slice/kora-workspaces.service");
    allowed(&client, &mine).await;
    refused(&client, &format!("{}-desktop", uid + 1), "is not yours").await;

    // Settings that cannot be read refuse everyone but root, the registry too.
    fs::write(&settings, "target_group_writers: kora-workspaces.service\n").unwrap();
    refused(&client, &mine, "settings are unreadable").await;

    // Only what was allowed reached the manager.
    let expected: Vec<String> = ["add", "activate", "remove"]
        .iter()
        .map(|how| format!("{how} {mine}"))
        .collect();
    assert_eq!(
        *changes.lock().unwrap(),
        [expected.clone(), expected].concat()
    );
}
