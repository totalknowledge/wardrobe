pub(crate) mod mongodb;
pub(crate) mod mysql;
pub(crate) mod neo4j;
pub(crate) mod postgres;
pub(crate) mod redb;
pub(crate) mod rocksdb;
pub(crate) mod sqlite;
pub(crate) mod surrealdb;
pub(crate) mod wardrobe_embedded;
pub(crate) mod wardrobe_remote;

pub(crate) use mongodb::MongoTarget;
pub(crate) use mysql::MySqlTarget;
pub(crate) use neo4j::Neo4jTarget;
pub(crate) use postgres::PostgresTarget;
pub(crate) use redb::RedbTarget;
pub(crate) use rocksdb::RocksdbTarget;
pub(crate) use sqlite::SqliteTarget;
pub(crate) use surrealdb::SurrealdbTarget;
pub(crate) use wardrobe_embedded::WardrobeTarget;

use crate::config::{
    DEFAULT_MYSQL_CREDENTIALS_FILE, DEFAULT_MYSQL_PASSWORD_ENV, DEFAULT_MYSQL_USER_ENV,
    DEFAULT_NEO4J_CREDENTIALS_FILE, DEFAULT_NEO4J_PASSWORD_ENV, DEFAULT_NEO4J_USER_ENV,
    DEFAULT_POSTGRES_CREDENTIALS_FILE, DEFAULT_POSTGRES_PASSWORD_ENV, DEFAULT_POSTGRES_USER_ENV,
    DEFAULT_SURREAL_CREDENTIALS_FILE, DEFAULT_SURREAL_PASSWORD_ENV, DEFAULT_SURREAL_USER_ENV,
    LibraryProfile,
};
use crate::engine::{PhaseRecorder, ProgressReporter};
use serde_json::{Value, json};
use std::fs;
use std::io::{self, Error, ErrorKind};
use ::wardrobe_embedded::{Command, CommandResult, ReadResult, UpsertResult};

pub(crate) trait BenchmarkTarget {
    fn name(&self) -> &str;
    fn provision_schema(
        &mut self,
        profile: &LibraryProfile,
        progress: &ProgressReporter,
    ) -> io::Result<()>;
    fn massive_ingestion(
        &mut self,
        profile: &LibraryProfile,
        recorder: &mut PhaseRecorder,
        progress: &ProgressReporter,
    ) -> io::Result<u64>;
    fn index_mutation(
        &mut self,
        profile: &LibraryProfile,
        recorder: &mut PhaseRecorder,
        progress: &ProgressReporter,
    ) -> io::Result<u64>;
    fn point_lookup(
        &mut self,
        profile: &LibraryProfile,
        recorder: &mut PhaseRecorder,
        progress: &ProgressReporter,
    ) -> io::Result<u64>;
    fn range_lookup(
        &mut self,
        profile: &LibraryProfile,
        recorder: &mut PhaseRecorder,
        progress: &ProgressReporter,
    ) -> io::Result<u64>;
    fn complex_traversal(
        &mut self,
        profile: &LibraryProfile,
        recorder: &mut PhaseRecorder,
        progress: &ProgressReporter,
    ) -> io::Result<u64>;
    fn delete_by_id(
        &mut self,
        profile: &LibraryProfile,
        recorder: &mut PhaseRecorder,
        progress: &ProgressReporter,
    ) -> io::Result<u64>;
    fn targeted_purge(
        &mut self,
        profile: &LibraryProfile,
        recorder: &mut PhaseRecorder,
        progress: &ProgressReporter,
    ) -> io::Result<u64>;
    fn compaction(
        &mut self,
        profile: &LibraryProfile,
        recorder: &mut PhaseRecorder,
        progress: &ProgressReporter,
    ) -> io::Result<u64>;
    fn flush(&mut self) -> io::Result<()>;
    fn storage_footprint_bytes(&mut self) -> io::Result<u64>;
    fn storage_diagnostics(&mut self) -> io::Result<Vec<String>> {
        Ok(Vec::new())
    }
}

pub(crate) trait WardrobeCommandRunner {
    fn execute(&mut self, command: Command) -> io::Result<CommandResult>;
}

#[derive(Default, Debug, PartialEq, Eq)]
pub(crate) struct ServiceCredentials {
    pub(crate) user: Option<String>,
    pub(crate) password: Option<String>,
}

