use super::*;

fn load(text: Option<&str>) -> Result<Settings, String> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("inputplumber.yaml");
    if let Some(text) = text {
        fs::write(&path, text).unwrap();
    }
    Settings::load(&path)
}

#[test]
fn no_file_or_no_list_lets_any_caller_write() {
    for text in [
        None,
        Some(""),
        Some("# nothing yet\n"),
        Some("target_group_writers: []\n"),
    ] {
        assert_eq!(load(text).unwrap(), Settings::default(), "{text:?}");
    }
}

#[test]
fn the_writers_are_read() {
    let settings = load(Some(
        "target_group_writers:\n  - kora-workspaces.service\n  - other.scope\n",
    ))
    .unwrap();
    assert_eq!(
        settings.target_group_writers,
        ["kora-workspaces.service", "other.scope"]
    );
}

#[test]
fn a_mistake_is_an_error_not_the_defaults() {
    for text in [
        "target_group_writers: kora-workspaces.service\n",
        "target_group_writers: [\n",
        "target_groups_writers:\n  - kora-workspaces.service\n",
        "- kora-workspaces.service\n",
    ] {
        assert!(load(Some(text)).is_err(), "{text}");
    }
    let dir = tempfile::tempdir().unwrap();
    // A directory where the file should be cannot be read.
    assert!(Settings::load(dir.path()).is_err());
}
