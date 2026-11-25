// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! The seam to the person's Isonomic ID: the broker checks that the caller
//! is the shell (`proto::auth`), then asks here. The ID itself is made and
//! kept by the `identity` crate, which depends on this one.

use isonomic_proto::identity::{
    Identity as PersonIdentity, KitRestore, KitRestored, KitStatus, RecoveryKit,
};
use isonomic_proto::{Call, ErrorCode, Manifest, WireError};

use crate::agents::BoxFuture;

pub trait Identity: Send + Sync + 'static {
    /// The person's ID, made the first time it is asked for.
    fn get(&self) -> BoxFuture<'_, Result<PersonIdentity, WireError>>;

    /// May the app `manifest` make `call`? Asked for every app call, after
    /// the manifest's own checks. Yes by default.
    fn check<'a>(
        &'a self,
        _manifest: &'a Manifest,
        _call: &'a Call,
    ) -> BoxFuture<'a, Result<(), WireError>> {
        Box::pin(async { Ok(()) })
    }

    /// An app was approved (`apps.grant`, recorded): give it its ID and
    /// grants. Nothing to do by default.
    fn granted<'a>(&'a self, _manifest: &'a Manifest) -> BoxFuture<'a, Result<(), WireError>> {
        Box::pin(async { Ok(()) })
    }

    /// The person's recovery kit (`identity.kit_export`), only when the
    /// person asked. The shell alone reaches this (`proto::auth`). Refused
    /// by default.
    fn kit_export(&self, _person_asked: bool) -> BoxFuture<'_, Result<RecoveryKit, WireError>> {
        Box::pin(async { Err(no_kit()) })
    }

    /// The person's ID from a recovery kit (`identity.kit_restore`).
    /// Refused by default.
    fn kit_restore(&self, _restore: KitRestore) -> BoxFuture<'_, Result<KitRestored, WireError>> {
        Box::pin(async { Err(no_kit()) })
    }

    /// The person confirmed saving the kit (`identity.kit_saved`). Refused
    /// by default.
    fn kit_saved(&self) -> BoxFuture<'_, Result<(), WireError>> {
        Box::pin(async { Err(no_kit()) })
    }

    /// When the kit was last exported and saved (`identity.kit_status`).
    /// Never by default.
    fn kit_status(&self) -> BoxFuture<'_, Result<KitStatus, WireError>> {
        Box::pin(async { Ok(KitStatus::default()) })
    }
}

/// What an engine without an ID service answers a kit call.
fn no_kit() -> WireError {
    WireError::new(
        ErrorCode::RecoveryRefused,
        "This engine can't make an Isonomic ID, so it has no recovery kit.",
    )
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
