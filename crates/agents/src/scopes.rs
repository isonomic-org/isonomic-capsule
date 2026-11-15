//! The harness host's `ScopeResolver` over the engine's approved apps, and
//! the user-action tokens that let a person's decision through.
//!
//! A capability token resolves exactly as the broker resolves it: to the
//! app it was issued for, from its approved manifest. Every app the engine
//! grants is first-party (the shell approves it from its manifest); Views
//! and third-party code get their own kinds when they arrive.
//!
//! A user-action token is minted when the shell reports that a person acted
//! on one approval. It is random, works once, only for that caller and that
//! approval in that session, and expires after `USER_ACTION_TTL_SECS` on the engine's clock.
//! Tokens live in memory only: an engine restart forgets them, and the
//! person acts again.

use std::collections::HashMap;
use std::io::Read;
use std::sync::{Arc, Mutex};

use broker::Apps;
use capsule_core::Clock;
use harness_core::{ApprovalId, SessionId};
use harness_host::{
    AppKind, Caller, CapabilityToken, HostError, Scope, ScopeResolver, UserActionToken,
};
use isonomic_proto::Token;

/// How long a person's action stays good for.
pub const USER_ACTION_TTL_SECS: i64 = 120;

struct Minted {
    scope: Scope,
    session: SessionId,
    approval: ApprovalId,
    expires_unix: i64,
}

pub struct EngineScopes {
    apps: Arc<Apps>,
    clock: Arc<dyn Clock>,
    actions: Mutex<HashMap<String, Minted>>,
}

fn random_hex() -> std::io::Result<String> {
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

impl EngineScopes {
    pub fn new(apps: Arc<Apps>, clock: Arc<dyn Clock>) -> Self {
        Self {
            apps,
            clock,
            actions: Mutex::new(HashMap::new()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Minted>> {
        self.actions.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// A person acted on `approval` in `session` for `caller`: the token
    /// that proves it.
    pub fn mint(
        &self,
        caller: &Caller,
        session: &SessionId,
        approval: &ApprovalId,
    ) -> std::io::Result<UserActionToken> {
        let token = random_hex()?;
        let now = self.clock.now_unix();
        let mut actions = self.lock();
        // Expired tokens are dropped whenever a new one is minted.
        actions.retain(|_, minted| minted.expires_unix > now);
        actions.insert(
            token.clone(),
            Minted {
                scope: caller.scope.clone(),
                session: session.clone(),
                approval: approval.clone(),
                expires_unix: now + USER_ACTION_TTL_SECS,
            },
        );
        Ok(UserActionToken(token))
    }
}

impl ScopeResolver for EngineScopes {
    fn resolve(&self, capability: &CapabilityToken) -> Result<Caller, HostError> {
        let manifest = self
            .apps
            .manifest(&Token::new(capability.0.clone()))
            .map_err(|_| HostError::Unauthorized)?;
        Ok(Caller::app(&manifest.id, AppKind::FirstParty))
    }

    fn verify_user_action(
        &self,
        caller: &Caller,
        token: &UserActionToken,
        session: &SessionId,
        approval: &ApprovalId,
    ) -> bool {
        // Spent on the first try, right or wrong, so a token is never
        // guessed at twice.
        let Some(minted) = self.lock().remove(&token.0) else {
            return false;
        };
        minted.scope == caller.scope
            && &minted.session == session
            && &minted.approval == approval
            && self.clock.now_unix() < minted.expires_unix
    }
}

#[cfg(test)]
#[path = "scopes_tests.rs"]
mod tests;
