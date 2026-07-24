use super::{
    BenchmarkTarget, book_value, materialized_book_value, read_default_postgres_credentials,
    sql_string, verify_deleted_count, verify_record_id, verify_record_range,
};
use crate::config::{
    DEFAULT_POSTGRES_PASSWORD, DEFAULT_POSTGRES_PASSWORD_ENV, DEFAULT_POSTGRES_USER,
    DEFAULT_POSTGRES_USER_ENV, LibraryProfile,
};
use crate::engine::{PhaseRecorder, ProgressReporter, report_record_progress};
use crate::utils::{chunk_ranges, to_io_error};
use postgres::{Client, NoTls, Row};
use std::env;
use std::io::{self, Error, ErrorKind};

pub(crate) struct PostgresTarget {
    connection: Client,
}

impl PostgresTarget {
    pub(crate) fn new(
        host: String,
        port: u16,
        database: String,
        user: Option<String>,
        password_env: Option<String>,
    ) -> io::Result<Self> {
        let fallback = read_default_postgres_credentials()?;
        let user = user
            .or_else(|| env::var(DEFAULT_POSTGRES_USER_ENV).ok())
            .or(fallback.user)
            .unwrap_or_else(|| DEFAULT_POSTGRES_USER.to_string());
        let password = match password_env {
            Some(name) => match env::var(&name) {
                Ok(value) => value,
                Err(env::VarError::NotPresent) if name == DEFAULT_POSTGRES_PASSWORD_ENV => fallback
                    .password
                    .unwrap_or_else(|| DEFAULT_POSTGRES_PASSWORD.to_string()),
                Err(env::VarError::NotPresent) => {
                    return Err(Error::new(
                        ErrorKind::InvalidInput,
                        format!("PostgreSQL password environment variable '{name}' is not set"),
                    ));
                }
                Err(env::VarError::NotUnicode(_)) => {
                    return Err(Error::new(
                        ErrorKind::InvalidInput,
                        format!(
                            "PostgreSQL password environment variable '{name}' is not valid UTF-8"
                        ),
                    ));
                }
            },
            None => String::new(),
        };
        let connection = format!(
            "host={} port={} dbname={} user={} password={}",
            pg_value(&host),
            port,
            pg_value(&database),
            pg_value(&user),
            pg_value(&password)
        );
        Client::connect(&connection, NoTls)
            .map(|connection| Self { connection })
            .map_err(to_io_error)
    }
}

impl BenchmarkTarget for PostgresTarget {
    fn name(&self) -> &str {
        "PostgreSQL (Relational Pointer Base Comparison)"
    }

    fn provision_schema(
        &mut self,
        _: &LibraryProfile,
        progress: &ProgressReporter,
    ) -> io::Result<()> {
        progress.log(format!("{}: creating benchmark tables", self.name()));
        self.connection.batch_execute("DROP TABLE IF EXISTS books; DROP TABLE IF EXISTS entities; CREATE TABLE entities (id TEXT PRIMARY KEY, display_name TEXT NOT NULL, role TEXT NOT NULL, cohort BIGINT NOT NULL); CREATE TABLE books (id TEXT PRIMARY KEY, isbn TEXT NOT NULL, title TEXT NOT NULL, author_id TEXT NOT NULL REFERENCES entities(id), editor_id TEXT NOT NULL REFERENCES entities(id), branch TEXT NOT NULL, quantity BIGINT NOT NULL, purge_bucket BIGINT NOT NULL); CREATE INDEX idx_books_quantity ON books(quantity);").map_err(to_io_error)
    }

