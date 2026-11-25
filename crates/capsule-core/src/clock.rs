// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Injectable time. Engine code that stamps time takes a `Clock`, so no test
//! depends on the wall clock.

use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

pub trait Clock: Send + Sync {
    /// Seconds since the Unix epoch.
    fn now_unix(&self) -> i64;
}

/// The real clock.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_unix(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
    }
}

/// A clock that only moves when a test moves it.
#[derive(Debug, Default)]
pub struct ManualClock(AtomicI64);

impl ManualClock {
    pub fn at(unix: i64) -> Self {
        Self(AtomicI64::new(unix))
    }

    pub fn set(&self, unix: i64) {
        self.0.store(unix, Ordering::SeqCst);
    }

    pub fn advance(&self, secs: i64) {
        self.0.fetch_add(secs, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now_unix(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_clock_moves_only_when_told() {
        let clock = ManualClock::at(1_000);
        assert_eq!(clock.now_unix(), 1_000);
        clock.advance(30);
        assert_eq!(clock.now_unix(), 1_030);
        clock.set(5);
        assert_eq!(clock.now_unix(), 5);
    }

    #[test]
    fn system_clock_is_after_2020() {
        assert!(SystemClock.now_unix() > 1_577_836_800);
    }
}
