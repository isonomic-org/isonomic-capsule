//! The identity library's own checks, `identities.rs` and `vectors.rs`,
//! compiled here with this repo's lockfile, so the engine's build of the
//! library is held to the same answers as the library's own. They are
//! included verbatim from the sibling checkout at `ID_REF`; the frozen
//! vectors are reached through the `vectors` link at this repo's root.

#[allow(clippy::all, clippy::pedantic)]
#[path = "../../../../isonomic-id/crates/id-check/tests/identities.rs"]
mod identities;

#[allow(clippy::all, clippy::pedantic)]
#[path = "../../../../isonomic-id/crates/id-check/tests/vectors.rs"]
mod vectors;
