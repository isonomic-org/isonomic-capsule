//! Every data call an app makes goes through the check function (R7,
//! step 4), after the manifest's own checks (`proto::auth`).
//!
//! A call becomes an action on a resource:
//!
//! | Calls | Action | Resource |
//! |---|---|---|
//! | `blobs.get`, `blobs.info` | read | the app's space |
//! | `blobs.put`, `create`, `write`, `delete` | write | the app's space |
//! | `sql.query`, `sql.subscribe` | read | the schema it runs in |
//! | `sql.execute`, `sql.batch` | write | the schema it runs in |
//! | `events.subscribe` | read | the app's events |
//! | `events.publish` | write | the app's events |
//! | agents calls that only look (list, open, subscribe, presets and artifacts read) | read | the app's space |
//! | every other agents call | write | the app's space |
//!
//! Blobs and sessions are checked against the space that holds them: every
//! grant in version 1 covers whole spaces, and a space's grant covers the
//! space and everything in it, so the answer is the same as for each one.
//! Records of their own are needed only once a grant can name one blob or
//! session. Unsubscribing, models, and the shell's own calls are not checked
//! here.

use id_grants::Action;
use isonomic_proto::{Call, Manifest};

use crate::apps::{read, write};

/// What a call is checked against.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// The app's space: its blobs and its sessions.
    Space,
    /// A schema, by name.
    Schema(String),
    /// The app's events.
    Events,
}

/// The schema a statement runs in: the one it names, else the app's first.
fn schema(manifest: &Manifest, named: &Option<String>) -> Target {
    let name = named
        .clone()
        .or_else(|| manifest.schemas.first().map(|s| s.name.clone()))
        .unwrap_or_default();
    Target::Schema(name)
}

/// Agents calls that only look.
const LOOKING: &[&str] = &[
    "sessions.list",
    "sessions.open",
    "sessions.subscribe",
    "presets.list",
    "presets.get",
    "artifacts.get",
    "artifacts.list",
    "approvals.list",
];

/// The action and resource `call` needs, or `None` when it is not checked
/// here.
pub fn target(manifest: &Manifest, call: &Call) -> Option<(Action, Target)> {
    let found = match call {
        Call::BlobsGet { .. } | Call::BlobsInfo { .. } => (read(), Target::Space),
        Call::BlobsPut { .. }
        | Call::BlobsCreate { .. }
        | Call::BlobsWrite { .. }
        | Call::BlobsDelete { .. } => (write(), Target::Space),
        Call::SqlQuery { schema: s, .. } | Call::SqlSubscribe { schema: s, .. } => {
            (read(), schema(manifest, s))
        }
        Call::SqlExecute { schema: s, .. } | Call::SqlBatch { schema: s, .. } => {
            (write(), schema(manifest, s))
        }
        Call::EventsSubscribe { .. } => (read(), Target::Events),
        Call::EventsPublish { .. } => (write(), Target::Events),
        call if call.interface() == isonomic_proto::auth::Interface::Agents => {
            let method = call.method();
            if method.ends_with(".unsubscribe") {
                return None;
            }
            let action = if LOOKING.contains(&method) {
                read()
            } else {
                write()
            };
            (action, Target::Space)
        }
        _ => return None,
    };
    Some(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use isonomic_proto::manifest::Schema;

    fn notes() -> Manifest {
        Manifest {
            id: "com.isonomic.notes".into(),
            name: "Notes".into(),
            version: "0.1.0".into(),
            icon: "sparkle".into(),
            capabilities: Default::default(),
            schemas: vec![Schema {
                name: "notes".into(),
            }],
        }
    }

    #[test]
    fn calls_become_an_action_on_a_resource() {
        let m = notes();
        let query = Call::SqlQuery {
            sql: "select 1".into(),
            params: vec![],
            schema: None,
        };
        assert_eq!(
            target(&m, &query),
            Some((read(), Target::Schema("notes".into())))
        );
        let other = Call::SqlExecute {
            sql: "x".into(),
            params: vec![],
            schema: Some("reader".into()),
        };
        assert_eq!(
            target(&m, &other),
            Some((write(), Target::Schema("reader".into())))
        );
        let get = Call::BlobsGet {
            handle: "h".into(),
            offset: 0,
            length: None,
        };
        assert_eq!(target(&m, &get), Some((read(), Target::Space)));
        let publish = Call::EventsPublish {
            topic: "t".into(),
            data: serde_json::json!({}).into(),
            sender: None,
            app: None,
        };
        assert_eq!(target(&m, &publish), Some((write(), Target::Events)));
        assert_eq!(target(&m, &Call::SqlUnsubscribe { subscription: 1 }), None);
        assert_eq!(target(&m, &Call::SystemStatus), None);
    }
}
