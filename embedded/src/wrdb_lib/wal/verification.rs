use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WalVerification {
    pub path: String,
    pub entry_count: usize,
    pub last_sequence: Option<u64>,
}

pub(crate) fn verify(
    root_directory: &Path,
    database_name: Option<&str>,
) -> Result<WalVerification> {
    let database_path = match database_name {
        Some(database_name) => crate::wrdb_lib::catalog_validation::database_path_from_name(
            root_directory,
            database_name,
        )?,
        None => root_directory.to_path_buf(),
    };
    WalJournal::at_database_path(database_path).verify()
}
