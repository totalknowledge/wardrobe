use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use wardrobe_core::{
    Command, DEFAULT_NETWORK_PORT, DurabilityPolicy, ProtocolFrame, ProtocolOpcode, WardrobeEngine,
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
    pub durability_policy: DurabilityPolicy,
    pub profile_commands: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ServerRuntimeConfig {
    pub profile_commands: bool,
}

impl ServerConfig {
    pub fn from_args<I>(args: I) -> io::Result<Self>
    where
        I: IntoIterator<Item = String>,
    {
        let mut data_dir = String::from("./wardrobe");
        let mut check_only = false;
        let mut tcp_bind = Some(format!("127.0.0.1:{DEFAULT_NETWORK_PORT}"));
        let mut unix_socket = None;
        let mut connection_pool_limit = None;
        let mut durability_policy = DurabilityPolicy::Strict;
        let mut profile_commands = false;
        let mut args = args.into_iter();

        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--data-dir" => {
                    data_dir = args.next().ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "--data-dir requires a directory path",
                        )
                    })?;
                }
                "--tcp-bind" => {
                    tcp_bind = Some(args.next().ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "--tcp-bind requires an address such as 127.0.0.1:24842",
                        )
                    })?);
                }
                "--no-tcp" => tcp_bind = None,
                "--unix-socket" => {
                    unix_socket = Some(PathBuf::from(args.next().ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "--unix-socket requires a socket path",
                        )
                    })?));
                }
                "--connection-pool-limit" => {
                    connection_pool_limit = Some(parse_connection_pool_limit(&mut args, &arg)?);
                }
                "--durability" => {
                    durability_policy = parse_durability_policy(&mut args, &arg)?;
                }
                "--group-commit-window-ms" => {
                    let commit_window_ms = parse_positive_u64(&mut args, &arg)?;
                    durability_policy =
                        update_group_commit_window(durability_policy, commit_window_ms);
                }
                "--group-commit-max-batch" => {
                    let max_batch_size = parse_positive_usize(&mut args, &arg)?;
                    durability_policy =
                        update_group_commit_max_batch(durability_policy, max_batch_size);
                }
                "--profile-commands" => profile_commands = true,
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

        Ok(Self {
            data_dir,
            check_only,
            tcp_bind,
            unix_socket,
            connection_pool_limit,
            durability_policy,
            profile_commands,
        })
    }
}

pub fn print_help() {
    println!("wardrobe-server");
    println!("  --data-dir <path>          Storage directory for the Wardrobe database");
    println!("  --tcp-bind <addr:port>     Bind TCP listener, default 127.0.0.1:24842");
    println!("  --no-tcp                   Disable TCP listener");
    println!("  --unix-socket <path>       Bind Unix domain socket listener on Unix");
    println!("  --connection-pool-limit <count>  Maximum active worker connections");
    println!("  --durability <mode>        WAL durability mode: strict or grouped");
    println!(
        "  --group-commit-window-ms <ms>  Grouped WAL commit window, default {DEFAULT_GROUP_COMMIT_WINDOW_MS}"
    );
    println!(
        "  --group-commit-max-batch <count>  Grouped WAL max batch, default {DEFAULT_GROUP_COMMIT_MAX_BATCH}"
    );
    println!("  --profile-commands         Print per-command protocol and engine timings");
    println!("  --check                    Initialize the daemon and exit without blocking");
}

