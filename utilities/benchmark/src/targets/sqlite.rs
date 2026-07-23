use super::{
    BenchmarkTarget, book_value, materialized_book_value, sql_string, verify_deleted_count,
    verify_record_id, verify_record_range,
};
use crate::config::LibraryProfile;
use crate::engine::{PhaseRecorder, ProgressReporter, report_record_progress};
use crate::utils::{chunk_ranges, file_size_or_zero, sync_file_if_exists, to_io_error};
use rusqlite::{Connection, OptionalExtension};
use serde_json::Value;
use std::fs;
use std::io::{self, Error, ErrorKind};
use std::path::{Path, PathBuf};

pub(crate) const SQLITE_MATERIALIZED_BOOK_QUERY: &str = r#"
SELECT
    b.id,
    b.isbn,
    b.title,
    b.author_id,
    b.editor_id,
    b.branch,
    b.quantity,
    b.purge_bucket,
    author.id,
    author.display_name,
    author.role,
    author.cohort,
    editor.id,
    editor.display_name,
    editor.role,
    editor.cohort
FROM books b
JOIN entities author ON author.id = b.author_id
JOIN entities editor ON editor.id = b.editor_id
WHERE b.author_id = ?1 AND b.editor_id = ?1;
"#;
pub(crate) const SQLITE_BOOK_BY_ID_QUERY: &str = r#"
SELECT id, isbn, title, author_id, editor_id, branch, quantity, purge_bucket
FROM books
WHERE id = ?1;
"#;
pub(crate) struct SqliteTarget {
    connection: Connection,
    path: PathBuf,
}

impl SqliteTarget {
    pub(crate) fn new(path: PathBuf) -> io::Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let connection = Connection::open(&path).map_err(to_io_error)?;
        connection
            .execute_batch("PRAGMA foreign_keys = ON;")
            .map_err(to_io_error)?;
        Ok(Self { connection, path })
    }
}

impl BenchmarkTarget for SqliteTarget {
    fn name(&self) -> &str {
        "SQLite (Local WAL File Mode)"
    }

    fn provision_schema(
        &mut self,
        _profile: &LibraryProfile,
        progress: &ProgressReporter,
    ) -> io::Result<()> {
        progress.log(format!("{}: executing schema setup SQL", self.name()));
        self.connection
            .execute_batch(
                r#"
PRAGMA journal_mode=WAL;
PRAGMA synchronous=FULL;
DROP TABLE IF EXISTS books;
DROP TABLE IF EXISTS entities;
CREATE TABLE entities (
    id TEXT PRIMARY KEY,
    display_name TEXT NOT NULL,
    role TEXT NOT NULL,
    cohort INTEGER NOT NULL
);
CREATE TABLE books (
    id TEXT PRIMARY KEY,
    isbn TEXT NOT NULL,
    title TEXT NOT NULL,
    author_id TEXT NOT NULL,
    editor_id TEXT NOT NULL,
    branch TEXT NOT NULL,
    quantity INTEGER NOT NULL,
    purge_bucket INTEGER NOT NULL,
    FOREIGN KEY(author_id) REFERENCES entities(id),
    FOREIGN KEY(editor_id) REFERENCES entities(id)
);
CREATE INDEX IF NOT EXISTS idx_books_quantity ON books(quantity);
"#,
            )
            .map_err(to_io_error)?;
        self.flush()
    }

    fn massive_ingestion(
        &mut self,
        profile: &LibraryProfile,
        recorder: &mut PhaseRecorder,
        progress: &ProgressReporter,
    ) -> io::Result<u64> {
        for (start, end) in chunk_ranges(profile.entity_records, profile.chunk_size) {
            let sql = sqlite_entity_insert(profile, start, end);
            recorder.measure((end - start) as u64, || {
                self.connection.execute_batch(&sql).map_err(to_io_error)
            })?;
            report_record_progress(
                progress,
                &format!("{}: entities ingested", self.name()),
                end,
                profile.entity_records,
            );
        }
        for (start, end) in chunk_ranges(profile.book_records, profile.chunk_size) {
            let sql = sqlite_book_insert(profile, start, end);
            recorder.measure((end - start) as u64, || {
                self.connection.execute_batch(&sql).map_err(to_io_error)
            })?;
            report_record_progress(
                progress,
                &format!("{}: books ingested", self.name()),
                end,
                profile.book_records,
            );
        }
        Ok((profile.entity_records + profile.book_records) as u64)
    }

