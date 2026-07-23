use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::WebPkiClientVerifier;
use rustls::{RootCertStore, ServerConnection, StreamOwned};
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use wardrobe_core::{
    ApplicationLogEvent, ApplicationLogLevel, ApplicationLoggingConfig, Command, CreateRequest,
    DurabilityPolicy, ProtocolFrame, ProtocolOpcode, SecurityConfig, SecurityMode, WardrobeConfig,
    WardrobeEngine, certificate_identity_from_der, certificate_identity_from_pem,
    certificate_is_revoked, emit_application_log, init_application_logging, initialize_managed_pki,
    issue_managed_client_certificate, reissue_managed_server_certificate, rotate_managed_ca,
};

const DEFAULT_GROUP_COMMIT_WINDOW_MS: u64 = 5;
const DEFAULT_GROUP_COMMIT_MAX_BATCH: usize = 128;

#[cfg(unix)]
use std::os::unix::net::UnixListener;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerConfig {
    pub data_dir: String,
    pub check_only: bool,
    pub tcp_bind: Option<String>,
    pub unix_socket: Option<PathBuf>,
    pub connection_pool_limit: Option<usize>,
    pub max_cached_drawers: Option<usize>,
    pub wal_checkpoint_size_bytes: u64,
    pub wal_checkpoint_ops: u64,
    pub durability_policy: DurabilityPolicy,
    pub profile_commands: bool,
    pub logging: ApplicationLoggingConfig,
    pub security: SecurityConfig,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ServerRuntimeConfig {
    pub profile_commands: bool,
}

#[derive(Clone)]
struct ServerTlsRuntime {
    config: Arc<rustls::ServerConfig>,
    security_dir: PathBuf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProtocolWritePolicy {
    Flush,
    Unflushed,
}

impl ServerConfig {
    pub fn from_args<I>(args: I) -> io::Result<Self>
    where
        I: IntoIterator<Item = String>,
    {
        let args = args.into_iter().collect::<Vec<_>>();
        let (config_path, args) = extract_config_path(args)?;
        let mut wardrobe_config = match config_path {
            Some(path) => WardrobeConfig::from_toml_file(path)?,
            None => WardrobeConfig::default(),
        };
        let mut check_only = false;
        let mut connection_pool_limit = None;
        let mut profile_commands = false;
        let mut args = args.into_iter();

        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--data-dir" => {
                    wardrobe_config.data.directory =
                        PathBuf::from(args.next().ok_or_else(|| {
                            io::Error::new(
                                io::ErrorKind::InvalidInput,
                                "--data-dir requires a directory path",
                            )
                        })?);
                }
                "--tcp-bind" => {
                    wardrobe_config.network.tcp_enabled = true;
                    wardrobe_config.network.tcp_bind = args.next().ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "--tcp-bind requires an address such as 127.0.0.1:24842",
                        )
                    })?;
                }
                "--no-tcp" => wardrobe_config.network.tcp_enabled = false,
                "--unix-socket" => {
                    wardrobe_config.network.unix_socket_enabled = true;
                    wardrobe_config.network.unix_socket =
                        PathBuf::from(args.next().ok_or_else(|| {
                            io::Error::new(
                                io::ErrorKind::InvalidInput,
                                "--unix-socket requires a socket path",
                            )
                        })?);
                }
                "--connection-pool-limit" => {
                    connection_pool_limit = Some(parse_connection_pool_limit(&mut args, &arg)?);
                }
                "--max-cached-drawers" => {
                    wardrobe_config.cache.max_cached_drawers =
                        Some(parse_positive_usize(&mut args, &arg)?);
                }
                "--wal-checkpoint-size-bytes" => {
                    wardrobe_config.wal.checkpoint_size_bytes =
                        parse_positive_u64(&mut args, &arg)?;
                }
                "--wal-checkpoint-ops" => {
                    wardrobe_config.wal.checkpoint_ops = parse_positive_u64(&mut args, &arg)?;
                }
                "--durability" => {
                    wardrobe_config.wal.durability = parse_durability_policy(&mut args, &arg)?;
                }
                "--group-commit-window-ms" => {
                    let commit_window_ms = parse_positive_u64(&mut args, &arg)?;
                    wardrobe_config.wal.durability = update_group_commit_window(
                        wardrobe_config.wal.durability,
                        commit_window_ms,
                    );
                }
                "--group-commit-max-batch" => {
                    let max_batch_size = parse_positive_usize(&mut args, &arg)?;
                    wardrobe_config.wal.durability = update_group_commit_max_batch(
                        wardrobe_config.wal.durability,
                        max_batch_size,
                    );
                }
                "--profile-commands" => profile_commands = true,
                "--security-mode" => {
                    wardrobe_config.security.mode =
                        SecurityMode::parse(&args.next().ok_or_else(|| {
                            io::Error::new(
                                io::ErrorKind::InvalidInput,
                                "--security-mode requires managed, external, or disabled",
                            )
                        })?)?;
                }
                "--security-dir" => {
                    wardrobe_config.security.security_dir =
                        PathBuf::from(args.next().ok_or_else(|| {
                            io::Error::new(
                                io::ErrorKind::InvalidInput,
                                "--security-dir requires a directory path",
                            )
                        })?);
                }
                "--server-certificate" => {
                    wardrobe_config.security.server_certificate =
                        Some(PathBuf::from(args.next().ok_or_else(|| {
                            io::Error::new(
                                io::ErrorKind::InvalidInput,
                                "--server-certificate requires a PEM file path",
                            )
                        })?));
                }
                "--server-private-key" => {
                    wardrobe_config.security.server_private_key =
                        Some(PathBuf::from(args.next().ok_or_else(|| {
                            io::Error::new(
                                io::ErrorKind::InvalidInput,
                                "--server-private-key requires a PEM file path",
                            )
                        })?));
                }
                "--trusted-client-ca" => {
                    wardrobe_config
                        .security
                        .trusted_client_ca_bundles
                        .push(PathBuf::from(args.next().ok_or_else(|| {
                            io::Error::new(
                                io::ErrorKind::InvalidInput,
                                "--trusted-client-ca requires a PEM bundle path",
                            )
                        })?));
                }
                "--unsafe-disable-auth" => {
                    wardrobe_config.security.unsafe_allow_remote_disabled = true;
                }
                "--log-level" => {
                    let raw = args.next().ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "--log-level requires trace, debug, info, warn, error, or off",
                        )
                    })?;
                    wardrobe_config.logging.level = ApplicationLogLevel::parse(&raw)?;
                }
                "--log-format" => {
                    let raw = args.next().ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "--log-format requires pretty or json",
                        )
                    })?;
                    wardrobe_config.logging.format =
                        wardrobe_core::ApplicationLogFormat::parse(&raw)?;
                }
                "--log-destination" => {
                    let raw = args.next().ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "--log-destination requires stderr, stdout, or file",
                        )
                    })?;
                    wardrobe_config.logging.destination =
                        wardrobe_core::ApplicationLogDestination::parse(&raw)?;
                }
                "--log-file" => {
                    wardrobe_config.logging.file =
                        Some(PathBuf::from(args.next().ok_or_else(|| {
                            io::Error::new(
                                io::ErrorKind::InvalidInput,
                                "--log-file requires a file path",
                            )
                        })?));
                }
                "--check" => check_only = true,
                "--help" | "-h" => {
                    print_help();
                    std::process::exit(0);
                }
                unknown => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("Unknown server argument: {unknown}"),
                    ));
                }
            }
        }

        wardrobe_config.validate_for_server()?;
        let tcp_bind = if wardrobe_config.network.tcp_enabled {
            Some(wardrobe_config.network.tcp_bind.clone())
        } else {
            None
        };
        let unix_socket = if wardrobe_config.network.unix_socket_enabled {
            Some(wardrobe_config.network.unix_socket.clone())
        } else {
            None
        };

        Ok(Self {
            data_dir: wardrobe_config.data.directory.display().to_string(),
            check_only,
            tcp_bind,
            unix_socket,
            connection_pool_limit,
            max_cached_drawers: wardrobe_config.cache.max_cached_drawers,
            wal_checkpoint_size_bytes: wardrobe_config.wal.checkpoint_size_bytes,
            wal_checkpoint_ops: wardrobe_config.wal.checkpoint_ops,
            durability_policy: wardrobe_config.wal.durability,
            profile_commands,
            logging: wardrobe_config.logging,
            security: wardrobe_config.security,
        })
    }

    fn engine_config(&self) -> WardrobeConfig {
        let mut config = WardrobeConfig::default();
        config.data.directory = PathBuf::from(&self.data_dir);
        config.network.tcp_enabled = self.tcp_bind.is_some();
        if let Some(tcp_bind) = &self.tcp_bind {
            config.network.tcp_bind = tcp_bind.clone();
        }
        config.network.unix_socket_enabled = self.unix_socket.is_some();
        if let Some(unix_socket) = &self.unix_socket {
            config.network.unix_socket = unix_socket.clone();
        }
        config.cache.max_cached_drawers = self.max_cached_drawers;
        config.wal.durability = self.durability_policy.clone();
        config.wal.checkpoint_size_bytes = self.wal_checkpoint_size_bytes;
        config.wal.checkpoint_ops = self.wal_checkpoint_ops;
        config.logging = self.logging.clone();
        config.security = self.security.clone();
        config
    }
}

