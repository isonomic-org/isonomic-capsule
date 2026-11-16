//! The seam to the person's Isonomic ID: the broker checks that the caller
//! is the shell (`proto::auth`), then asks here. The ID itself is made and
//! kept by the `identity` crate, which depends on this one.

use isonomic_proto::identity::Identity as PersonIdentity;
use isonomic_proto::{ErrorCode, WireError};

use crate::agents::BoxFuture;

pub trait Identity: Send + Sync + 'static {
    /// The person's ID, made the first time it is asked for.
    fn get(&self) -> BoxFuture<'_, Result<PersonIdentity, WireError>>;
}

/// No ID service: every call is answered `identity_unavailable`.
pub struct NoIdentity;

impl Identity for NoIdentity {
    fn get(&self) -> BoxFuture<'_, Result<PersonIdentity, WireError>> {
        Box::pin(async {
            Err(WireError::new(
                ErrorCode::IdentityUnavailable,
                "This engine can't make an Isonomic ID.",
            ))
        })
    }
}