    fn massive_ingestion(
        &mut self,
        profile: &LibraryProfile,
        recorder: &mut PhaseRecorder,
        progress: &ProgressReporter,
    ) -> io::Result<u64> {
        for (start, end) in chunk_ranges(profile.entity_records, profile.chunk_size) {
            recorder.measure((end - start) as u64, || {
                self.connection
                    .batch_execute(&postgres_entity_insert(profile, start, end))
                    .map_err(to_io_error)
            })?;
            report_record_progress(
                progress,
                &format!("{}: entities ingested", self.name()),
                end,
                profile.entity_records,
            );
        }
        for (start, end) in chunk_ranges(profile.book_records, profile.chunk_size) {
            recorder.measure((end - start) as u64, || {
                self.connection
                    .batch_execute(&postgres_book_insert(profile, start, end))
                    .map_err(to_io_error)
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
        _: &LibraryProfile,
        recorder: &mut PhaseRecorder,
        progress: &ProgressReporter,
    ) -> io::Result<u64> {
        for (index, sql) in [
            "CREATE INDEX idx_books_isbn ON books(isbn)",
            "DROP INDEX idx_books_isbn",
            "CREATE INDEX idx_books_isbn ON books(isbn)",
        ]
        .iter()
        .enumerate()
        {
            progress.log(format!(
                "{}: index mutation step {}/3",
                self.name(),
                index + 1
            ));
            recorder.measure(1, || {
                self.connection.batch_execute(sql).map_err(to_io_error)
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
        for (index, id) in ids.iter().enumerate() {
            recorder.measure(1, || {
                let row = self.connection.query_opt("SELECT id, isbn, title, author_id, editor_id, branch, quantity, purge_bucket FROM books WHERE id = $1", &[&id]).map_err(to_io_error)?.ok_or_else(|| Error::new(ErrorKind::NotFound, format!("PostgreSQL point lookup did not find book '{id}'")))?;
                verify_record_id(&postgres_book_value(&row)?, id)
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
        for (index, (low, high)) in bounds.iter().enumerate() {
            recorder.measure(1, || {
                for row in self.connection.query("SELECT id, isbn, title, author_id, editor_id, branch, quantity, purge_bucket FROM books WHERE quantity BETWEEN $1 AND $2 ORDER BY id", &[low, high]).map_err(to_io_error)? { verify_record_range(&postgres_book_value(&row)?, "quantity", *low, *high)?; }
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
            let id = profile.traversal_entity_id(query_index);
            recorder.measure(1, || {
                for row in self
                    .connection
                    .query(POSTGRES_MATERIALIZED_BOOK_QUERY, &[&id])
                    .map_err(to_io_error)?
                {
                    let _ = postgres_materialized_book_value(&row)?;
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
        for (index, id) in ids.iter().enumerate() {
            recorder.measure(1, || {
                let deleted = self
                    .connection
                    .execute("DELETE FROM books WHERE id = $1", &[&id])
                    .map_err(to_io_error)? as usize;
                verify_deleted_count(deleted, id)
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
        _: &ProgressReporter,
    ) -> io::Result<u64> {
        let operations = profile.expected_purge_count() as u64;
        recorder.measure(operations.max(1), || {
            self.connection
                .execute("DELETE FROM books WHERE purge_bucket = 0", &[])
                .map(|_| ())
                .map_err(to_io_error)
        })?;
        Ok(operations.max(1))
    }

    fn compaction(
        &mut self,
        _: &LibraryProfile,
        recorder: &mut PhaseRecorder,
        _: &ProgressReporter,
    ) -> io::Result<u64> {
        recorder.measure(1, || {
            self.connection
                .batch_execute("VACUUM ANALYZE books")
                .map_err(to_io_error)
        })?;
        Ok(1)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.connection
            .simple_query("CHECKPOINT")
            .map(|_| ())
            .map_err(to_io_error)
    }
    fn storage_footprint_bytes(&mut self) -> io::Result<u64> {
        self.connection
            .query_one("SELECT pg_database_size(current_database())", &[])
            .map(|row| row.get::<_, i64>(0) as u64)
            .map_err(to_io_error)
    }
}

const POSTGRES_MATERIALIZED_BOOK_QUERY: &str = "SELECT b.id AS book_id, b.isbn AS book_isbn, b.title AS book_title, b.author_id AS book_author_id, b.editor_id AS book_editor_id, b.branch AS book_branch, b.quantity AS book_quantity, b.purge_bucket AS book_purge_bucket, author.id AS author_entity_id, author.display_name AS author_display_name, author.role AS author_role, author.cohort AS author_cohort, editor.id AS editor_entity_id, editor.display_name AS editor_display_name, editor.role AS editor_role, editor.cohort AS editor_cohort FROM books b JOIN entities author ON author.id = b.author_id JOIN entities editor ON editor.id = b.editor_id WHERE b.author_id = $1 AND b.editor_id = $1";

fn postgres_book_value(row: &Row) -> io::Result<serde_json::Value> {
    Ok(book_value(
        row_text(row, "id")?,
        row_text(row, "isbn")?,
        row_text(row, "title")?,
        row_text(row, "author_id")?,
        row_text(row, "editor_id")?,
        row_text(row, "branch")?,
        row.get("quantity"),
        row.get("purge_bucket"),
    ))
}
fn postgres_materialized_book_value(row: &Row) -> io::Result<serde_json::Value> {
    Ok(materialized_book_value(
        row_text(row, "book_id")?,
        row_text(row, "book_isbn")?,
        row_text(row, "book_title")?,
        row_text(row, "book_author_id")?,
        row_text(row, "book_editor_id")?,
        row_text(row, "book_branch")?,
        row.get("book_quantity"),
        row.get("book_purge_bucket"),
        row_text(row, "author_entity_id")?,
        row_text(row, "author_display_name")?,
        row_text(row, "author_role")?,
        row.get("author_cohort"),
        row_text(row, "editor_entity_id")?,
        row_text(row, "editor_display_name")?,
        row_text(row, "editor_role")?,
        row.get("editor_cohort"),
    ))
}
fn row_text(row: &Row, name: &str) -> io::Result<String> {
    row.try_get(name).map_err(to_io_error)
}
fn pg_value(value: &str) -> String {
    value.replace('\\', "\\\\").replace(' ', "\\ ")
}
fn postgres_entity_insert(profile: &LibraryProfile, start: usize, end: usize) -> String {
    let values = (start..end)
        .map(|index| {
            format!(
                "({}, {}, {}, {})",
                sql_string(&profile.entity_id(index)),
                sql_string(&format!("Library Entity {index:08}")),
                sql_string(if index % 2 == 0 { "author" } else { "editor" }),
                index % 97
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "INSERT INTO entities (id, display_name, role, cohort) VALUES {values} ON CONFLICT (id) DO UPDATE SET display_name = EXCLUDED.display_name, role = EXCLUDED.role, cohort = EXCLUDED.cohort"
    )
}
fn postgres_book_insert(profile: &LibraryProfile, start: usize, end: usize) -> String {
    let values = (start..end)
        .map(|index| {
            let value = profile.book_payload(index);
            format!(
                "({}, {}, {}, {}, {}, {}, {}, {})",
                sql_string(value["_id"].as_str().unwrap_or_default()),
                sql_string(value["isbn"].as_str().unwrap_or_default()),
                sql_string(value["title"].as_str().unwrap_or_default()),
                sql_string(value["author_id"].as_str().unwrap_or_default()),
                sql_string(value["editor_id"].as_str().unwrap_or_default()),
                sql_string(value["branch"].as_str().unwrap_or_default()),
                value["quantity"].as_u64().unwrap_or_default(),
                value["purge_bucket"].as_u64().unwrap_or_default()
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "INSERT INTO books (id, isbn, title, author_id, editor_id, branch, quantity, purge_bucket) VALUES {values} ON CONFLICT (id) DO UPDATE SET isbn = EXCLUDED.isbn, title = EXCLUDED.title, author_id = EXCLUDED.author_id, editor_id = EXCLUDED.editor_id, branch = EXCLUDED.branch, quantity = EXCLUDED.quantity, purge_bucket = EXCLUDED.purge_bucket"
    )
}
