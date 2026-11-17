// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Nothing a service runs outlives the engine.
//!
//! A service runs in its own process group, so when the engine is killed
//! outright (SIGKILL, a crash) the group keeps running: the database holds
//! the Capsule's data and its memory until the next engine reaps it. A
//! guardian is a small shell beside each service, reading a pipe whose
//! write end only the engine holds. The kernel closes that end when the
//! engine dies, however it dies. The guardian then stops the service as
//! a deliberate stop would: the service's own stop signal first (the
//! database's fast shutdown, a clean checkpoint), and a hard kill once the
//! stop grace has passed.
//!
//! A deliberate stop never wakes it. The supervisor drops a child's guardian
//! once that child is reaped. Dropping kills the shell before the pipe
//! closes, so it never signals a group whose number may be reused.

use std::process::Stdio;
use std::time::Duration;

use tokio::process::{Child, Command};

use crate::service::StopSignal;

// `$1` the group, `$2` the stop signal's name, `$3` the grace in seconds.
// `kill -s NAME -- -PGID` is the POSIX spelling, and `kill` is a shell
// builtin; `sleep` is named in full, as the guardian has no PATH.
const SCRIPT: &str = "trap '' TERM INT HUP; read ignored; \
    kill -s \"$2\" -- \"-$1\" 2>/dev/null; i=0; \
    while [ \"$i\" -lt \"$3\" ] && kill -s 0 -- \"-$1\" 2>/dev/null; do /bin/sleep 1; i=$((i+1)); done; \
    kill -s KILL -- \"-$1\" 2>/dev/null";

/// One service child's guardian. Hold it for as long as the child runs.
pub(crate) struct Guardian {
    /// The service's process group.
    pub(crate) pgid: u32,
    // Field order matters: the shell is killed before the pipe closes.
    _shell: Child,
    _alive: std::io::PipeWriter,
}

fn signal_name(signal: StopSignal) -> &'static str {
    match signal {
        StopSignal::Term => "TERM",
        StopSignal::Int => "INT",
    }
}

impl Guardian {
    /// Watch the service whose process group is `pgid`.
    pub(crate) fn watch(pgid: u32, signal: StopSignal, grace: Duration) -> std::io::Result<Self> {
        let (reader, writer) = std::io::pipe()?;
        let grace = grace.as_secs() + u64::from(grace.subsec_nanos() > 0);
        let mut cmd = std::process::Command::new("/bin/sh");
        cmd.arg("-c")
            .arg(SCRIPT)
            .arg("isonomic-service-guardian")
            .arg(pgid.to_string())
            .arg(signal_name(signal))
            .arg(grace.max(1).to_string())
            // It needs nothing from the engine's environment.
            .env_clear()
            .stdin(Stdio::from(reader))
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        {
            use std::os::unix::process::CommandExt;
            // Its own group: stopping the service's group leaves it alone.
            cmd.process_group(0);
        }
        let mut cmd = Command::from(cmd);
        cmd.kill_on_drop(true);
        Ok(Self {
            pgid,
            _shell: cmd.spawn()?,
            _alive: writer,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn group_alive(pgid: u32) -> bool {
        // SAFETY: signal 0 only checks that the group exists.
        unsafe { libc::kill(-(pgid as libc::pid_t), 0) == 0 }
    }

    /// A process in its own group that ignores `ignore`, if given.
    fn service(ignore: Option<&str>) -> std::process::Child {
        use std::os::unix::process::CommandExt;
        let trap = ignore.map_or(String::new(), |s| format!("trap '' {s}; "));
        std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(format!("{trap}while :; do /bin/sleep 1; done"))
            .process_group(0)
            .spawn()
            .unwrap()
    }

    /// Wait for the group to end; the limit is only a safety net.
    fn ended(child: &mut std::process::Child, pgid: u32, within: Duration) -> bool {
        let deadline = Instant::now() + within;
        while Instant::now() < deadline {
            let _ = child.try_wait();
            if !group_alive(pgid) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    /// The engine dying closes the pipe; here, the write end alone is
    /// closed while the shell keeps running, as the kernel would do.
    fn engine_dies(guardian: Guardian) -> Child {
        let Guardian { _shell, _alive, .. } = guardian;
        drop(_alive);
        _shell
    }

    #[tokio::test]
    async fn the_engine_dying_stops_its_service_with_the_stop_signal() {
        let mut child = service(None);
        let pgid = child.id();
        let guardian = Guardian::watch(pgid, StopSignal::Int, Duration::from_secs(5)).unwrap();
        let _shell = engine_dies(guardian);
        assert!(
            ended(&mut child, pgid, Duration::from_secs(4)),
            "stopped politely"
        );
    }

    #[tokio::test]
    async fn a_service_that_ignores_its_signal_is_killed_after_the_grace() {
        let mut child = service(Some("INT"));
        let pgid = child.id();
        let guardian = Guardian::watch(pgid, StopSignal::Int, Duration::from_secs(1)).unwrap();
        let _shell = engine_dies(guardian);
        assert!(
            ended(&mut child, pgid, Duration::from_secs(10)),
            "killed after the grace"
        );
    }

    #[tokio::test]
    async fn dropping_the_guardian_leaves_the_service_alone() {
        let mut child = service(None);
        let pgid = child.id();
        drop(Guardian::watch(pgid, StopSignal::Term, Duration::from_secs(1)).unwrap());
        assert!(
            !ended(&mut child, pgid, Duration::from_millis(500)),
            "still running"
        );
        child.kill().unwrap();
        let _ = child.wait();
    }
}
