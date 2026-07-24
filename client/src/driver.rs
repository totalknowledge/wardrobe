use crate::connection::ConnectionTarget;
use crate::model::*;
use crate::network::NetworkTransport;
use crate::unix::UnixSocketTransport;
use serde_json::Value;
use std::io::{Error, ErrorKind, Result};

pub enum ClientDriver {
    Network(NetworkTransport),
    UnixSocket(UnixSocketTransport),
}

impl ClientDriver {
    pub(crate) fn open(target: &ConnectionTarget) -> Result<Self> {
        Self::open_with_tls(target, None)
    }

    pub(crate) fn open_with_tls(
        target: &ConnectionTarget,
        tls: Option<&ClientTlsConfig>,
    ) -> Result<Self> {
        match target {
            ConnectionTarget::EmbeddedPath(path) => Err(Error::new(
                ErrorKind::InvalidInput,
                format!(
                    "wardrobe-client is a pure network client library and does not include embedded flatfile engine storage for {}",
                    path.display()
                ),
            )),
            ConnectionTarget::Network { host, port } => Ok(Self::Network(
                NetworkTransport::connect(host.clone(), *port, tls)?,
            )),
            ConnectionTarget::UnixSocket { path } => {
                if tls.is_some() {
                    return Err(Error::new(
                        ErrorKind::InvalidInput,
                        "certificate profiles apply only to TCP Wardrobe connections",
                    ));
                }
                Ok(Self::UnixSocket(UnixSocketTransport::connect(
                    path.clone(),
                )?))
            }
        }
    }

    pub(crate) fn execute_transport(&self, command: Value) -> Result<Value> {
        match self {
            Self::Network(transport) => transport.execute(&command),
            Self::UnixSocket(transport) => transport.execute(&command),
        }
    }
}
