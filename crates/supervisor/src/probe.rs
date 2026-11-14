//! Readiness probes (architecture spec, section 4.2): two-phase per service.
//! Connect (liveness), then speak the service's real protocol (readiness).
//! No probe shells out to a client tool, so nothing in the user's environment
//! can redirect one.

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::endpoint::{Endpoint, Stream};

/// Per-service readiness protocol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Readiness {
    /// HTTP GET on `path`, expect 200.
    Http { path: String },
    /// RESP `PING`, expect `+PONG`.
    RedisPing,
    /// Postgres startup handshake + `SELECT 1` to `ReadyForQuery`. Assumes the
    /// role authenticates with trust auth (first-time setup configures this
    /// for the user-only Unix socket); a password challenge is treated as
    /// not-ready under that assumption.
    PostgresQuery { user: String, database: String },
}

impl Readiness {
    pub async fn check(&self, endpoint: &Endpoint, timeout: Duration) -> bool {
        match self {
            Readiness::Http { path } => crate::health::probe_http(endpoint, path, timeout).await,
            Readiness::RedisPing => tokio::time::timeout(timeout, redis_ping(endpoint))
                .await
                .unwrap_or_default(),
            Readiness::PostgresQuery { user, database } => {
                tokio::time::timeout(timeout, pg_select_1(endpoint, user, database))
                    .await
                    .unwrap_or_default()
            }
        }
    }
}

async fn redis_ping(endpoint: &Endpoint) -> bool {
    let Ok(mut stream) = endpoint.connect().await else {
        return false;
    };
    if stream.write_all(b"*1\r\n$4\r\nPING\r\n").await.is_err() {
        return false;
    }
    let mut buf = [0u8; 64];
    match stream.read(&mut buf).await {
        Ok(n) if n > 0 => buf[..n].starts_with(b"+PONG"),
        _ => false,
    }
}

/// Postgres v3 protocol. We don't decode message bodies — after the (typeless)
/// startup message, every backend message is framed as `[type:u8][len:i32]`,
/// so we frame-walk to `ReadyForQuery` ('Z'), send `SELECT 1`, and walk to the
/// next 'Z'. Any error message ('E') or EOF is not-ready.
async fn pg_select_1(endpoint: &Endpoint, user: &str, database: &str) -> bool {
    let Ok(mut stream) = endpoint.connect().await else {
        return false;
    };
    if send_startup(&mut stream, user, database).await.is_err() {
        return false;
    }
    if !walk_to_ready(&mut stream).await {
        return false;
    }
    // Simple Query: 'Q' + len + "SELECT 1;\0".
    let query = b"SELECT 1;\0";
    let mut msg = Vec::with_capacity(5 + query.len());
    msg.push(b'Q');
    msg.extend_from_slice(&((4 + query.len()) as i32).to_be_bytes());
    msg.extend_from_slice(query);
    if stream.write_all(&msg).await.is_err() {
        return false;
    }
    walk_to_ready(&mut stream).await
}

async fn send_startup(stream: &mut Stream, user: &str, database: &str) -> std::io::Result<()> {
    // StartupMessage: len:i32, protocol:i32 (196608 = 3.0), then key\0value\0…\0.
    let mut params = Vec::new();
    for (k, v) in [
        ("user", user),
        ("database", database),
        ("application_name", "isonomic-probe"),
    ] {
        params.extend_from_slice(k.as_bytes());
        params.push(0);
        params.extend_from_slice(v.as_bytes());
        params.push(0);
    }
    params.push(0); // terminating empty key
    let len = 8 + params.len();
    let mut msg = Vec::with_capacity(len);
    msg.extend_from_slice(&(len as i32).to_be_bytes());
    msg.extend_from_slice(&196608i32.to_be_bytes());
    msg.extend_from_slice(&params);
    stream.write_all(&msg).await
}

/// True if an Authentication ('R') message body is a password/SASL challenge
/// rather than AuthenticationOk (auth type 0). Under our loopback trust-auth
/// assumption any challenge means the probe should report not-ready.
fn is_auth_challenge(body: &[u8]) -> bool {
    body.len() >= 4 && i32::from_be_bytes([body[0], body[1], body[2], body[3]]) != 0
}

