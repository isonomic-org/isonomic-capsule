// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! The four recovery kit calls as the broker answers them (`identity.kit_*`,
//! shell only). The work is in [`crate::kit`]; here it moves off the runtime,
//! since the key store may block, and its refusals become wire errors.
//!
//! **What may be said back:** every refusal is `recovery_refused` with one of
//! [`crate::kit`]'s fixed sentences, which never quote the kit or the words
//! (a word is named by its position only). A failure to read or keep the ID
//! is `identity_unavailable`. Nothing here logs; the words leave only in the
//! reply.

use isonomic_proto::identity::{KitRestore, KitRestored, RecoveryKit, RecoveryWords};
use isonomic_proto::{ErrorCode, WireError};

use crate::kit::RecoveryError;
use crate::IdentityService;

fn wire(e: RecoveryError) -> WireError {
    let code = match &e {
        RecoveryError::NotAsked
        | RecoveryError::NoId
        | RecoveryError::NotExported
        | RecoveryError::Kit(_) => ErrorCode::RecoveryRefused,
        RecoveryError::Identity(_) | RecoveryError::Save(_) => ErrorCode::IdentityUnavailable,
    };
    WireError::new(code, e.to_string())
}

/// A panic's text could hold anything; say only that it stopped.
fn stopped(_: tokio::task::JoinError) -> WireError {
    WireError::new(
        ErrorCode::IdentityUnavailable,
        "Isonomic stopped before it finished with the recovery kit. Nothing was changed; try again.",
    )
}

pub(crate) async fn export(
    service: IdentityService,
    person_asked: bool,
) -> Result<RecoveryKit, WireError> {
    let done = tokio::task::spawn_blocking(move || service.kit_export(person_asked)).await;
    let exported = done.map_err(stopped)?.map_err(wire)?;
    Ok(RecoveryKit {
        id: exported.id,
        words: RecoveryWords::new(exported.words),
        kit: exported.document,
    })
}

pub(crate) async fn restore(
    service: IdentityService,
    restore: KitRestore,
) -> Result<KitRestored, WireError> {
    let done = tokio::task::spawn_blocking(move || {
        service.kit_restore(&restore.kit, restore.words.reveal(), restore.replace_unused)
    })
    .await;
    let restored = done.map_err(stopped)?.map_err(wire)?;
    Ok(KitRestored {
        id: restored.id,
        words: RecoveryWords::new(restored.words),
        kit: restored.document,
        kit_made: restored.kit_made,
        note: restored.note,
    })
}

pub(crate) async fn saved(service: IdentityService) -> Result<(), WireError> {
    let done = tokio::task::spawn_blocking(move || service.kit_saved()).await;
    done.map_err(stopped)?.map_err(wire)
}