fn extract_config_path(args: Vec<String>) -> io::Result<(Option<PathBuf>, Vec<String>)> {
    let mut config_path = None;
    let mut remaining = Vec::new();
    let mut args = args.into_iter().enumerate();

    while let Some((index, arg)) = args.next() {
        if index == 0 && !arg.starts_with('-') {
            set_config_path(&mut config_path, PathBuf::from(arg))?;
            continue;
        }

        if arg == "--config" {
            let path = args.next().map(|(_, value)| value).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "--config requires a TOML config file path",
                )
            })?;
            set_config_path(&mut config_path, PathBuf::from(path))?;
            continue;
        }

        if let Some(path) = arg.strip_prefix("--config=") {
            if path.is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "--config requires a TOML config file path",
                ));
            }
            set_config_path(&mut config_path, PathBuf::from(path))?;
            continue;
        }

        remaining.push(arg);
    }

    Ok((config_path, remaining))
}

fn set_config_path(slot: &mut Option<PathBuf>, path: PathBuf) -> io::Result<()> {
    if slot.is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Only one Wardrobe server config file can be provided",
        ));
    }
    *slot = Some(path);
    Ok(())
}

pub fn run_from_args<I>(args: I) -> io::Result<()>
where
    I: IntoIterator<Item = String>,
{
    let args = args.into_iter().collect::<Vec<_>>();
    match args.first().map(String::as_str) {
        Some("init") => run_init_command(&args[1..]),
        Some("bootstrap-admin") => run_bootstrap_admin_command(&args[1..]),
        Some("reissue-server-certificate") => run_reissue_server_certificate_command(&args[1..]),
        Some("rotate-ca") => run_rotate_ca_command(&args[1..]),
        _ => run(ServerConfig::from_args(args)?),
    }
}

fn run_init_command(args: &[String]) -> io::Result<()> {
    let mut data_dir = PathBuf::from("./wardrobe");
    let mut security_dir = PathBuf::from("./security");
    let mut server_names = Vec::new();
    let mut server_ips = Vec::new();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--data-dir" => data_dir = PathBuf::from(command_value(args, &mut index)?),
            "--security-dir" => security_dir = PathBuf::from(command_value(args, &mut index)?),
            "--server-name" => server_names.push(command_value(args, &mut index)?.to_string()),
            "--server-ip" => {
                let raw = command_value(args, &mut index)?;
                server_ips.push(raw.parse().map_err(|error| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("Invalid --server-ip '{raw}': {error}"),
                    )
                })?);
            }
            unknown => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("Unknown init argument: {unknown}"),
                ));
            }
        }
        index += 1;
    }
    if server_names.is_empty() && server_ips.is_empty() {
        server_names.push("localhost".to_string());
        server_ips.push(
            "127.0.0.1"
                .parse()
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, format!("{error}")))?,
        );
        server_ips.push(
            "::1"
                .parse()
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, format!("{error}")))?,
        );
    }
    std::fs::create_dir_all(&data_dir)?;
    let initialized = initialize_managed_pki(&security_dir, &server_names, &server_ips)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&initialized).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Failed to render initialization result: {error}"),
            )
        })?
    );
    Ok(())
}

fn run_reissue_server_certificate_command(args: &[String]) -> io::Result<()> {
    let mut security_dir = PathBuf::from("./security");
    let mut server_names = Vec::new();
    let mut server_ips = Vec::new();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--security-dir" => security_dir = PathBuf::from(command_value(args, &mut index)?),
            "--server-name" => server_names.push(command_value(args, &mut index)?.to_string()),
            "--server-ip" => {
                let raw = command_value(args, &mut index)?;
                server_ips.push(raw.parse().map_err(|error| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("Invalid --server-ip '{raw}': {error}"),
                    )
                })?);
            }
            unknown => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("Unknown reissue-server-certificate argument: {unknown}"),
                ));
            }
        }
        index += 1;
    }
    let initialized =
        reissue_managed_server_certificate(&security_dir, &server_names, &server_ips)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&initialized).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Failed to render server certificate result: {error}"),
            )
        })?
    );
    Ok(())
}

fn run_rotate_ca_command(args: &[String]) -> io::Result<()> {
    let mut security_dir = PathBuf::from("./security");
    let mut server_names = Vec::new();
    let mut server_ips = Vec::new();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--security-dir" => security_dir = PathBuf::from(command_value(args, &mut index)?),
            "--server-name" => server_names.push(command_value(args, &mut index)?.to_string()),
            "--server-ip" => {
                let raw = command_value(args, &mut index)?;
                server_ips.push(raw.parse().map_err(|error| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("Invalid --server-ip '{raw}': {error}"),
                    )
                })?);
            }
            unknown => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("Unknown rotate-ca argument: {unknown}"),
                ));
            }
        }
        index += 1;
    }
    let initialized = rotate_managed_ca(&security_dir, &server_names, &server_ips)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&initialized).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Failed to render CA rotation result: {error}"),
            )
        })?
    );
    Ok(())
}

fn run_bootstrap_admin_command(args: &[String]) -> io::Result<()> {
    let mut data_dir = PathBuf::from("./wardrobe");
    let mut security_dir = PathBuf::from("./security");
    let mut username = None;
    let mut output = None;
    let mut certificate = None;
    let mut server_name = "localhost".to_string();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--data-dir" => data_dir = PathBuf::from(command_value(args, &mut index)?),
            "--security-dir" => security_dir = PathBuf::from(command_value(args, &mut index)?),
            "--username" => username = Some(command_value(args, &mut index)?.to_string()),
            "--output" => output = Some(PathBuf::from(command_value(args, &mut index)?)),
            "--certificate" => certificate = Some(PathBuf::from(command_value(args, &mut index)?)),
            "--server-name" => server_name = command_value(args, &mut index)?.to_string(),
            unknown => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("Unknown bootstrap-admin argument: {unknown}"),
                ));
            }
        }
        index += 1;
    }
    let username = username.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "bootstrap-admin requires --username",
        )
    })?;
    validate_bootstrap_username(&username)?;
    let identity = format!("wardrobe:user:{username}");
    let certificate_record = if let Some(certificate) = certificate {
        let certificate_identity = certificate_identity_from_pem(&certificate)?;
        if certificate_identity.identity != identity {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "bootstrap certificate identity '{}' does not match '{}'",
                    certificate_identity.identity, identity
                ),
            ));
        }
        None
    } else {
        let output =
            output.unwrap_or_else(|| security_dir.join("bootstrap").join(username.as_str()));
        Some(issue_managed_client_certificate(
            &security_dir,
            &identity,
            "bootstrap",
            Some(&output),
            &server_name,
        )?)
    };

    let engine = WardrobeEngine::open(data_dir.to_string_lossy().as_ref())?;
    engine.create(CreateRequest::user(serde_json::json!({
        "username": username,
        "role": "administrator",
        "permissions": ["*"],
        "certificate_identities": [identity],
    })))?;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "ok": true,
            "username": username,
            "role": "administrator",
            "certificate": certificate_record,
        }))
        .map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Failed to render bootstrap result: {error}"),
            )
        })?
    );
    Ok(())
}