/// Read framed backend messages until `ReadyForQuery` ('Z' → true) or an error
/// / auth challenge / EOF (→ false).
async fn walk_to_ready(stream: &mut Stream) -> bool {
    loop {
        let mut header = [0u8; 5];
        if stream.read_exact(&mut header).await.is_err() {
            return false;
        }
        let msg_type = header[0];
        let len = i32::from_be_bytes([header[1], header[2], header[3], header[4]]);
        if !(4..=1_000_000).contains(&len) {
            return false;
        }
        let body_len = (len - 4) as usize;
        let mut body = vec![0u8; body_len];
        if body_len > 0 && stream.read_exact(&mut body).await.is_err() {
            return false;
        }
        match msg_type {
            b'Z' => return true,                              // ReadyForQuery
            b'E' => return false,                             // ErrorResponse
            b'R' if is_auth_challenge(&body) => return false, // password/SASL wanted
            _ => {} // AuthenticationOk, params, keydata, row data, … — skip
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn fake_line_server(reply: &'static [u8]) -> SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                tokio::spawn(async move {
                    let mut buf = [0u8; 64];
                    let _ = sock.read(&mut buf).await;
                    let _ = sock.write_all(reply).await;
                });
            }
        });
        addr
    }

    fn backend_msg(t: u8, body: &[u8]) -> Vec<u8> {
        let mut m = vec![t];
        m.extend_from_slice(&((4 + body.len()) as i32).to_be_bytes());
        m.extend_from_slice(body);
        m
    }

    /// Minimal Postgres server: AuthenticationOk, ReadyForQuery, then on the
    /// query CommandComplete + ReadyForQuery.
    async fn fake_pg_server(healthy: bool) -> SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                tokio::spawn(async move {
                    let mut buf = [0u8; 512];
                    let _ = sock.read(&mut buf).await; // startup
                    if !healthy {
                        // ErrorResponse then close.
                        let _ = sock.write_all(&backend_msg(b'E', b"Sfatal\0")).await;
                        return;
                    }
                    let mut out = Vec::new();
                    out.extend_from_slice(&backend_msg(b'R', &0i32.to_be_bytes())); // AuthenticationOk
                    out.extend_from_slice(&backend_msg(b'Z', b"I")); // ReadyForQuery
                    let _ = sock.write_all(&out).await;
                    let _ = sock.read(&mut buf).await; // the SELECT 1 query
                    let mut out2 = Vec::new();
                    out2.extend_from_slice(&backend_msg(b'C', b"SELECT 1\0")); // CommandComplete
                    out2.extend_from_slice(&backend_msg(b'Z', b"I")); // ReadyForQuery
                    let _ = sock.write_all(&out2).await;
                });
            }
        });
        addr
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn redis_ping_accepts_pong_and_rejects_errors() {
        let good = fake_line_server(b"+PONG\r\n").await;
        assert!(
            Readiness::RedisPing
                .check(&Endpoint::Tcp(good), Duration::from_secs(2))
                .await
        );

        let bad = fake_line_server(b"-ERR loading\r\n").await;
        assert!(
            !Readiness::RedisPing
                .check(&Endpoint::Tcp(bad), Duration::from_secs(2))
                .await
        );

        let dead = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let dead_addr = dead.local_addr().unwrap();
        drop(dead);
        assert!(
            !Readiness::RedisPing
                .check(&Endpoint::Tcp(dead_addr), Duration::from_millis(500))
                .await
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn postgres_probe_completes_select_1_and_rejects_errors() {
        let pg = Readiness::PostgresQuery {
            user: "supervisor".to_string(),
            database: "postgres".to_string(),
        };
        let healthy = fake_pg_server(true).await;
        assert!(
            pg.check(&Endpoint::Tcp(healthy), Duration::from_secs(2))
                .await
        );

        let erroring = fake_pg_server(false).await;
        assert!(
            !pg.check(&Endpoint::Tcp(erroring), Duration::from_secs(2))
                .await
        );

        let dead = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let dead_addr = dead.local_addr().unwrap();
        drop(dead);
        assert!(
            !pg.check(&Endpoint::Tcp(dead_addr), Duration::from_millis(500))
                .await
        );
    }

    #[test]
    fn auth_challenge_detection() {
        assert!(!is_auth_challenge(&0i32.to_be_bytes())); // AuthenticationOk
        assert!(is_auth_challenge(&5i32.to_be_bytes())); // MD5 password
        assert!(!is_auth_challenge(&[1, 2])); // truncated → treat as ok/skip
    }
}
