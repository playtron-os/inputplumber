use std::{
    collections::HashMap,
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU32, Ordering},
};

use super::*;

/// A scratch directory removed when dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let path = std::env::temp_dir().join(format!(
            "inputplumber-target-groups-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// An input directory with nodes, and views of it.
fn fixture(nodes: &[&str]) -> (Scratch, PathBuf, GroupViews) {
    let scratch = Scratch::new();
    let input = scratch.0.join("input");
    fs::create_dir_all(input.join("by-id")).unwrap();
    for node in nodes {
        fs::write(input.join(node), node).unwrap();
    }
    let views = GroupViews::new(scratch.0.join("groups"), &input);
    (scratch, input, views)
}

fn listing(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.file_name().to_string_lossy().to_string())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

fn owners(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let map: HashMap<String, String> = pairs
        .iter()
        .map(|(node, group)| (node.to_string(), group.to_string()))
        .collect();
    move |node| map.get(node).cloned()
}

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

#[test]
fn names_are_checked() {
    for good in [
        "desktop",
        "1000-gameframe",
        "a.b_c-9",
        &"x".repeat(MAX_NAME_LEN),
    ] {
        assert!(validate_name(good).is_ok(), "{good}");
    }
    for bad in [
        "",
        ".",
        "..",
        "a/b",
        "a b",
        "é",
        &"x".repeat(MAX_NAME_LEN + 1),
    ] {
        assert!(validate_name(bad).is_err(), "{bad}");
    }
}

#[test]
fn the_phys_carries_the_group() {
    let phys = phys_for("1000-gameframe");
    assert!(phys.len() <= 64);
    assert_eq!(group_of_phys(&phys), Some("1000-gameframe"));
    assert_eq!(
        group_of_phys(&phys_for(&"x".repeat(MAX_NAME_LEN))).map(str::len),
        Some(40)
    );
    assert!(phys_for(&"x".repeat(MAX_NAME_LEN)).len() <= 64);
    assert_eq!(group_of_phys("usb-0000:00:14.0-1/input0"), None);
    assert_eq!(group_of_phys("inputplumber/group/a/b"), None);
    assert_eq!(group_of_phys(""), None);
}

#[test]
fn each_group_gets_its_own_persistent_id() {
    assert_ne!(
        persistent_id_for("abc", "one"),
        persistent_id_for("abc", "two")
    );
}

#[test]
fn the_first_group_becomes_active() {
    let mut groups = TargetGroups::default();
    assert_eq!(groups.active(), "");
    assert_eq!(groups.add("desktop"), Ok(true));
    assert_eq!(groups.add("desktop"), Ok(false));
    assert_eq!(groups.add("game"), Ok(true));
    assert_eq!(groups.active(), "desktop");
    assert_eq!(groups.names(), ["desktop", "game"]);
    assert!(groups.add("no/slash").is_err());
}

#[test]
fn activating_a_new_group_adds_it() {
    let mut groups = TargetGroups::default();
    assert_eq!(groups.set_active(""), Ok(false));
    assert_eq!(groups.set_active("game"), Ok(true));
    assert_eq!(groups.set_active("game"), Ok(false));
    assert_eq!(groups.set_active("desktop"), Ok(true));
    assert_eq!(groups.active(), "desktop");
    assert_eq!(groups.names(), ["game", "desktop"]);
    assert!(groups.set_active("").is_err());
}

#[test]
fn the_active_group_goes_only_last() {
    let mut groups = TargetGroups::default();
    groups.set_active("desktop").unwrap();
    groups.add("game").unwrap();
    assert!(groups.remove("desktop").is_err());
    assert_eq!(groups.remove("game"), Ok(true));
    assert_eq!(groups.remove("game"), Ok(false));
    assert_eq!(groups.remove("desktop"), Ok(true));
    assert_eq!(groups, TargetGroups::default());
}

#[test]
fn groups_survive_a_restart() {
    let scratch = Scratch::new();
    let path = scratch.0.join("state/target-groups");
    let mut groups = TargetGroups::default();
    groups.add("desktop").unwrap();
    groups.set_active("game").unwrap();
    groups.save(&path).unwrap();
    assert_eq!(TargetGroups::load(&path), groups);

    groups.remove("desktop").unwrap();
    groups.remove("game").unwrap();
    groups.save(&path).unwrap();
    assert!(!path.exists());
    assert_eq!(TargetGroups::load(&path), TargetGroups::default());
}

#[test]
fn a_damaged_state_file_loads_what_it_can() {
    let scratch = Scratch::new();
    let path = scratch.0.join("target-groups");
    fs::write(&path, "bad/name\ndesktop\nno way\n").unwrap();
    let groups = TargetGroups::load(&path);
    assert_eq!(groups.names(), ["desktop"]);
    assert_eq!(groups.active(), "desktop");
}

#[test]
fn a_view_holds_shared_nodes_and_its_own_gamepads() {
    let (_scratch, input, mut views) = fixture(&["event0", "event5", "event6", "js0", "mice"]);
    let owner = owners(&[("event5", "desktop"), ("event6", "game"), ("js0", "game")]);
    views
        .set_groups(&names(&["desktop", "game"]), owner)
        .unwrap();

    assert_eq!(
        listing(&views.path_of("desktop")),
        ["event0", "event5", "mice"]
    );
    assert_eq!(
        listing(&views.path_of("game")),
        ["event0", "event6", "js0", "mice"]
    );

    // The same node, not a copy: permissions and ACLs follow the original.
    let original = fs::metadata(input.join("event6")).unwrap();
    let linked = fs::metadata(views.path_of("game").join("event6")).unwrap();
    assert_eq!(
        (original.dev(), original.ino()),
        (linked.dev(), linked.ino())
    );
}

