use super::execute_on_socket_stream;
use crate::wrdb_lib::command::{Command, CommandResult};
use crate::wrdb_lib::config::ClientTlsConfig;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};
use std::fs;
use std::io::{BufReader, Error, ErrorKind, Read, Result, Write};
use std::net::TcpStream;
use std::path::Path;
use std::sync::{Arc, Mutex};

pub(crate) struct NetworkTransport {
    host: String,
    port: u16,
    stream: Mutex<NetworkStream>,
}

enum NetworkStream {
    Plain(TcpStream),
    Tls(Box<StreamOwned<ClientConnection, TcpStream>>),
}

impl NetworkTransport {
    pub(crate) fn connect(host: String, port: u16, tls: Option<&ClientTlsConfig>) -> Result<Self> {
        let stream = TcpStream::connect((host.as_str(), port))?;
        stream.set_nodelay(true)?;
        let stream = match tls {
            Some(tls) => NetworkStream::Tls(Box::new(connect_tls(stream, tls)?)),
            None => NetworkStream::Plain(stream),
        };
        Ok(Self {
            host,
            port,
            stream: Mutex::new(stream),
        })
    }

    pub(crate) fn execute(&self, command: Command) -> Result<CommandResult> {
        execute_on_socket_stream(
            &self.stream,
            command,
            format!("{}:{}", self.host, self.port),
        )
    }
}

impl Read for NetworkStream {
    fn read(&mut self, buffer: &mut [u8]) -> Result<usize> {
        match self {
            Self::Plain(stream) => stream.read(buffer),
            Self::Tls(stream) => stream.read(buffer),
        }
    }
}

impl Write for NetworkStream {
    fn write(&mut self, buffer: &[u8]) -> Result<usize> {
        match self {
            Self::Plain(stream) => stream.write(buffer),
            Self::Tls(stream) => stream.write(buffer),
        }
    }

    fn flush(&mut self) -> Result<()> {
        match self {
            Self::Plain(stream) => stream.flush(),
            Self::Tls(stream) => stream.flush(),
        }
    }
}

impl NetworkStream {
    #[cfg(test)]
    fn nodelay(&self) -> Result<bool> {
        match self {
            Self::Plain(stream) => stream.nodelay(),
            Self::Tls(stream) => stream.sock.nodelay(),
        }
    }
}

fn connect_tls(
    stream: TcpStream,
    tls: &ClientTlsConfig,
) -> Result<StreamOwned<ClientConnection, TcpStream>> {
    let mut roots = RootCertStore::empty();
    for certificate in load_certificates(&tls.ca_cert)? {
        roots.add(certificate).map_err(|error| {
            Error::new(
                ErrorKind::InvalidData,
                format!(
                    "Invalid trusted server CA certificate {}: {error}",
                    tls.ca_cert.display()
                ),
            )
        })?;
    }
    let config = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_client_auth_cert(
            load_certificates(&tls.client_cert)?,
            load_private_key(&tls.client_key)?,
        )
        .map_err(|error| {
            Error::new(
                ErrorKind::InvalidData,
                format!("Invalid client TLS identity: {error}"),
            )
        })?;
    let server_name = ServerName::try_from(tls.server_name.clone()).map_err(|error| {
        Error::new(
            ErrorKind::InvalidInput,
            format!("Invalid TLS server name '{}': {error}", tls.server_name),
        )
    })?;
    let connection =
        ClientConnection::new(Arc::new(config), server_name).map_err(tls_connection_error)?;
    Ok(StreamOwned::new(connection, stream))
}

fn load_certificates(path: &Path) -> Result<Vec<CertificateDer<'static>>> {
    let bytes = fs::read(path)?;
    let mut reader = BufReader::new(bytes.as_slice());
    let certificates = rustls_pemfile::certs(&mut reader).collect::<Result<Vec<_>>>()?;
    if certificates.is_empty() {
        return Err(Error::new(
            ErrorKind::InvalidData,
            format!("No certificates found in {}", path.display()),
        ));
    }
    Ok(certificates)
}

fn load_private_key(path: &Path) -> Result<PrivateKeyDer<'static>> {
    let bytes = fs::read(path)?;
    rustls_pemfile::private_key(&mut BufReader::new(bytes.as_slice()))?.ok_or_else(|| {
        Error::new(
            ErrorKind::InvalidData,
            format!("No private key found in {}", path.display()),
        )
    })
}

fn tls_connection_error(error: rustls::Error) -> Error {
    Error::new(
        ErrorKind::ConnectionRefused,
        format!("Failed to configure TLS connection: {error}"),
    )
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

        let transport = NetworkTransport::connect(address.ip().to_string(), address.port(), None)
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
