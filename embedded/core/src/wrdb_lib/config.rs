use crate::wrdb_lib::application_logging::ApplicationLoggingConfig;
use crate::wrdb_lib::database::Database;
use crate::wrdb_lib::transport::connection::DEFAULT_NETWORK_PORT;
use crate::wrdb_lib::wal::DurabilityPolicy;
use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose, SanType, SerialNumber,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::fs;
use std::io::{Error, ErrorKind, Result};
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use uuid::Uuid;
use x509_parser::extensions::GeneralName;
use x509_parser::parse_x509_certificate;

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
    pub mode: SecurityMode,
    pub security_dir: PathBuf,
    pub server_certificate: Option<PathBuf>,
    pub server_private_key: Option<PathBuf>,
    pub trusted_client_ca_bundles: Vec<PathBuf>,
    pub server_names: Vec<String>,
    pub server_ips: Vec<IpAddr>,
    pub unsafe_allow_remote_disabled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecurityMode {
    Managed,
    External,
    Disabled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientTlsConfig {
    pub ca_cert: PathBuf,
    pub client_cert: PathBuf,
    pub client_key: PathBuf,
    pub server_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientCertificateProfile {
    pub identity: String,
    pub server_name: String,
    pub ca_cert: PathBuf,
    pub client_cert: PathBuf,
    pub client_key: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CertificateRecord {
    pub serial: String,
    pub identity: String,
    pub device: String,
    pub certificate: PathBuf,
    pub profile: PathBuf,
    pub subject: String,
    pub issuer: String,
    pub not_before: String,
    pub not_after: String,
    pub revoked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PkiInitialization {
    pub security_dir: PathBuf,
    pub ca_certificate: PathBuf,
    pub server_certificate: PathBuf,
    pub server_private_key: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertificateIdentity {
    pub serial: String,
    pub identity: String,
    pub subject: String,
    pub issuer: String,
    pub not_before: String,
    pub not_after: String,
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            access_control_file: PathBuf::from("_wardrobe_access_control.json"),
            auth_required: false,
            mode: SecurityMode::Disabled,
            security_dir: PathBuf::from("./security"),
            server_certificate: None,
            server_private_key: None,
            trusted_client_ca_bundles: Vec::new(),
            server_names: vec!["localhost".to_string()],
            server_ips: vec![
                IpAddr::from([127, 0, 0, 1]),
                IpAddr::from([0, 0, 0, 0, 0, 0, 0, 1]),
            ],
            unsafe_allow_remote_disabled: false,
        }
    }
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
            security: SecurityConfig::default(),
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
        if self.security.security_dir.as_os_str().is_empty() {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "security.security_dir cannot be empty",
            ));
        }
        match self.security.mode {
            SecurityMode::Managed => {
                if self.security.server_names.is_empty() && self.security.server_ips.is_empty() {
                    return Err(Error::new(
                        ErrorKind::InvalidInput,
                        "managed security requires at least one server name or server IP",
                    ));
                }
            }
            SecurityMode::External => {
                if self.security.server_certificate.is_none() {
                    return Err(Error::new(
                        ErrorKind::InvalidInput,
                        "external security requires security.server_certificate",
                    ));
                }
                if self.security.server_private_key.is_none() {
                    return Err(Error::new(
                        ErrorKind::InvalidInput,
                        "external security requires security.server_private_key",
                    ));
                }
                if self.security.trusted_client_ca_bundles.is_empty() {
                    return Err(Error::new(
                        ErrorKind::InvalidInput,
                        "external security requires at least one trusted client CA bundle",
                    ));
                }
            }
            SecurityMode::Disabled => {}
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
        if self.security.mode != SecurityMode::Disabled && self.network.unix_socket_enabled {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "managed and external security require the TCP TLS listener; Unix sockets must use disabled mode",
            ));
        }
        if self.security.mode == SecurityMode::Disabled
            && self.network.tcp_enabled
            && !self.security.unsafe_allow_remote_disabled
            && !tcp_bind_is_local(&self.network.tcp_bind)
        {
            return Err(Error::new(
                ErrorKind::PermissionDenied,
                "disabled authentication may bind TCP only to localhost; set security.unsafe_allow_remote_disabled = true to override",
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
                "mode": self.security.mode.as_str(),
                "security_dir": self.security.security_dir.display().to_string(),
                "server_certificate": self.security.server_certificate.as_ref().map(|path| path.display().to_string()),
                "server_private_key": self.security.server_private_key.as_ref().map(|_| "<redacted>"),
                "trusted_client_ca_bundles": self.security.trusted_client_ca_bundles.iter().map(|path| path.display().to_string()).collect::<Vec<_>>(),
                "server_names": self.security.server_names,
                "server_ips": self.security.server_ips.iter().map(ToString::to_string).collect::<Vec<_>>(),
                "unsafe_allow_remote_disabled": self.security.unsafe_allow_remote_disabled,
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

impl SecurityMode {
    pub fn parse(raw: &str) -> Result<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "managed" => Ok(Self::Managed),
            "external" => Ok(Self::External),
            "disabled" | "off" | "none" => Ok(Self::Disabled),
            other => Err(Error::new(
                ErrorKind::InvalidInput,
                format!(
                    "Unsupported security mode '{other}'; expected managed, external, or disabled"
                ),
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Managed => "managed",
            Self::External => "external",
            Self::Disabled => "disabled",
        }
    }
}

impl ClientTlsConfig {
    pub fn from_profile(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let contents = fs::read_to_string(path)?;
        let profile: ClientCertificateProfile = toml::from_str(&contents).map_err(|error| {
            Error::new(
                ErrorKind::InvalidData,
                format!(
                    "Invalid Wardrobe client profile {}: {error}",
                    path.display()
                ),
            )
        })?;
        let base = path.parent().unwrap_or_else(|| Path::new("."));
        Ok(Self {
            ca_cert: resolve_profile_path(base, profile.ca_cert),
            client_cert: resolve_profile_path(base, profile.client_cert),
            client_key: resolve_profile_path(base, profile.client_key),
            server_name: profile.server_name,
        })
    }
}

fn resolve_profile_path(base: &Path, path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        base.join(path)
    }
}

fn tcp_bind_is_local(bind: &str) -> bool {
    if let Ok(address) = bind.parse::<SocketAddr>() {
        return address.ip().is_loopback();
    }
    let host = bind
        .rsplit_once(':')
        .map(|(host, _)| host.trim_matches(['[', ']']))
        .unwrap_or(bind);
    host.eq_ignore_ascii_case("localhost")
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
    mode: Option<String>,
    security_dir: Option<PathBuf>,
    server_certificate: Option<PathBuf>,
    server_private_key: Option<PathBuf>,
    trusted_client_ca_bundles: Option<Vec<PathBuf>>,
    server_names: Option<Vec<String>>,
    server_ips: Option<Vec<IpAddr>>,
    unsafe_allow_remote_disabled: Option<bool>,
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
            if let Some(mode) = security.mode {
                config.security.mode = SecurityMode::parse(&mode)?;
            }
            if let Some(security_dir) = security.security_dir {
                config.security.security_dir = security_dir;
            }
            if let Some(server_certificate) = security.server_certificate {
                config.security.server_certificate = Some(server_certificate);
            }
            if let Some(server_private_key) = security.server_private_key {
                config.security.server_private_key = Some(server_private_key);
            }
            if let Some(trusted_client_ca_bundles) = security.trusted_client_ca_bundles {
                config.security.trusted_client_ca_bundles = trusted_client_ca_bundles;
            }
            if let Some(server_names) = security.server_names {
                config.security.server_names = server_names;
            }
            if let Some(server_ips) = security.server_ips {
                config.security.server_ips = server_ips;
            }
            if let Some(unsafe_allow_remote_disabled) = security.unsafe_allow_remote_disabled {
                config.security.unsafe_allow_remote_disabled = unsafe_allow_remote_disabled;
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

#[derive(Debug, Default, Serialize, Deserialize)]
struct CertificateRegistry {
    certificates: Vec<CertificateRecord>,
}

pub fn initialize_managed_pki(
    security_dir: impl AsRef<Path>,
    server_names: &[String],
    server_ips: &[IpAddr],
) -> Result<PkiInitialization> {
    let security_dir = security_dir.as_ref();
    let ca_certificate = security_dir.join("ca").join("ca.crt");
    let ca_private_key = security_dir.join("ca").join("ca.key");
    let server_certificate = security_dir.join("server").join("server.crt");
    let server_private_key = security_dir.join("server").join("server.key");
    if [
        &ca_certificate,
        &ca_private_key,
        &server_certificate,
        &server_private_key,
    ]
    .iter()
    .any(|path| path.exists())
    {
        return Err(Error::new(
            ErrorKind::AlreadyExists,
            format!(
                "Wardrobe security identity already exists in {}; startup and initialization never overwrite it",
                security_dir.display()
            ),
        ));
    }
    if server_names.is_empty() && server_ips.is_empty() {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "managed PKI initialization requires at least one server name or IP address",
        ));
    }

    fs::create_dir_all(security_dir.join("ca"))?;
    fs::create_dir_all(security_dir.join("server"))?;
    fs::create_dir_all(security_dir.join("bootstrap"))?;
    fs::create_dir_all(security_dir.join("clients"))?;

    let (ca_pem, ca_key_pem) = generate_managed_ca()?;
    fs::write(&ca_certificate, ca_pem)?;
    write_private_key(&ca_private_key, &ca_key_pem)?;
    rebuild_ca_bundle(security_dir)?;

    issue_server_certificate(
        security_dir,
        server_names,
        server_ips,
        &ca_certificate,
        &ca_private_key,
    )?;
    fs::write(
        security_dir.join("revoked.json"),
        serde_json::to_vec_pretty(&Vec::<String>::new()).map_err(json_error)?,
    )?;
    write_certificate_registry(security_dir, &CertificateRegistry::default())?;

    Ok(PkiInitialization {
        security_dir: security_dir.to_path_buf(),
        ca_certificate,
        server_certificate,
        server_private_key,
    })
}

pub fn rotate_managed_ca(
    security_dir: impl AsRef<Path>,
    server_names: &[String],
    server_ips: &[IpAddr],
) -> Result<PkiInitialization> {
    let security_dir = security_dir.as_ref();
    let ca_certificate = security_dir.join("ca").join("ca.crt");
    let ca_private_key = security_dir.join("ca").join("ca.key");
    require_file(&ca_certificate, "managed CA certificate")?;
    require_file(&ca_private_key, "managed CA private key")?;
    let archive = security_dir
        .join("ca")
        .join("archive")
        .join(Uuid::new_v4().simple().to_string());
    fs::create_dir_all(&archive)?;
    fs::copy(&ca_certificate, archive.join("ca.crt"))?;
    let archived_key = archive.join("ca.key");
    fs::copy(&ca_private_key, &archived_key)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&archived_key, fs::Permissions::from_mode(0o600))?;
    }

    let (ca_pem, ca_key_pem) = generate_managed_ca()?;
    fs::write(&ca_certificate, ca_pem)?;
    write_private_key(&ca_private_key, &ca_key_pem)?;
    rebuild_ca_bundle(security_dir)?;
    issue_server_certificate(
        security_dir,
        server_names,
        server_ips,
        &ca_certificate,
        &ca_private_key,
    )?;
    refresh_managed_profile_ca_bundles(security_dir)?;
    Ok(PkiInitialization {
        security_dir: security_dir.to_path_buf(),
        ca_certificate,
        server_certificate: security_dir.join("server").join("server.crt"),
        server_private_key: security_dir.join("server").join("server.key"),
    })
}

pub fn reissue_managed_server_certificate(
    security_dir: impl AsRef<Path>,
    server_names: &[String],
    server_ips: &[IpAddr],
) -> Result<PkiInitialization> {
    let security_dir = security_dir.as_ref();
    let ca_certificate = security_dir.join("ca").join("ca.crt");
    let ca_private_key = security_dir.join("ca").join("ca.key");
    require_file(&ca_certificate, "managed CA certificate")?;
    require_file(&ca_private_key, "managed CA private key")?;
    issue_server_certificate(
        security_dir,
        server_names,
        server_ips,
        &ca_certificate,
        &ca_private_key,
    )?;
    Ok(PkiInitialization {
        security_dir: security_dir.to_path_buf(),
        ca_certificate,
        server_certificate: security_dir.join("server").join("server.crt"),
        server_private_key: security_dir.join("server").join("server.key"),
    })
}

fn issue_server_certificate(
    security_dir: &Path,
    server_names: &[String],
    server_ips: &[IpAddr],
    ca_certificate: &Path,
    ca_private_key: &Path,
) -> Result<()> {
    if server_names.is_empty() && server_ips.is_empty() {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "server certificate requires at least one DNS name or IP address",
        ));
    }
    let ca_pem = fs::read_to_string(ca_certificate)?;
    let ca_key = KeyPair::from_pem(&fs::read_to_string(ca_private_key)?).map_err(pki_error)?;
    let issuer = Issuer::from_ca_cert_pem(&ca_pem, ca_key).map_err(pki_error)?;
    let server_key = KeyPair::generate().map_err(pki_error)?;
    let mut params = CertificateParams::new(server_names.to_vec()).map_err(pki_error)?;
    params
        .distinguished_name
        .push(DnType::CommonName, "Wardrobe Server");
    for ip in server_ips {
        if !params.subject_alt_names.contains(&SanType::IpAddress(*ip)) {
            params.subject_alt_names.push(SanType::IpAddress(*ip));
        }
    }
    params.serial_number = Some(new_serial());
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let certificate = params.signed_by(&server_key, &issuer).map_err(pki_error)?;
    fs::create_dir_all(security_dir.join("server"))?;
    fs::write(
        security_dir.join("server").join("server.crt"),
        certificate.pem(),
    )?;
    write_private_key(
        &security_dir.join("server").join("server.key"),
        &server_key.serialize_pem(),
    )
}