    fn index_mutation(
        &mut self,
        _profile: &LibraryProfile,
        recorder: &mut PhaseRecorder,
        progress: &ProgressReporter,
    ) -> io::Result<u64> {
        for (index, (label, sql)) in [
            (
                "create idx_books_isbn",
                "CREATE INDEX IF NOT EXISTS idx_books_isbn ON books(isbn);",
            ),
            (
                "drop idx_books_isbn",
                "DROP INDEX IF EXISTS idx_books_isbn;",
            ),
            (
                "recreate idx_books_isbn",
                "CREATE INDEX IF NOT EXISTS idx_books_isbn ON books(isbn);",
            ),
        ]
        .into_iter()
        .enumerate()
        {
            progress.log(format!(
                "{}: index mutation step {}/3: {}",
                self.name(),
                index + 1,
                label
            ));
            recorder.measure(1, || {
                self.connection.execute_batch(sql).map_err(to_io_error)
            })?;
        }
        Ok(3)
    }

    fn point_lookup(
        &mut self,
        profile: &LibraryProfile,
        recorder: &mut PhaseRecorder,
        progress: &ProgressReporter,
    ) -> io::Result<u64> {
        let ids = profile.point_lookup_book_ids();
        progress.log(format!(
            "{}: reading {} book records by primary id",
            self.name(),
            ids.len()
        ));
        let mut statement = self
            .connection
            .prepare(SQLITE_BOOK_BY_ID_QUERY)
            .map_err(to_io_error)?;
        for (index, id) in ids.iter().enumerate() {
            recorder.measure(1, || {
                let record = statement
                    .query_row([id.as_str()], sqlite_book_value)
                    .optional()
                    .map_err(to_io_error)?
                    .ok_or_else(|| {
                        Error::new(
                            ErrorKind::NotFound,
                            format!("SQLite point lookup did not find book '{id}'"),
                        )
                    })?;
                verify_record_id(&record, id)
            })?;
            report_record_progress(
                progress,
                &format!("{}: point lookups completed", self.name()),
                index + 1,
                ids.len(),
            );
        }
        Ok(ids.len() as u64)
    }

    fn range_lookup(
        &mut self,
        profile: &LibraryProfile,
        recorder: &mut PhaseRecorder,
        progress: &ProgressReporter,
    ) -> io::Result<u64> {
        let bounds = profile.range_lookup_bounds();
        progress.log(format!(
            "{}: reading {} book records with numeric quantity ranges",
            self.name(),
            bounds.len()
        ));
        let mut statement = self
            .connection
            .prepare(
                "SELECT id, isbn, title, author_id, editor_id, branch, quantity, purge_bucket FROM books WHERE quantity BETWEEN ?1 AND ?2 ORDER BY id;",
            )
            .map_err(to_io_error)?;
        for (index, (low, high)) in bounds.iter().enumerate() {
            recorder.measure(1, || {
                let rows = statement
                    .query_map([low, high], sqlite_book_value)
                    .map_err(to_io_error)?;
                for row in rows {
                    let record = row.map_err(to_io_error)?;
                    verify_record_range(&record, "quantity", *low, *high)?;
                }
                Ok(())
            })?;
            report_record_progress(
                progress,
                &format!("{}: range lookups completed", self.name()),
                index + 1,
                bounds.len(),
            );
        }
        Ok(bounds.len() as u64)
    }

    fn complex_traversal(
        &mut self,
        profile: &LibraryProfile,
        recorder: &mut PhaseRecorder,
        progress: &ProgressReporter,
    ) -> io::Result<u64> {
        for query_index in 0..profile.traversal_queries {
            let entity_id = profile.traversal_entity_id(query_index);
            recorder.measure(1, || {
                let mut statement = self
                    .connection
                    .prepare(SQLITE_MATERIALIZED_BOOK_QUERY)
                    .map_err(to_io_error)?;
                let rows = statement
                    .query_map([entity_id.as_str()], sqlite_materialized_book_value)
                    .map_err(to_io_error)?;
                for row in rows {
                    let _record = row.map_err(to_io_error)?;
                }
                Ok(())
            })?;
            report_record_progress(
                progress,
                &format!("{}: traversal queries completed", self.name()),
                query_index + 1,
                profile.traversal_queries,
            );
        }
        Ok(profile.traversal_queries as u64)
    }

    fn delete_by_id(
        &mut self,
        profile: &LibraryProfile,
        recorder: &mut PhaseRecorder,
        progress: &ProgressReporter,
    ) -> io::Result<u64> {
        let ids = profile.delete_by_id_book_ids();
        progress.log(format!(
            "{}: deleting {} book records by primary id",
            self.name(),
            ids.len()
        ));
        for (index, id) in ids.iter().enumerate() {
            recorder.measure(1, || {
                verify_deleted_count(
                    self.connection
                        .execute("DELETE FROM books WHERE id = ?1;", [id.as_str()])
                        .map_err(to_io_error)?,
                    id,
                )?;
                let remaining = self
                    .connection
                    .query_row(
                        "SELECT COUNT(*) FROM books WHERE id = ?1;",
                        [id.as_str()],
                        |row| row.get::<_, i64>(0),
                    )
                    .map_err(to_io_error)?;
                if remaining == 0 {
                    Ok(())
                } else {
                    Err(Error::new(
                        ErrorKind::InvalidData,
                        format!("SQLite delete-by-ID left book '{id}' behind"),
                    ))
                }
            })?;
            report_record_progress(
                progress,
                &format!("{}: delete-by-ID operations completed", self.name()),
                index + 1,
                ids.len(),
            );
        }
        Ok(ids.len() as u64)
    }