pub(crate) fn read_default_mysql_credentials() -> io::Result<ServiceCredentials> {
    read_credentials_file(
        DEFAULT_MYSQL_CREDENTIALS_FILE,
        DEFAULT_MYSQL_USER_ENV,
        DEFAULT_MYSQL_PASSWORD_ENV,
    )
}

pub(crate) fn read_default_neo4j_credentials() -> io::Result<ServiceCredentials> {
    read_credentials_file(
        DEFAULT_NEO4J_CREDENTIALS_FILE,
        DEFAULT_NEO4J_USER_ENV,
        DEFAULT_NEO4J_PASSWORD_ENV,
    )
}

pub(crate) fn read_default_postgres_credentials() -> io::Result<ServiceCredentials> {
    read_credentials_file(
        DEFAULT_POSTGRES_CREDENTIALS_FILE,
        DEFAULT_POSTGRES_USER_ENV,
        DEFAULT_POSTGRES_PASSWORD_ENV,
    )
}

pub(crate) fn read_default_surreal_credentials() -> io::Result<ServiceCredentials> {
    read_credentials_file(
        DEFAULT_SURREAL_CREDENTIALS_FILE,
        DEFAULT_SURREAL_USER_ENV,
        DEFAULT_SURREAL_PASSWORD_ENV,
    )
}

pub(crate) fn read_credentials_file(
    path: &str,
    user_env: &str,
    password_env: &str,
) -> io::Result<ServiceCredentials> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return Ok(ServiceCredentials::default());
        }
        Err(error) => return Err(error),
    };
    Ok(parse_credentials(&contents, user_env, password_env))
}

pub(crate) fn parse_credentials(
    contents: &str,
    user_env: &str,
    password_env: &str,
) -> ServiceCredentials {
    let mut credentials = ServiceCredentials::default();
    for line in contents
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        if let Some((key, value)) = line.split_once('=') {
            match key.trim() {
                key if key == user_env => credentials.user = Some(value.trim().to_string()),
                key if key == password_env => credentials.password = Some(value.trim().to_string()),
                _ => {}
            }
        }
    }
    credentials
}

pub(crate) fn expect_inventory(result: CommandResult) -> io::Result<()> {
    match result {
        CommandResult::Create(::wardrobe_embedded::CreateResult::StorageInventory(_)) => Ok(()),
        other => unexpected_wardrobe_result("storage inventory", other),
    }
}

pub(crate) fn expect_pointers(result: CommandResult) -> io::Result<()> {
    match result {
        CommandResult::Upsert(UpsertResult::Pointers(_)) => Ok(()),
        other => unexpected_wardrobe_result("pointers", other),
    }
}

pub(crate) fn expect_records(result: CommandResult) -> io::Result<Vec<Value>> {
    match result {
        CommandResult::Read(ReadResult::Records(records)) => Ok(records),
        CommandResult::Read(ReadResult::Page(page)) => Ok(page.records),
        other => unexpected_wardrobe_result("records", other),
    }
}

pub(crate) fn expect_record(result: CommandResult) -> io::Result<Value> {
    match result {
        CommandResult::Read(ReadResult::Record(Some(record))) => Ok(record),
        CommandResult::Read(ReadResult::Records(records)) if records.len() == 1 => {
            Ok(records.into_iter().next().unwrap_or(Value::Null))
        }
        CommandResult::Read(ReadResult::Page(page)) if page.records.len() == 1 => {
            Ok(page.records.into_iter().next().unwrap_or(Value::Null))
        }
        other => unexpected_wardrobe_result("single record", other),
    }
}

pub(crate) fn expect_missing_record(result: CommandResult) -> io::Result<()> {
    match result {
        CommandResult::Read(ReadResult::Record(None)) => Ok(()),
        CommandResult::Read(ReadResult::Records(records)) if records.is_empty() => Ok(()),
        CommandResult::Read(ReadResult::Page(page)) if page.records.is_empty() => Ok(()),
        other => unexpected_wardrobe_result("missing record", other),
    }
}

pub(crate) fn expect_count(result: CommandResult) -> io::Result<usize> {
    match result {
        CommandResult::Count(count) => Ok(count),
        other => unexpected_wardrobe_result("count", other),
    }
}