pub fn issue_managed_client_certificate(
    security_dir: impl AsRef<Path>,
    identity: &str,
    device: &str,
    output: Option<&Path>,
    server_name: &str,
) -> Result<CertificateRecord> {
    validate_certificate_identity(identity)?;
    let device = validate_device_name(device)?;
    if server_name.trim().is_empty() {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "client profile server name cannot be empty",
        ));
    }
    let security_dir = security_dir.as_ref();
    let ca_path = security_dir.join("ca").join("ca.crt");
    let ca_key_path = security_dir.join("ca").join("ca.key");
    require_file(&ca_path, "managed CA certificate")?;
    require_file(&ca_key_path, "managed CA private key")?;
    let ca_pem = fs::read_to_string(&ca_path)?;
    let ca_key = KeyPair::from_pem(&fs::read_to_string(&ca_key_path)?).map_err(pki_error)?;
    let issuer = Issuer::from_ca_cert_pem(&ca_pem, ca_key).map_err(pki_error)?;

    let client_key = KeyPair::generate().map_err(pki_error)?;
    let mut params = CertificateParams::new(Vec::<String>::new()).map_err(pki_error)?;
    let display_identity = identity.rsplit(':').next().unwrap_or(identity);
    params
        .distinguished_name
        .push(DnType::CommonName, display_identity);
    params.subject_alt_names.push(SanType::URI(
        identity
            .try_into()
            .map_err(|error| Error::new(ErrorKind::InvalidInput, format!("{error}")))?,
    ));
    params.serial_number = Some(new_serial());
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
    let certificate = params.signed_by(&client_key, &issuer).map_err(pki_error)?;
    let info = certificate_identity_from_der(certificate.der().as_ref())?;

    let output = output.map(Path::to_path_buf).unwrap_or_else(|| {
        security_dir
            .join("clients")
            .join(path_component(identity))
            .join(path_component(device))
    });
    fs::create_dir_all(&output)?;
    let certificate_path = output.join("client.crt");
    let private_key_path = output.join("client.key");
    let exported_ca_path = output.join("ca.crt");
    let profile_path = output.join("profile.toml");
    fs::write(&certificate_path, certificate.pem())?;
    write_private_key(&private_key_path, &client_key.serialize_pem())?;
    let ca_bundle = security_dir.join("ca").join("ca-bundle.crt");
    fs::copy(
        if ca_bundle.is_file() {
            &ca_bundle
        } else {
            &ca_path
        },
        &exported_ca_path,
    )?;
    let profile = ClientCertificateProfile {
        identity: identity.to_string(),
        server_name: server_name.to_string(),
        ca_cert: PathBuf::from("ca.crt"),
        client_cert: PathBuf::from("client.crt"),
        client_key: PathBuf::from("client.key"),
    };
    fs::write(
        &profile_path,
        toml::to_string_pretty(&profile).map_err(toml_error)?,
    )?;

    let mut record = CertificateRecord {
        serial: info.serial,
        identity: info.identity,
        device: device.to_string(),
        certificate: absolute_path(&certificate_path)?,
        profile: absolute_path(&profile_path)?,
        subject: info.subject,
        issuer: info.issuer,
        not_before: info.not_before,
        not_after: info.not_after,
        revoked: false,
    };
    let revoked = read_revoked_serials(security_dir)?;
    record.revoked = revoked.contains(&record.serial);
    let mut registry = read_certificate_registry(security_dir)?;
    registry.certificates.push(record.clone());
    write_certificate_registry(security_dir, &registry)?;
    Ok(record)
}