fn command_value<'a>(args: &'a [String], index: &mut usize) -> io::Result<&'a str> {
    *index += 1;
    args.get(*index).map(String::as_str).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} requires a value", args[*index - 1]),
        )
    })
}

fn validate_bootstrap_username(username: &str) -> io::Result<()> {
    if username.is_empty()
        || !username
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "-_.".contains(character))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "bootstrap username must contain only letters, digits, dash, underscore, or dot",
        ));
    }
    Ok(())
}

pub fn print_help() {
    println!("wardrobe-server");
    println!("  init [options]             Initialize managed CA and persistent server identity");
    println!(
        "  bootstrap-admin [options]  Locally create the first administrator and client profile"
    );
    println!(
        "  reissue-server-certificate [options]  Explicitly replace the managed server certificate"
    );
    println!("  rotate-ca [options]        Rotate the managed CA with overlapping trust");
    println!("  <config.toml>              Optional first positional TOML config file");
    println!("  --config <path>            Load TOML config file");
    println!("  --data-dir <path>          Storage directory for the Wardrobe database");
    println!("  --tcp-bind <addr:port>     Bind TCP listener, default 127.0.0.1:24842");
    println!("  --no-tcp                   Disable TCP listener");
    println!("  --unix-socket <path>       Bind Unix domain socket listener on Unix");
    println!("  --connection-pool-limit <count>  Maximum active worker connections");
    println!("  --max-cached-drawers <count>  Maximum cached drawers in the embedded engine");
    println!("  --wal-checkpoint-size-bytes <bytes>  WAL checkpoint byte threshold");
    println!("  --wal-checkpoint-ops <count>  WAL checkpoint operation threshold");
    println!("  --durability <mode>        WAL durability mode: strict or grouped");
    println!(
        "  --group-commit-window-ms <ms>  Grouped WAL commit window, default {DEFAULT_GROUP_COMMIT_WINDOW_MS}"
    );
    println!(
        "  --group-commit-max-batch <count>  Grouped WAL max batch, default {DEFAULT_GROUP_COMMIT_MAX_BATCH}"
    );
    println!("  --profile-commands         Print per-command protocol and engine timings");
    println!("  --security-mode <mode>     Security mode: managed, external, or disabled");
    println!("  --security-dir <path>      Persistent managed security directory");
    println!("  --server-certificate <path>  External server certificate");
    println!("  --server-private-key <path>  External server private key");
    println!("  --trusted-client-ca <path>   Trusted external client CA bundle; repeatable");
    println!("  --unsafe-disable-auth      Allow disabled authentication on a non-local TCP bind");
    println!(
        "  --log-level <level>        Application log level: trace, debug, info, warn, error, off"
    );
    println!("  --log-format <format>      Application log format: pretty or json");
    println!("  --log-destination <dest>   Application log destination: stderr, stdout, or file");
    println!("  --log-file <path>          File path when --log-destination file is used");
    println!("  --check                    Initialize the daemon and exit without blocking");
}

fn server_log(
    level: ApplicationLogLevel,
    message: &'static str,
    fields: Vec<(&'static str, String)>,
) {
    emit_application_log(ApplicationLogEvent::new(
        level,
        "wardrobe_server",
        message,
        fields,
    ));
}

fn server_error_fields(error: &io::Error) -> Vec<(&'static str, String)> {
    vec![
        ("error_kind", format!("{:?}", error.kind())),
        ("error", error.to_string()),
    ]
}

fn build_server_tls_runtime(security: &SecurityConfig) -> io::Result<Option<ServerTlsRuntime>> {
    if security.mode == SecurityMode::Disabled {
        return Ok(None);
    }
    let (server_certificate, server_private_key, trusted_client_cas) = match security.mode {
        SecurityMode::Managed => (
            security.security_dir.join("server").join("server.crt"),
            security.security_dir.join("server").join("server.key"),
            vec![{
                let bundle = security.security_dir.join("ca").join("ca-bundle.crt");
                if bundle.is_file() {
                    bundle
                } else {
                    security.security_dir.join("ca").join("ca.crt")
                }
            }],
        ),
        SecurityMode::External => (
            security.server_certificate.clone().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "external security requires a server certificate",
                )
            })?,
            security.server_private_key.clone().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "external security requires a server private key",
                )
            })?,
            security.trusted_client_ca_bundles.clone(),
        ),
        SecurityMode::Disabled => return Ok(None),
    };
    let mut roots = RootCertStore::empty();
    for path in &trusted_client_cas {
        for certificate in load_certificates(path)? {
            roots.add(certificate).map_err(|error| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("Invalid trusted client CA {}: {error}", path.display()),
                )
            })?;
        }
    }
    if roots.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "certificate authentication requires at least one trusted client CA",
        ));
    }
    let verifier = WebPkiClientVerifier::builder(Arc::new(roots))
        .build()
        .map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Failed to build client certificate verifier: {error}"),
            )
        })?;
    let config = rustls::ServerConfig::builder()
        .with_client_cert_verifier(verifier)
        .with_single_cert(
            load_certificates(&server_certificate)?,
            load_private_key(&server_private_key)?,
        )
        .map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Invalid server TLS identity: {error}"),
            )
        })?;
    Ok(Some(ServerTlsRuntime {
        config: Arc::new(config),
        security_dir: security.security_dir.clone(),
    }))
}

fn load_certificates(path: &Path) -> io::Result<Vec<CertificateDer<'static>>> {
    let bytes = std::fs::read(path)?;
    let mut reader = io::BufReader::new(bytes.as_slice());
    let certificates = rustls_pemfile::certs(&mut reader).collect::<io::Result<Vec<_>>>()?;
    if certificates.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("No certificates found in {}", path.display()),
        ));
    }
    Ok(certificates)
}

fn load_private_key(path: &Path) -> io::Result<PrivateKeyDer<'static>> {
    let bytes = std::fs::read(path)?;
    rustls_pemfile::private_key(&mut io::BufReader::new(bytes.as_slice()))?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("No private key found in {}", path.display()),
        )
    })
}

