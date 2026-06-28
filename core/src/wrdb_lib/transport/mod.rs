pub mod connection;
pub(crate) mod driver;
pub(crate) mod network;
pub mod protocol;
pub(crate) mod unix;

pub(crate) use network::NetworkTransport;
pub(crate) use unix::UnixSocketTransport;

use crate::wrdb_lib::command::{Command, CommandResult};
use protocol::{ProtocolFrame, ProtocolOpcode};
use std::io::{Error, ErrorKind, Read, Result, Write};
use std::sync::Mutex;

pub(crate) fn execute_on_stream<S>(
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
}
