use super::command::{Command, CommandResult};
use super::protocol::{ProtocolFrame, ProtocolOpcode};
use std::io::{Error, ErrorKind, Read, Result, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::Mutex;

#[cfg(unix)]
use std::os::unix::net::UnixStream;

pub(crate) struct NetworkTransport {
    host: String,
    port: u16,
    stream: Mutex<TcpStream>,
}

pub(crate) struct UnixSocketTransport {
    path: PathBuf,
    #[cfg(unix)]
    stream: Mutex<UnixStream>,
}

impl NetworkTransport {
    pub(crate) fn connect(host: String, port: u16) -> Result<Self> {
        let stream = TcpStream::connect((host.as_str(), port))?;
        stream.set_nodelay(true)?;
        Ok(Self {
            host,
            port,
            stream: Mutex::new(stream),
        })
    }

    pub(crate) fn execute(&self, command: Command) -> Result<CommandResult> {
        execute_on_stream(
            &self.stream,
            command,
            format!("{}:{}", self.host, self.port),
        )
    }
}

impl UnixSocketTransport {
    pub(crate) fn connect(path: PathBuf) -> Result<Self> {
        #[cfg(unix)]
        {
            let stream = UnixStream::connect(&path)?;
            Ok(Self {
                path,
                stream: Mutex::new(stream),
            })
        }

        #[cfg(not(unix))]
        {
            Err(Error::new(
                ErrorKind::Unsupported,
                format!(
                    "Wardrobe Unix socket driver selected for {}, but Unix sockets are not available on this platform",
                    path.display()
                ),
            ))
        }
    }

    pub(crate) fn execute(&self, command: Command) -> Result<CommandResult> {
        #[cfg(unix)]
        {
            execute_on_stream(&self.stream, command, self.path.display().to_string())
        }

        #[cfg(not(unix))]
        {
            let _ = command;
            Err(Error::new(
                ErrorKind::Unsupported,
                format!(
                    "Wardrobe Unix socket driver selected for {}, but Unix sockets are not available on this platform",
                    self.path.display()
                ),
            ))
        }
    }
}

fn execute_on_stream<S>(
    stream: &Mutex<S>,
    command: Command,
    target_description: String,
) -> Result<CommandResult>
where
    S: Read + Write,
{
    let payload = serde_json::to_vec(&command).map_err(|error| {
        Error::new(
            ErrorKind::InvalidInput,
            format!("Failed to serialize Wardrobe command: {error}"),
        )
    })?;

    let mut stream = stream.lock().map_err(|_| {
        Error::other(format!(
            "Wardrobe protocol stream lock was poisoned for {target_description}",
        ))
    })?;

    ProtocolFrame::new(ProtocolOpcode::Command, payload).write_to_stream(&mut *stream)?;
    let response = ProtocolFrame::read_from_stream(&mut *stream)?;

    match response.opcode {
        ProtocolOpcode::Result => serde_json::from_slice(&response.payload).map_err(|error| {
            Error::new(
                ErrorKind::InvalidData,
                format!("Failed to deserialize Wardrobe command result: {error}"),
            )
        }),
        ProtocolOpcode::Error => Err(Error::new(
            ErrorKind::Other,
            String::from_utf8_lossy(&response.payload).into_owned(),
        )),
        ProtocolOpcode::Command => Err(Error::new(
            ErrorKind::InvalidData,
            "Wardrobe server returned a command frame where a result was expected",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wrdb_lib::command::{OperationFilter, OperationOptions};
    use std::io::Cursor;
    use std::net::TcpListener;
    use std::thread;

    struct FakeStream {
        read: Cursor<Vec<u8>>,
        write: Vec<u8>,
    }

    impl Read for FakeStream {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.read.read(buf)
        }
    }

    impl Write for FakeStream {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.write.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn execute_on_stream_roundtrips_result() {
        let payload = serde_json::to_vec(&CommandResult::Count(3)).expect("serialize");
        let mut response_bytes = Vec::new();
        ProtocolFrame::new(ProtocolOpcode::Result, payload)
            .write_to_stream(&mut response_bytes)
            .expect("frame write");

        let stream = FakeStream {
            read: Cursor::new(response_bytes),
            write: Vec::new(),
        };
        let mutex = Mutex::new(stream);

        let command = Command::Count {
            filter: OperationFilter::drawer("gem"),
            options: OperationOptions::default(),
        };
        let result = execute_on_stream(&mutex, command, "test-target".to_string())
            .expect("execute should succeed");
        assert_eq!(result, CommandResult::Count(3));
    }

    #[test]
    fn network_transport_connect_disables_nagle() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener should bind");
        let address = listener.local_addr().expect("listener should have address");
        let accept_thread = thread::spawn(move || {
            let _ = listener.accept().expect("listener should accept client");
        });

        let transport = NetworkTransport::connect(address.ip().to_string(), address.port())
            .expect("transport should connect");

        assert!(
            transport
                .stream
                .lock()
                .expect("transport stream should lock")
                .nodelay()
                .expect("transport stream should report nodelay")
        );

        drop(transport);
        accept_thread.join().expect("accept thread should exit");
    }
}
