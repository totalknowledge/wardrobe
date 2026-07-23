use super::{
    BenchmarkTarget, book_value, materialized_book_value, read_default_mysql_credentials,
    sql_string, verify_deleted_count, verify_record_id, verify_record_range,
};
use crate::config::{
    DEFAULT_MYSQL_PASSWORD, DEFAULT_MYSQL_PASSWORD_ENV, DEFAULT_MYSQL_USER, DEFAULT_MYSQL_USER_ENV,
    LibraryProfile,
};
use crate::engine::{PhaseRecorder, ProgressReporter, report_record_progress};
use crate::utils::{chunk_ranges, to_io_error};
use mysql::prelude::Queryable;
use mysql::{OptsBuilder, Pool, PooledConn, Row};
use serde_json::Value;
use std::env;
use std::io::{self, Error, ErrorKind};

pub(crate) const MYSQL_BOOK_BY_ID_QUERY: &str = r#"
SELECT id, isbn, title, author_id, editor_id, branch, quantity, purge_bucket
FROM books
WHERE id = ?
"#;
pub(crate) struct MySqlTarget {
    connection: PooledConn,
    database: String,
}

impl MySqlTarget {
    pub(crate) fn new(
        host: String,
        port: u16,
        database: String,
        user: Option<String>,
        password_env: Option<String>,
    ) -> io::Result<Self> {
        let fallback_credentials = read_default_mysql_credentials()?;
        let resolved_user = user
            .or_else(|| env::var(DEFAULT_MYSQL_USER_ENV).ok())
            .or(fallback_credentials.user)
            .unwrap_or_else(|| DEFAULT_MYSQL_USER.to_string());
        let password = match password_env {
            Some(name) => match env::var(&name) {
                Ok(value) => Some(value),
                Err(env::VarError::NotPresent) if name == DEFAULT_MYSQL_PASSWORD_ENV => {
                    fallback_credentials
                        .password
                        .or_else(|| Some(DEFAULT_MYSQL_PASSWORD.to_string()))
                }
                Err(env::VarError::NotPresent) => {
                    return Err(Error::new(
                        ErrorKind::InvalidInput,
                        format!("MySQL password environment variable '{name}' is not set"),
                    ));
                }
                Err(env::VarError::NotUnicode(_)) => {
                    return Err(Error::new(
                        ErrorKind::InvalidInput,
                        format!("MySQL password environment variable '{name}' is not valid UTF-8"),
                    ));
                }
            },
            None => None,
        };
        let mut builder = OptsBuilder::new().ip_or_hostname(Some(host)).tcp_port(port);
        builder = builder.user(Some(resolved_user));
        if let Some(password) = password {
            builder = builder.pass(Some(password));
        }
        let pool = Pool::new(builder).map_err(to_io_error)?;
        let connection = pool.get_conn().map_err(to_io_error)?;
        Ok(Self {
            connection,
            database,
        })
    }
}

impl BenchmarkTarget for MySqlTarget {
    fn name(&self) -> &str {
        "MySQL / MariaDB (Relational Pointer Base Comparison)"
    }

