//! The engine's own log. Every crate in the engine writes through [`line`].
//!
//! - Once [`install`]ed (when the Capsule has been accepted), lines go to
//!   `logs/engine.log`, rotated past [`ROTATE_AT`] with one previous file
//!   kept (`engine.log.1`), and also to the terminal when the engine runs in
//!   one. Before that, and for a Capsule that is refused, they go to
//!   standard error only, so a refused Capsule gets no log written into it.
//! - Every line also goes into a ring of the last [`RING_LINES`] lines,
//!   which a panic report carries.
//!
//! The engine logs state changes and errors, never what a person types or
//! what a model says.

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// How many lines the ring keeps: what a report carries.
pub const RING_LINES: usize = 50;
/// The log file is rotated past this size.
pub const ROTATE_AT: u64 = 1 << 20;
/// What each line starts with on standard error.
const TAG: &str = "[isonomic-capsule]";

static RING: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());
static FILE: OnceLock<Mutex<LogFile>> = OnceLock::new();

/// The engine's log file, rotated by size.
pub struct LogFile {
    path: PathBuf,
    file: Option<File>,
    written: u64,
    rotate_at: u64,
    echo: bool,
}

impl LogFile {
    /// The log at `path`, appended to, rotated past `rotate_at` bytes.
    pub fn open(path: &Path, rotate_at: u64) -> std::io::Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        let written = file.metadata()?.len();
        Ok(Self {
            path: path.to_path_buf(),
            file: Some(file),
            written,
            rotate_at,
            echo: std::io::stderr().is_terminal(),
        })
    }

    /// Append one line, rotating first if the file is full. A write that
    /// fails is dropped: the log must never stop the engine.
    pub fn write(&mut self, text: &str) {
        let bytes = text.len() as u64 + 1;
        if self.written > 0 && self.written + bytes > self.rotate_at {
            self.rotate();
        }
        if let Some(file) = &mut self.file {
            if writeln!(file, "{text}").is_ok() {
                self.written += bytes;
            }
        }
        if self.echo {
            eprintln!("{TAG} {text}");
        }
    }

    fn rotate(&mut self) {
        self.file = None;
        let previous = self.path.with_extension("log.1");
        let _ = std::fs::rename(&self.path, previous);
        self.file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .ok();
        self.written = 0;
    }
}

/// Write the engine's log to `logs_dir/engine.log` from now on. The first
/// call wins.
pub fn install(logs_dir: &Path) -> std::io::Result<()> {
    let file = LogFile::open(&logs_dir.join("engine.log"), ROTATE_AT)?;
    let _ = FILE.set(Mutex::new(file));
    Ok(())
}

/// Write one line to the log (or standard error before it is installed)
/// and the ring.
pub fn line(text: &str) {
    match FILE.get() {
        Some(file) => file.lock().unwrap_or_else(|p| p.into_inner()).write(text),
        None => eprintln!("{TAG} {text}"),
    }
    remember(text);
}

/// Keep `text` in the ring only.
pub fn remember(text: &str) {
    let mut ring = RING.lock().unwrap_or_else(|p| p.into_inner());
    if ring.len() == RING_LINES {
        ring.pop_front();
    }
    ring.push_back(text.to_string());
}

/// The ring's lines, oldest first. Never waits on the lock, so a panic hook
/// can call it while another thread holds it.
pub fn tail() -> Vec<String> {
    match RING.try_lock() {
        Ok(ring) => ring.iter().cloned().collect(),
        Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner().iter().cloned().collect(),
        Err(std::sync::TryLockError::WouldBlock) => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ring_keeps_the_last_lines() {
        for i in 0..(RING_LINES + 10) {
            line(&format!("ring test {i}"));
        }
        let tail = tail();
        assert_eq!(tail.len(), RING_LINES);
        assert_eq!(
            tail.last().unwrap(),
            &format!("ring test {}", RING_LINES + 9)
        );
    }

    #[test]
    fn the_file_rotates_keeping_one_previous_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("logs").join("engine.log");
        let mut log = LogFile::open(&path, 100).unwrap();
        log.echo = false;
        for i in 0..30 {
            log.write(&format!("line {i:02}"));
        }
        let now = std::fs::read_to_string(&path).unwrap();
        let before = std::fs::read_to_string(path.with_extension("log.1")).unwrap();
        assert!(
            now.len() <= 100 && before.len() <= 100,
            "{now:?} {before:?}"
        );
        assert!(now.ends_with("line 29\n"), "{now}");
        assert!(!dir.path().join("logs/engine.log.2").exists());
        // Reopening appends, and counts what is already there.
        let mut again = LogFile::open(&path, 100).unwrap();
        again.echo = false;
        assert_eq!(again.written, now.len() as u64);
    }
}