pub fn run(config: ServerConfig) -> io::Result<()> {
    let engine_config = config.engine_config();
    if config.check_only {
        engine_config.validate()?;
    } else {
        engine_config.validate_for_server()?;
    }
    init_application_logging(config.logging.clone())?;
    let tls_security = build_server_tls_runtime(&config.security)?;
    server_log(
        ApplicationLogLevel::Info,
        "config_loaded",
        vec![
            ("operation", "config_loading".to_string()),
            ("storage_root", config.data_dir.clone()),
            (
                "tcp_bind",
                config
                    .tcp_bind
                    .clone()
                    .unwrap_or_else(|| "disabled".to_string()),
            ),
            (
                "unix_socket",
                config
                    .unix_socket
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "disabled".to_string()),
            ),
            ("log_level", config.logging.level.as_str().to_string()),
            ("log_format", config.logging.format.as_str().to_string()),
            (
                "log_destination",
                config.logging.destination.as_str().to_string(),
            ),
            ("security_mode", config.security.mode.as_str().to_string()),
        ],
    );
    server_log(
        ApplicationLogLevel::Info,
        "startup",
        vec![
            ("operation", "startup".to_string()),
            ("storage_root", config.data_dir.clone()),
        ],
    );
    let engine = match WardrobeEngine::open_for_server_with_config(engine_config) {
        Ok(engine) => Arc::new(engine),
        Err(error) => {
            let mut fields = vec![
                ("operation", "startup".to_string()),
                ("storage_root", config.data_dir.clone()),
                ("success", "false".to_string()),
            ];
            fields.extend(server_error_fields(&error));
            server_log(ApplicationLogLevel::Error, "startup_failure", fields);
            return Err(error);
        }
    };
    server_log(
        ApplicationLogLevel::Info,
        "startup_complete",
        vec![
            ("operation", "startup".to_string()),
            ("storage_root", config.data_dir.clone()),
            ("success", "true".to_string()),
        ],
    );

    println!(
        "Wardrobe daemon initialized with storage directory: {}",
        config.data_dir
    );

    if config.check_only {
        println!("Wardrobe daemon check completed.");
        server_log(
            ApplicationLogLevel::Info,
            "shutdown",
            vec![
                ("operation", "shutdown".to_string()),
                ("reason", "check_only".to_string()),
            ],
        );
        return Ok(());
    }

    if config.tcp_bind.is_none() && config.unix_socket.is_none() {
        let error = io::Error::new(
            io::ErrorKind::InvalidInput,
            "At least one Wardrobe server listener must be enabled",
        );
        let mut fields = vec![
            ("operation", "listener_resolution".to_string()),
            ("success", "false".to_string()),
        ];
        fields.extend(server_error_fields(&error));
        server_log(
            ApplicationLogLevel::Error,
            "listener_resolution_failure",
            fields,
        );
        return Err(error);
    }

    let mut listener_threads = Vec::new();

    if let Some(tcp_bind) = config.tcp_bind {
        let listener = TcpListener::bind(&tcp_bind)?;
        let local_addr = listener.local_addr()?;
        println!("Wardrobe daemon listening on TCP: {local_addr}");
        server_log(
            ApplicationLogLevel::Info,
            "listener_bound",
            vec![
                ("operation", "listener_bind".to_string()),
                ("listener", "tcp".to_string()),
                ("bind", local_addr.to_string()),
            ],
        );
        let engine = Arc::clone(&engine);
        let connection_pool_limit = config.connection_pool_limit;
        let runtime = ServerRuntimeConfig {
            profile_commands: config.profile_commands,
        };
        let tls_security = tls_security.clone();
        listener_threads.push(thread::spawn(move || {
            serve_tcp_listener_with_security(
                listener,
                engine,
                connection_pool_limit,
                runtime,
                tls_security,
            )
        }));
    }

    if let Some(socket_path) = config.unix_socket {
        #[cfg(unix)]
        {
            if socket_path.exists() {
                std::fs::remove_file(&socket_path)?;
            }
            let listener = UnixListener::bind(&socket_path)?;
            println!(
                "Wardrobe daemon listening on Unix socket: {}",
                socket_path.display()
            );
            server_log(
                ApplicationLogLevel::Info,
                "listener_bound",
                vec![
                    ("operation", "listener_bind".to_string()),
                    ("listener", "unix".to_string()),
                    ("path", socket_path.display().to_string()),
                ],
            );
            let engine = Arc::clone(&engine);
            let connection_pool_limit = config.connection_pool_limit;
            let runtime = ServerRuntimeConfig {
                profile_commands: config.profile_commands,
            };
            listener_threads.push(thread::spawn(move || {
                serve_unix_listener_with_config(listener, engine, connection_pool_limit, runtime)
            }));
        }

        #[cfg(not(unix))]
        {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!(
                    "Unix socket listener requested at {}, but Unix sockets are not available on this platform",
                    socket_path.display()
                ),
            ));
        }
    }

    for handle in listener_threads {
        join_listener(handle)?;
    }

    server_log(
        ApplicationLogLevel::Info,
        "shutdown",
        vec![("operation", "shutdown".to_string())],
    );
    Ok(())
}

pub fn serve_tcp_listener(
    listener: TcpListener,
    engine: Arc<WardrobeEngine>,
    connection_pool_limit: Option<usize>,
) -> io::Result<()> {
    serve_tcp_listener_with_config(
        listener,
        engine,
        connection_pool_limit,
        ServerRuntimeConfig::default(),
    )
}

pub fn serve_tcp_listener_with_config(
    listener: TcpListener,
    engine: Arc<WardrobeEngine>,
    connection_pool_limit: Option<usize>,
    runtime: ServerRuntimeConfig,
) -> io::Result<()> {
    serve_tcp_listener_with_security(listener, engine, connection_pool_limit, runtime, None)
}

pub fn serve_tls_tcp_listener(
    listener: TcpListener,
    engine: Arc<WardrobeEngine>,
    connection_pool_limit: Option<usize>,
    security: SecurityConfig,
) -> io::Result<()> {
    if security.mode == SecurityMode::Disabled {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "TLS listener requires managed or external security",
        ));
    }
    let tls_security = build_server_tls_runtime(&security)?;
    serve_tcp_listener_with_security(
        listener,
        engine,
        connection_pool_limit,
        ServerRuntimeConfig::default(),
        tls_security,
    )
}

fn serve_tcp_listener_with_security(
    listener: TcpListener,
    engine: Arc<WardrobeEngine>,
    connection_pool_limit: Option<usize>,
    runtime: ServerRuntimeConfig,
    tls_security: Option<ServerTlsRuntime>,
) -> io::Result<()> {
    let connection_pool = connection_pool_limit.map(ConnectionPool::new);
    let mut idle_polls = 0usize;

    loop {
        let permit = connection_pool.as_ref().map(ConnectionPool::acquire);
        match listener.accept() {
            Ok((stream, peer_addr)) => {
                configure_tcp_stream(&stream)?;
                idle_polls = 0;
                server_log(
                    ApplicationLogLevel::Info,
                    "connection_accepted",
                    vec![
                        ("operation", "connection_accept".to_string()),
                        ("listener", "tcp".to_string()),
                        ("peer_addr", peer_addr.to_string()),
                    ],
                );
                if let Some(tls_security) = tls_security.clone() {
                    spawn_tls_connection_handler(
                        Arc::clone(&engine),
                        stream,
                        permit,
                        runtime,
                        tls_security,
                    );
                } else {
                    spawn_connection_handler(
                        Arc::clone(&engine),
                        stream,
                        permit,
                        runtime,
                        ProtocolWritePolicy::Unflushed,
                    );
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                drop(permit);
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                drop(permit);
                if listener_is_idle(connection_pool.as_ref(), &mut idle_polls) {
                    return Ok(());
                }
                thread::sleep(nonblocking_idle_sleep());
            }
            Err(error) => {
                drop(permit);
                return Err(error);
            }
        }
    }
}

fn configure_tcp_stream(stream: &TcpStream) -> io::Result<()> {
    stream.set_nodelay(true)?;
    stream.set_nonblocking(false)
}

#[cfg(unix)]
pub fn serve_unix_listener(
    listener: UnixListener,
    engine: Arc<WardrobeEngine>,
    connection_pool_limit: Option<usize>,
) -> io::Result<()> {
    serve_unix_listener_with_config(
        listener,
        engine,
        connection_pool_limit,
        ServerRuntimeConfig::default(),
    )
}

#[cfg(unix)]
pub fn serve_unix_listener_with_config(
    listener: UnixListener,
    engine: Arc<WardrobeEngine>,
    connection_pool_limit: Option<usize>,
    runtime: ServerRuntimeConfig,
) -> io::Result<()> {
    let connection_pool = connection_pool_limit.map(ConnectionPool::new);
    let mut idle_polls = 0usize;

    loop {
        let permit = connection_pool.as_ref().map(ConnectionPool::acquire);
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_nonblocking(false)?;
                idle_polls = 0;
                server_log(
                    ApplicationLogLevel::Info,
                    "connection_accepted",
                    vec![
                        ("operation", "connection_accept".to_string()),
                        ("listener", "unix".to_string()),
                    ],
                );
                spawn_connection_handler(
                    Arc::clone(&engine),
                    stream,
                    permit,
                    runtime,
                    ProtocolWritePolicy::Unflushed,
                );
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                drop(permit);
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                drop(permit);
                if listener_is_idle(connection_pool.as_ref(), &mut idle_polls) {
                    return Ok(());
                }
                thread::sleep(nonblocking_idle_sleep());
            }
            Err(error) => {
                drop(permit);
                return Err(error);
            }
        }
    }
}