pub fn renew_managed_client_certificate(
    security_dir: impl AsRef<Path>,
    identity: &str,
    device: &str,
    output: Option<&Path>,
    server_name: &str,
) -> Result<CertificateRecord> {
    let security_dir = security_dir.as_ref();
    let records = list_managed_certificates(security_dir)?;
    for record in records
        .iter()
        .filter(|record| record.identity == identity && record.device == device && !record.revoked)
    {
        revoke_managed_certificate(security_dir, &record.serial)?;
    }
    issue_managed_client_certificate(security_dir, identity, device, output, server_name)
}

pub fn revoke_managed_certificate(security_dir: impl AsRef<Path>, serial: &str) -> Result<bool> {
    let security_dir = security_dir.as_ref();
    let serial = normalize_serial(serial);
    if serial.is_empty() {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "certificate serial cannot be empty",
        ));
    }
    let mut revoked = read_revoked_serials(security_dir)?;
    let inserted = revoked.insert(serial.clone());
    write_revoked_serials(security_dir, &revoked)?;
    let mut registry = read_certificate_registry(security_dir)?;
    for record in &mut registry.certificates {
        if normalize_serial(&record.serial) == serial {
            record.revoked = true;
        }
    }
    write_certificate_registry(security_dir, &registry)?;
    Ok(inserted)
}

