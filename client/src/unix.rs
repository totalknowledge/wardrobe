use crate::protocol::{ProtocolFrame, ProtocolOpcode};
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

    pub(crate) fn execute(&self, command: &Value) -> Result<Value> {
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