pub fn handle_protocol_stream<S>(engine: Arc<WardrobeEngine>, mut stream: S) -> io::Result<()>
where
    S: Read + Write,
{
    handle_protocol_stream_with_policy(
        engine,
        &mut stream,
        ServerRuntimeConfig::default(),
        ProtocolWritePolicy::Flush,
    )
}

pub fn handle_protocol_stream_with_config<S>(
    engine: Arc<WardrobeEngine>,
    stream: &mut S,
    runtime: ServerRuntimeConfig,
) -> io::Result<()>
where
    S: Read + Write,
{
    handle_protocol_stream_with_policy(engine, stream, runtime, ProtocolWritePolicy::Flush)
}

fn handle_protocol_stream_with_policy<S>(
    engine: Arc<WardrobeEngine>,
    stream: &mut S,
    runtime: ServerRuntimeConfig,
    write_policy: ProtocolWritePolicy,
) -> io::Result<()>
where
    S: Read + Write,
{
    loop {
        let receive_started = Instant::now();
        let frame = match ProtocolFrame::read_from_stream(&mut *stream) {
            Ok(frame) => frame,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::UnexpectedEof
                        | io::ErrorKind::InvalidData
                        | io::ErrorKind::ConnectionReset
                        | io::ErrorKind::ConnectionAborted
                ) =>
            {
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        let protocol_receive = receive_started.elapsed();

        if frame.opcode != ProtocolOpcode::Command {
            server_log(
                ApplicationLogLevel::Warn,
                "command_failure",
                vec![
                    ("operation", "command_decode".to_string()),
                    ("error_kind", "InvalidData".to_string()),
                    (
                        "error",
                        "Wardrobe server expected a command frame from the client".to_string(),
                    ),
                    ("mutation_phase", "before_mutation".to_string()),
                    ("success", "false".to_string()),
                ],
            );
            write_error_frame(
                &mut *stream,
                "Wardrobe server expected a command frame from the client",
                write_policy,
            )?;
            continue;
        }

        let deserialization_started = Instant::now();
        let command = match serde_json::from_slice::<Command>(&frame.payload) {
            Ok(command) => command,
            Err(error) => {
                server_log(
                    ApplicationLogLevel::Warn,
                    "command_failure",
                    vec![
                        ("operation", "command_deserialize".to_string()),
                        ("error_kind", "InvalidData".to_string()),
                        ("error", error.to_string()),
                        ("mutation_phase", "before_mutation".to_string()),
                        ("success", "false".to_string()),
                    ],
                );
                write_error_frame(
                    &mut *stream,
                    &format!("Failed to deserialize Wardrobe command: {error}"),
                    write_policy,
                )?;
                continue;
            }
        };
        let command_deserialization = deserialization_started.elapsed();
        let command_name = command_label(&command);
        let request_bytes = frame.payload.len();
        server_log(
            ApplicationLogLevel::Info,
            "command_start",
            vec![
                ("operation", "command_execute".to_string()),
                ("command", command_name.to_string()),
                ("request_bytes", request_bytes.to_string()),
            ],
        );

        let execution_started = Instant::now();
        match engine.execute_command(command) {
            Ok(result) => {
                let engine_execution = execution_started.elapsed();
                let serialization_started = Instant::now();
                let payload = serde_json::to_vec(&result).map_err(|error| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("Failed to serialize Wardrobe command result: {error}"),
                    )
                })?;
                let response_serialization = serialization_started.elapsed();
                let response_bytes = payload.len();
                let transmission_started = Instant::now();
                write_protocol_payload(
                    &mut *stream,
                    ProtocolOpcode::Result,
                    &payload,
                    write_policy,
                )?;
                let protocol_transmission = transmission_started.elapsed();
                server_log(
                    ApplicationLogLevel::Info,
                    "command_finish",
                    vec![
                        ("operation", "command_execute".to_string()),
                        ("command", command_name.to_string()),
                        ("duration_us", engine_execution.as_micros().to_string()),
                        ("response_bytes", response_bytes.to_string()),
                        ("success", "true".to_string()),
                    ],
                );
                if runtime.profile_commands {
                    emit_command_profile(ServerCommandProfile {
                        command_name,
                        request_bytes,
                        response_bytes,
                        protocol_receive,
                        command_deserialization,
                        engine_execution,
                        response_serialization,
                        protocol_transmission,
                        status: "ok",
                    });
                }
            }
            Err(error) => {
                let engine_execution = execution_started.elapsed();
                let response = error.to_string();
                let serialization_started = Instant::now();
                let response_bytes = response.len();
                let response_serialization = serialization_started.elapsed();
                let transmission_started = Instant::now();
                write_error_frame(&mut *stream, &response, write_policy)?;
                let protocol_transmission = transmission_started.elapsed();
                let mut fields = vec![
                    ("operation", "command_execute".to_string()),
                    ("command", command_name.to_string()),
                    ("duration_us", engine_execution.as_micros().to_string()),
                    ("response_bytes", response_bytes.to_string()),
                    ("mutation_phase", "unknown".to_string()),
                    ("success", "false".to_string()),
                ];
                fields.extend(server_error_fields(&error));
                server_log(ApplicationLogLevel::Error, "command_failure", fields);
                if runtime.profile_commands {
                    emit_command_profile(ServerCommandProfile {
                        command_name,
                        request_bytes,
                        response_bytes,
                        protocol_receive,
                        command_deserialization,
                        engine_execution,
                        response_serialization,
                        protocol_transmission,
                        status: "error",
                    });
                }
            }
        }
    }
}

struct ServerCommandProfile {
    command_name: &'static str,
    request_bytes: usize,
    response_bytes: usize,
    protocol_receive: Duration,
    command_deserialization: Duration,
    engine_execution: Duration,
    response_serialization: Duration,
    protocol_transmission: Duration,
    status: &'static str,
}

fn emit_command_profile(profile: ServerCommandProfile) {
    eprintln!(
        "[wardrobe-server profile] command={} status={} request_bytes={} response_bytes={} protocol_receive_us={} command_deserialize_us={} engine_execute_us={} response_serialize_us={} protocol_transmit_us={}",
        profile.command_name,
        profile.status,
        profile.request_bytes,
        profile.response_bytes,
        profile.protocol_receive.as_micros(),
        profile.command_deserialization.as_micros(),
        profile.engine_execution.as_micros(),
        profile.response_serialization.as_micros(),
        profile.protocol_transmission.as_micros()
    );
}

fn command_label(command: &Command) -> &'static str {
    match command {
        Command::Upsert { .. } => "upsert",
        Command::Read { .. } => "read",
        Command::Delete { .. } => "delete",
        Command::Inspect { .. } => "inspect",
        Command::Count { .. } => "count",
        Command::Compact(_) => "compact",
        Command::Create(_) => "create",
        Command::Alter(_) => "alter",
        Command::Drop(_) => "drop",
        Command::Backup { .. } => "backup",
        Command::Restore { .. } => "restore",
        Command::Grant(_) => "grant",
        Command::Revoke(_) => "revoke",
        Command::Status(_) => "status",
        Command::ExecuteForTenant { .. } => "execute_for_tenant",
        Command::Execute { .. } => "execute",
        Command::ExecuteInScope { .. } => "execute_in_scope",
    }
}

fn write_error_frame<S>(
    stream: &mut S,
    message: &str,
    write_policy: ProtocolWritePolicy,
) -> io::Result<()>
where
    S: Write,
{
    write_protocol_payload(
        stream,
        ProtocolOpcode::Error,
        message.as_bytes(),
        write_policy,
    )
}

