use super::*;

/// The group part of the Manager, as the daemon serves it.
#[derive(Default)]
struct FakeManager {
    groups: Vec<String>,
    active: String,
}

#[zbus::interface(name = "org.shadowblip.InputManager")]
impl FakeManager {
    #[zbus(property)]
    fn target_groups(&self) -> Vec<String> {
        self.groups.clone()
    }

    #[zbus(property)]
    fn active_target_group(&self) -> String {
        self.active.clone()
    }

    #[zbus(property)]
    fn set_active_target_group(&mut self, name: String) -> zbus::Result<()> {
        if !is_valid_group_name(&name) {
            return Err(zbus::fdo::Error::InvalidArgs(name).into());
        }
        if !self.groups.contains(&name) {
            self.groups.push(name.clone());
        }
        self.active = name;
        Ok(())
    }

    fn add_target_group(&mut self, name: String) {
        if !self.groups.contains(&name) {
            self.groups.push(name);
        }
    }

    fn remove_target_group(&mut self, name: String) -> zbus::fdo::Result<()> {
        if name == self.active && self.groups.len() > 1 {
            return Err(zbus::fdo::Error::InvalidArgs(name));
        }
        self.groups.retain(|g| *g != name);
        Ok(())
    }
}

/// A client connection to a fake Manager, peer to peer.
async fn connect() -> (zbus::Connection, zbus::Connection) {
    let (ours, theirs) = tokio::net::UnixStream::pair().unwrap();
    let server = zbus::connection::Builder::unix_stream(theirs)
        .server(zbus::Guid::generate())
        .unwrap()
        .p2p()
        .serve_at(MANAGER_PATH, FakeManager::default())
        .unwrap()
        .build();
    let client = zbus::connection::Builder::unix_stream(ours).p2p().build();
    let (server, client) = tokio::try_join!(server, client).unwrap();
    (server, client)
}

#[tokio::test]
async fn the_proxy_speaks_the_target_group_api() {
    let (_server, client) = connect().await;
    let manager = InputManagerProxy::builder(&client)
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()
        .await
        .unwrap();

    assert!(manager.target_groups().await.unwrap().is_empty());
    manager.add_target_group("1000-default").await.unwrap();
    manager
        .set_active_target_group("1000-gameframe")
        .await
        .unwrap();
    assert_eq!(
        manager.target_groups().await.unwrap(),
        ["1000-default", "1000-gameframe"]
    );
    assert_eq!(
        manager.active_target_group().await.unwrap(),
        "1000-gameframe"
    );

    assert!(manager.remove_target_group("1000-gameframe").await.is_err());
    manager.remove_target_group("1000-default").await.unwrap();
    assert_eq!(manager.target_groups().await.unwrap(), ["1000-gameframe"]);
    assert!(manager.set_active_target_group("no/slash").await.is_err());
}

#[test]
fn group_names_follow_the_daemons_rules() {
    for good in [
        "default",
        "1000-gameframe",
        "a.b_c-9",
        &"x".repeat(GROUP_NAME_MAX),
    ] {
        assert!(is_valid_group_name(good), "{good}");
    }
    for bad in [
        "",
        ".",
        "..",
        "a/b",
        "a b",
        "é",
        &"x".repeat(GROUP_NAME_MAX + 1),
    ] {
        assert!(!is_valid_group_name(bad), "{bad}");
    }
}

#[test]
fn each_group_has_its_view() {
    assert_eq!(
        group_view("1000-default"),
        PathBuf::from("/dev/inputplumber/groups/1000-default")
    );
}