#[test]
fn nodes_plugged_in_later_appear_in_the_right_views() {
    let (_scratch, input, mut views) = fixture(&[]);
    views
        .set_groups(&names(&["desktop", "game"]), owners(&[]))
        .unwrap();
    let desktop = views.path_of("desktop");
    let game = views.path_of("game");

    fs::write(input.join("event7"), "").unwrap();
    views.node_added("event7", Some("game"));
    fs::write(input.join("event8"), "").unwrap();
    views.node_added("event8", None);
    fs::write(input.join("event9"), "").unwrap();
    views.node_added("event9", Some("gone"));

    assert_eq!(listing(&desktop), ["event8"]);
    assert_eq!(listing(&game), ["event7", "event8"]);

    fs::remove_file(input.join("event7")).unwrap();
    views.node_removed("event7");
    assert_eq!(listing(&game), ["event8"]);
}

#[test]
fn a_reused_node_name_never_keeps_the_old_device() {
    let (_scratch, input, mut views) = fixture(&["event3"]);
    let owner = owners(&[("event3", "game")]);
    views
        .set_groups(&names(&["desktop", "game"]), owner)
        .unwrap();
    assert_eq!(listing(&views.path_of("game")), ["event3"]);

    // The removal was missed, and the number went to another group's pad.
    fs::remove_file(input.join("event3")).unwrap();
    fs::write(input.join("event3"), "new").unwrap();
    views.node_added("event3", Some("desktop"));
    assert_eq!(listing(&views.path_of("game")), Vec::<String>::new());
    assert_eq!(listing(&views.path_of("desktop")), ["event3"]);
}

#[test]
fn a_node_already_moved_away_is_skipped() {
    let (_scratch, _input, mut views) = fixture(&[]);
    views.set_groups(&names(&["desktop"]), owners(&[])).unwrap();
    views.node_added("event4", None);
    assert_eq!(listing(&views.path_of("desktop")), Vec::<String>::new());
}

#[test]
fn views_follow_the_groups_and_keep_their_directories() {
    let (scratch, _input, mut views) = fixture(&["event0"]);
    views
        .set_groups(&names(&["desktop", "game"]), owners(&[]))
        .unwrap();
    let desktop = fs::metadata(views.path_of("desktop")).unwrap().ino();

    views.set_groups(&names(&["desktop"]), owners(&[])).unwrap();
    assert_eq!(listing(&scratch.0.join("groups")), ["desktop"]);
    assert_eq!(
        fs::metadata(views.path_of("desktop")).unwrap().ino(),
        desktop
    );

    views.set_groups(&[], owners(&[])).unwrap();
    assert!(!scratch.0.join("groups").exists());
}

#[test]
fn a_restarted_daemon_refills_the_views_it_finds() {
    let (scratch, input, mut views) = fixture(&["event0"]);
    views.set_groups(&names(&["game"]), owners(&[])).unwrap();
    let dir = views.path_of("game");
    let ino = fs::metadata(&dir).unwrap().ino();
    // Left by the last run: a node that is gone, and a stray file.
    fs::write(dir.join("event9"), "stale").unwrap();

    let mut restarted = GroupViews::new(scratch.0.join("groups"), &input);
    restarted
        .set_groups(&names(&["game"]), owners(&[]))
        .unwrap();
    assert_eq!(listing(&dir), ["event0"]);
    assert_eq!(fs::metadata(&dir).unwrap().ino(), ino);
}

#[test]
fn odd_node_names_are_ignored() {
    let (_scratch, _input, mut views) = fixture(&[]);
    views.set_groups(&names(&["desktop"]), owners(&[])).unwrap();
    views.node_added("../escape", None);
    views.node_removed("..");
    assert_eq!(listing(&views.path_of("desktop")), Vec::<String>::new());
}

fn kind(id: &str) -> TargetDeviceTypeId {
    TargetDeviceTypeId::try_from(id).unwrap()
}

#[test]
fn without_groups_there_is_one_set_of_targets() {
    let groups = TargetGroups::default();
    let types = [kind("xbox-elite"), kind("mouse"), kind("keyboard")];
    assert_eq!(
        groups.targets_for(&types),
        types.iter().map(|k| (*k, None)).collect::<Vec<_>>()
    );
    assert!(groups.receives_input(None));
}

#[test]
fn each_group_gets_its_own_gamepads_and_shares_the_rest() {
    let mut groups = TargetGroups::default();
    groups.set_active("desktop").unwrap();
    groups.add("game").unwrap();
    let types = [
        kind("xbox-elite"),
        kind("mouse"),
        kind("deck-uhid"),
        kind("touchscreen"),
    ];
    assert_eq!(
        groups.targets_for(&types),
        vec![
            (kind("xbox-elite"), Some("desktop".to_string())),
            (kind("xbox-elite"), Some("game".to_string())),
            (kind("mouse"), None),
            (kind("deck-uhid"), Some("desktop".to_string())),
            (kind("deck-uhid"), Some("game".to_string())),
            (kind("touchscreen"), None),
        ]
    );
}

#[test]
fn only_the_active_groups_gamepads_receive_input() {
    let mut groups = TargetGroups::default();
    groups.set_active("desktop").unwrap();
    groups.add("game").unwrap();
    assert!(groups.receives_input(Some("desktop")));
    assert!(!groups.receives_input(Some("game")));
    assert!(groups.receives_input(None), "shared targets always do");

    groups.set_active("game").unwrap();
    assert!(!groups.receives_input(Some("desktop")));
    assert!(groups.receives_input(Some("game")));
}
