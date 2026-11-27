// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Signing in to a remote tool server: the authorization code flow with
//! PKCE (S256). The server's protected-resource metadata names its
//! authorization server; that server's metadata gives the endpoints. The
//! engine registers itself there each time (dynamic client registration):
//! it holds no client id made ahead of time, so a server that does not take
//! registrations cannot be signed in to. Tokens never reach a log, an
//! error or a `Debug` print.

use std::time::{SystemTime, UNIX_EPOCH};

use reqwest::Url;
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// The name the engine registers under.
pub const CLIENT_NAME: &str = "Isonomic";

/// What the engine needs to send the person to sign in, and to finish.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoints {
    pub authorization: String,
    pub token: String,
    pub registration: String,
    /// The resource the tokens are for: the server's address.
    pub resource: String,
    pub scopes: Vec<String>,
}

/// A finished sign-in, kept in the vault as JSON.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grant {
    pub client_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
    pub token_endpoint: String,
    pub resource: String,
    pub access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    /// Seconds since the epoch, when the server said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
}

impl std::fmt::Debug for Grant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Grant")
            .field("client_id", &self.client_id)
            .field("token_endpoint", &self.token_endpoint)
            .field("resource", &self.resource)
            .field("tokens", &"[secret]")
            .finish()
    }
}

/// A sign-in under way: what the browser's return is checked against.
pub struct Pending {
    pub endpoints: Endpoints,
    pub client_id: String,
    pub client_secret: Option<String>,
    pub redirect: String,
    pub state: String,
    verifier: String,
}

fn fail(what: &str) -> String {
    format!("The sign-in could not start: {what}.")
}

/// URL-safe base64 without padding.
pub fn base64url(bytes: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().fold(0u32, |n, b| (n << 8) | u32::from(*b)) << (8 * (3 - chunk.len()));
        for i in 0..=chunk.len() {
            out.push(ABC[((n >> (18 - 6 * i)) & 63) as usize] as char);
        }
    }
    out
}

fn random(bytes: usize) -> Result<String, String> {
    let mut buf = vec![0u8; bytes];
    SystemRandom::new()
        .fill(&mut buf)
        .map_err(|_| fail("no random numbers"))?;
    Ok(base64url(&buf))
}

/// The PKCE challenge for `verifier`.
pub fn challenge(verifier: &str) -> String {
    base64url(&Sha256::digest(verifier.as_bytes()))
}

async fn get_json(client: &reqwest::Client, url: &str) -> Option<Value> {
    let response = client
        .get(url)
        .header("accept", "application/json")
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    response.json::<Value>().await.ok()
}

/// `origin` + `/.well-known/<name>` + the path of `url`, as the metadata
/// documents are found.
fn well_known(url: &Url, name: &str) -> String {
    let path = url.path().trim_end_matches('/');
    let origin = url.origin().ascii_serialization();
    format!("{origin}/.well-known/{name}{path}")
}

