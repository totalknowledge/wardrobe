use super::WardrobeCommandRunner;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};
use std::fs;
use std::io::{self, BufReader, Error, ErrorKind};
use std::net::TcpStream;
use std::path::Path;
use std::sync::Arc;
use wardrobe_core::{
    ClientTlsConfig, Command, CommandResult, ConnectionTarget, ProtocolFrame, ProtocolOpcode,
};

pub(crate) struct TcpWardrobeRunner {
    pub(crate) stream: StreamOwned<ClientConnection, TcpStream>,
}

impl TcpWardrobeRunner {
    pub(crate) fn connect(uri: &str, profile: &Path) -> io::Result<Self> {
        let target = ConnectionTarget::parse(uri)?;
        let ConnectionTarget::Network { host, port } = target else {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "--wardrobe-remote-uri must be a wardrobe://host:port TCP URI",
            ));
        };
        let stream = TcpStream::connect((host.as_str(), port))?;
        stream.set_nodelay(true)?;
        let tls = ClientTlsConfig::from_profile(profile)?;
        let mut roots = RootCertStore::empty();
        for certificate in load_benchmark_certificates(&tls.ca_cert)? {
            roots.add(certificate).map_err(|error| {
                Error::new(
                    ErrorKind::InvalidData,
                    format!(
                        "Invalid trusted Wardrobe CA certificate {}: {error}",
                        tls.ca_cert.display()
                    ),
                )
            })?;
        }
        let config = ClientConfig::builder()
            .with_root_certificates(roots)
            .with_client_auth_cert(
                load_benchmark_certificates(&tls.client_cert)?,
                load_benchmark_private_key(&tls.client_key)?,
            )
            .map_err(|error| {
                Error::new(
                    ErrorKind::InvalidData,
                    format!("Invalid benchmark client TLS identity: {error}"),
                )
            })?;
        let server_name = ServerName::try_from(tls.server_name.clone()).map_err(|error| {
            Error::new(
                ErrorKind::InvalidInput,
                format!("Invalid TLS server name '{}': {error}", tls.server_name),
            )
        })?;
        let connection = ClientConnection::new(Arc::new(config), server_name).map_err(|error| {
            Error::new(
                ErrorKind::ConnectionRefused,
                format!("Failed to configure benchmark TLS connection: {error}"),
            )
        })?;
        let mut stream = StreamOwned::new(connection, stream);
        while stream.conn.is_handshaking() {
            stream.conn.complete_io(&mut stream.sock)?;
        }
        Ok(Self { stream })
    }
}

pub(crate) fn load_benchmark_certificates(path: &Path) -> io::Result<Vec<CertificateDer<'static>>> {
    let bytes = fs::read(path)?;
    let mut reader = BufReader::new(bytes.as_slice());
    let certificates = rustls_pemfile::certs(&mut reader).collect::<io::Result<Vec<_>>>()?;
    if certificates.is_empty() {
        return Err(Error::new(
            ErrorKind::InvalidData,
            format!("No certificates found in {}", path.display()),
        ));
    }
    Ok(certificates)
}

pub(crate) fn load_benchmark_private_key(path: &Path) -> io::Result<PrivateKeyDer<'static>> {
    let bytes = fs::read(path)?;
    rustls_pemfile::private_key(&mut BufReader::new(bytes.as_slice()))?.ok_or_else(|| {
        Error::new(
            ErrorKind::InvalidData,
            format!("No private key found in {}", path.display()),
        )
    })
}

impl WardrobeCommandRunner for TcpWardrobeRunner {
    fn execute(&mut self, command: Command) -> io::Result<CommandResult> {
        let payload = serde_json::to_vec(&command).map_err(|error| {
            Error::new(
                ErrorKind::InvalidInput,
                format!("Failed to serialize Wardrobe benchmark command: {error}"),
            )
        })?;
        ProtocolFrame::write_payload_to_stream_unflushed(
            ProtocolOpcode::Command,
            &payload,
            &mut self.stream,
        )?;
        let response = ProtocolFrame::read_from_stream(&mut self.stream)?;
        match response.opcode {
            ProtocolOpcode::Result => serde_json::from_slice(&response.payload).map_err(|error| {
                Error::new(
                    ErrorKind::InvalidData,
                    format!("Failed to deserialize Wardrobe benchmark result: {error}"),
                )
            }),
            ProtocolOpcode::Error => Err(Error::other(
                String::from_utf8_lossy(&response.payload).into_owned(),
            )),
            ProtocolOpcode::Command => Err(Error::new(
                ErrorKind::InvalidData,
                "Wardrobe benchmark expected a result frame, got a command frame",
            )),
        }
    }
}