pub fn run(config: ServerConfig) -> io::Result<()> {
    let engine = Arc::new(WardrobeEngine::open_with_durability_policy(
        &config.data_dir,
        config.durability_policy.clone(),
    )?);

    println!(
        "Wardrobe daemon initialized with storage directory: {}",
        config.data_dir
    );

    if config.check_only {
        println!("Wardrobe daemon check completed.");
        return Ok(());
    }

    if config.tcp_bind.is_none() && config.unix_socket.is_none() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "At least one Wardrobe server listener must be enabled",
        ));
    }

    let mut listener_threads = Vec::new();

    if let Some(tcp_bind) = config.tcp_bind {
        let listener = TcpListener::bind(&tcp_bind)?;
        let local_addr = listener.local_addr()?;
        println!("Wardrobe daemon listening on TCP: {local_addr}");
        let engine = Arc::clone(&engine);
        let connection_pool_limit = config.connection_pool_limit;
        let runtime = ServerRuntimeConfig {
            profile_commands: config.profile_commands,
        };
        listener_threads.push(thread::spawn(move || {
            serve_tcp_listener_with_config(listener, engine, connection_pool_limit, runtime)
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
    let connection_pool = connection_pool_limit.map(ConnectionPool::new);
    let mut idle_polls = 0usize;

    loop {
        let permit = connection_pool.as_ref().map(ConnectionPool::acquire);
        match listener.accept() {
            Ok((stream, _)) => {
                configure_tcp_stream(&stream)?;
                idle_polls = 0;
                spawn_connection_handler(Arc::clone(&engine), stream, permit, runtime);
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
                spawn_connection_handler(Arc::clone(&engine), stream, permit, runtime);
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
    handle_protocol_stream_with_config(engine, &mut stream, ServerRuntimeConfig::default())
}

pub fn handle_protocol_stream_with_config<S>(
    engine: Arc<WardrobeEngine>,
    stream: &mut S,
    runtime: ServerRuntimeConfig,
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
            write_error_frame(
                &mut *stream,
                "Wardrobe server expected a command frame from the client",
            )?;
            continue;
        }

        let deserialization_started = Instant::now();
        let command = match serde_json::from_slice::<Command>(&frame.payload) {
            Ok(command) => command,
            Err(error) => {
                write_error_frame(
                    &mut *stream,
                    &format!("Failed to deserialize Wardrobe command: {error}"),
                )?;
                continue;
            }
        };
        let command_deserialization = deserialization_started.elapsed();
        let command_name = command_label(&command);
        let request_bytes = frame.payload.len();

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
                ProtocolFrame::new(ProtocolOpcode::Result, payload)
                    .write_to_stream(&mut *stream)?;
                let protocol_transmission = transmission_started.elapsed();
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
                write_error_frame(&mut *stream, &response)?;
                let protocol_transmission = transmission_started.elapsed();
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

fn write_error_frame<S>(stream: &mut S, message: &str) -> io::Result<()>
where
    S: Write,
{
    ProtocolFrame::new(ProtocolOpcode::Error, message.as_bytes().to_vec()).write_to_stream(stream)
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
) where
    S: Read + Write + Send + 'static,
{
    thread::spawn(move || {
        let _permit = permit;
        let mut stream = stream;
        if let Err(error) = handle_protocol_stream_with_config(engine, &mut stream, runtime) {
            eprintln!("Wardrobe connection handler failed: {error}");
        }
    });
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
    use wardrobe_core::{
        AlterRequest, BackupArchive, BackupArchiveFile, CompactRequest, CreateRequest, DropRequest,
        OperationFilter, OperationOptions, PermissionRequest, StatusRequest, StorageCoordinate,
        StorageScope,
    };

    #[test]
    fn server_config_from_args_defaults() {
        let cfg = ServerConfig::from_args(Vec::<String>::new()).expect("should parse defaults");
        assert_eq!(cfg.data_dir, "./wardrobe");
        assert_eq!(cfg.tcp_bind.unwrap().starts_with("127.0.0.1"), true);
        assert!(!cfg.check_only);
        assert!(!cfg.profile_commands);
        assert_eq!(cfg.durability_policy, DurabilityPolicy::Strict);
    }

    #[test]
    fn server_config_parses_command_profiling_flag() {
        let cfg = ServerConfig::from_args(vec!["--profile-commands".to_string()])
            .expect("profile flag should parse");
        assert!(cfg.profile_commands);
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
        write_error_frame(&mut buf, "boom").expect("write should succeed");
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
            (Command::Status(StatusRequest::tenants()), "status"),
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