fn write_protocol_payload<S>(
    stream: &mut S,
    opcode: ProtocolOpcode,
    payload: &[u8],
    write_policy: ProtocolWritePolicy,
) -> io::Result<()>
where
    S: Write,
{
    match write_policy {
        ProtocolWritePolicy::Flush => {
            ProtocolFrame::write_payload_to_stream(opcode, payload, stream)
        }
        ProtocolWritePolicy::Unflushed => {
            ProtocolFrame::write_payload_to_stream_unflushed(opcode, payload, stream)
        }
    }
}

fn join_listener(handle: JoinHandle<io::Result<()>>) -> io::Result<()> {
    handle
        .join()
        .map_err(|_| io::Error::other("Wardrobe listener thread panicked"))?
}

fn parse_connection_pool_limit(
    args: &mut impl Iterator<Item = String>,
    flag: &str,
) -> io::Result<usize> {
    parse_positive_usize(args, flag)
}

fn parse_positive_usize(args: &mut impl Iterator<Item = String>, flag: &str) -> io::Result<usize> {
    let raw = args.next().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{flag} requires a positive integer"),
        )
    })?;
    let parsed = raw.parse::<usize>().map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("Invalid {flag} value '{raw}': {error}"),
        )
    })?;
    if parsed == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{flag} must be greater than zero"),
        ));
    }
    Ok(parsed)
}

fn parse_positive_u64(args: &mut impl Iterator<Item = String>, flag: &str) -> io::Result<u64> {
    let raw = args.next().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{flag} requires a positive integer"),
        )
    })?;
    let parsed = raw.parse::<u64>().map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("Invalid {flag} value '{raw}': {error}"),
        )
    })?;
    if parsed == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{flag} must be greater than zero"),
        ));
    }
    Ok(parsed)
}

fn parse_durability_policy(
    args: &mut impl Iterator<Item = String>,
    flag: &str,
) -> io::Result<DurabilityPolicy> {
    let raw = args.next().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{flag} requires strict or grouped"),
        )
    })?;
    match raw.trim().to_ascii_lowercase().as_str() {
        "strict" => Ok(DurabilityPolicy::Strict),
        "grouped" | "group" | "group-commit" => Ok(default_grouped_durability_policy()),
        other => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("Unsupported {flag} value: {other}"),
        )),
    }
}

fn default_grouped_durability_policy() -> DurabilityPolicy {
    DurabilityPolicy::Grouped {
        commit_window_ms: DEFAULT_GROUP_COMMIT_WINDOW_MS,
        max_batch_size: DEFAULT_GROUP_COMMIT_MAX_BATCH,
    }
}

fn update_group_commit_window(policy: DurabilityPolicy, commit_window_ms: u64) -> DurabilityPolicy {
    match policy {
        DurabilityPolicy::Grouped { max_batch_size, .. } => DurabilityPolicy::Grouped {
            commit_window_ms,
            max_batch_size,
        },
        DurabilityPolicy::Strict => DurabilityPolicy::Grouped {
            commit_window_ms,
            max_batch_size: DEFAULT_GROUP_COMMIT_MAX_BATCH,
        },
    }
}

fn update_group_commit_max_batch(
    policy: DurabilityPolicy,
    max_batch_size: usize,
) -> DurabilityPolicy {
    match policy {
        DurabilityPolicy::Grouped {
            commit_window_ms, ..
        } => DurabilityPolicy::Grouped {
            commit_window_ms,
            max_batch_size,
        },
        DurabilityPolicy::Strict => DurabilityPolicy::Grouped {
            commit_window_ms: DEFAULT_GROUP_COMMIT_WINDOW_MS,
            max_batch_size,
        },
    }
}

fn spawn_connection_handler<S>(
    engine: Arc<WardrobeEngine>,
    stream: S,
    permit: Option<ConnectionPoolPermit>,
    runtime: ServerRuntimeConfig,
    write_policy: ProtocolWritePolicy,
) where
    S: Read + Write + Send + 'static,
{
    thread::spawn(move || {
        let _permit = permit;
        let mut stream = stream;
        if let Err(error) =
            handle_protocol_stream_with_policy(engine, &mut stream, runtime, write_policy)
        {
            eprintln!("Wardrobe connection handler failed: {error}");
        }
    });
}

fn spawn_tls_connection_handler(
    engine: Arc<WardrobeEngine>,
    stream: TcpStream,
    permit: Option<ConnectionPoolPermit>,
    runtime: ServerRuntimeConfig,
    tls: ServerTlsRuntime,
) {
    thread::spawn(move || {
        let _permit = permit;
        if let Err(error) = handle_authenticated_tls_connection(engine, stream, runtime, tls) {
            server_log(
                ApplicationLogLevel::Warn,
                "authentication_failure",
                vec![
                    ("operation", "client_authentication".to_string()),
                    ("error_kind", format!("{:?}", error.kind())),
                    ("error", error.to_string()),
                    ("success", "false".to_string()),
                ],
            );
        }
    });
}

fn handle_authenticated_tls_connection(
    engine: Arc<WardrobeEngine>,
    mut stream: TcpStream,
    runtime: ServerRuntimeConfig,
    tls: ServerTlsRuntime,
) -> io::Result<()> {
    let mut connection = ServerConnection::new(tls.config).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Failed to initialize server TLS connection: {error}"),
        )
    })?;
    while connection.is_handshaking() {
        connection.complete_io(&mut stream).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("TLS client authentication failed: {error}"),
            )
        })?;
    }
    let certificate = connection
        .peer_certificates()
        .and_then(|certificates| certificates.first())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "TLS client did not provide a certificate",
            )
        })?;
    let identity = certificate_identity_from_der(certificate.as_ref())?;
    if certificate_is_revoked(&tls.security_dir, &identity.serial)? {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("Client certificate {} has been revoked", identity.serial),
        ));
    }
    let username = engine
        .resolve_certificate_identity(&identity.identity)?
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "Certificate identity '{}' is not registered to a Wardrobe user",
                    identity.identity
                ),
            )
        })?;
    server_log(
        ApplicationLogLevel::Info,
        "authentication_success",
        vec![
            ("operation", "client_authentication".to_string()),
            ("username", username),
            ("certificate_identity", identity.identity),
            ("certificate_serial", identity.serial),
            ("success", "true".to_string()),
        ],
    );
    let mut tls_stream = StreamOwned::new(connection, stream);
    handle_protocol_stream_with_policy(
        engine,
        &mut tls_stream,
        runtime,
        ProtocolWritePolicy::Unflushed,
    )
}

fn listener_is_idle(connection_pool: Option<&ConnectionPool>, idle_polls: &mut usize) -> bool {
    if connection_pool.is_none_or(ConnectionPool::is_idle) {
        *idle_polls += 1;
    } else {
        *idle_polls = 0;
    }

    *idle_polls >= nonblocking_idle_poll_limit()
}

fn nonblocking_idle_sleep() -> Duration {
    Duration::from_millis(10)
}

fn nonblocking_idle_poll_limit() -> usize {
    100
}

#[derive(Clone)]
struct ConnectionPool {
    inner: Arc<ConnectionPoolState>,
    limit: usize,
}

struct ConnectionPoolState {
    active_connections: Mutex<usize>,
    slot_available: Condvar,
}

struct ConnectionPoolPermit {
    pool: ConnectionPool,
}

impl ConnectionPool {
    fn new(limit: usize) -> Self {
        Self {
            inner: Arc::new(ConnectionPoolState {
                active_connections: Mutex::new(0),
                slot_available: Condvar::new(),
            }),
            limit,
        }
    }

    fn acquire(&self) -> ConnectionPoolPermit {
        let mut active_connections = self
            .inner
            .active_connections
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        while *active_connections >= self.limit {
            active_connections = self
                .inner
                .slot_available
                .wait(active_connections)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }

        *active_connections += 1;
        ConnectionPoolPermit { pool: self.clone() }
    }