pub fn remove_managed_identity(security_dir: impl AsRef<Path>, identity: &str) -> Result<usize> {
    validate_certificate_identity(identity)?;
    let security_dir = security_dir.as_ref();
    let records = list_managed_certificates(security_dir)?;
    let mut removed = 0;
    for record in records
        .iter()
        .filter(|record| record.identity == identity && !record.revoked)
    {
        if revoke_managed_certificate(security_dir, &record.serial)? {
            removed += 1;
        }
    }
    Ok(removed)
}

pub fn list_managed_certificates(security_dir: impl AsRef<Path>) -> Result<Vec<CertificateRecord>> {
    let security_dir = security_dir.as_ref();
    let mut registry = read_certificate_registry(security_dir)?;
    let revoked = read_revoked_serials(security_dir)?;
    for record in &mut registry.certificates {
        record.revoked = revoked.contains(&normalize_serial(&record.serial));
    }
    Ok(registry.certificates)
}

pub fn managed_identity_certificates(
    security_dir: impl AsRef<Path>,
    identity: &str,
) -> Result<Vec<CertificateRecord>> {
    let identity = if identity.starts_with("wardrobe:") {
        identity.to_string()
    } else {
        format!("wardrobe:user:{identity}")
    };
    Ok(list_managed_certificates(security_dir)?
        .into_iter()
        .filter(|record| record.identity == identity)
        .collect())
}

pub fn certificate_is_revoked(security_dir: impl AsRef<Path>, serial: &str) -> Result<bool> {
    Ok(read_revoked_serials(security_dir.as_ref())?.contains(&normalize_serial(serial)))
}

pub fn certificate_identity_from_pem(path: impl AsRef<Path>) -> Result<CertificateIdentity> {
    let bytes = fs::read(path.as_ref())?;
    let mut reader = std::io::BufReader::new(bytes.as_slice());
    let certificate = rustls_pemfile::certs(&mut reader)
        .next()
        .transpose()?
        .ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidData,
                format!("No X.509 certificate found in {}", path.as_ref().display()),
            )
        })?;
    certificate_identity_from_der(certificate.as_ref())
}

