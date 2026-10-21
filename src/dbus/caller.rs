//! Who is calling: the uid and the systemd unit of a DBus caller.
//!
//! polkit tells callers apart by user and session only, so two processes of
//! the same user in the same session look alike to it. Their systemd units
//! differ, and the unit is the first name under the slices in the caller's
//! cgroup.

use std::{
    fs, io,
    os::fd::{AsFd, AsRawFd, BorrowedFd},
    path::Path,
};

use zbus::{
    fdo::{self, ConnectionCredentials},
    message::Header,
    names::BusName,
    proxy::CacheProperties,
    Connection,
};

/// What the bus knows about the sender of `hdr`.
pub async fn credentials(
    conn: &Connection,
    hdr: &Header<'_>,
) -> fdo::Result<ConnectionCredentials> {
    let sender = hdr
        .sender()
        .ok_or_else(|| fdo::Error::AccessDenied("Missing sender".into()))?;
    fdo::DBusProxy::builder(conn)
        .cache_properties(CacheProperties::No)
        .build()
        .await?
        .get_connection_credentials(BusName::from(sender.to_owned()))
        .await
}

/// The caller's `cgroup` file under `proc` (normally `/proc`). With the pidfd
/// the bus hands over (`ProcessFD`), the file read is the caller's own: its pid
/// cannot pass to another process while the pidfd still names a live one.
/// Without one, the caller's pid is all there is.
pub fn cgroup(proc: &Path, creds: &ConnectionCredentials) -> io::Result<String> {
    let Some(pidfd) = creds.process_fd() else {
        let pid = creds
            .process_id()
            .ok_or_else(|| io::Error::other("the bus gave no process for the caller"))?;
        return fs::read_to_string(proc.join(pid.to_string()).join("cgroup"));
    };
    let pidfd = pidfd.as_fd();
    let pid = pid_of(pidfd)?;
    let cgroup = fs::read_to_string(proc.join(pid.to_string()).join("cgroup"))?;
    if !alive(pidfd) || pid_of(pidfd)? != pid {
        return Err(io::Error::other("the caller exited while being checked"));
    }
    Ok(cgroup)
}

/// The pid a pidfd names, from its fdinfo; an error once the process is gone.
fn pid_of(pidfd: BorrowedFd<'_>) -> io::Result<u32> {
    let info = fs::read_to_string(format!("/proc/self/fdinfo/{}", pidfd.as_raw_fd()))?;
    info.lines()
        .find_map(|line| line.strip_prefix("Pid:"))
        .and_then(|pid| pid.trim().parse::<i64>().ok())
        .and_then(|pid| u32::try_from(pid).ok())
        .filter(|pid| *pid > 0)
        .ok_or_else(|| io::Error::other("the caller exited while being checked"))
}

/// Whether the process a pidfd names still exists, by sending it no signal.
fn alive(pidfd: BorrowedFd<'_>) -> bool {
    // SAFETY: signal 0 only checks the process; no siginfo is passed.
    let sent = unsafe {
        libc::syscall(
            libc::SYS_pidfd_send_signal,
            pidfd.as_raw_fd(),
            0,
            std::ptr::null::<libc::siginfo_t>(),
            0,
        )
    };
    sent == 0
}

/// The systemd unit a process of `uid` runs in, from its `/proc/<pid>/cgroup`:
/// for a process of the user's own manager (`user@<uid>.service`) the user
/// unit, else the system unit. The unit is the first name under the slices: a
/// unit may hold cgroups of any name under its own, so a deeper name says
/// nothing. `None` unless the file holds exactly one cgroup v2 line naming a
/// unit, and for a process in another user's manager.
pub fn unit_of(cgroup: &str, uid: u32) -> Option<String> {
    let mut unified = cgroup.lines().filter_map(|line| line.strip_prefix("0::"));
    let path = unified.next()?;
    if unified.next().is_some() {
        return None;
    }
    let path = path.strip_prefix('/')?;
    if path.is_empty() {
        return None;
    }
    let mut names = path.split('/');
    if names.clone().any(str::is_empty) {
        return None;
    }
    let mut below_slices = || names.find(|name| !name.ends_with(".slice"));
    let mut unit = below_slices()?;
    if let Some(manager) = unit
        .strip_prefix("user@")
        .and_then(|rest| rest.strip_suffix(".service"))
    {
        if manager != uid.to_string() {
            return None;
        }
        unit = below_slices()?;
    }
    Some(unit.to_string())
}

/// Whether a caller in `unit` may change target groups, given the `writers`
/// units allowed to: any caller when there is no list.
pub fn is_writer(writers: &[String], unit: Option<&str>) -> bool {
    writers.is_empty() || unit.is_some_and(|unit| writers.iter().any(|w| w == unit))
}

#[cfg(test)]
#[path = "caller_test.rs"]
mod tests;