    fn provision_schema(
        &mut self,
        _profile: &LibraryProfile,
        progress: &ProgressReporter,
    ) -> io::Result<()> {
        progress.log(format!(
            "{}: creating database and benchmark tables in '{}'",
            self.name(),
            self.database
        ));
        let database = mysql_identifier(&self.database);
        self.connection
            .query_drop(format!("CREATE DATABASE IF NOT EXISTS `{database}`"))
            .map_err(to_io_error)?;
        self.connection
            .query_drop(format!("USE `{database}`"))
            .map_err(to_io_error)?;
        self.connection
            .query_drop("DROP TABLE IF EXISTS books")
            .map_err(to_io_error)?;
        self.connection
            .query_drop("DROP TABLE IF EXISTS entities")
            .map_err(to_io_error)?;
        self.connection
            .query_drop(
                r#"
CREATE TABLE entities (
    id VARCHAR(64) PRIMARY KEY,
    display_name VARCHAR(255) NOT NULL,
    role VARCHAR(32) NOT NULL,
    cohort BIGINT NOT NULL
) ENGINE=InnoDB
"#,
            )
            .map_err(to_io_error)?;
        self.connection
            .query_drop(
                r#"
CREATE TABLE books (
    id VARCHAR(64) PRIMARY KEY,
    isbn VARCHAR(64) NOT NULL,
    title VARCHAR(255) NOT NULL,
    author_id VARCHAR(64) NOT NULL,
    editor_id VARCHAR(64) NOT NULL,
    branch VARCHAR(32) NOT NULL,
    quantity BIGINT NOT NULL,
    purge_bucket BIGINT NOT NULL,
    CONSTRAINT fk_books_author FOREIGN KEY (author_id) REFERENCES entities(id),
    CONSTRAINT fk_books_editor FOREIGN KEY (editor_id) REFERENCES entities(id)
) ENGINE=InnoDB
"#,
            )
            .map_err(to_io_error)?;
        self.connection
            .query_drop("CREATE INDEX idx_books_quantity ON books(quantity)")
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
            let sql = mysql_entity_insert(profile, start, end);
            recorder.measure((end - start) as u64, || {
                self.connection.query_drop(&sql).map_err(to_io_error)
            })?;
            report_record_progress(
                progress,
                &format!("{}: entities ingested", self.name()),
                end,
                profile.entity_records,
            );
        }
        for (start, end) in chunk_ranges(profile.book_records, profile.chunk_size) {
            let sql = mysql_book_insert(profile, start, end);
            recorder.measure((end - start) as u64, || {
                self.connection.query_drop(&sql).map_err(to_io_error)
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
                "CREATE INDEX idx_books_isbn ON books(isbn)",
            ),
            ("drop idx_books_isbn", "DROP INDEX idx_books_isbn ON books"),
            (
                "recreate idx_books_isbn",
                "CREATE INDEX idx_books_isbn ON books(isbn)",
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
            recorder.measure(1, || self.connection.query_drop(sql).map_err(to_io_error))?;
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
        for (index, id) in ids.iter().enumerate() {
            recorder.measure(1, || {
                let row = self
                    .connection
                    .exec_first::<Row, _, _>(MYSQL_BOOK_BY_ID_QUERY, (id.as_str(),))
                    .map_err(to_io_error)?
                    .ok_or_else(|| {
                        Error::new(
                            ErrorKind::NotFound,
                            format!("MySQL point lookup did not find book '{id}'"),
                        )
                    })?;
                let record = mysql_book_value(&row)?;
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
        for (index, (low, high)) in bounds.iter().enumerate() {
            recorder.measure(1, || {
                let sql = format!(
                    "SELECT id, isbn, title, author_id, editor_id, branch, quantity, purge_bucket FROM books WHERE quantity BETWEEN {low} AND {high} ORDER BY id"
                );
                let rows = self.connection.query_iter(sql).map_err(to_io_error)?;
                for row in rows {
                    let row = row.map_err(to_io_error)?;
                    let record = mysql_book_value(&row)?;
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
            let entity_id = sql_string(&profile.traversal_entity_id(query_index));
            recorder.measure(1, || {
                let rows = self
                    .connection
                    .query_iter(mysql_materialized_book_query(&entity_id))
                    .map_err(to_io_error)?;
                for row in rows {
                    let row = row.map_err(to_io_error)?;
                    let _record = mysql_materialized_book_value(&row)?;
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
                self.connection
                    .exec_drop("DELETE FROM books WHERE id = ?", (id.as_str(),))
                    .map_err(to_io_error)?;
                verify_deleted_count(self.connection.affected_rows() as usize, id)?;
                let remaining = self
                    .connection
                    .exec_first::<u64, _, _>(
                        "SELECT COUNT(*) FROM books WHERE id = ?",
                        (id.as_str(),),
                    )
                    .map_err(to_io_error)?
                    .unwrap_or(0);
                if remaining == 0 {
                    Ok(())
                } else {
                    Err(Error::new(
                        ErrorKind::InvalidData,
                        format!("MySQL delete-by-ID left book '{id}' behind"),
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
                .query_drop("DELETE FROM books WHERE purge_bucket = 0")
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
        progress.log(format!("{}: running OPTIMIZE TABLE books", self.name()));
        recorder.measure(1, || {
            self.connection
                .query_drop("OPTIMIZE TABLE books")
                .map_err(to_io_error)
        })?;
        Ok(1)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.connection
            .query_drop("FLUSH TABLES")
            .map_err(to_io_error)
    }

    fn storage_footprint_bytes(&mut self) -> io::Result<u64> {
        let database = sql_string(&self.database);
        self.connection
            .query_first::<u64, _>(format!(
                "SELECT COALESCE(SUM(DATA_LENGTH + INDEX_LENGTH), 0) FROM information_schema.TABLES WHERE TABLE_SCHEMA = {database}"
            ))
            .map(|value| value.unwrap_or(0))
            .map_err(to_io_error)
    }
}

pub(crate) fn mysql_materialized_book_query(entity_id: &str) -> String {
    format!(
        r#"
SELECT
    b.id AS book_id,
    b.isbn AS book_isbn,
    b.title AS book_title,
    b.author_id AS book_author_id,
    b.editor_id AS book_editor_id,
    b.branch AS book_branch,
    b.quantity AS book_quantity,
    b.purge_bucket AS book_purge_bucket,
    author.id AS author_entity_id,
    author.display_name AS author_display_name,
    author.role AS author_role,
    author.cohort AS author_cohort,
    editor.id AS editor_entity_id,
    editor.display_name AS editor_display_name,
    editor.role AS editor_role,
    editor.cohort AS editor_cohort
FROM books b
JOIN entities author ON author.id = b.author_id
JOIN entities editor ON editor.id = b.editor_id
WHERE b.author_id = {entity_id} AND b.editor_id = {entity_id}
"#
    )
}

pub(crate) fn mysql_materialized_book_value(row: &Row) -> io::Result<Value> {
    Ok(materialized_book_value(
        mysql_string(row, "book_id")?,
        mysql_string(row, "book_isbn")?,
        mysql_string(row, "book_title")?,
        mysql_string(row, "book_author_id")?,
        mysql_string(row, "book_editor_id")?,
        mysql_string(row, "book_branch")?,
        mysql_i64(row, "book_quantity")?,
        mysql_i64(row, "book_purge_bucket")?,
        mysql_string(row, "author_entity_id")?,
        mysql_string(row, "author_display_name")?,
        mysql_string(row, "author_role")?,
        mysql_i64(row, "author_cohort")?,
        mysql_string(row, "editor_entity_id")?,
        mysql_string(row, "editor_display_name")?,
        mysql_string(row, "editor_role")?,
        mysql_i64(row, "editor_cohort")?,
    ))
}

pub(crate) fn mysql_book_value(row: &Row) -> io::Result<Value> {
    Ok(book_value(
        mysql_string(row, "id")?,
        mysql_string(row, "isbn")?,
        mysql_string(row, "title")?,
        mysql_string(row, "author_id")?,
        mysql_string(row, "editor_id")?,
        mysql_string(row, "branch")?,
        mysql_i64(row, "quantity")?,
        mysql_i64(row, "purge_bucket")?,
    ))
}

pub(crate) fn mysql_string(row: &Row, column: &str) -> io::Result<String> {
    row.get(column).ok_or_else(|| missing_mysql_column(column))
}

pub(crate) fn mysql_i64(row: &Row, column: &str) -> io::Result<i64> {
    row.get(column).ok_or_else(|| missing_mysql_column(column))
}

pub(crate) fn missing_mysql_column(column: &str) -> io::Error {
    Error::new(
        ErrorKind::InvalidData,
        format!("MySQL result row is missing column '{column}'"),
    )
}

pub(crate) fn mysql_entity_insert(profile: &LibraryProfile, start: usize, end: usize) -> String {
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
        "INSERT INTO entities (id, display_name, role, cohort) VALUES\n{values}\nON DUPLICATE KEY UPDATE display_name = VALUES(display_name), role = VALUES(role), cohort = VALUES(cohort)"
    )
}

pub(crate) fn mysql_book_insert(profile: &LibraryProfile, start: usize, end: usize) -> String {
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
        "INSERT INTO books (id, isbn, title, author_id, editor_id, branch, quantity, purge_bucket) VALUES\n{values}\nON DUPLICATE KEY UPDATE isbn = VALUES(isbn), title = VALUES(title), author_id = VALUES(author_id), editor_id = VALUES(editor_id), branch = VALUES(branch), quantity = VALUES(quantity), purge_bucket = VALUES(purge_bucket)"
    )
}

pub(crate) fn mysql_identifier(value: &str) -> String {
    value.replace('`', "``")
}