/// Find the endpoints for the server at `resource`. `hint` is the
/// metadata address its refusal named, if any.
pub async fn discover(
    client: &reqwest::Client,
    resource: &str,
    hint: Option<&str>,
) -> Result<Endpoints, String> {
    let url = Url::parse(resource).map_err(|_| fail("the server's address is not valid"))?;
    let mut candidates: Vec<String> = hint.map(str::to_string).into_iter().collect();
    candidates.push(well_known(&url, "oauth-protected-resource"));
    candidates.push(format!(
        "{}/.well-known/oauth-protected-resource",
        url.origin().ascii_serialization()
    ));
    let mut metadata = None;
    for candidate in candidates {
        if let Some(found) = get_json(client, &candidate).await {
            metadata = Some(found);
            break;
        }
    }
    // An older server has no resource metadata: its own origin is then
    // its authorization server.
    let issuer = metadata
        .as_ref()
        .and_then(|m| {
            m.get("authorization_servers")?
                .as_array()?
                .first()?
                .as_str()
        })
        .map(str::to_string)
        .unwrap_or_else(|| url.origin().ascii_serialization());
    let scopes: Vec<String> = metadata
        .as_ref()
        .and_then(|m| m.get("scopes_supported")?.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|s| s.as_str().map(str::to_string))
        .collect();
    let issuer_url = Url::parse(&issuer).map_err(|_| fail("its sign-in server is not valid"))?;
    let mut server = None;
    for candidate in [
        well_known(&issuer_url, "oauth-authorization-server"),
        well_known(&issuer_url, "openid-configuration"),
        format!(
            "{}/.well-known/openid-configuration",
            issuer.trim_end_matches('/')
        ),
    ] {
        if let Some(found) = get_json(client, &candidate).await {
            server = Some(found);
            break;
        }
    }
    let server = server.ok_or_else(|| fail("its sign-in server did not describe itself"))?;
    let field = |name: &str| server.get(name).and_then(Value::as_str).map(str::to_string);
    let registration = field("registration_endpoint").ok_or_else(|| {
        "This tool server needs an app registered with it ahead of time, so it cannot be \
         signed in to from here. Paste a token from the service instead, if it offers one."
            .to_string()
    })?;
    let methods = server
        .get("code_challenge_methods_supported")
        .and_then(Value::as_array);
    if methods.is_some_and(|m| !m.iter().any(|v| v.as_str() == Some("S256"))) {
        return Err(fail("its sign-in server does not take a secure challenge"));
    }
    Ok(Endpoints {
        authorization: field("authorization_endpoint").ok_or_else(|| fail("no sign-in page"))?,
        token: field("token_endpoint").ok_or_else(|| fail("no token endpoint"))?,
        registration,
        resource: resource.to_string(),
        scopes,
    })
}

/// Register the engine with the sign-in server, for `redirect`.
pub async fn register(
    client: &reqwest::Client,
    endpoints: &Endpoints,
    redirect: &str,
) -> Result<(String, Option<String>), String> {
    let body = serde_json::json!({
        "client_name": CLIENT_NAME,
        "redirect_uris": [redirect],
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
        "token_endpoint_auth_method": "none",
    });
    let response = client
        .post(&endpoints.registration)
        .json(&body)
        .send()
        .await
        .map_err(|_| fail("its sign-in server could not be reached"))?;
    if !response.status().is_success() {
        return Err(fail("its sign-in server refused to register this app"));
    }
    let answer: Value = response
        .json()
        .await
        .map_err(|_| fail("its sign-in server's answer was not readable"))?;
    let id = answer
        .get("client_id")
        .and_then(Value::as_str)
        .ok_or_else(|| fail("its sign-in server gave no client id"))?;
    let secret = answer.get("client_secret").and_then(Value::as_str);
    Ok((id.to_string(), secret.map(str::to_string)))
}

impl Pending {
    pub fn new(
        endpoints: Endpoints,
        client_id: String,
        client_secret: Option<String>,
        redirect: String,
    ) -> Result<Self, String> {
        Ok(Self {
            endpoints,
            client_id,
            client_secret,
            redirect,
            state: random(16)?,
            verifier: random(32)?,
        })
    }

    /// The page to open in the person's browser.
    pub fn url(&self) -> Result<String, String> {
        let mut url = Url::parse(&self.endpoints.authorization)
            .map_err(|_| fail("its sign-in page is not valid"))?;
        {
            let mut query = url.query_pairs_mut();
            query
                .append_pair("response_type", "code")
                .append_pair("client_id", &self.client_id)
                .append_pair("redirect_uri", &self.redirect)
                .append_pair("code_challenge", &challenge(&self.verifier))
                .append_pair("code_challenge_method", "S256")
                .append_pair("state", &self.state)
                .append_pair("resource", &self.endpoints.resource);
            if !self.endpoints.scopes.is_empty() {
                query.append_pair("scope", &self.endpoints.scopes.join(" "));
            }
        }
        Ok(url.into())
    }

