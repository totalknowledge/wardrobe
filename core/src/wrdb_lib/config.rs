use crate::wrdb_lib::application_logging::ApplicationLoggingConfig;
use crate::wrdb_lib::database::Database;
use crate::wrdb_lib::transport::connection::DEFAULT_NETWORK_PORT;
use crate::wrdb_lib::wal::DurabilityPolicy;
use serde::Deserialize;
use serde_json::{Value, json};
use std::fs;
use std::io::{Error, ErrorKind, Result};
use std::path::{Path, PathBuf};

pub const DEFAULT_GROUP_COMMIT_WINDOW_MS: u64 = 5;
pub const DEFAULT_GROUP_COMMIT_MAX_BATCH: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WardrobeConfig {
    pub data: DataConfig,
    pub network: NetworkConfig,
    pub cache: CacheConfig,
    pub wal: WalConfig,
    pub transactions: TransactionConfig,
    pub security: SecurityConfig,
    pub logging: ApplicationLoggingConfig,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataConfig {
    pub directory: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkConfig {
    pub tcp_enabled: bool,
    pub tcp_bind: String,
    pub unix_socket_enabled: bool,
    pub unix_socket: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheConfig {
    pub max_cached_drawers: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalConfig {
    pub durability: DurabilityPolicy,
    pub checkpoint_size_bytes: u64,
    pub checkpoint_ops: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransactionConfig {
    pub enabled: bool,
    pub log_directory: PathBuf,
    pub recovery: TransactionRecoveryMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransactionRecoveryMode {
    Automatic,
    Manual,
    Disabled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecurityConfig {
    pub access_control_file: PathBuf,
    pub auth_required: bool,
}

impl Default for WardrobeConfig {
    fn default() -> Self {
        let (checkpoint_size_bytes, checkpoint_ops) = Database::default_wal_thresholds();
        Self {
            data: DataConfig {
                directory: PathBuf::from("./wardrobe"),
            },
            network: NetworkConfig {
                tcp_enabled: true,
                tcp_bind: format!("127.0.0.1:{DEFAULT_NETWORK_PORT}"),
                unix_socket_enabled: false,
                unix_socket: PathBuf::from("/tmp/wardrobe.sock"),
            },
            cache: CacheConfig {
                max_cached_drawers: None,
            },
            wal: WalConfig {
                durability: DurabilityPolicy::Strict,
                checkpoint_size_bytes,
                checkpoint_ops,
            },
            transactions: TransactionConfig {
                enabled: true,
                log_directory: PathBuf::from("./wardrobe/.transactions"),
                recovery: TransactionRecoveryMode::Automatic,
            },
            security: SecurityConfig {
                access_control_file: PathBuf::from("_wardrobe_access_control.json"),
                auth_required: false,
            },
            logging: ApplicationLoggingConfig::default(),
        }
    }
}

impl WardrobeConfig {
    pub fn from_toml_file(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let contents = fs::read_to_string(path)?;
        Self::from_toml_str(&contents).map_err(|error| {
            Error::new(
                error.kind(),
                format!(
                    "Failed to load Wardrobe config from {}: {error}",
                    path.display()
                ),
            )
        })
    }

    pub fn from_toml_str(contents: &str) -> Result<Self> {
        let raw: RawWardrobeConfig = toml::from_str(contents).map_err(|error| {
            Error::new(
                ErrorKind::InvalidData,
                format!("Invalid Wardrobe TOML config: {error}"),
            )
        })?;
        raw.resolve()
    }

    pub fn validate(&self) -> Result<()> {
        if self.data.directory.as_os_str().is_empty() {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "data.directory cannot be empty",
            ));
        }
        if self.cache.max_cached_drawers == Some(0) {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "cache.max_cached_drawers must be greater than zero when provided",
            ));
        }
        if self.wal.checkpoint_size_bytes == 0 {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "wal.checkpoint_size_bytes must be greater than zero",
            ));
        }
        if self.wal.checkpoint_ops == 0 {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "wal.checkpoint_ops must be greater than zero",
            ));
        }
        if self.network.tcp_enabled && self.network.tcp_bind.trim().is_empty() {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "network.tcp_bind cannot be empty when TCP is enabled",
            ));
        }
        if self.network.unix_socket_enabled && self.network.unix_socket.as_os_str().is_empty() {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "network.unix_socket cannot be empty when Unix sockets are enabled",
            ));
        }
        if self.transactions.log_directory.as_os_str().is_empty() {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "transactions.log_directory cannot be empty",
            ));
        }
        if self.security.access_control_file.as_os_str().is_empty() {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "security.access_control_file cannot be empty",
            ));
        }
        self.logging.validate()
    }

    pub fn validate_for_server(&self) -> Result<()> {
        self.validate()?;
        if !self.network.tcp_enabled && !self.network.unix_socket_enabled {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "server network config must enable TCP or Unix socket listener",
            ));
        }
        Ok(())
    }

    pub fn redacted_summary(&self) -> Value {
        json!({
            "data": {
                "directory": self.data.directory.display().to_string(),
            },
            "network": {
                "tcp_enabled": self.network.tcp_enabled,
                "tcp_bind": self.network.tcp_bind,
                "unix_socket_enabled": self.network.unix_socket_enabled,
                "unix_socket": self.network.unix_socket.display().to_string(),
            },
            "cache": {
                "max_cached_drawers": self.cache.max_cached_drawers,
            },
            "wal": {
                "durability": durability_policy_name(&self.wal.durability),
                "checkpoint_size_bytes": self.wal.checkpoint_size_bytes,
                "checkpoint_ops": self.wal.checkpoint_ops,
            },
            "transactions": {
                "enabled": self.transactions.enabled,
                "log_directory": self.transactions.log_directory.display().to_string(),
                "recovery": self.transactions.recovery.as_str(),
            },
            "security": {
                "access_control_file": self.security.access_control_file.display().to_string(),
                "auth_required": self.security.auth_required,
            },
            "logging": {
                "level": self.logging.level.as_str(),
                "format": self.logging.format.as_str(),
                "destination": self.logging.destination.as_str(),
                "file": self.logging.file.as_ref().map(|path| path.display().to_string()),
            }
        })
    }
}

