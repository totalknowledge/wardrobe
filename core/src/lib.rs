#![deny(unsafe_code)]

#[path = "wrdb_lib/mod.rs"]
pub mod wrdb_lib;

pub mod client;
pub mod engine;

pub use client::WardrobeClient;
pub use engine::{
    AlterRequest, BackupArchive, BackupArchiveFile, CheckEntry, CheckReport, Command,
    CommandResult, CompactMode, CompactRequest, CreateRequest, CreateResult, DeleteResult,
    DrawerInspectionMetrics, DropRequest, InspectResult, OperationFilter, OperationOptions,
    OrderDirection, PermissionRequest, PermissionScopeDescriptor, QueryModifiers, ReadResult,
    RestoreReport, ReturnShape, StatusRequest, StatusResult, StorageCoordinate, StorageDiagnosis,
    StorageInventory, StorageLocator, StorageScope, UpsertResult, WardrobeEngine,
};
pub use wrdb_lib::application_logging::{
    ApplicationLogDestination, ApplicationLogEvent, ApplicationLogFormat, ApplicationLogLevel,
    ApplicationLoggingConfig, application_logging_is_configured, emit_application_log,
    init_application_logging, shutdown_application_logging,
};
pub use wrdb_lib::connection::{ConnectionTarget, DEFAULT_NETWORK_PORT, DriverKind};
pub use wrdb_lib::database::Database;
pub use wrdb_lib::drawer::{Drawer, VacuumReport};
pub use wrdb_lib::protocol::{PROTOCOL_MAGIC, ProtocolFrame, ProtocolOpcode};
pub use wrdb_lib::reader::DatabaseReader;
pub use wrdb_lib::recycler::Recycler;
pub use wrdb_lib::registry::{
    CATALOG_FILE_NAME, CatalogEntry, CatalogRegistry, CatalogTenantRoute,
};
pub use wrdb_lib::storage_format::{BsonBinaryFormat, StorageFormat};
pub use wrdb_lib::wal::{
    DurabilityPolicy, WAL_FILE_NAME, WalEntry, WalJournal, WalOperation, WalVerification,
};
pub use wrdb_lib::writer::DatabaseWriter;