pub fn certificate_identity_from_der(der: &[u8]) -> Result<CertificateIdentity> {
    let (_, certificate) = parse_x509_certificate(der).map_err(|error| {
        Error::new(
            ErrorKind::InvalidData,
            format!("Invalid X.509 certificate: {error}"),
        )
    })?;
    let subject_alternative_name = certificate
        .subject_alternative_name()
        .map_err(|error| {
            Error::new(
                ErrorKind::InvalidData,
                format!("Invalid certificate subject alternative name: {error}"),
            )
        })?
        .ok_or_else(|| {
            Error::new(
                ErrorKind::PermissionDenied,
                "client certificate does not contain a URI SAN identity",
            )
        })?;
    let identities = subject_alternative_name
        .value
        .general_names
        .iter()
        .filter_map(|name| match name {
            GeneralName::URI(uri) if uri.starts_with("wardrobe:") => Some((*uri).to_string()),
            _ => None,
        })
        .collect::<Vec<_>>();
    if identities.len() != 1 {
        return Err(Error::new(
            ErrorKind::PermissionDenied,
            "client certificate must contain exactly one wardrobe URI SAN identity",
        ));
    }
    Ok(CertificateIdentity {
        serial: normalize_serial(&certificate.raw_serial_as_string()),
        identity: identities[0].clone(),
        subject: certificate.subject().to_string(),
        issuer: certificate.issuer().to_string(),
        not_before: certificate.validity().not_before.to_string(),
        not_after: certificate.validity().not_after.to_string(),
    })
}

fn validate_certificate_identity(identity: &str) -> Result<()> {
    let valid_prefix =
        identity.starts_with("wardrobe:user:") || identity.starts_with("wardrobe:service:");
    let name = identity.rsplit(':').next().unwrap_or_default();
    if !valid_prefix
        || name.is_empty()
        || !name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "-_.".contains(character))
    {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "certificate identity must be wardrobe:user:<name> or wardrobe:service:<name>",
        ));
    }
    Ok(())
}

fn generate_managed_ca() -> Result<(String, String)> {
    let ca_key = KeyPair::generate().map_err(pki_error)?;
    let mut ca_params = CertificateParams::new(Vec::<String>::new()).map_err(pki_error)?;
    ca_params
        .distinguished_name
        .push(DnType::CommonName, "Wardrobe Managed CA");
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params.key_usages = vec![
        KeyUsagePurpose::DigitalSignature,
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::CrlSign,
    ];
    let ca = ca_params.self_signed(&ca_key).map_err(pki_error)?;
    Ok((ca.pem(), ca_key.serialize_pem()))
}

fn rebuild_ca_bundle(security_dir: &Path) -> Result<()> {
    let ca_dir = security_dir.join("ca");
    let mut certificates = vec![fs::read_to_string(ca_dir.join("ca.crt"))?];
    let archive_dir = ca_dir.join("archive");
    if archive_dir.is_dir() {
        let mut archived = fs::read_dir(&archive_dir)?
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path().join("ca.crt"))
            .filter(|path| path.is_file())
            .collect::<Vec<_>>();
        archived.sort();
        for path in archived {
            certificates.push(fs::read_to_string(path)?);
        }
    }
    let mut bundle = certificates.join("");
    if !bundle.ends_with('\n') {
        bundle.push('\n');
    }
    fs::write(ca_dir.join("ca-bundle.crt"), bundle)
}

fn refresh_managed_profile_ca_bundles(security_dir: &Path) -> Result<()> {
    let bundle = security_dir.join("ca").join("ca-bundle.crt");
    let registry = read_certificate_registry(security_dir)?;
    for record in registry.certificates {
        if let Some(directory) = record.profile.parent() {
            let profile_ca = directory.join("ca.crt");
            if profile_ca.is_file() {
                fs::copy(&bundle, profile_ca)?;
            }
        }
    }
    Ok(())
}

fn validate_device_name(device: &str) -> Result<&str> {
    let device = device.trim();
    if device.is_empty()
        || !device
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "-_.".contains(character))
    {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "certificate device name must contain only letters, digits, dash, underscore, or dot",
        ));
    }
    Ok(device)
}

fn new_serial() -> SerialNumber {
    let mut bytes = *Uuid::new_v4().as_bytes();
    bytes[0] &= 0x7f;
    SerialNumber::from_slice(&bytes)
}

fn path_component(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || "-_.".contains(character) {
                character
            } else {
                '_'
            }
        })
        .collect()
}

fn normalize_serial(serial: &str) -> String {
    serial
        .chars()
        .filter(|character| character.is_ascii_hexdigit())
        .flat_map(char::to_lowercase)
        .collect()
}

fn read_revoked_serials(security_dir: &Path) -> Result<std::collections::HashSet<String>> {
    let path = security_dir.join("revoked.json");
    if !path.exists() {
        return Ok(std::collections::HashSet::new());
    }
    let serials: Vec<String> = serde_json::from_slice(&fs::read(&path)?).map_err(|error| {
        Error::new(
            ErrorKind::InvalidData,
            format!(
                "Invalid certificate revocation file {}: {error}",
                path.display()
            ),
        )
    })?;
    Ok(serials
        .into_iter()
        .map(|serial| normalize_serial(&serial))
        .collect())
}