impl TransactionRecoveryMode {
    pub fn parse(raw: &str) -> Result<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "automatic" | "auto" => Ok(Self::Automatic),
            "manual" => Ok(Self::Manual),
            "disabled" | "off" | "none" => Ok(Self::Disabled),
            other => Err(Error::new(
                ErrorKind::InvalidInput,
                format!(
                    "Unsupported transaction recovery mode '{other}'; expected automatic, manual, or disabled"
                ),
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Automatic => "automatic",
            Self::Manual => "manual",
            Self::Disabled => "disabled",
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawWardrobeConfig {
    data: Option<RawDataConfig>,
    network: Option<RawNetworkConfig>,
    cache: Option<RawCacheConfig>,
    wal: Option<RawWalConfig>,
    transactions: Option<RawTransactionConfig>,
    security: Option<RawSecurityConfig>,
    logging: Option<RawLoggingConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDataConfig {
    directory: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawNetworkConfig {
    tcp_enabled: Option<bool>,
    tcp_bind: Option<String>,
    unix_socket_enabled: Option<bool>,
    unix_socket: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCacheConfig {
    max_cached_drawers: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawWalConfig {
    durability: Option<String>,
    checkpoint_size_bytes: Option<u64>,
    checkpoint_ops: Option<u64>,
    group_commit_window_ms: Option<u64>,
    group_commit_max_batch: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTransactionConfig {
    enabled: Option<bool>,
    log_directory: Option<PathBuf>,
    recovery: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSecurityConfig {
    access_control_file: Option<PathBuf>,
    auth_required: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLoggingConfig {
    level: Option<String>,
    format: Option<String>,
    destination: Option<String>,
    file: Option<PathBuf>,
}

impl RawWardrobeConfig {
    fn resolve(self) -> Result<WardrobeConfig> {
        let mut config = WardrobeConfig::default();

        if let Some(data) = self.data {
            if let Some(directory) = data.directory {
                config.data.directory = directory;
            }
        }

        if let Some(network) = self.network {
            if let Some(tcp_enabled) = network.tcp_enabled {
                config.network.tcp_enabled = tcp_enabled;
            }
            if let Some(tcp_bind) = network.tcp_bind {
                config.network.tcp_bind = tcp_bind;
            }
            if let Some(unix_socket_enabled) = network.unix_socket_enabled {
                config.network.unix_socket_enabled = unix_socket_enabled;
            }
            if let Some(unix_socket) = network.unix_socket {
                config.network.unix_socket = unix_socket;
            }
        }

        if let Some(cache) = self.cache {
            config.cache.max_cached_drawers = cache.max_cached_drawers;
        }

        if let Some(wal) = self.wal {
            let group_commit_window_ms = wal
                .group_commit_window_ms
                .unwrap_or(DEFAULT_GROUP_COMMIT_WINDOW_MS);
            let group_commit_max_batch = wal
                .group_commit_max_batch
                .unwrap_or(DEFAULT_GROUP_COMMIT_MAX_BATCH);
            if let Some(durability) = wal.durability {
                config.wal.durability = parse_durability_policy(
                    &durability,
                    group_commit_window_ms,
                    group_commit_max_batch,
                )?;
            }
            if let Some(checkpoint_size_bytes) = wal.checkpoint_size_bytes {
                config.wal.checkpoint_size_bytes = checkpoint_size_bytes;
            }
            if let Some(checkpoint_ops) = wal.checkpoint_ops {
                config.wal.checkpoint_ops = checkpoint_ops;
            }
        }

        if let Some(transactions) = self.transactions {
            if let Some(enabled) = transactions.enabled {
                config.transactions.enabled = enabled;
            }
            if let Some(log_directory) = transactions.log_directory {
                config.transactions.log_directory = log_directory;
            }
            if let Some(recovery) = transactions.recovery {
                config.transactions.recovery = TransactionRecoveryMode::parse(&recovery)?;
            }
        }

        if let Some(security) = self.security {
            if let Some(access_control_file) = security.access_control_file {
                config.security.access_control_file = access_control_file;
            }
            if let Some(auth_required) = security.auth_required {
                config.security.auth_required = auth_required;
            }
        }

        if let Some(logging) = self.logging {
            config.logging = ApplicationLoggingConfig::from_parts(
                logging.level.as_deref(),
                logging.format.as_deref(),
                logging.destination.as_deref(),
                logging.file,
            )?;
        }

        config.validate()?;
        Ok(config)
    }
}

fn parse_durability_policy(
    raw: &str,
    commit_window_ms: u64,
    max_batch_size: usize,
) -> Result<DurabilityPolicy> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "strict" => Ok(DurabilityPolicy::Strict),
        "grouped" => {
            if commit_window_ms == 0 {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "wal.group_commit_window_ms must be greater than zero",
                ));
            }
            if max_batch_size == 0 {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "wal.group_commit_max_batch must be greater than zero",
                ));
            }
            Ok(DurabilityPolicy::Grouped {
                commit_window_ms,
                max_batch_size,
            })
        }
        other => Err(Error::new(
            ErrorKind::InvalidInput,
            format!("Unsupported WAL durability '{other}'; expected strict or grouped"),
        )),
    }
}

fn durability_policy_name(policy: &DurabilityPolicy) -> &'static str {
    match policy {
        DurabilityPolicy::Strict => "strict",
        DurabilityPolicy::Grouped { .. } => "grouped",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wrdb_lib::application_logging::ApplicationLogLevel;

    #[test]
    fn default_config_validates() {
        WardrobeConfig::default()
            .validate_for_server()
            .expect("default config should validate for server");
    }

    #[test]
    fn toml_config_deserializes_and_omitted_sections_default() {
        let config = WardrobeConfig::from_toml_str(
            r#"
            [data]
            directory = "./data"

            [network]
            tcp_bind = "127.0.0.1:3000"

            [cache]
            max_cached_drawers = 128

            [wal]
            durability = "grouped"
            group_commit_window_ms = 7
            group_commit_max_batch = 64
            checkpoint_size_bytes = 1048576
            checkpoint_ops = 1000

            [logging]
            level = "info"
            format = "json"
            destination = "stderr"
            "#,
        )
        .expect("config should parse");

        assert_eq!(config.data.directory, PathBuf::from("./data"));
        assert_eq!(config.network.tcp_bind, "127.0.0.1:3000");
        assert_eq!(config.cache.max_cached_drawers, Some(128));
        assert_eq!(config.wal.checkpoint_ops, 1000);
        assert_eq!(config.logging.level, ApplicationLogLevel::Info);
        assert!(matches!(
            config.wal.durability,
            DurabilityPolicy::Grouped {
                commit_window_ms: 7,
                max_batch_size: 64
            }
        ));
        assert!(config.transactions.enabled);
    }

    #[test]
    fn invalid_config_fails_with_useful_errors() {
        assert!(
            WardrobeConfig::from_toml_str(
                r#"
                [wal]
                checkpoint_size_bytes = 0
                "#,
            )
            .expect_err("zero wal size should fail")
            .to_string()
            .contains("checkpoint_size_bytes")
        );

        assert!(
            WardrobeConfig::from_toml_str(
                r#"
                [cache]
                max_cached_drawers = 0
                "#,
            )
            .is_err()
        );

        assert!(
            WardrobeConfig::from_toml_str(
                r#"
                [logging]
                level = "verbose"
                "#,
            )
            .is_err()
        );
    }
}
