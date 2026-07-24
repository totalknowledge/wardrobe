#[cfg(unix)]
use super::execute_on_socket_stream;
use crate::wrdb_lib::command::{Command, CommandResult};
use std::io::Result;
#[cfg(not(unix))]
use std::io::{Error, ErrorKind};
use std::path::PathBuf;

#[cfg(unix)]
use std::os::unix::net::UnixStream;
#[cfg(unix)]
use std::sync::Mutex;

pub(crate) struct UnixSocketTransport {
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

    pub(crate) fn execute(&self, command: Command) -> Result<CommandResult> {
        #[cfg(unix)]
        {
            execute_on_socket_stream(&self.stream, command, self.path.display().to_string())
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
