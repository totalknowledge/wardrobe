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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{PhaseName, PhaseRecorder, ProgressReporter};
    use serde_json::Value;
    use std::collections::HashMap;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::thread;

    fn write_message(stream: &mut TcpStream, tag: u8, body: &[u8]) {
        stream.write_all(&[tag]).expect("write backend tag");
        stream
            .write_all(&((body.len() + 4) as i32).to_be_bytes())
            .expect("write backend length");
        stream.write_all(body).expect("write backend body");
    }

    fn command_complete(stream: &mut TcpStream, tag: &str) {
        let mut body = tag.as_bytes().to_vec();
        body.push(0);
        write_message(stream, b'C', &body);
    }

    fn cstring(body: &[u8], start: usize) -> (String, usize) {
        let end = body[start..]
            .iter()
            .position(|value| *value == 0)
            .map(|offset| start + offset)
            .expect("frontend cstring terminator");
        (
            String::from_utf8(body[start..end].to_vec()).expect("frontend UTF-8"),
            end + 1,
        )
    }

    fn query_parameters(query: &str) -> Vec<i32> {
        if query.contains("$2") {
            vec![20, 20]
        } else if query.contains("$1") {
            vec![25]
        } else {
            Vec::new()
        }
    }

    fn query_columns(query: &str) -> Vec<(&'static str, i32)> {
        if query.contains("pg_database_size") {
            vec![("pg_database_size", 20)]
        } else if query.contains("SELECT id, isbn, title") {
            vec![
                ("id", 25),
                ("isbn", 25),
                ("title", 25),
                ("author_id", 25),
                ("editor_id", 25),
                ("branch", 25),
                ("quantity", 20),
                ("purge_bucket", 20),
            ]
        } else if query.contains("SELECT b.id AS book_id") {
            vec![
                ("book_id", 25),
                ("book_isbn", 25),
                ("book_title", 25),
                ("book_author_id", 25),
                ("book_editor_id", 25),
                ("book_branch", 25),
                ("book_quantity", 20),
                ("book_purge_bucket", 20),
                ("author_entity_id", 25),
                ("author_display_name", 25),
                ("author_role", 25),
                ("author_cohort", 20),
                ("editor_entity_id", 25),
                ("editor_display_name", 25),
                ("editor_role", 25),
                ("editor_cohort", 20),
            ]
        } else {
            Vec::new()
        }
    }

    fn parameter_description(stream: &mut TcpStream, query: &str) {
        let parameters = query_parameters(query);
        let mut body = Vec::new();
        body.extend_from_slice(&(parameters.len() as i16).to_be_bytes());
        for parameter in parameters {
            body.extend_from_slice(&parameter.to_be_bytes());
        }
        write_message(stream, b't', &body);
    }

    fn row_description(stream: &mut TcpStream, query: &str) {
        let columns = query_columns(query);
        if columns.is_empty() {
            write_message(stream, b'n', &[]);
            return;
        }
        let mut body = Vec::new();
        body.extend_from_slice(&(columns.len() as i16).to_be_bytes());
        for (name, oid) in columns {
            body.extend_from_slice(name.as_bytes());
            body.push(0);
            body.extend_from_slice(&0_i32.to_be_bytes());
            body.extend_from_slice(&0_i16.to_be_bytes());
            body.extend_from_slice(&oid.to_be_bytes());
            body.extend_from_slice(&(if oid == 20 { 8_i16 } else { -1_i16 }).to_be_bytes());
            body.extend_from_slice(&(-1_i32).to_be_bytes());
            body.extend_from_slice(&0_i16.to_be_bytes());
        }
        write_message(stream, b'T', &body);
    }

    fn data_row(stream: &mut TcpStream, values: &[Vec<u8>]) {
        let mut body = Vec::new();
        body.extend_from_slice(&(values.len() as i16).to_be_bytes());
        for value in values {
            body.extend_from_slice(&(value.len() as i32).to_be_bytes());
            body.extend_from_slice(value);
        }
        write_message(stream, b'D', &body);
    }

    fn point_row(point_book: &Value) -> Vec<Vec<u8>> {
        ["_id", "isbn", "title", "author_id", "editor_id", "branch"]
            .into_iter()
            .map(|field| {
                point_book[field]
                    .as_str()
                    .expect("point book string")
                    .as_bytes()
                    .to_vec()
            })
            .chain([
                (point_book["quantity"].as_i64().expect("point quantity") as i64)
                    .to_be_bytes()
                    .to_vec(),
                (point_book["purge_bucket"]
                    .as_i64()
                    .expect("point purge bucket") as i64)
                    .to_be_bytes()
                    .to_vec(),
            ])
            .collect()
    }

    fn execute_response(stream: &mut TcpStream, query: &str, point_book: &Value) {
        if query.contains("WHERE id = $1") && query.starts_with("SELECT") {
            data_row(stream, &point_row(point_book));
            command_complete(stream, "SELECT 1");
        } else if query.contains("pg_database_size") {
            data_row(stream, &[4096_i64.to_be_bytes().to_vec()]);
            command_complete(stream, "SELECT 1");
        } else if query.starts_with("SELECT") {
            command_complete(stream, "SELECT 0");
        } else if query.starts_with("DELETE") {
            command_complete(stream, "DELETE 1");
        } else {
            command_complete(stream, "OK");
        }
    }

    fn serve_postgres(mut stream: TcpStream, point_book: Value) {
        loop {
            let mut length = [0_u8; 4];
            stream
                .read_exact(&mut length)
                .expect("read PostgreSQL startup length");
            let length = i32::from_be_bytes(length) as usize;
            let mut body = vec![0_u8; length - 4];
            stream
                .read_exact(&mut body)
                .expect("read PostgreSQL startup body");
            if body == 80877103_i32.to_be_bytes() {
                stream.write_all(b"N").expect("reject PostgreSQL TLS");
            } else {
                break;
            }
        }

        write_message(&mut stream, b'R', &0_i32.to_be_bytes());
        let mut parameter = b"server_version\0".to_vec();
        parameter.extend_from_slice(b"16.0\0");
        write_message(&mut stream, b'S', &parameter);
        let mut key = 1_i32.to_be_bytes().to_vec();
        key.extend_from_slice(&2_i32.to_be_bytes());
        write_message(&mut stream, b'K', &key);
        write_message(&mut stream, b'Z', b"I");
        stream.flush().expect("flush PostgreSQL startup");

        let mut statements = HashMap::<String, String>::new();
        let mut portals = HashMap::<String, String>::new();
        while let Ok(()) = (|| -> io::Result<()> {
            let mut tag = [0_u8; 1];
            stream.read_exact(&mut tag)?;
            let mut length = [0_u8; 4];
            stream.read_exact(&mut length)?;
            let length = i32::from_be_bytes(length) as usize;
            let mut body = vec![0_u8; length - 4];
            stream.read_exact(&mut body)?;
            match tag[0] {
                b'Q' => {
                    command_complete(&mut stream, "OK");
                    write_message(&mut stream, b'Z', b"I");
                }
                b'P' => {
                    let (statement, offset) = cstring(&body, 0);
                    let (query, _) = cstring(&body, offset);
                    statements.insert(statement, query);
                    write_message(&mut stream, b'1', &[]);
                }
                b'D' => {
                    let (name, _) = cstring(&body, 1);
                    let query = if body[0] == b'S' {
                        statements.get(&name)
                    } else {
                        portals
                            .get(&name)
                            .and_then(|statement| statements.get(statement))
                    }
                    .expect("described query");
                    if body[0] == b'S' {
                        parameter_description(&mut stream, query);
                    }
                    row_description(&mut stream, query);
                }
                b'B' => {
                    let (portal, offset) = cstring(&body, 0);
                    let (statement, _) = cstring(&body, offset);
                    portals.insert(portal, statement);
                    write_message(&mut stream, b'2', &[]);
                }
                b'E' => {
                    let (portal, _) = cstring(&body, 0);
                    let query = portals
                        .get(&portal)
                        .and_then(|statement| statements.get(statement))
                        .expect("executed query");
                    execute_response(&mut stream, query, &point_book);
                }
                b'C' => write_message(&mut stream, b'3', &[]),
                b'S' => write_message(&mut stream, b'Z', b"I"),
                b'H' => {}
                b'X' => return Err(io::Error::from(ErrorKind::UnexpectedEof)),
                tag => panic!("unexpected PostgreSQL frontend message {}", tag as char),
            }
            stream.flush()?;
            Ok(())
        })() {}
    }

    fn spawn_postgres(point_book: Value) -> (u16, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind PostgreSQL fake");
        let port = listener
            .local_addr()
            .expect("PostgreSQL fake address")
            .port();
        let handle = thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept PostgreSQL client");
            serve_postgres(stream, point_book);
        });
        (port, handle)
    }

    #[test]
    fn postgres_target_runs_every_benchmark_phase_against_wire_fake() {
        let profile = LibraryProfile {
            entity_records: 2,
            book_records: 3,
            chunk_size: 2,
            traversal_queries: 1,
            point_lookups: 1,
            range_lookups: 1,
            delete_by_id_operations: 1,
            purge_buckets: 2,
        };
        let point_id = profile
            .point_lookup_book_ids()
            .into_iter()
            .next()
            .expect("point lookup ID");
        let point_index = point_id
            .strip_prefix("book_")
            .expect("book ID prefix")
            .parse::<usize>()
            .expect("book ID number");
        let (port, server) = spawn_postgres(profile.book_payload(point_index));
        let mut target = PostgresTarget::new(
            "127.0.0.1".to_string(),
            port,
            "benchmark".to_string(),
            Some("benchmark".to_string()),
            Some(DEFAULT_POSTGRES_PASSWORD_ENV.to_string()),
        )
        .expect("connect to PostgreSQL wire fake");
        let progress = ProgressReporter::new(false);

        assert_eq!(
            target.name(),
            "PostgreSQL (Relational Pointer Base Comparison)"
        );
        target
            .provision_schema(&profile, &progress)
            .expect("provision PostgreSQL schema");
        assert_eq!(
            target
                .massive_ingestion(
                    &profile,
                    &mut PhaseRecorder::new(PhaseName::MassiveIngestion),
                    &progress,
                )
                .expect("run PostgreSQL ingestion"),
            5
        );
        assert_eq!(
            target
                .index_mutation(
                    &profile,
                    &mut PhaseRecorder::new(PhaseName::IndexMutation),
                    &progress,
                )
                .expect("run PostgreSQL index mutation"),
            3
        );
        assert_eq!(
            target
                .point_lookup(
                    &profile,
                    &mut PhaseRecorder::new(PhaseName::PointLookup),
                    &progress,
                )
                .expect("run PostgreSQL point lookup"),
            1
        );
        assert_eq!(
            target
                .range_lookup(
                    &profile,
                    &mut PhaseRecorder::new(PhaseName::RangeLookup),
                    &progress,
                )
                .expect("run PostgreSQL range lookup"),
            1
        );
        assert_eq!(
            target
                .complex_traversal(
                    &profile,
                    &mut PhaseRecorder::new(PhaseName::ComplexTraversal),
                    &progress,
                )
                .expect("run PostgreSQL traversal"),
            1
        );
        assert_eq!(
            target
                .delete_by_id(
                    &profile,
                    &mut PhaseRecorder::new(PhaseName::DeleteById),
                    &progress,
                )
                .expect("run PostgreSQL deletion"),
            1
        );
        assert_eq!(
            target
                .targeted_purge(
                    &profile,
                    &mut PhaseRecorder::new(PhaseName::TargetedPurge),
                    &progress,
                )
                .expect("run PostgreSQL purge"),
            2
        );
        assert_eq!(
            target
                .compaction(
                    &profile,
                    &mut PhaseRecorder::new(PhaseName::Compaction),
                    &progress,
                )
                .expect("run PostgreSQL compaction"),
            1
        );
        target.flush().expect("flush PostgreSQL");
        assert_eq!(
            target
                .storage_footprint_bytes()
                .expect("read PostgreSQL storage"),
            4096
        );

        drop(target);
        server.join().expect("join PostgreSQL wire fake");
    }

    #[test]
    fn postgres_target_rejects_missing_custom_password_variable() {
        let variable = format!("WARDROBE_BENCH_POSTGRES_MISSING_{}", std::process::id());
        let error = PostgresTarget::new(
            "127.0.0.1".to_string(),
            1,
            "benchmark".to_string(),
            Some("benchmark".to_string()),
            Some(variable.clone()),
        )
        .err()
        .expect("custom missing password variable should fail");
        assert_eq!(error.kind(), ErrorKind::InvalidInput);
        assert!(error.to_string().contains(&variable));
        assert_eq!(pg_value("host name\\part"), "host\\ name\\\\part");
    }
}
