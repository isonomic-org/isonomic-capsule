//! The engine's own log: each line goes to standard error, which the login
//! agent keeps in `logs/engine.log`, and into a ring of the last
//! [`LOG_LINES`](crate::LOG_LINES) lines, which a panic report carries. The
//! engine logs state changes and errors, never what a person types or what
//! a model says.

use std::collections::VecDeque;
use std::sync::Mutex;

use crate::LOG_LINES;

static RING: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());

/// Write one line to standard error and the ring.
pub fn line(text: &str) {
    eprintln!("[isonomic-capsule] {text}");
    remember(text);
}

/// Keep `text` in the ring only.
pub fn remember(text: &str) {
    let mut ring = RING.lock().unwrap_or_else(|p| p.into_inner());
    if ring.len() == LOG_LINES {
        ring.pop_front();
    }
    ring.push_back(text.to_string());
}

/// The ring's lines, oldest first. Never blocks on a poisoned lock, so the
/// panic hook can call it.
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
        for i in 0..(LOG_LINES + 10) {
            remember(&format!("ring test {i}"));
        }
        let tail = tail();
        assert_eq!(tail.len(), LOG_LINES);
        assert_eq!(
            tail.last().unwrap(),
            &format!("ring test {}", LOG_LINES + 9)
        );
    }
}
