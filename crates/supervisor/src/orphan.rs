//! Services left running by an engine that died without stopping them.
//!
//! A service runs in its own process group, so when the engine is killed
//! outright its services keep running. The next engine would then find the
//! old database holding the data directory and the old cache answering on
//! the socket. Each start therefore records the service's pid in its pid
//! file; before the next engine starts the service, it stops whatever that
//! file names, if it is still alive and still the same program.

use std::path::{Path, PathBuf};

use crate::service::ServiceConfig;
use crate::spawn::terminate_group;

/// Note the pid of the service just spawned.
pub(crate) fn record(config: &ServiceConfig, pid: u32) {
    if let Some(path) = &config.pid_file {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, format!("{pid}\n"));
    }
}

/// The service was stopped on purpose: nothing is left behind.
pub(crate) fn forget(config: &ServiceConfig) {
    if let Some(path) = &config.pid_file {
        let _ = std::fs::remove_file(path);
    }
}

/// Stop what an earlier engine left running, if anything. Returns its pid.
pub(crate) async fn reap(config: &ServiceConfig) -> Option<u32> {
    let path = config.pid_file.as_ref()?;
    let pid = std::fs::read_to_string(path)
        .ok()
        .and_then(|text| text.trim().parse::<u32>().ok());
    let _ = std::fs::remove_file(path);
    let pid = pid.filter(|pid| *pid > 1)?;
    if !alive(pid) || !same_program(pid, &config.binary_path) {
        return None;
    }
    // Its own stop signal first (fast shutdown for the database), then the
    // usual grace, then a kill.
    terminate_group(pid, config.stop_signal, config.stop_grace).await;
    Some(pid)
}

fn alive(pid: u32) -> bool {
    // SAFETY: signal 0 only checks that the process exists and is ours.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

/// A pid can be reused, so only a process running a program of the same
/// name counts. The name, not the path: an app that was moved still leaves
/// its own services behind.
fn same_program(pid: u32, binary: &Path) -> bool {
    match (program_of(pid), binary.file_name()) {
        (Some(running), Some(name)) => running.file_name() == Some(name),
        _ => false,
    }
}

#[cfg(target_os = "macos")]
fn program_of(pid: u32) -> Option<PathBuf> {
    let mut buffer = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: the buffer is valid for its full length.
    let length = unsafe {
        libc::proc_pidpath(
            pid as libc::c_int,
            buffer.as_mut_ptr().cast(),
            buffer.len() as u32,
        )
    };
    if length <= 0 {
        return None;
    }
    buffer.truncate(length as usize);
    String::from_utf8(buffer).ok().map(PathBuf::from)
}

#[cfg(not(target_os = "macos"))]
fn program_of(pid: u32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/exe")).ok()
}
