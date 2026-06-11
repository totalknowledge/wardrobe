use std::io::{Error, ErrorKind, Result};
use std::path::PathBuf;

pub const DEFAULT_NETWORK_PORT: u16 = 24842;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriverKind {
    Embedded,
    Network,
    UnixSocket,
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
