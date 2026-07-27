use crate::wrdb_lib::protocol::{ProtocolFrame, ProtocolOpcode};
use serde_json::Value;
use std::io::{Error, ErrorKind, Result};
use std::path::PathBuf;

#[cfg(unix)]
use std::os::unix::net::UnixStream;
#[cfg(unix)]
use std::sync::Mutex;

pub struct UnixSocketTransport {
    path: PathBuf,
    #[cfg(unix)]
    stream: Mutex<UnixStream>,
}

impl UnixSocketTransport {
    pub fn connect(path: PathBuf) -> Result<Self> {
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

    pub fn execute(&self, command: &Value) -> Result<Value> {
        #[cfg(unix)]
        {
            let payload = serde_json::to_vec(command).map_err(|error| {
                Error::new(
                    ErrorKind::InvalidInput,
                    format!("Failed to serialize Wardrobe command: {error}"),
                )
            })?;

            let mut stream = self.stream.lock().map_err(|_| {
                Error::other(format!(
                    "Wardrobe protocol stream lock was poisoned for {}",
                    self.path.display()
                ))
            })?;

            ProtocolFrame::write_payload_to_stream_unflushed(
                ProtocolOpcode::Command,
                &payload,
                &mut *stream,
            )?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(unix)]
    fn test_unix_socket_transport() {
        use std::os::unix::net::UnixListener;
        let temp_dir = std::env::temp_dir();
        let sock_path = temp_dir.join(format!("test_sock_{}.sock", uuid::Uuid::new_v4()));

        let listener = UnixListener::bind(&sock_path).unwrap();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let frame = ProtocolFrame::read_from_stream(&mut stream).unwrap();
            assert_eq!(frame.opcode, ProtocolOpcode::Command);
            let resp_frame = ProtocolFrame::new(ProtocolOpcode::Result, b"{\"status\":\"ok\"}".to_vec());
            resp_frame.write_to_stream(&mut stream).unwrap();
        });

        let transport = UnixSocketTransport::connect(sock_path.clone()).unwrap();
        let res = transport.execute(&serde_json::json!({"op": "ping"})).unwrap();
        assert_eq!(res["status"], "ok");

        handle.join().unwrap();
        let _ = std::fs::remove_file(sock_path);
    }
}
