// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! The engine's side of the harness host's remaining traits: time, the
//! person's locale, background work, and the parts V1 Chat does not use yet
//! (client tools, attachments), plus a model client for when no model is
//! available.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use harness_core::future::{iter, ready, BoxFuture, BoxStream};
use harness_core::{AppId, Clock, ErrorInfo, Millis, RequestContext, SessionId, ToolCall};
use harness_host::{
    CancelToken, ChannelError, ClientToolChannel, Locale, ModelCaps, ModelClient, Spawner,
};
use harness_log::{ModelEvent, ModelRequest};
use harness_toolkit::LocalZone;

/// The wall clock, in milliseconds.
#[derive(Debug, Default)]
pub struct WallClock;

impl Clock for WallClock {
    fn now_ms(&self) -> Millis {
        let since = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        since.as_millis() as Millis
    }
}

/// Background work on the engine's Tokio runtime.
#[derive(Debug, Default)]
pub struct TokioSpawner;

impl Spawner for TokioSpawner {
    fn spawn(&self, task: BoxFuture<'static, ()>) {
        tokio::spawn(task);
    }

    fn sleep(&self, ms: u64) -> BoxFuture<'static, ()> {
        Box::pin(tokio::time::sleep(Duration::from_millis(ms)))
    }
}

/// The person's locale as the engine sees it: a time zone name and its
/// offset, and a language. Read once at start; the engine restarts with
/// the machine, so a move across zones is picked up then.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineLocale {
    pub zone: String,
    pub offset_minutes: i32,
    pub language: String,
}

impl EngineLocale {
    /// From the system: the zone from `/etc/localtime`, its current offset
    /// from the C library, and the language from `LANG`. Falls back to UTC
    /// and English.
    pub fn from_system() -> Self {
        let zone = std::fs::read_link("/etc/localtime")
            .ok()
            .and_then(|target| zone_name(&target.to_string_lossy()))
            .unwrap_or_else(|| "UTC".to_string());
        let language = std::env::var("LANG")
            .ok()
            .and_then(|lang| language(&lang))
            .unwrap_or_else(|| "en".to_string());
        Self {
            zone,
            offset_minutes: local_offset_minutes(),
            language,
        }
    }
}

/// `…/zoneinfo/Europe/Lisbon` → `Europe/Lisbon`.
fn zone_name(target: &str) -> Option<String> {
    let (_, name) = target.split_once("zoneinfo/")?;
    (!name.is_empty()).then(|| name.to_string())
}

/// `pt_PT.UTF-8` → `pt`; `C` and `POSIX` say nothing.
fn language(lang: &str) -> Option<String> {
    let code = lang.split(['_', '.', '@']).next()?.to_ascii_lowercase();
    let plain = code.len() >= 2 && code.chars().all(|c| c.is_ascii_lowercase());
    (plain && code != "c" && code != "posix").then_some(code)
}

fn local_offset_minutes() -> i32 {
    // SAFETY: `time` and `localtime_r` only write to the values passed in.
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&now, &mut tm).is_null() {
            return 0;
        }
        (tm.tm_gmtoff / 60) as i32
    }
}

/// `YYYY-MM-DD` for a local day count from 1970-01-01.
fn civil_date(days: i64) -> String {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

impl Locale for EngineLocale {
    fn context(&self, _: &AppId, _: &SessionId, now: Millis, _opt_in: bool) -> RequestContext {
        let local_s = now as i64 / 1000 + i64::from(self.offset_minutes) * 60;
        RequestContext {
            date: civil_date(local_s.div_euclid(86_400)),
            time_zone: self.zone.clone(),
            language: self.language.clone(),
            // The engine has no location source yet, opted in or not.
            location: None,
        }
    }
}

impl LocalZone for EngineLocale {
    fn zone(&self, _: &SessionId, _: Millis) -> (String, i32) {
        (self.zone.clone(), self.offset_minutes)
    }
}

/// No client tools yet: V1 Chat registers none. A call is refused, and no
/// app counts as connected for one.
#[derive(Debug, Default)]
pub struct NoClientTools;

impl ClientToolChannel for NoClientTools {
    fn invoke(
        &self,
        _: &AppId,
        _: &SessionId,
        _: &ToolCall,
    ) -> BoxFuture<'static, Result<(), ChannelError>> {
        ready(Err(ChannelError("client tools are not served yet".into())))
    }

    fn is_connected(&self, _: &AppId) -> bool {
        false
    }
}

/// The model client while the engine has no model gateway: every request
/// fails with a readable error, recorded in the session's log.
#[derive(Debug, Default)]
pub struct NoModel;

impl ModelClient for NoModel {
    fn caps(&self, _: &str) -> ModelCaps {
        ModelCaps {
            prefill: false,
            reasoning: false,
            tools: false,
            context_tokens: 0,
        }
    }

    fn stream(
        &self,
        _: ModelRequest,
        _: CancelToken,
    ) -> BoxStream<'static, Result<ModelEvent, ErrorInfo>> {
        let error = ErrorInfo::new("model_unavailable", "No model is available yet.");
        iter(vec![Err(error)])
    }
}

/// A shareable model client.
pub type Model = Arc<dyn ModelClient>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_locale_reads_zone_language_and_local_date() {
        assert_eq!(
            zone_name("/var/db/timezone/zoneinfo/Europe/Lisbon").as_deref(),
            Some("Europe/Lisbon")
        );
        assert_eq!(zone_name("/etc/whatever"), None);
        assert_eq!(language("pt_PT.UTF-8").as_deref(), Some("pt"));
        assert_eq!(language("C.UTF-8"), None);
        let locale = EngineLocale {
            zone: "America/New_York".into(),
            offset_minutes: -300,
            language: "en".into(),
        };
        let app = AppId::from("com.isonomic.chat");
        let s: SessionId = serde_json::from_value(serde_json::json!("s-1")).unwrap();
        // 2026-11-15 02:00 UTC is still the 14th in New York.
        let at = 1_794_708_000_000;
        let context = locale.context(&app, &s, at, false);
        assert_eq!(context.date, "2026-11-14");
        assert_eq!(context.time_zone, "America/New_York");
    }
}
