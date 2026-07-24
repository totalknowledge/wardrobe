#![deny(unsafe_code)]

pub use crate as wardrobe_embedded;
pub use crate as wardrobe_core;
pub use wardrobe_client::*;

pub mod wrdb_lib;

pub mod client;
pub mod engine;

pub use client::WardrobeClient;
pub use engine::{
    AlterRequest, BackupArchive, BackupArchiveFile, CheckEntry, CheckReport, Command,
    CommandResult, CompactMode, CompactRequest, CreateRequest, CreateResult, DeleteResult,
    DrawerInspectionMetrics, DropRequest, InspectResult, OperationFilter, OperationOptions,
    OrderDirection, PaginatedReadResult, PaginationMetadata, PermissionRequest,
    PermissionScopeDescriptor, QueryModifiers, ReadResult, RestoreReport, ReturnShape,
    StatusRequest, StatusRequestOutput, StorageCoordinate, StorageDiagnosis, StorageInventory,
    StorageLocator, StorageScope, TypedStatusRequest, UpsertResult, WardrobeEngine,
    WardrobeEngineBuilder,
};
pub use wrdb_lib::application_logging::{
    ApplicationLogDestination, ApplicationLogEvent, ApplicationLogFormat, ApplicationLogLevel,
    ApplicationLoggingConfig, application_logging_is_configured, emit_application_log,
    init_application_logging, shutdown_application_logging,
};
pub use wrdb_lib::config::{
    CacheConfig, CertificateIdentity, CertificateRecord, ClientCertificateProfile, ClientTlsConfig,
    DataConfig, NetworkConfig, PkiInitialization, SecurityConfig, SecurityMode, TransactionConfig,
    TransactionRecoveryMode, WalConfig, WardrobeConfig, certificate_identity_from_der,
    certificate_identity_from_pem, certificate_is_revoked, initialize_managed_pki,
    issue_managed_client_certificate, list_managed_certificates, managed_identity_certificates,
    reissue_managed_server_certificate, remove_managed_identity, renew_managed_client_certificate,
    revoke_managed_certificate, rotate_managed_ca,
};
pub use wrdb_lib::connection::{ConnectionTarget, DEFAULT_NETWORK_PORT, DriverKind};
pub use wrdb_lib::core::reader::DatabaseReader;
pub use wrdb_lib::core::recycler::Recycler;
pub use wrdb_lib::core::storage_format::{
    BsonBinaryFormat, NativeBinaryIndexFormat, StorageFormat,
};
pub use wrdb_lib::core::writer::DatabaseWriter;
pub use wrdb_lib::database::Database;
pub use wrdb_lib::drawer::{Drawer, VacuumReport};
pub use wrdb_lib::protocol::{PROTOCOL_MAGIC, ProtocolFrame, ProtocolOpcode};
pub use wrdb_lib::registry::{
    CATALOG_FILE_NAME, CatalogEntry, CatalogRegistry, CatalogTenantRoute,
};
pub use wrdb_lib::wal::{
    DurabilityPolicy, WAL_FILE_NAME, WalEntry, WalJournal, WalOperation, WalVerification,
};
