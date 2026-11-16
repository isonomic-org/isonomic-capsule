//! The engine's own log. Every crate in the engine writes through [`line`]:
//! each line goes to standard error, which the login agent keeps in
//! `logs/engine.log`, and into a ring of the last [`RING_LINES`] lines,
//! which a panic report carries. The engine logs state changes and errors,
//! never what a person types or what a model says.

use std::collections::VecDeque;
use std::sync::Mutex;

/// How many lines the ring keeps: what a report carries.
pub const RING_LINES: usize = 50;

static RING: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());

/// Write one line to standard error and the ring.
pub fn line(text: &str) {
    eprintln!("[isonomic-capsule] {text}");
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
}
