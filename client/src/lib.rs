//! Thin network transport library crate for connecting to Wardrobe database servers.

pub mod connection;
pub mod driver;
pub mod model;
pub mod network;
pub mod protocol;
pub mod unix;
pub mod client;

pub use connection::{ConnectionTarget, DriverKind, DEFAULT_NETWORK_PORT};
pub use driver::ClientDriver;
pub use model::*;
pub use protocol::{ProtocolFrame, ProtocolOpcode, PROTOCOL_MAGIC};
pub use client::WardrobeClient;