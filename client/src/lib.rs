//! Thin network transport library crate for connecting to Wardrobe database servers.

pub mod client;
pub mod wrdb_lib;

pub use client::WardrobeClient;
pub use wrdb_lib::command::model;
pub use wrdb_lib::{connection, driver, network, protocol, unix};

pub use connection::{ConnectionTarget, DEFAULT_NETWORK_PORT, DriverKind};
pub use driver::ClientDriver;
pub use model::*;
pub use protocol::{PROTOCOL_MAGIC, ProtocolFrame, ProtocolOpcode};
