use super::*;

const REGISTRY: &str = "kora-workspaces.service";

fn writers(units: &[&str]) -> Vec<String> {
    units.iter().map(|u| u.to_string()).collect()
}

fn user_unit(path: &str) -> String {
    format!("0::/user.slice/user-1000.slice/user@1000.service/{path}\n")
}

#[test]
fn a_user_unit_is_named_below_the_users_manager() {
    let cgroup = user_unit("app.slice/kora-workspaces.service");
    assert_eq!(unit_of(&cgroup, 1000).as_deref(), Some(REGISTRY));
    assert!(is_writer(
        &writers(&[REGISTRY]),
        unit_of(&cgroup, 1000).as_deref()
    ));
    // Slices around it are no part of the name.
    let sliced = user_unit("app.slice/app-kora.slice/kora-workspaces.service");
    assert_eq!(unit_of(&sliced, 1000).as_deref(), Some(REGISTRY));
}

#[test]
fn a_system_unit_is_named_below_the_system_slices() {
    let cgroup = "0::/system.slice/inputplumber.service\n";
    assert_eq!(unit_of(cgroup, 0).as_deref(), Some("inputplumber.service"));
    let login = "0::/user.slice/user-1000.slice/session-4.scope\n";
    assert_eq!(unit_of(login, 1000).as_deref(), Some("session-4.scope"));
}

#[test]
fn another_unit_is_refused() {
    let allowed = writers(&[REGISTRY]);
    for path in [
        "app.slice/kora-ws-session-4242.scope",
        "app.slice/kora-ws-elevate.service",
        "app.slice/kora-workspaces.service.d",
        "app.slice/kora-workspaces.scope",
        "init.scope",
    ] {
        let unit = unit_of(&user_unit(path), 1000);
        assert!(!is_writer(&allowed, unit.as_deref()), "{path}: {unit:?}");
    }
    // An SSH or console login, and a system unit of another name.
    for cgroup in [
        "0::/user.slice/user-1000.slice/session-7.scope\n",
        "0::/system.slice/sshd.service\n",
    ] {
        let unit = unit_of(cgroup, 1000);
        assert!(!is_writer(&allowed, unit.as_deref()), "{cgroup}");
    }
}

#[test]
fn a_cgroup_under_a_unit_belongs_to_that_unit() {
    let allowed = writers(&[REGISTRY]);
    // The registry's own sub-cgroup is still the registry.
    let own = user_unit("app.slice/kora-workspaces.service/worker");
    assert_eq!(unit_of(&own, 1000).as_deref(), Some(REGISTRY));
    // A scope that names a cgroup of its own after the registry is still
    // the scope, however deep.
    for path in [
        "app.slice/run-u12.scope/kora-workspaces.service",
        "app.slice/run-u12.scope/a.slice/kora-workspaces.service",
        "app.slice/kora-ws-session-1.scope/x/kora-workspaces.service",
    ] {
        let unit = unit_of(&user_unit(path), 1000);
        assert_ne!(unit.as_deref(), Some(REGISTRY), "{path}");
        assert!(!is_writer(&allowed, unit.as_deref()), "{path}");
    }
}

#[test]
fn another_users_manager_names_no_unit() {
    let theirs =
        "0::/user.slice/user-1001.slice/user@1001.service/app.slice/kora-workspaces.service\n";
    assert_eq!(unit_of(theirs, 1000), None);
    let odd =
        "0::/user.slice/user-1000.slice/user@1000x.service/app.slice/kora-workspaces.service\n";
    assert_eq!(unit_of(odd, 1000), None);
}

#[test]
fn a_malformed_cgroup_names_no_unit() {
    for cgroup in [
        "",
        "\n",
        // cgroup v1 only.
        "12:pids:/user.slice/user-1000.slice/user@1000.service/app.slice/kora-workspaces.service\n",
        // The root, or slices only.
        "0::/\n",
        "0::/user.slice/user-1000.slice\n",
        "0::/user.slice/user-1000.slice/user@1000.service\n",
        "0::/user.slice/user-1000.slice/user@1000.service/app.slice\n",
        // Not absolute, or with an empty name.
        "0::user.slice/user-1000.slice/user@1000.service/app.slice/kora-workspaces.service\n",
        "0::/user.slice//user@1000.service/app.slice/kora-workspaces.service\n",
        // A second unified line, as a cgroup named with a newline would make.
        "0::/user.slice/user-1000.slice/user@1000.service/app.slice/x.scope/a\n0::/system.slice/kora-workspaces.service\n",
    ] {
        assert_eq!(unit_of(cgroup, 1000), None, "{cgroup:?}");
        assert!(!is_writer(&writers(&[REGISTRY]), unit_of(cgroup, 1000).as_deref()));
    }
    // A cgroup removed under the process is not its unit's any more.
    let deleted = user_unit("app.slice/kora-workspaces.service (deleted)");
    assert!(!is_writer(
        &writers(&[REGISTRY]),
        unit_of(&deleted, 1000).as_deref()
    ));
}

#[test]
fn hybrid_hierarchies_use_the_unified_line() {
    let cgroup = "12:pids:/user.slice\n1:name=systemd:/user.slice/user-1000.slice/user@1000.service/app.slice/x.scope\n0::/user.slice/user-1000.slice/user@1000.service/app.slice/kora-workspaces.service\n";
    assert_eq!(unit_of(cgroup, 1000).as_deref(), Some(REGISTRY));
}

#[test]
fn without_a_list_every_caller_writes() {
    assert!(is_writer(&[], None));
    assert!(is_writer(&[], Some("session-7.scope")));
    assert!(!is_writer(&writers(&[REGISTRY]), None));
}

#[test]
fn the_callers_own_pidfd_names_it_while_it_lives() {
    let pidfd =
        unsafe { libc::syscall(libc::SYS_pidfd_open, std::process::id() as libc::c_int, 0) };
    assert!(pidfd >= 0, "{}", io::Error::last_os_error());
    // SAFETY: a pidfd just opened and owned here.
    let pidfd =
        unsafe { <std::os::fd::OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(pidfd as i32) };
    assert_eq!(pid_of(pidfd.as_fd()).unwrap(), std::process::id());
    assert!(alive(pidfd.as_fd()));
}