    fn targeted_purge(
        &mut self,
        profile: &LibraryProfile,
        recorder: &mut PhaseRecorder,
        progress: &ProgressReporter,
    ) -> io::Result<u64> {
        let operations = profile.expected_purge_count() as u64;
        progress.log(format!(
            "{}: deleting about {} book records where purge_bucket = 0",
            self.name(),
            operations
        ));
        recorder.measure(operations.max(1), || {
            self.connection
                .execute("DELETE FROM books WHERE purge_bucket = 0;", [])
                .map(|_| ())
                .map_err(to_io_error)
        })?;
        Ok(operations.max(1))
    }

    fn compaction(
        &mut self,
        _profile: &LibraryProfile,
        recorder: &mut PhaseRecorder,
        progress: &ProgressReporter,
    ) -> io::Result<u64> {
        progress.log(format!("{}: running VACUUM", self.name()));
        recorder.measure(1, || {
            self.connection
                .execute_batch("VACUUM;")
                .map_err(to_io_error)
        })?;
        Ok(1)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.connection
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
            .map_err(to_io_error)?;
        sync_file_if_exists(&self.path)?;
        sync_file_if_exists(&sqlite_sidecar(&self.path, "-wal"))?;
        sync_file_if_exists(&sqlite_sidecar(&self.path, "-shm"))?;
        Ok(())
    }

    fn storage_footprint_bytes(&mut self) -> io::Result<u64> {
        Ok(file_size_or_zero(&self.path)?
            + file_size_or_zero(sqlite_sidecar(&self.path, "-wal"))?
            + file_size_or_zero(sqlite_sidecar(&self.path, "-shm"))?)
    }
}

pub(crate) fn sqlite_book_value(row: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    Ok(book_value(
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
    ))
}

pub(crate) fn sqlite_materialized_book_value(row: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    Ok(materialized_book_value(
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
        row.get(9)?,
        row.get(10)?,
        row.get(11)?,
        row.get(12)?,
        row.get(13)?,
        row.get(14)?,
        row.get(15)?,
    ))
}

pub(crate) fn sqlite_entity_insert(profile: &LibraryProfile, start: usize, end: usize) -> String {
    let values = (start..end)
        .map(|index| {
            let id = profile.entity_id(index);
            format!(
                "({}, {}, {}, {})",
                sql_string(&id),
                sql_string(&format!("Library Entity {index:08}")),
                sql_string(if index % 2 == 0 { "author" } else { "editor" }),
                index % 97,
            )
        })
        .collect::<Vec<_>>()
        .join(",\n");
    format!(
        "BEGIN IMMEDIATE;\nINSERT OR REPLACE INTO entities (id, display_name, role, cohort) VALUES\n{values};\nCOMMIT;"
    )
}

pub(crate) fn sqlite_book_insert(profile: &LibraryProfile, start: usize, end: usize) -> String {
    let values = (start..end)
        .map(|index| {
            let payload = profile.book_payload(index);
            format!(
                "({}, {}, {}, {}, {}, {}, {}, {})",
                sql_string(payload["_id"].as_str().unwrap_or_default()),
                sql_string(payload["isbn"].as_str().unwrap_or_default()),
                sql_string(payload["title"].as_str().unwrap_or_default()),
                sql_string(payload["author_id"].as_str().unwrap_or_default()),
                sql_string(payload["editor_id"].as_str().unwrap_or_default()),
                sql_string(payload["branch"].as_str().unwrap_or_default()),
                payload["quantity"].as_u64().unwrap_or_default(),
                payload["purge_bucket"].as_u64().unwrap_or_default(),
            )
        })
        .collect::<Vec<_>>()
        .join(",\n");
    format!(
        "BEGIN IMMEDIATE;\nINSERT OR REPLACE INTO books (id, isbn, title, author_id, editor_id, branch, quantity, purge_bucket) VALUES\n{values};\nCOMMIT;"
    )
}

pub(crate) fn sqlite_sidecar(path: &Path, suffix: &str) -> PathBuf {
    PathBuf::from(format!("{}{}", path.display(), suffix))
}