    fn is_idle(&self) -> bool {
        *self
            .inner
            .active_connections
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            == 0
    }
}

impl Drop for ConnectionPoolPermit {
    fn drop(&mut self) {
        let mut active_connections = self
            .pool
            .inner
            .active_connections
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *active_connections = active_connections.saturating_sub(1);
        self.pool.inner.slot_available.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Cursor;
    use std::thread;
    use std::time::{SystemTime, UNIX_EPOCH};
    use wardrobe_core::{
        AlterRequest, ApplicationLogDestination, ApplicationLogFormat, ApplicationLogLevel,
        BackupArchive, BackupArchiveFile, CompactRequest, CreateRequest, DropRequest,
        OperationFilter, OperationOptions, PermissionRequest, StatusRequest, StorageCoordinate,
        StorageScope, WardrobeClient,
    };

    fn security_test_directory(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("wardrobe_server_security_{name}_{nanos}"))
    }

    #[test]
    fn server_config_from_args_defaults() {
        let cfg = ServerConfig::from_args(Vec::<String>::new()).expect("should parse defaults");
        assert_eq!(cfg.data_dir, "./wardrobe");
        assert_eq!(cfg.tcp_bind.unwrap().starts_with("127.0.0.1"), true);
        assert!(!cfg.check_only);
        assert!(!cfg.profile_commands);
        assert_eq!(cfg.durability_policy, DurabilityPolicy::Strict);
        assert_eq!(cfg.logging.level, ApplicationLogLevel::Off);
        assert_eq!(cfg.logging.destination, ApplicationLogDestination::Stderr);
    }

    #[test]
    fn server_config_parses_command_profiling_flag() {
        let cfg = ServerConfig::from_args(vec!["--profile-commands".to_string()])
            .expect("profile flag should parse");
        assert!(cfg.profile_commands);
    }

    #[test]
    fn server_config_parses_security_modes_and_enforces_disabled_bind_safety() {
        let managed = ServerConfig::from_args(vec![
            "--security-mode".to_string(),
            "managed".to_string(),
            "--security-dir".to_string(),
            "/security".to_string(),
        ])
        .expect("managed security flags should parse");
        assert_eq!(managed.security.mode, SecurityMode::Managed);
        assert_eq!(managed.security.security_dir, PathBuf::from("/security"));

        let external = ServerConfig::from_args(vec![
            "--tcp-bind".to_string(),
            "0.0.0.0:24842".to_string(),
            "--security-mode".to_string(),
            "external".to_string(),
            "--server-certificate".to_string(),
            "/pki/server.crt".to_string(),
            "--server-private-key".to_string(),
            "/pki/server.key".to_string(),
            "--trusted-client-ca".to_string(),
            "/pki/clients-a.crt".to_string(),
            "--trusted-client-ca".to_string(),
            "/pki/clients-b.crt".to_string(),
        ])
        .expect("external security flags should parse");
        assert_eq!(external.security.mode, SecurityMode::External);
        assert_eq!(external.security.trusted_client_ca_bundles.len(), 2);

        assert!(
            ServerConfig::from_args(vec![
                "--tcp-bind".to_string(),
                "0.0.0.0:24842".to_string(),
                "--security-mode".to_string(),
                "disabled".to_string(),
            ])
            .is_err()
        );
        assert!(
            ServerConfig::from_args(vec![
                "--tcp-bind".to_string(),
                "0.0.0.0:24842".to_string(),
                "--security-mode".to_string(),
                "disabled".to_string(),
                "--unsafe-disable-auth".to_string(),
            ])
            .is_ok()
        );
    }

