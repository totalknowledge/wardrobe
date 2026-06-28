use super::execute_on_stream;
use crate::wrdb_lib::command::{Command, CommandResult};
use std::io::Result;
use std::net::TcpStream;
use std::sync::Mutex;

pub(crate) struct NetworkTransport {
    host: String,
    port: u16,
    stream: Mutex<TcpStream>,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::thread;

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
