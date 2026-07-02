use wardrobe_core::{Command, WardrobeEngine};

pub const NPM_PACKAGE_NAME: &str = "@wardrobe/database";
pub const CRATE_NAME: &str = "wardrobe-js-ts";
pub const PACKAGE_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const DEFAULT_NETWORK_PORT: u16 = 24842;
pub const SUPPORTED_OPERATIONS: &[&str] = &[
    "read", "upsert", "delete", "inspect", "count", "clean", "create", "alter", "drop", "backup",
    "restore", "grant", "revoke", "status",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingDriverMode {
    Embedded,
    Network,
    UnixSocket,
}

impl BindingDriverMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Embedded => "embedded",
            Self::Network => "network",
            Self::UnixSocket => "unix-socket",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BindingTarget {
    pub mode: BindingDriverMode,
    pub requires_embedded_engine: bool,
    pub uses_socket_transport: bool,
}

impl BindingTarget {
    pub const fn new(mode: BindingDriverMode) -> Self {
        Self {
            mode,
            requires_embedded_engine: matches!(mode, BindingDriverMode::Embedded),
            uses_socket_transport: matches!(
                mode,
                BindingDriverMode::Network | BindingDriverMode::UnixSocket
            ),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BindingPackageMetadata {
    pub crate_name: &'static str,
    pub npm_package_name: &'static str,
    pub version: &'static str,
    pub default_network_port: u16,
    pub operations: &'static [&'static str],
}

pub const fn package_metadata() -> BindingPackageMetadata {
    BindingPackageMetadata {
        crate_name: CRATE_NAME,
        npm_package_name: NPM_PACKAGE_NAME,
        version: PACKAGE_VERSION,
        default_network_port: DEFAULT_NETWORK_PORT,
        operations: SUPPORTED_OPERATIONS,
    }
}

pub fn classify_connection_target(connection_string: &str) -> Result<BindingTarget, String> {
    let connection_string = connection_string.trim();
    if connection_string.is_empty() {
        return Err(String::from("Wardrobe connection string cannot be empty"));
    }

    let mode = if let Some(path) = connection_string.strip_prefix("wardrobe://local/") {
        embedded_mode(path)?
    } else if let Some(path) = connection_string.strip_prefix("wardrobe+file://") {
        embedded_mode(path)?
    } else if let Some(path) = connection_string.strip_prefix("file://") {
        embedded_mode(path)?
    } else if let Some(path) = connection_string.strip_prefix("wardrobe+unix://") {
        unix_socket_mode(path)?
    } else if let Some(path) = connection_string.strip_prefix("wardrobe://unix/") {
        unix_socket_mode(path)?
    } else if let Some(authority) = connection_string.strip_prefix("wardrobe://") {
        network_mode(authority)?
    } else if connection_string.contains("://") {
        return Err(format!(
            "Unsupported Wardrobe connection scheme: {connection_string}"
        ));
    } else {
        BindingDriverMode::Embedded
    };

    Ok(BindingTarget::new(mode))
}

fn embedded_mode(path: &str) -> Result<BindingDriverMode, String> {
    if path.is_empty() {
        return Err(String::from(
            "Embedded Wardrobe connection URI requires a file-system path",
        ));
    }

    Ok(BindingDriverMode::Embedded)
}

fn unix_socket_mode(path: &str) -> Result<BindingDriverMode, String> {
    if path.is_empty() {
        return Err(String::from(
            "Unix socket Wardrobe connection URI requires a socket path",
        ));
    }

    Ok(BindingDriverMode::UnixSocket)
}

fn network_mode(authority: &str) -> Result<BindingDriverMode, String> {
    let authority = authority.trim_matches('/');
    if authority.is_empty() {
        return Err(String::from(
            "Network Wardrobe connection URI requires a host",
        ));
    }

    if authority.contains('/') {
        return Err(String::from(
            "Network Wardrobe connection URI should not contain a path",
        ));
    }

    if let Some((host, port)) = authority.rsplit_once(':') {
        if host.is_empty() {
            return Err(String::from(
                "Network Wardrobe connection URI requires a host before the port",
            ));
        }

        port.parse::<u16>()
            .map_err(|error| format!("Invalid Wardrobe network port '{port}': {error}"))?;
    }

    Ok(BindingDriverMode::Network)
}

#[napi_derive::napi]
pub fn execute_command(target: String, command_json: String) -> Result<String, napi::Error> {
    let command: Command = serde_json::from_str(&command_json)
        .map_err(|e| napi::Error::from_reason(format!("Failed to deserialize command JSON: {}", e)))?;

    let engine = WardrobeEngine::open(&target)
        .map_err(|e| napi::Error::from_reason(format!("Failed to open engine: {}", e)))?;

    let result = engine.execute_command(command)
        .map_err(|e| napi::Error::from_reason(format!("Execution failed: {}", e)))?;

    let result_json = serde_json::to_string(&result)
        .map_err(|e| napi::Error::from_reason(format!("Failed to serialize command result: {}", e)))?;

    Ok(result_json)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_metadata_matches_npm_publish_readiness_values() {
        let metadata = package_metadata();

        assert_eq!(metadata.crate_name, "wardrobe-js-ts");
        assert_eq!(metadata.npm_package_name, "@wardrobe/database");
        assert_eq!(metadata.version, "0.1.0");
        assert_eq!(metadata.default_network_port, 24842);
        assert_eq!(
            metadata.operations,
            &[
                "read", "upsert", "delete", "inspect", "count", "clean", "create", "alter", "drop",
                "backup", "restore", "grant", "revoke", "status",
            ]
        );
    }

    #[test]
    fn classifies_connection_targets_with_core_rules() {
        assert_eq!(
            classify_connection_target("./data").unwrap(),
            BindingTarget::new(BindingDriverMode::Embedded)
        );
        assert_eq!(
            classify_connection_target("wardrobe://localhost").unwrap(),
            BindingTarget::new(BindingDriverMode::Network)
        );
        assert_eq!(
            classify_connection_target("wardrobe+unix:///tmp/wardrobe.sock").unwrap(),
            BindingTarget::new(BindingDriverMode::UnixSocket)
        );
    }

    #[test]
    fn rejects_invalid_connection_targets() {
        let error = classify_connection_target("https://example.com").unwrap_err();

        assert!(error.contains("Unsupported Wardrobe connection scheme"));
    }
}
