#![deny(unsafe_code)]

#[path = "wrdb_lib/mod.rs"]
pub mod wrdb_lib;

pub mod client;
pub mod engine;

pub use client::WardrobeClient;
pub use engine::{
    Command, CommandResult, OrderDirection, QueryModifiers, StorageCoordinate, StorageLocator,
    StorageScope, WardrobeEngine,
};
pub use wrdb_lib::connection::{ConnectionTarget, DEFAULT_NETWORK_PORT, DriverKind};
pub use wrdb_lib::database::Database;
pub use wrdb_lib::drawer::{Drawer, VacuumReport};
pub use wrdb_lib::protocol::{PROTOCOL_MAGIC, ProtocolFrame, ProtocolOpcode};
pub use wrdb_lib::reader::DatabaseReader;
pub use wrdb_lib::recycler::Recycler;
pub use wrdb_lib::storage_format::{PlainTextJsonFormat, StorageFormat};
pub use wrdb_lib::writer::DatabaseWriter;