    /// Trade the code the browser brought back for tokens.
    pub async fn finish(&self, client: &reqwest::Client, code: &str) -> Result<Grant, String> {
        let mut form = vec![
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", self.redirect.as_str()),
            ("client_id", self.client_id.as_str()),
            ("code_verifier", self.verifier.as_str()),
            ("resource", self.endpoints.resource.as_str()),
        ];
        if let Some(secret) = &self.client_secret {
            form.push(("client_secret", secret));
        }
        let mut grant = Grant {
            client_id: self.client_id.clone(),
            client_secret: self.client_secret.clone(),
            token_endpoint: self.endpoints.token.clone(),
            resource: self.endpoints.resource.clone(),
            access_token: String::new(),
            refresh_token: None,
            expires_at: None,
        };
        tokens(client, &self.endpoints.token, &form, &mut grant).await?;
        Ok(grant)
    }
}

/// Refresh `grant`'s access token, if it has a refresh token.
pub async fn refresh(client: &reqwest::Client, grant: &mut Grant) -> Result<(), String> {
    let refresh = grant
        .refresh_token
        .clone()
        .ok_or_else(|| "The sign-in has ended; sign in again.".to_string())?;
    let (client_id, resource) = (grant.client_id.clone(), grant.resource.clone());
    let secret = grant.client_secret.clone();
    let mut form = vec![
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh.as_str()),
        ("client_id", client_id.as_str()),
        ("resource", resource.as_str()),
    ];
    if let Some(secret) = &secret {
        form.push(("client_secret", secret));
    }
    let endpoint = grant.token_endpoint.clone();
    tokens(client, &endpoint, &form, grant).await
}

async fn tokens(
    client: &reqwest::Client,
    endpoint: &str,
    form: &[(&str, &str)],
    grant: &mut Grant,
) -> Result<(), String> {
    let response = client
        .post(endpoint)
        .form(form)
        .send()
        .await
        .map_err(|_| "The sign-in server could not be reached.".to_string())?;
    if !response.status().is_success() {
        return Err("The sign-in server refused; sign in again.".into());
    }
    let answer: Value = response
        .json()
        .await
        .map_err(|_| "The sign-in server's answer was not readable.".to_string())?;
    let access = answer
        .get("access_token")
        .and_then(Value::as_str)
        .ok_or_else(|| "The sign-in server gave no token.".to_string())?;
    grant.access_token = access.to_string();
    if let Some(refresh) = answer.get("refresh_token").and_then(Value::as_str) {
        grant.refresh_token = Some(refresh.to_string());
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    grant.expires_at = answer
        .get("expires_in")
        .and_then(Value::as_u64)
        .map(|s| now + s);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64url_has_no_padding() {
        assert_eq!(base64url(b""), "");
        assert_eq!(base64url(b"f"), "Zg");
        assert_eq!(base64url(b"fo"), "Zm8");
        assert_eq!(base64url(b"foo"), "Zm9v");
        assert_eq!(base64url(&[0xfb, 0xff]), "-_8");
    }

    #[test]
    fn the_challenge_is_the_standards_example() {
        // The PKCE standard's own example (RFC 7636, appendix B).
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            challenge(verifier),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn a_grants_debug_print_hides_its_tokens() {
        let grant = Grant {
            client_id: "c".into(),
            client_secret: Some("s3cret".into()),
            token_endpoint: "https://a.example/token".into(),
            resource: "https://a.example/api".into(),
            access_token: "acc3ss".into(),
            refresh_token: Some("r3fresh".into()),
            expires_at: None,
        };
        let shown = format!("{grant:?}");
        for secret in ["s3cret", "acc3ss", "r3fresh"] {
            assert!(!shown.contains(secret), "{shown}");
        }
    }

    #[test]
    fn metadata_is_looked_for_beside_the_path() {
        let url = Url::parse("https://tools.example.com/api/v1").unwrap();
        assert_eq!(
            well_known(&url, "oauth-protected-resource"),
            "https://tools.example.com/.well-known/oauth-protected-resource/api/v1"
        );
    }
}