pub(crate) fn expect_delete(result: CommandResult) -> io::Result<usize> {
    match result {
        CommandResult::Delete(result) => Ok(result.deleted),
        other => unexpected_wardrobe_result("delete result", other),
    }
}

pub(crate) fn expect_vacuumed(result: CommandResult) -> io::Result<()> {
    match result {
        CommandResult::Compact(_) => Ok(()),
        other => unexpected_wardrobe_result("vacuum report", other),
    }
}

pub(crate) fn verify_record_id(record: &Value, expected_id: &str) -> io::Result<()> {
    let actual = ["_id", "book_id", "entity_id", "id"]
        .into_iter()
        .find_map(|field| record.get(field).and_then(Value::as_str));
    match actual {
        Some(actual_id) if actual_id == expected_id => Ok(()),
        Some(actual_id) => Err(Error::new(
            ErrorKind::InvalidData,
            format!("Expected record id '{expected_id}', got '{actual_id}'"),
        )),
        None => Err(Error::new(
            ErrorKind::InvalidData,
            format!("Record is missing expected primary id '{expected_id}'"),
        )),
    }
}

pub(crate) fn verify_record_range(
    record: &Value,
    field: &str,
    low: i64,
    high: i64,
) -> io::Result<()> {
    let actual = record
        .get(field)
        .and_then(Value::as_i64)
        .ok_or_else(|| Error::new(ErrorKind::InvalidData, format!("record is missing {field}")))?;
    if actual < low || actual > high {
        return Err(Error::new(
            ErrorKind::InvalidData,
            format!(
                "record field '{field}' value {actual} fell outside requested range {low}..={high}"
            ),
        ));
    }
    Ok(())
}

pub(crate) fn verify_deleted_count(deleted: usize, expected_id: &str) -> io::Result<()> {
    if deleted == 1 {
        Ok(())
    } else {
        Err(Error::new(
            ErrorKind::InvalidData,
            format!(
                "Expected delete-by-ID for '{expected_id}' to remove 1 record, removed {deleted}"
            ),
        ))
    }
}

pub(crate) fn expect_admin(result: CommandResult) -> io::Result<()> {
    match result {
        CommandResult::Create(::wardrobe_embedded::CreateResult::Admin(_))
        | CommandResult::Alter(_)
        | CommandResult::Drop(_)
        | CommandResult::Grant(_)
        | CommandResult::Revoke(_) => Ok(()),
        other => unexpected_wardrobe_result("admin response", other),
    }
}

pub(crate) fn unexpected_wardrobe_result<T>(
    expected: &str,
    actual: CommandResult,
) -> io::Result<T> {
    Err(Error::new(
        ErrorKind::InvalidData,
        format!("Expected Wardrobe {expected}, got {actual:?}"),
    ))
}

pub(crate) fn materialized_book_value(
    book_id: String,
    isbn: String,
    title: String,
    author_id: String,
    editor_id: String,
    branch: String,
    quantity: i64,
    purge_bucket: i64,
    author_entity_id: String,
    author_display_name: String,
    author_role: String,
    author_cohort: i64,
    editor_entity_id: String,
    editor_display_name: String,
    editor_role: String,
    editor_cohort: i64,
) -> Value {
    json!({
        "_id": book_id.clone(),
        "book_id": book_id,
        "isbn": isbn,
        "title": title,
        "author_id": author_id,
        "editor_id": editor_id,
        "branch": branch,
        "quantity": quantity,
        "purge_bucket": purge_bucket,
        "author": {
            "_id": author_entity_id.clone(),
            "entity_id": author_entity_id,
            "display_name": author_display_name,
            "role": author_role,
            "cohort": author_cohort,
        },
        "editor": {
            "_id": editor_entity_id.clone(),
            "entity_id": editor_entity_id,
            "display_name": editor_display_name,
            "role": editor_role,
            "cohort": editor_cohort,
        },
    })
}

pub(crate) fn book_value(
    book_id: String,
    isbn: String,
    title: String,
    author_id: String,
    editor_id: String,
    branch: String,
    quantity: i64,
    purge_bucket: i64,
) -> Value {
    json!({
        "_id": book_id.clone(),
        "book_id": book_id,
        "isbn": isbn,
        "title": title,
        "author_id": author_id,
        "editor_id": editor_id,
        "branch": branch,
        "quantity": quantity,
        "purge_bucket": purge_bucket,
    })
}

pub(crate) fn sql_string(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}
