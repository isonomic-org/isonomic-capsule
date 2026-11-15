//! Approved apps and their capability tokens (SDK request R1).
//!
//! A token is a keyed hash of the app id and its approved capabilities,
//! with a secret kept in the Capsule directory. The same approval gives the
//! same token after a restart, so the window's open tabs keep working, and
//! nobody without the secret can forge one. The registry stores only a
//! digest of each token. Tokens never appear in logs or error messages.

use std::io::Write;
use std::path::Path;

use isonomic_proto::{ErrorCode, Manifest, Token, WireError};
use registry::{Grant, GrantStore};
use sha2::{Digest, Sha256};

const KEY_BYTES: usize = 32;
const TOKEN_PREFIX: &str = "tok-";

/// The engine's token secret. `Debug` never prints it.
pub struct TokenKey([u8; KEY_BYTES]);

impl std::fmt::Debug for TokenKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TokenKey(..)")
    }
}

impl TokenKey {
    /// A fixed key (tests).
    pub fn fixed(bytes: [u8; KEY_BYTES]) -> Self {
        Self(bytes)
    }

    /// Read the key at `path`, or create it, user-only, from the system's
    /// random source. A file of the wrong length is an error, not replaced:
    /// replacing it would silently invalidate every token.
    pub fn load_or_create(path: &Path) -> std::io::Result<Self> {
        match std::fs::read(path) {
            Ok(bytes) => {
                let key = bytes.try_into().map_err(|_| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("{} is not a {KEY_BYTES}-byte key", path.display()),
                    )
                })?;
                return Ok(Self(key));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut key = [0u8; KEY_BYTES];
        {
            use std::io::Read;
            std::fs::File::open("/dev/urandom")?.read_exact(&mut key)?;
        }
        let mut file = {
            use std::os::unix::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(path)?
        };
        file.write_all(&key)?;
        file.sync_all()?;
        Ok(Self(key))
    }

    /// HMAC-SHA256 of `message` under this key.
    fn mac(&self, message: &[u8]) -> [u8; 32] {
        const BLOCK: usize = 64;
        let mut padded = [0u8; BLOCK];
        padded[..KEY_BYTES].copy_from_slice(&self.0);
        let pad = |byte: u8| padded.map(|b| b ^ byte);
        let inner = Sha256::new()
            .chain_update(pad(0x36))
            .chain_update(message)
            .finalize();
        Sha256::new()
            .chain_update(pad(0x5c))
            .chain_update(inner)
            .finalize()
            .into()
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// What the registry stores for a token: enough to recognise it, useless
/// for making calls.
fn digest(token: &Token) -> String {
    hex(&Sha256::digest(token.as_str().as_bytes()))
}

fn internal(what: &str, error: impl std::fmt::Display) -> WireError {
    WireError::new(ErrorCode::Internal, format!("{what}: {error}"))
}

/// The approved apps, persisted in the registry.
pub struct Apps {
    store: GrantStore,
    key: TokenKey,
}

impl Apps {
    pub fn new(store: GrantStore, key: TokenKey) -> Self {
        Self { store, key }
    }

    /// The token this approval yields: the same for the same app id and
    /// capabilities, under the same key.
    fn token_for(&self, manifest: &Manifest) -> Token {
        let capabilities =
            serde_json::to_string(&manifest.capabilities).expect("capabilities serialize");
        // The id cannot contain a NUL, so the two parts cannot run together.
        let message = format!("{}\0{capabilities}", manifest.id);
        Token::new(format!(
            "{TOKEN_PREFIX}{}",
            hex(&self.key.mac(message.as_bytes()))
        ))
    }

    /// `apps.grant`: validate, persist, then answer with the token.
    pub fn grant(&self, manifest: &Manifest) -> Result<Token, WireError> {
        manifest
            .validate()
            .map_err(|error| WireError::new(ErrorCode::InvalidManifest, error.to_string()))?;
        let token = self.token_for(manifest);
        let grant = Grant {
            app: manifest.id.clone(),
            manifest: serde_json::to_string(manifest).map_err(|e| internal("manifest", e))?,
        };
        self.store
            .record(&grant, &digest(&token))
            .map_err(|e| internal("could not record the grant", e))?;
        Ok(token)
    }

    /// The approved manifest a token was issued for, as it stands now.
    pub fn manifest(&self, token: &Token) -> Result<Manifest, WireError> {
        let found = self
            .store
            .find(&digest(token))
            .map_err(|e| internal("could not read the grants", e))?;
        let Some(grant) = found else {
            return Err(WireError::new(
                ErrorCode::UnknownToken,
                "this token was never issued, or was revoked",
            ));
        };
        serde_json::from_str(&grant.manifest).map_err(|e| internal("stored manifest", e))
    }
}

#[cfg(test)]
#[path = "apps_tests.rs"]
mod tests;