    #[test]
    fn managed_init_and_bootstrap_commands_persist_admin_profile() {
        let root = security_test_directory("bootstrap");
        let data_dir = root.join("data");
        let security_dir = root.join("security");
        let output = root.join("admin-profile");

        run_from_args(vec![
            "init".to_string(),
            "--data-dir".to_string(),
            data_dir.display().to_string(),
            "--security-dir".to_string(),
            security_dir.display().to_string(),
            "--server-name".to_string(),
            "localhost".to_string(),
            "--server-ip".to_string(),
            "127.0.0.1".to_string(),
        ])
        .expect("init command should succeed");
        run_from_args(vec![
            "bootstrap-admin".to_string(),
            "--data-dir".to_string(),
            data_dir.display().to_string(),
            "--security-dir".to_string(),
            security_dir.display().to_string(),
            "--username".to_string(),
            "adminuser".to_string(),
            "--output".to_string(),
            output.display().to_string(),
        ])
        .expect("bootstrap command should succeed");

        assert!(security_dir.join("ca").join("ca.crt").is_file());
        assert!(security_dir.join("server").join("server.crt").is_file());
        assert!(output.join("profile.toml").is_file());
        let registry = std::fs::read_to_string(data_dir.join("_wardrobe_access_control.json"))
            .expect("access-control registry should exist");
        assert!(registry.contains("\"adminuser\""));
        assert!(registry.contains("\"administrator\""));
        assert!(
            run_from_args(vec![
                "init".to_string(),
                "--security-dir".to_string(),
                security_dir.display().to_string(),
            ])
            .is_err()
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn managed_tls_authenticates_registered_identity_and_rejects_revocation() {
        let root = security_test_directory("managed_tls");
        let data_dir = root.join("data");
        let security_dir = root.join("security");
        initialize_managed_pki(
            &security_dir,
            &["localhost".to_string()],
            &["127.0.0.1".parse().expect("IP should parse")],
        )
        .expect("managed PKI should initialize");
        let certificate = issue_managed_client_certificate(
            &security_dir,
            "wardrobe:user:adminuser",
            "desktop",
            None,
            "localhost",
        )
        .expect("client certificate should issue");
        rotate_managed_ca(
            &security_dir,
            &["localhost".to_string()],
            &["127.0.0.1".parse().expect("IP should parse")],
        )
        .expect("managed CA should rotate with overlapping trust");
        let engine =
            Arc::new(WardrobeEngine::open(data_dir.to_string_lossy().as_ref()).expect("engine"));
        engine
            .create(CreateRequest::user(json!({
                "username": "adminuser",
                "role": "administrator",
                "certificate_identities": ["wardrobe:user:adminuser"]
            })))
            .expect("user should register");

        let mut security = SecurityConfig::default();
        security.mode = SecurityMode::Managed;
        security.security_dir = security_dir.clone();
        let tls = build_server_tls_runtime(&security)
            .expect("TLS config should build")
            .expect("managed mode should enable TLS");
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener should bind");
        let address = listener.local_addr().expect("listener address");
        listener
            .set_nonblocking(true)
            .expect("listener should be nonblocking");
        let server_engine = Arc::clone(&engine);
        let server = thread::spawn(move || {
            serve_tcp_listener_with_security(
                listener,
                server_engine,
                Some(2),
                ServerRuntimeConfig::default(),
                Some(tls),
            )
        });

        let client = WardrobeClient::open_with_profile(
            format!("wardrobe://{address}"),
            &certificate.profile,
        )
        .expect("TLS client should connect");
        let tenants = client
            .status(StatusRequest::tenants())
            .expect("authenticated command should execute");
        assert!(tenants.is_empty());
        drop(client);

        wardrobe_core::revoke_managed_certificate(&security_dir, &certificate.serial)
            .expect("certificate should revoke");
        let revoked_client = WardrobeClient::open_with_profile(
            format!("wardrobe://{address}"),
            &certificate.profile,
        )
        .expect("TCP connection should open before TLS command");
        assert!(revoked_client.status(StatusRequest::tenants()).is_err());
        drop(revoked_client);

        server
            .join()
            .expect("server thread should join")
            .expect("server should stop after idle");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn server_config_parses_application_logging_flags() {
        let cfg = ServerConfig::from_args(vec![
            "--log-level".to_string(),
            "debug".to_string(),
            "--log-format".to_string(),
            "json".to_string(),
            "--log-destination".to_string(),
            "file".to_string(),
            "--log-file".to_string(),
            "logs/wardrobe.log".to_string(),
        ])
        .expect("logging flags should parse");

        assert_eq!(cfg.logging.level, ApplicationLogLevel::Debug);
        assert_eq!(cfg.logging.format, ApplicationLogFormat::Json);
        assert_eq!(cfg.logging.destination, ApplicationLogDestination::File);
        assert_eq!(cfg.logging.file, Some(PathBuf::from("logs/wardrobe.log")));
    }

    #[test]
    fn server_config_rejects_invalid_application_logging_flags() {
        assert!(
            ServerConfig::from_args(vec!["--log-level".to_string(), "verbose".to_string()])
                .is_err()
        );
        assert!(
            ServerConfig::from_args(vec!["--log-format".to_string(), "xml".to_string()]).is_err()
        );
        assert!(
            ServerConfig::from_args(vec!["--log-destination".to_string(), "syslog".to_string()])
                .is_err()
        );
        assert!(
            ServerConfig::from_args(vec![
                "--log-level".to_string(),
                "info".to_string(),
                "--log-destination".to_string(),
                "file".to_string()
            ])
            .is_err()
        );
    }

    #[test]
    fn server_config_parses_grouped_durability_flags() {
        let cfg = ServerConfig::from_args(vec![
            "--durability".to_string(),
            "grouped".to_string(),
            "--group-commit-window-ms".to_string(),
            "9".to_string(),
            "--group-commit-max-batch".to_string(),
            "33".to_string(),
        ])
        .expect("durability flags should parse");

        assert_eq!(
            cfg.durability_policy,
            DurabilityPolicy::Grouped {
                commit_window_ms: 9,
                max_batch_size: 33
            }
        );
    }

    #[test]
    fn configure_tcp_stream_disables_nagle() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener should bind");
        let address = listener.local_addr().expect("listener should have address");
        let client = std::net::TcpStream::connect(address).expect("client should connect");
        let (stream, _) = listener.accept().expect("listener should accept client");

        configure_tcp_stream(&stream).expect("stream should configure");

        assert!(
            stream
                .nodelay()
                .expect("stream should report nodelay status")
        );

        drop(client);
    }

    #[test]
    fn server_config_invalid_connection_pool_limit_zero() {
        let args = vec!["--connection-pool-limit".to_string(), "0".to_string()];
        let res = ServerConfig::from_args(args);
        assert!(res.is_err());
        assert_eq!(res.err().unwrap().kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn join_listener_handles_ok_result() {
        let handle = thread::spawn(|| -> io::Result<()> { Ok(()) });
        let res = join_listener(handle);
        assert!(res.is_ok());
    }

    #[test]
    fn write_error_frame_writes_error_opcode_and_message() {
        let mut buf: Vec<u8> = Vec::new();
        write_error_frame(&mut buf, "boom", ProtocolWritePolicy::Flush)
            .expect("write should succeed");
        let mut cursor = Cursor::new(buf);
        let frame = ProtocolFrame::read_from_stream(&mut cursor).expect("frame should parse");
        assert_eq!(frame.opcode, ProtocolOpcode::Error);
        assert!(String::from_utf8_lossy(&frame.payload).contains("boom"));
    }

    #[test]
    fn server_config_from_args_unknown_arg_errors() {
        let args = vec!["--no-such-arg".to_string()];
        let res = ServerConfig::from_args(args);
        assert!(res.is_err());
        assert_eq!(res.err().unwrap().kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn server_config_parse_connection_pool_limit_invalid_value() {
        let args = vec![
            "--connection-pool-limit".to_string(),
            "not-a-number".to_string(),
        ];
        let res = ServerConfig::from_args(args);
        assert!(res.is_err());
        assert_eq!(res.err().unwrap().kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn server_config_unix_socket_requires_path() {
        let args = vec!["--unix-socket".to_string()];
        let res = ServerConfig::from_args(args);
        assert!(res.is_err());
        assert_eq!(res.err().unwrap().kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn command_label_covers_canonical_protocol_command_set() {
        let archive = BackupArchive {
            format: "wardrobe-backup-v1".to_string(),
            source_path: "source".to_string(),
            scope: "directory".to_string(),
            files: vec![BackupArchiveFile {
                path: "gem.drw".to_string(),
                bytes_hex: "00".to_string(),
            }],
        };
        let commands = vec![
            (
                Command::Read {
                    filter: OperationFilter::drawer("gem"),
                    options: OperationOptions::default(),
                },
                "read",
            ),
            (
                Command::Upsert {
                    payload: json!({"_id": "one"}),
                    filter: OperationFilter::drawer("gem"),
                    options: OperationOptions::default(),
                },
                "upsert",
            ),
            (
                Command::Delete {
                    filter: OperationFilter::pointer("@gem:one"),
                    options: OperationOptions::default(),
                },
                "delete",
            ),
            (
                Command::Inspect {
                    filter: OperationFilter::drawer("gem"),
                    options: OperationOptions::default(),
                },
                "inspect",
            ),
            (
                Command::Count {
                    filter: OperationFilter::drawer("gem"),
                    options: OperationOptions::default(),
                },
                "count",
            ),
            (Command::Compact(CompactRequest::drawer("gem")), "compact"),
            (Command::Create(CreateRequest::database("db")), "create"),
            (
                Command::Alter(AlterRequest::schema_rule(
                    "gem",
                    "add",
                    "index",
                    "element",
                    json!({"type": "hash"}),
                )),
                "alter",
            ),
            (
                Command::Drop(DropRequest::schema_rule(
                    "gem",
                    "index",
                    "element",
                    json!({}),
                )),
                "drop",
            ),
            (
                Command::Backup {
                    source_path: "source".to_string(),
                },
                "backup",
            ),
            (
                Command::Restore {
                    destination_path: "destination".to_string(),
                    archive,
                },
                "restore",
            ),
            (
                Command::Grant(PermissionRequest::new("alice", "db:rud")),
                "grant",
            ),
            (
                Command::Revoke(PermissionRequest::new("alice", "db:rud")),
                "revoke",
            ),
            (
                Command::Status(StatusRequest::tenants().into_request()),
                "status",
            ),
            (
                Command::ExecuteForTenant {
                    tenant_id: "tenant".to_string(),
                    database_name: "db".to_string(),
                    schema_name: "public".to_string(),
                    command: Box::new(Command::Read {
                        filter: OperationFilter::drawer("gem"),
                        options: OperationOptions::default(),
                    }),
                },
                "execute_for_tenant",
            ),
            (
                Command::Execute {
                    coordinate: StorageCoordinate::new("tenant", "db", "public"),
                    command: Box::new(Command::Count {
                        filter: OperationFilter::drawer("gem"),
                        options: OperationOptions::default(),
                    }),
                },
                "execute",
            ),
            (
                Command::ExecuteInScope {
                    scope: StorageScope::schema("db", "public"),
                    command: Box::new(Command::Delete {
                        filter: OperationFilter::query_in("gem", json!({"element": "Fire"})),
                        options: OperationOptions::new().multi(true),
                    }),
                },
                "execute_in_scope",
            ),
        ];

        for (command, expected_label) in commands {
            assert_eq!(command_label(&command), expected_label);
        }
    }

    #[test]
    fn connection_pool_and_idle_tracking_cover_permit_lifecycle() {
        let pool = ConnectionPool::new(1);
        assert!(pool.is_idle());

        {
            let _permit = pool.acquire();
            assert!(!pool.is_idle());
        }

        assert!(pool.is_idle());

        let mut idle_polls = 0;
        for _ in 1..nonblocking_idle_poll_limit() {
            assert!(!listener_is_idle(Some(&pool), &mut idle_polls));
        }
        assert!(listener_is_idle(Some(&pool), &mut idle_polls));

        idle_polls = 0;
        for _ in 1..nonblocking_idle_poll_limit() {
            assert!(!listener_is_idle(None, &mut idle_polls));
        }
        assert!(listener_is_idle(None, &mut idle_polls));
    }
}
