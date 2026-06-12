use std::io::{self, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use wardrobe_core::{Command, DEFAULT_NETWORK_PORT, ProtocolFrame, ProtocolOpcode, WardrobeEngine};

#[cfg(unix)]
use std::os::unix::net::UnixListener;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerConfig {
    pub data_dir: String,
    pub check_only: bool,
    pub tcp_bind: Option<String>,
    pub unix_socket: Option<PathBuf>,
    pub max_connections: Option<usize>,
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
        let mut max_connections = None;
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
                "--max-connections" => {
                    let raw = args.next().ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "--max-connections requires a positive integer",
                        )
                    })?;
                    let parsed = raw.parse::<usize>().map_err(|error| {
                        io::Error::new(
                            io::ErrorKind::InvalidInput,
                            format!("Invalid --max-connections value '{raw}': {error}"),
                        )
                    })?;
                    if parsed == 0 {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "--max-connections must be greater than zero",
                        ));
                    }
                    max_connections = Some(parsed);
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

        Ok(Self {
            data_dir,
            check_only,
            tcp_bind,
            unix_socket,
            max_connections,
        })
    }
}

pub fn print_help() {
    println!("wardrobe-server");
    println!("  --data-dir <path>          Storage directory for the Wardrobe database");
    println!("  --tcp-bind <addr:port>     Bind TCP listener, default 127.0.0.1:24842");
    println!("  --no-tcp                   Disable TCP listener");
    println!("  --unix-socket <path>       Bind Unix domain socket listener on Unix");
    println!("  --max-connections <count>  Stop after accepting count connections");
    println!("  --check                    Initialize the daemon and exit without blocking");
}

pub fn run(config: ServerConfig) -> io::Result<()> {
    let engine = Arc::new(WardrobeEngine::open(&config.data_dir)?);

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
        let max_connections = config.max_connections;
        listener_threads.push(thread::spawn(move || {
            serve_tcp_listener(listener, engine, max_connections)
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
            let max_connections = config.max_connections;
            listener_threads.push(thread::spawn(move || {
                serve_unix_listener(listener, engine, max_connections)
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
    max_connections: Option<usize>,
) -> io::Result<()> {
    let mut handlers = Vec::new();

    for stream in listener.incoming() {
        let stream = stream?;
        let engine = Arc::clone(&engine);
        handlers.push(thread::spawn(move || {
            handle_protocol_stream(engine, stream)
        }));

        if max_connections.is_some_and(|limit| handlers.len() >= limit) {
            break;
        }
    }

    join_handlers(handlers)
}

#[cfg(unix)]
pub fn serve_unix_listener(
    listener: UnixListener,
    engine: Arc<WardrobeEngine>,
    max_connections: Option<usize>,
) -> io::Result<()> {
    let mut handlers = Vec::new();

    for stream in listener.incoming() {
        let stream = stream?;
        let engine = Arc::clone(&engine);
        handlers.push(thread::spawn(move || {
            handle_protocol_stream(engine, stream)
        }));

        if max_connections.is_some_and(|limit| handlers.len() >= limit) {
            break;
        }
    }

    join_handlers(handlers)
}

pub fn handle_protocol_stream<S>(engine: Arc<WardrobeEngine>, mut stream: S) -> io::Result<()>
where
    S: Read + Write,
{
    loop {
        let frame = match ProtocolFrame::read_from_stream(&mut stream) {
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

        if frame.opcode != ProtocolOpcode::Command {
            write_error_frame(
                &mut stream,
                "Wardrobe server expected a command frame from the client",
            )?;
            continue;
        }

        let command = match serde_json::from_slice::<Command>(&frame.payload) {
            Ok(command) => command,
            Err(error) => {
                write_error_frame(
                    &mut stream,
                    &format!("Failed to deserialize Wardrobe command: {error}"),
                )?;
                continue;
            }
        };

        match engine.execute_command(command) {
            Ok(result) => {
                let payload = serde_json::to_vec(&result).map_err(|error| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("Failed to serialize Wardrobe command result: {error}"),
                    )
                })?;
                ProtocolFrame::new(ProtocolOpcode::Result, payload).write_to_stream(&mut stream)?;
            }
            Err(error) => {
                write_error_frame(&mut stream, &error.to_string())?;
            }
        }
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

fn join_handlers(handlers: Vec<JoinHandle<io::Result<()>>>) -> io::Result<()> {
    for handle in handlers {
        handle
            .join()
            .map_err(|_| io::Error::other("Wardrobe connection handler thread panicked"))??;
    }

    Ok(())
}
