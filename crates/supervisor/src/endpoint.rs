// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Where a supervised service listens: a loopback TCP address or a Unix
//! socket inside the Capsule directory.

use std::fmt;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use tokio::io::{AsyncRead, AsyncWrite};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Endpoint {
    Tcp(SocketAddr),
    Unix(PathBuf),
}

/// A connected byte stream of either kind.
pub trait AsyncStream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> AsyncStream for T {}
pub type Stream = Box<dyn AsyncStream>;

impl Endpoint {
    pub fn port(&self) -> Option<u16> {
        match self {
            Endpoint::Tcp(addr) => Some(addr.port()),
            Endpoint::Unix(_) => None,
        }
    }

    pub fn socket(&self) -> Option<&Path> {
        match self {
            Endpoint::Tcp(_) => None,
            Endpoint::Unix(path) => Some(path),
        }
    }

    /// Value for an HTTP `Host` header.
    pub fn host_header(&self) -> String {
        match self {
            Endpoint::Tcp(addr) => addr.to_string(),
            Endpoint::Unix(_) => "localhost".to_string(),
        }
    }

    pub async fn connect(&self) -> std::io::Result<Stream> {
        match self {
            Endpoint::Tcp(addr) => Ok(Box::new(tokio::net::TcpStream::connect(addr).await?)),
            #[cfg(unix)]
            Endpoint::Unix(path) => Ok(Box::new(tokio::net::UnixStream::connect(path).await?)),
            #[cfg(not(unix))]
            Endpoint::Unix(_) => Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "unix sockets are not available on this platform",
            )),
        }
    }
}

impl fmt::Display for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Endpoint::Tcp(addr) => write!(f, "{addr}"),
            Endpoint::Unix(path) => write!(f, "unix:{}", path.display()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accessors_and_display_match_the_kind() {
        let tcp = Endpoint::Tcp("127.0.0.1:4000".parse().unwrap());
        assert_eq!(tcp.port(), Some(4000));
        assert_eq!(tcp.socket(), None);
        assert_eq!(tcp.to_string(), "127.0.0.1:4000");
        let unix = Endpoint::Unix(PathBuf::from("/c/run/s.sock"));
        assert_eq!(unix.port(), None);
        assert_eq!(unix.socket(), Some(Path::new("/c/run/s.sock")));
        assert_eq!(unix.to_string(), "unix:/c/run/s.sock");
        assert_eq!(unix.host_header(), "localhost");
    }
}
