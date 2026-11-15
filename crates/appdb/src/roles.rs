//! Each app's role, schemas and read access, set up from its manifest.
//!
//! An app's role logs in (over the user-only socket, as every role does)
//! but is no superuser and cannot create roles or databases. It owns the
//! schemas its manifest declares. For each schema listed under
//! `capabilities.sql.schemas` it gets usage and select, on the tables there
//! now and on those the schema's owner creates later. Nobody but owners
//! reaches the `public` schema.

use isonomic_proto::Manifest;
use sha2::{Digest, Sha256};
use tokio_postgres::Client;

use crate::quote;

/// The database role for an app id. A digest, so any id maps to a short,
/// plain name and two ids never share one.
pub fn role_name(app: &str) -> String {
    let digest = Sha256::digest(app.as_bytes());
    let hex: String = digest[..10].iter().map(|b| format!("{b:02x}")).collect();
    format!("app_{hex}")
}

/// A literal string for the database: single quotes, doubled inside.
fn literal(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

/// How far an app's setup got.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Setup {
    /// Everything its manifest asks for is in place.
    Complete,
    /// A schema it may read does not exist yet; set up again next time.
    Waiting,
}

/// The database-wide rules, once: only owners reach `public`.
pub(crate) async fn lock_down(admin: &Client, database: &str) -> Result<(), tokio_postgres::Error> {
    admin
        .batch_execute(&format!(
            "REVOKE ALL ON SCHEMA public FROM PUBLIC;
             REVOKE ALL ON DATABASE {db} FROM PUBLIC;",
            db = quote(database)
        ))
        .await
}

/// Set up `manifest`'s role, schemas and read access, as the engine's own
/// role. Idempotent. A schema another app already owns is refused.
pub(crate) async fn provision(
    admin: &Client,
    database: &str,
    manifest: &Manifest,
) -> Result<Setup, String> {
    let role = role_name(&manifest.id);
    let db_error = |e: tokio_postgres::Error| crate::pg::message(&e);
    let exists = admin
        .query_opt("SELECT 1 FROM pg_roles WHERE rolname = $1", &[&role])
        .await
        .map_err(db_error)?
        .is_some();
    if !exists {
        admin
            .batch_execute(&format!(
                "CREATE ROLE {r} LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT NOREPLICATION",
                r = quote(&role)
            ))
            .await
            .map_err(db_error)?;
    }
    admin
        .batch_execute(&format!(
            "GRANT CONNECT ON DATABASE {db} TO {r}",
            db = quote(database),
            r = quote(&role)
        ))
        .await
        .map_err(db_error)?;

    for schema in &manifest.schemas {
        let owner: Option<String> = admin
            .query_opt(
                "SELECT pg_get_userbyid(nspowner)::text FROM pg_namespace WHERE nspname = $1",
                &[&schema.name],
            )
            .await
            .map_err(db_error)?
            .map(|row| row.get(0));
        match owner {
            None => admin
                .batch_execute(&format!(
                    "CREATE SCHEMA {s} AUTHORIZATION {r}",
                    s = quote(&schema.name),
                    r = quote(&role)
                ))
                .await
                .map_err(db_error)?,
            Some(owner) if owner == role => {}
            Some(_) => {
                return Err(format!(
                    "schema {} belongs to another app",
                    literal(&schema.name)
                ))
            }
        }
    }

    let mut setup = Setup::Complete;
    let listed = manifest
        .capabilities
        .sql
        .iter()
        .flat_map(|sql| &sql.schemas);
    for schema in listed {
        let owner: Option<String> = admin
            .query_opt(
                "SELECT pg_get_userbyid(nspowner)::text FROM pg_namespace WHERE nspname = $1",
                &[schema],
            )
            .await
            .map_err(db_error)?
            .map(|row| row.get(0));
        let Some(owner) = owner else {
            setup = Setup::Waiting;
            continue;
        };
        admin
            .batch_execute(&format!(
                "GRANT USAGE ON SCHEMA {s} TO {r};
                 GRANT SELECT ON ALL TABLES IN SCHEMA {s} TO {r};
                 ALTER DEFAULT PRIVILEGES FOR ROLE {o} IN SCHEMA {s} GRANT SELECT ON TABLES TO {r};",
                s = quote(schema),
                r = quote(&role),
                o = quote(&owner)
            ))
            .await
            .map_err(db_error)?;
    }
    Ok(setup)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_names_are_short_plain_and_distinct() {
        let a = role_name("com.isonomic.chat");
        assert!(a.starts_with("app_") && a.len() == 24);
        assert!(a.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'));
        assert_ne!(role_name("com.isonomic.a-b"), role_name("com.isonomic.a.b"));
        assert_eq!(a, role_name("com.isonomic.chat"));
    }

    #[test]
    fn quoting_doubles_what_would_end_the_quote() {
        assert_eq!(quote("we\"ird"), "\"we\"\"ird\"");
        assert_eq!(literal("it's"), "'it''s'");
    }
}
