use crate::model::ClientTlsConfig;
use crate::protocol::{ProtocolFrame, ProtocolOpcode};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};
use serde_json::Value;
use std::fs;
use std::io::{BufReader, Error, ErrorKind, Read, Result, Write};
use std::net::TcpStream;
use std::path::Path;
use std::sync::Mutex;

pub struct NetworkTransport {
    host: String,
    port: u16,
    stream: Mutex<NetworkStream>,
}

enum NetworkStream {
    Plain(TcpStream),
    Tls(Box<StreamOwned<ClientConnection, TcpStream>>),
}

impl NetworkTransport {
    pub fn connect(host: String, port: u16, tls: Option<&ClientTlsConfig>) -> Result<Self> {
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

    pub fn execute(&self, command: &Value) -> Result<Value> {
        let payload = serde_json::to_vec(command).map_err(|error| {
            Error::new(
                ErrorKind::InvalidInput,
                format!("Failed to serialize Wardrobe command: {error}"),
            )
        })?;

        let mut stream = self.stream.lock().map_err(|_| {
            Error::other(format!(
                "Wardrobe protocol stream lock was poisoned for {}:{}",
                self.host, self.port
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
    let server_name = ServerName::try_from("localhost").map_err(|error| {
        Error::new(
            ErrorKind::InvalidInput,
            format!("Invalid TLS server name 'localhost': {error}"),
        )
    })?;
    let connection =
        ClientConnection::new(std::sync::Arc::new(config), server_name).map_err(tls_connection_error)?;
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
