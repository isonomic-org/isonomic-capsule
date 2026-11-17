// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! A reply after a stop lands on the slot that holds the conversation.
//!
//! The chat worker runs two slots. When the person stops a reply, the
//! engine drops its request at once, but the model server frees that slot
//! only when it next tries to send the stopped reply a token. A continue
//! (or a new prompt) sent before then finds the conversation's slot busy,
//! goes to the other one, which holds no cache of the conversation, and
//! reads the whole prompt again: about 5 s on the Intel Mac, against 0.2 s.
//!
//! So the engine remembers when a reply was stopped and how far apart its
//! last tokens came, and the next reply waits until about one token gap
//! has passed since the stop (at most [`MOST`]) before it is sent.

use std::sync::Mutex;
use std::time::{Duration, Instant};

/// The longest a reply ever waits for a stopped reply's slot.
pub const MOST: Duration = Duration::from_millis(600);
/// What a gap is taken to be before two tokens have come.
pub const UNKNOWN_GAP: Duration = Duration::from_millis(400);
/// Added to the gap, for the server to notice and free the slot.
const MARGIN: Duration = Duration::from_millis(50);

/// When the last reply was stopped, and its last gap between tokens.
#[derive(Default)]
pub struct Settle {
    stopped: Mutex<Option<(Instant, Duration)>>,
}

impl Settle {
    /// A reply was stopped, its last tokens `gap` apart (`None`: fewer
    /// than two came).
    pub fn stopped(&self, gap: Option<Duration>) {
        let gap = gap.unwrap_or(UNKNOWN_GAP);
        *self.stopped.lock().unwrap_or_else(|p| p.into_inner()) = Some((Instant::now(), gap));
    }

    /// How long a reply sent now should wait, if at all.
    pub fn wait(&self) -> Option<Duration> {
        let (at, gap) = (*self.stopped.lock().unwrap_or_else(|p| p.into_inner()))?;
        let until = (gap + MARGIN).min(MOST);
        until.checked_sub(at.elapsed()).filter(|d| !d.is_zero())
    }

    /// Wait as [`Settle::wait`] says, and say so in the engine's log.
    pub async fn settle(&self) {
        if let Some(wait) = self.wait() {
            capsule_core::log::line(&format!(
                "reply: waited {} ms for the stopped reply's slot",
                wait.as_millis()
            ));
            tokio::time::sleep(wait).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reply_waits_about_one_token_after_a_stop_and_never_long() {
        let settle = Settle::default();
        assert_eq!(settle.wait(), None, "no stop, no wait");
        settle.stopped(Some(Duration::from_millis(200)));
        let wait = settle.wait().unwrap();
        assert!(
            wait <= Duration::from_millis(250) && wait > Duration::from_millis(200),
            "{wait:?}"
        );
        settle.stopped(Some(Duration::from_secs(5)));
        assert!(settle.wait().unwrap() <= MOST);
        settle.stopped(None);
        assert!(settle.wait().unwrap() <= UNKNOWN_GAP + MARGIN);
    }
}
