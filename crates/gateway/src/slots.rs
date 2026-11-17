// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Whether the model server has finished a stopped reply.
//!
//! The chat worker runs two slots. When the person stops a reply, the
//! engine drops its request, but the server ends that task only when it
//! next tries to send it a token. A continue sent before then finds the
//! conversation's slot busy and goes to the other slot, which holds no
//! cache of the conversation: on the Intel Mac that is 3 s of prompt
//! reading instead of a few milliseconds.
//!
//! So after a stop, the next reply asks the server's own slot list
//! (`GET /slots`) until no slot is working, at most [`LIMIT`], before it is
//! sent. The server then gives it the slot whose cache matches it best:
//! the conversation's.

use std::time::{Duration, Instant};

use isonomic_llama::Endpoint;

/// The longest a reply waits for the server's slots.
pub const LIMIT: Duration = Duration::from_secs(1);
/// How often the slot list is asked.
pub const EVERY: Duration = Duration::from_millis(20);
/// How long after a stop the next reply still checks.
pub const AFTER_STOP: Duration = Duration::from_secs(3);

/// What waiting for the slots came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlotWait {
    /// Every slot was idle after this long.
    Idle(Duration),
    /// A slot was still working at the limit; the reply goes anyway.
    StillBusy,
    /// The server gave no slot list, and why.
    Unavailable(String),
}

/// Whether any slot in the server's slot list is working; `None` when it is
/// not a slot list.
pub fn busy(slots: &serde_json::Value) -> Option<bool> {
    let slots = slots.as_array()?;
    let mut any = false;
    for slot in slots {
        any |= slot.get("is_processing")?.as_bool()?;
    }
    Some(any)
}

/// Ask `endpoint`'s slot list until no slot is working, at most `limit`.
pub async fn wait_idle(endpoint: &Endpoint, limit: Duration) -> SlotWait {
    let client = reqwest::Client::new();
    let url = format!("{}/slots", endpoint.base_url());
    let started = Instant::now();
    loop {
        let mut request = client.get(&url).timeout(limit);
        if let Some(key) = &endpoint.api_key {
            request = request.bearer_auth(key);
        }
        let answer = match request.send().await {
            Ok(response) if response.status().is_success() => response.json().await,
            Ok(response) => return SlotWait::Unavailable(format!("status {}", response.status())),
            Err(e) => return SlotWait::Unavailable(e.to_string()),
        };
        match answer.ok().as_ref().and_then(busy) {
            Some(false) => return SlotWait::Idle(started.elapsed()),
            Some(true) if started.elapsed() >= limit => return SlotWait::StillBusy,
            Some(true) => tokio::time::sleep(EVERY).await,
            None => return SlotWait::Unavailable("not a slot list".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_slot_list_says_whether_a_slot_is_working() {
        let working = json!([{"id": 0, "is_processing": true}, {"id": 1, "is_processing": false}]);
        assert_eq!(busy(&working), Some(true));
        let idle = json!([{"id": 0, "is_processing": false}, {"id": 1, "is_processing": false}]);
        assert_eq!(busy(&idle), Some(false));
        assert_eq!(busy(&json!({"error": "not found"})), None);
        assert_eq!(busy(&json!([{"id": 0}])), None);
    }
}
