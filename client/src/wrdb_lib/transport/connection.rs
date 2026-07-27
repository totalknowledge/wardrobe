use std::io::{Error, ErrorKind, Result};
use std::path::PathBuf;

pub const DEFAULT_NETWORK_PORT: u16 = 24842;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriverKind {
    Embedded,
    Network,
    UnixSocket,
}

impl DriverKind {
    pub fn requires_embedded_engine(self) -> bool {
        matches!(self, Self::Embedded)
    }

    pub fn uses_socket_transport(self) -> bool {
        matches!(self, Self::Network | Self::UnixSocket)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionTarget {
    EmbeddedPath(PathBuf),
    Network { host: String, port: u16 },
    UnixSocket { path: PathBuf },
}

impl ConnectionTarget {
    pub fn driver_kind(&self) -> DriverKind {
        match self {
            Self::EmbeddedPath(_) => DriverKind::Embedded,
            Self::Network { .. } => DriverKind::Network,
            Self::UnixSocket { .. } => DriverKind::UnixSocket,
        }
    }

    pub fn requires_embedded_engine(&self) -> bool {
        self.driver_kind().requires_embedded_engine()
    }

    pub fn uses_socket_transport(&self) -> bool {
        self.driver_kind().uses_socket_transport()
    }

    pub fn parse(connection_string: &str) -> Result<Self> {
        let connection_string = connection_string.trim();
        if connection_string.is_empty() {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "Wardrobe connection string cannot be empty",
            ));
        }

        if let Some(rest) = connection_string.strip_prefix("wardrobe://local/") {
            return Self::embedded_from_uri_path(rest);
        }

        if let Some(rest) = connection_string.strip_prefix("wardrobe+file://") {
            return Self::embedded_from_uri_path(rest);
        }

        if let Some(rest) = connection_string.strip_prefix("file://") {
            return Self::embedded_from_uri_path(rest);
        }

        if let Some(rest) = connection_string.strip_prefix("wardrobe+unix://") {
            return Self::unix_from_uri_path(rest);
        }

        if let Some(rest) = connection_string.strip_prefix("wardrobe://unix/") {
            return Self::unix_from_uri_path(rest);
        }

        if let Some(rest) = connection_string.strip_prefix("wardrobe://") {
            return Self::network_from_authority(rest);
        }

        if connection_string.contains("://") {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                format!("Unsupported Wardrobe connection scheme: {connection_string}"),
            ));
        }

        Ok(Self::EmbeddedPath(PathBuf::from(connection_string)))
    }

    fn embedded_from_uri_path(path: &str) -> Result<Self> {
        let path = Self::normalize_uri_path(path);
        if path.as_os_str().is_empty() {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "Embedded Wardrobe connection URI requires a file-system path",
            ));
        }
        Ok(Self::EmbeddedPath(path))
    }

    fn unix_from_uri_path(path: &str) -> Result<Self> {
        let path = Self::normalize_uri_path(path);
        if path.as_os_str().is_empty() {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "Unix socket Wardrobe connection URI requires a socket path",
            ));
        }
        Ok(Self::UnixSocket { path })
    }

    fn network_from_authority(authority: &str) -> Result<Self> {
        let authority = authority.trim_matches('/');
        if authority.is_empty() {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "Network Wardrobe connection URI requires a host",
            ));
        }

        if authority.contains('/') {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "Network Wardrobe connection URI should not contain a path",
            ));
        }

        let (host, port) = if let Some((host, port)) = authority.rsplit_once(':') {
            if host.is_empty() {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "Network Wardrobe connection URI requires a host before the port",
                ));
            }
            let port = port.parse::<u16>().map_err(|error| {
                Error::new(
                    ErrorKind::InvalidInput,
                    format!("Invalid Wardrobe network port '{port}': {error}"),
                )
            })?;
            (host.to_string(), port)
        } else {
            (authority.to_string(), DEFAULT_NETWORK_PORT)
        };

        Ok(Self::Network { host, port })
    }

    fn normalize_uri_path(path: &str) -> PathBuf {
        let normalized = path.trim_start_matches('/');
        if cfg!(windows) && normalized.len() >= 2 && normalized.as_bytes().get(1) == Some(&b':') {
            return PathBuf::from(normalized);
        }

        if path.starts_with('/') {
            PathBuf::from(format!("/{normalized}"))
        } else {
            PathBuf::from(normalized)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_driver_kind() {
        assert!(DriverKind::Embedded.requires_embedded_engine());
        assert!(!DriverKind::Network.requires_embedded_engine());
        assert!(!DriverKind::UnixSocket.requires_embedded_engine());

        assert!(!DriverKind::Embedded.uses_socket_transport());
        assert!(DriverKind::Network.uses_socket_transport());
        assert!(DriverKind::UnixSocket.uses_socket_transport());
    }

    #[test]
    fn test_connection_target_parsing() {
        assert_eq!(
            ConnectionTarget::parse("wardrobe://local/tmp/db").unwrap(),
            ConnectionTarget::EmbeddedPath(PathBuf::from("tmp/db"))
        );
        assert_eq!(
            ConnectionTarget::parse("wardrobe+file:///tmp/db").unwrap(),
            ConnectionTarget::EmbeddedPath(PathBuf::from("/tmp/db"))
        );
        assert_eq!(
            ConnectionTarget::parse("file:///tmp/db").unwrap(),
            ConnectionTarget::EmbeddedPath(PathBuf::from("/tmp/db"))
        );
        assert_eq!(
            ConnectionTarget::parse("wardrobe+unix:///tmp/wardrobe.sock").unwrap(),
            ConnectionTarget::UnixSocket {
                path: PathBuf::from("/tmp/wardrobe.sock")
            }
        );
        assert_eq!(
            ConnectionTarget::parse("wardrobe://unix/tmp/wardrobe.sock").unwrap(),
            ConnectionTarget::UnixSocket {
                path: PathBuf::from("tmp/wardrobe.sock")
            }
        );
        assert_eq!(
            ConnectionTarget::parse("wardrobe://localhost:24842").unwrap(),
            ConnectionTarget::Network {
                host: "localhost".to_string(),
                port: 24842
            }
        );
        assert_eq!(
            ConnectionTarget::parse("wardrobe://127.0.0.1").unwrap(),
            ConnectionTarget::Network {
                host: "127.0.0.1".to_string(),
                port: DEFAULT_NETWORK_PORT
            }
        );
        assert_eq!(
            ConnectionTarget::parse("/var/lib/wardrobe").unwrap(),
            ConnectionTarget::EmbeddedPath(PathBuf::from("/var/lib/wardrobe"))
        );

        assert!(ConnectionTarget::parse("").is_err());
        assert!(ConnectionTarget::parse("   ").is_err());
        assert!(ConnectionTarget::parse("wardrobe://local/").is_err());
        assert!(ConnectionTarget::parse("wardrobe+unix://").is_err());
        assert!(ConnectionTarget::parse("wardrobe://").is_err());
        assert!(ConnectionTarget::parse("wardrobe://localhost/path").is_err());
        assert!(ConnectionTarget::parse("wardrobe://:24842").is_err());
        assert!(ConnectionTarget::parse("wardrobe://localhost:invalid").is_err());
        assert!(ConnectionTarget::parse("unknown+scheme://test").is_err());
    }

    #[test]
    fn test_target_helper_methods() {
        let embedded = ConnectionTarget::EmbeddedPath(PathBuf::from("/tmp"));
        assert_eq!(embedded.driver_kind(), DriverKind::Embedded);
        assert!(embedded.requires_embedded_engine());
        assert!(!embedded.uses_socket_transport());

        let net = ConnectionTarget::Network {
            host: "localhost".to_string(),
            port: 24842,
        };
        assert_eq!(net.driver_kind(), DriverKind::Network);
        assert!(!net.requires_embedded_engine());
        assert!(net.uses_socket_transport());

        let unix = ConnectionTarget::UnixSocket {
            path: PathBuf::from("/tmp/sock"),
        };
        assert_eq!(unix.driver_kind(), DriverKind::UnixSocket);
        assert!(!unix.requires_embedded_engine());
        assert!(unix.uses_socket_transport());
    }
}