fn write_revoked_serials(
    security_dir: &Path,
    revoked: &std::collections::HashSet<String>,
) -> Result<()> {
    let mut serials = revoked.iter().cloned().collect::<Vec<_>>();
    serials.sort();
    fs::write(
        security_dir.join("revoked.json"),
        serde_json::to_vec_pretty(&serials).map_err(json_error)?,
    )
}

fn read_certificate_registry(security_dir: &Path) -> Result<CertificateRegistry> {
    let path = security_dir.join("certificates.json");
    if !path.exists() {
        return Ok(CertificateRegistry::default());
    }
    serde_json::from_slice(&fs::read(&path)?).map_err(|error| {
        Error::new(
            ErrorKind::InvalidData,
            format!("Invalid certificate registry {}: {error}", path.display()),
        )
    })
}

fn write_certificate_registry(security_dir: &Path, registry: &CertificateRegistry) -> Result<()> {
    fs::create_dir_all(security_dir)?;
    fs::write(
        security_dir.join("certificates.json"),
        serde_json::to_vec_pretty(registry).map_err(json_error)?,
    )
}

fn write_private_key(path: &Path, pem: &str) -> Result<()> {
    fs::write(path, pem)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn absolute_path(path: &Path) -> Result<PathBuf> {
    path.canonicalize().or_else(|_| {
        if path.is_absolute() {
            Ok(path.to_path_buf())
        } else {
            Ok(std::env::current_dir()?.join(path))
        }
    })
}

fn require_file(path: &Path, label: &str) -> Result<()> {
    if path.is_file() {
        Ok(())
    } else {
        Err(Error::new(
            ErrorKind::NotFound,
            format!("{label} not found at {}", path.display()),
        ))
    }
}

fn pki_error(error: rcgen::Error) -> Error {
    Error::new(
        ErrorKind::InvalidData,
        format!("PKI operation failed: {error}"),
    )
}

fn json_error(error: serde_json::Error) -> Error {
    Error::new(
        ErrorKind::InvalidData,
        format!("Security JSON serialization failed: {error}"),
    )
}

fn toml_error(error: toml::ser::Error) -> Error {
    Error::new(
        ErrorKind::InvalidData,
        format!("Client profile serialization failed: {error}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wrdb_lib::application_logging::ApplicationLogLevel;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn security_test_directory(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("wardrobe_security_{name}_{nanos}"))
    }

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

    #[test]
    fn security_modes_validate_listener_and_external_pki_requirements() {
        let remote_disabled = WardrobeConfig::from_toml_str(
            r#"
            [network]
            tcp_bind = "0.0.0.0:24842"

            [security]
            mode = "disabled"
            "#,
        )
        .expect("base config should parse");
        assert_eq!(
            remote_disabled.validate_for_server().unwrap_err().kind(),
            ErrorKind::PermissionDenied
        );

        let external = WardrobeConfig::from_toml_str(
            r#"
            [network]
            tcp_bind = "0.0.0.0:24842"

            [security]
            mode = "external"
            server_certificate = "/pki/server.crt"
            server_private_key = "/pki/server.key"
            trusted_client_ca_bundles = ["/pki/a.crt", "/pki/b.crt"]
            "#,
        )
        .expect("external config should parse");
        assert_eq!(external.security.mode, SecurityMode::External);
        assert_eq!(external.security.trusted_client_ca_bundles.len(), 2);
        external
            .validate_for_server()
            .expect("complete external config should validate");
    }

    #[test]
    fn security_config_parsing_validation_and_summary_cover_all_modes() {
        let root = security_test_directory("config");
        fs::create_dir_all(&root).expect("config directory should create");
        let config_path = root.join("wardrobe.toml");
        fs::write(
            &config_path,
            r#"
            [data]
            directory = "./records"

            [network]
            tcp_enabled = true
            tcp_bind = "localhost:24842"
            unix_socket_enabled = false
            unix_socket = "/tmp/wardrobe-security.sock"

            [cache]
            max_cached_drawers = 8

            [wal]
            durability = "strict"
            checkpoint_size_bytes = 4096
            checkpoint_ops = 12

            [transactions]
            enabled = false
            log_directory = "./transactions"
            recovery = "manual"

            [security]
            access_control_file = "./access.json"
            auth_required = true
            mode = "disabled"
            security_dir = "./security"
            server_certificate = "/pki/server.crt"
            server_private_key = "/pki/server.key"
            trusted_client_ca_bundles = ["/pki/clients.crt"]
            server_names = ["localhost", "wardrobe.internal"]
            server_ips = ["127.0.0.1", "::1"]
            unsafe_allow_remote_disabled = true

            [logging]
            level = "warn"
            format = "pretty"
            destination = "stderr"
            "#,
        )
        .expect("config should write");
        let config =
            WardrobeConfig::from_toml_file(&config_path).expect("full config should parse");
        assert_eq!(
            config.transactions.recovery,
            TransactionRecoveryMode::Manual
        );
        assert_eq!(config.security.server_names.len(), 2);
        assert_eq!(config.security.server_ips.len(), 2);
        assert_eq!(
            config.redacted_summary()["security"]["server_private_key"],
            "<redacted>"
        );
        assert_eq!(config.redacted_summary()["wal"]["durability"], "strict");

        let invalid_path = root.join("invalid.toml");
        fs::write(&invalid_path, "[security\nmode = 'managed'")
            .expect("invalid config should write");
        assert!(
            WardrobeConfig::from_toml_file(&invalid_path)
                .expect_err("invalid config should fail")
                .to_string()
                .contains("Failed to load Wardrobe config")
        );

        assert_eq!(
            TransactionRecoveryMode::parse("AUTO").expect("auto should parse"),
            TransactionRecoveryMode::Automatic
        );
        assert_eq!(
            TransactionRecoveryMode::parse("off").expect("off should parse"),
            TransactionRecoveryMode::Disabled
        );
        assert_eq!(TransactionRecoveryMode::Automatic.as_str(), "automatic");
        assert_eq!(TransactionRecoveryMode::Manual.as_str(), "manual");
        assert_eq!(TransactionRecoveryMode::Disabled.as_str(), "disabled");
        assert!(TransactionRecoveryMode::parse("sometimes").is_err());
        assert_eq!(SecurityMode::parse("off").unwrap(), SecurityMode::Disabled);
        assert_eq!(SecurityMode::parse("none").unwrap(), SecurityMode::Disabled);
        assert_eq!(SecurityMode::Managed.as_str(), "managed");
        assert_eq!(SecurityMode::External.as_str(), "external");
        assert!(SecurityMode::parse("optional").is_err());
        assert!(tcp_bind_is_local("localhost:24842"));
        assert!(tcp_bind_is_local("LOCALHOST"));
        assert!(!tcp_bind_is_local("wardrobe.internal:24842"));

        assert!(matches!(
            parse_durability_policy("strict", 0, 0).unwrap(),
            DurabilityPolicy::Strict
        ));
        assert!(parse_durability_policy("grouped", 0, 1).is_err());
        assert!(parse_durability_policy("grouped", 1, 0).is_err());
        assert!(parse_durability_policy("eventual", 1, 1).is_err());

        let mut invalid = WardrobeConfig::default();
        invalid.data.directory = PathBuf::new();
        assert!(invalid.validate().is_err());
        invalid = WardrobeConfig::default();
        invalid.wal.checkpoint_ops = 0;
        assert!(invalid.validate().is_err());
        invalid = WardrobeConfig::default();
        invalid.network.tcp_bind.clear();
        assert!(invalid.validate().is_err());
        invalid = WardrobeConfig::default();
        invalid.network.unix_socket_enabled = true;
        invalid.network.unix_socket = PathBuf::new();
        assert!(invalid.validate().is_err());
        invalid = WardrobeConfig::default();
        invalid.transactions.log_directory = PathBuf::new();
        assert!(invalid.validate().is_err());
        invalid = WardrobeConfig::default();
        invalid.security.access_control_file = PathBuf::new();
        assert!(invalid.validate().is_err());
        invalid = WardrobeConfig::default();
        invalid.security.security_dir = PathBuf::new();
        assert!(invalid.validate().is_err());
        invalid = WardrobeConfig::default();
        invalid.security.mode = SecurityMode::Managed;
        invalid.security.server_names.clear();
        invalid.security.server_ips.clear();
        assert!(invalid.validate().is_err());
        invalid = WardrobeConfig::default();
        invalid.security.mode = SecurityMode::External;
        assert!(invalid.validate().is_err());
        invalid.security.server_certificate = Some(PathBuf::from("server.crt"));
        assert!(invalid.validate().is_err());
        invalid.security.server_private_key = Some(PathBuf::from("server.key"));
        assert!(invalid.validate().is_err());
        invalid.security.trusted_client_ca_bundles = vec![PathBuf::from("ca.crt")];
        invalid.network.unix_socket_enabled = true;
        assert!(invalid.validate_for_server().is_err());

        let absolute = root.join("ca.crt");
        assert_eq!(
            resolve_profile_path(Path::new("/ignored"), absolute.clone()),
            absolute
        );
        let malformed_profile = root.join("profile.toml");
        fs::write(&malformed_profile, "identity = [").expect("profile should write");
        assert!(ClientTlsConfig::from_profile(&malformed_profile).is_err());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn managed_pki_validation_reissue_and_removal_paths_are_covered() {
        let security_dir = security_test_directory("managed_edges");
        assert!(
            list_managed_certificates(&security_dir)
                .expect("missing registry should be empty")
                .is_empty()
        );
        assert!(!certificate_is_revoked(&security_dir, "aa").expect("missing revocations"));
        assert_eq!(
            initialize_managed_pki(&security_dir, &[], &[])
                .expect_err("empty identities should fail")
                .kind(),
            ErrorKind::InvalidInput
        );
        assert_eq!(
            reissue_managed_server_certificate(&security_dir, &["localhost".to_string()], &[])
                .expect_err("missing CA should fail")
                .kind(),
            ErrorKind::NotFound
        );

        initialize_managed_pki(
            &security_dir,
            &["localhost".to_string()],
            &["127.0.0.1".parse().expect("IP should parse")],
        )
        .expect("managed PKI should initialize");
        reissue_managed_server_certificate(
            &security_dir,
            &["localhost".to_string()],
            &["127.0.0.1".parse().expect("IP should parse")],
        )
        .expect("server certificate should reissue");
        assert_eq!(
            reissue_managed_server_certificate(&security_dir, &[], &[])
                .expect_err("empty server identity should fail")
                .kind(),
            ErrorKind::InvalidInput
        );
        assert!(
            issue_managed_client_certificate(&security_dir, "alice", "desktop", None, "localhost")
                .is_err()
        );
        assert!(
            issue_managed_client_certificate(
                &security_dir,
                "wardrobe:user:alice",
                "bad device",
                None,
                "localhost"
            )
            .is_err()
        );
        assert!(
            issue_managed_client_certificate(
                &security_dir,
                "wardrobe:user:alice",
                "desktop",
                None,
                " "
            )
            .is_err()
        );

        let output = security_dir.join("custom-service");
        let service = issue_managed_client_certificate(
            &security_dir,
            "wardrobe:service:sync",
            "worker-1",
            Some(&output),
            "localhost",
        )
        .expect("service certificate should issue");
        assert_eq!(
            managed_identity_certificates(&security_dir, "wardrobe:service:sync")
                .expect("service certificates should list")
                .len(),
            1
        );
        assert!(revoke_managed_certificate(&security_dir, "---").is_err());
        assert!(revoke_managed_certificate(&security_dir, &service.serial).unwrap());
        assert!(!revoke_managed_certificate(&security_dir, &service.serial).unwrap());

        issue_managed_client_certificate(
            &security_dir,
            "wardrobe:user:alice",
            "laptop",
            None,
            "localhost",
        )
        .expect("user certificate should issue");
        assert_eq!(
            remove_managed_identity(&security_dir, "wardrobe:user:alice")
                .expect("identity should remove"),
            1
        );
        assert_eq!(
            remove_managed_identity(&security_dir, "wardrobe:user:alice")
                .expect("identity should already be removed"),
            0
        );
        assert!(remove_managed_identity(&security_dir, "alice").is_err());
        assert!(certificate_identity_from_der(b"not a certificate").is_err());
        let empty_pem = security_dir.join("empty.pem");
        fs::write(&empty_pem, "").expect("empty PEM should write");
        assert!(certificate_identity_from_pem(&empty_pem).is_err());

        fs::write(security_dir.join("revoked.json"), b"{").expect("bad revocations should write");
        assert!(certificate_is_revoked(&security_dir, "aa").is_err());
        fs::write(security_dir.join("revoked.json"), b"[]").expect("revocations should restore");
        fs::write(security_dir.join("certificates.json"), b"{").expect("bad registry should write");
        assert!(list_managed_certificates(&security_dir).is_err());

        let _ = fs::remove_dir_all(security_dir);
    }

    #[test]
    fn managed_pki_persists_identity_profiles_and_revocation() {
        let security_dir = security_test_directory("managed_lifecycle");
        let server_names = vec!["localhost".to_string(), "wardrobe".to_string()];
        let server_ips = vec![
            "127.0.0.1".parse().expect("IPv4 should parse"),
            "::1".parse().expect("IPv6 should parse"),
        ];

        let initialized = initialize_managed_pki(&security_dir, &server_names, &server_ips)
            .expect("managed PKI should initialize");
        assert!(initialized.ca_certificate.is_file());
        assert!(initialized.server_certificate.is_file());
        assert_eq!(
            initialize_managed_pki(&security_dir, &server_names, &server_ips)
                .unwrap_err()
                .kind(),
            ErrorKind::AlreadyExists
        );

        let first = issue_managed_client_certificate(
            &security_dir,
            "wardrobe:user:adminuser",
            "desktop",
            None,
            "localhost",
        )
        .expect("client certificate should issue");
        let identity =
            certificate_identity_from_pem(&first.certificate).expect("certificate should parse");
        assert_eq!(identity.identity, "wardrobe:user:adminuser");
        let tls =
            ClientTlsConfig::from_profile(&first.profile).expect("client profile should parse");
        assert!(tls.ca_cert.is_file());
        assert!(tls.client_cert.is_file());
        assert!(tls.client_key.is_file());
        let original_ca = fs::read_to_string(&initialized.ca_certificate).expect("CA should read");
        rotate_managed_ca(&security_dir, &server_names, &server_ips)
            .expect("managed CA should rotate");
        let current_ca = fs::read_to_string(&initialized.ca_certificate).expect("CA should read");
        assert_ne!(original_ca, current_ca);
        let bundle = fs::read_to_string(security_dir.join("ca").join("ca-bundle.crt"))
            .expect("CA bundle should read");
        assert_eq!(bundle.matches("BEGIN CERTIFICATE").count(), 2);
        assert_eq!(
            fs::read_to_string(
                first
                    .profile
                    .parent()
                    .expect("profile directory")
                    .join("ca.crt")
            )
            .expect("profile CA should read"),
            bundle
        );

        let renewed = renew_managed_client_certificate(
            &security_dir,
            "wardrobe:user:adminuser",
            "desktop",
            first.profile.parent(),
            "localhost",
        )
        .expect("client certificate should renew");
        assert_ne!(first.serial, renewed.serial);
        assert!(certificate_is_revoked(&security_dir, &first.serial).expect("revocation lookup"));
        assert!(!certificate_is_revoked(&security_dir, &renewed.serial).expect("active lookup"));
        assert_eq!(
            managed_identity_certificates(&security_dir, "adminuser")
                .expect("identity listing")
                .len(),
            2
        );

        let _ = fs::remove_dir_all(security_dir);
    }
}
