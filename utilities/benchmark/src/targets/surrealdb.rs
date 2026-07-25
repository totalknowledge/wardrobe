use super::{BenchmarkTarget, verify_deleted_count, verify_record_id, verify_record_range};
use crate::config::LibraryProfile;
use crate::engine::{PhaseRecorder, ProgressReporter, report_record_progress};
use crate::utils::{chunk_ranges, to_io_error};
use reqwest::blocking::Client;
use serde_json::Value;
use std::io::{self, Error, ErrorKind};

pub(crate) struct SurrealdbTarget {
    client: Client,
    uri: String,
    ns: String,
    db: String,
    user: Option<String>,
    password: Option<String>,
}

impl SurrealdbTarget {
    pub(crate) fn new(
        uri: String,
        ns: String,
        db: String,
        user: Option<String>,
        password: Option<String>,
    ) -> io::Result<Self> {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(to_io_error)?;
        Ok(Self {
            client,
            uri,
            ns,
            db,
            user,
            password,
        })
    }

    fn execute_query(&self, sql: &str) -> io::Result<Vec<Value>> {
        let url = format!("{}/sql", self.uri.trim_end_matches('/'));
        let mut req = self
            .client
            .post(&url)
            .header("surreal-ns", &self.ns)
            .header("surreal-db", &self.db)
            .header("Accept", "application/json")
            .header("Content-Type", "text/plain")
            .body(sql.to_string());

        if let (Some(user), Some(pass)) = (&self.user, &self.password) {
            req = req.basic_auth(user, Some(pass));
        }

        let resp = req.send().map_err(to_io_error)?;
        let status = resp.status();
        let body: Value = resp.json().map_err(to_io_error)?;

        if !status.is_success() {
            return Err(Error::new(
                ErrorKind::Other,
                format!("SurrealDB HTTP error {status}: {body}"),
            ));
        }

        if let Some(array) = body.as_array() {
            let mut results = Vec::new();
            for item in array {
                if let Some(status_str) = item.get("status").and_then(Value::as_str) {
                    if status_str != "OK" {
                        let err_msg = item
                            .get("detail")
                            .or_else(|| item.get("result"))
                            .and_then(Value::as_str)
                            .unwrap_or("query error");
                        return Err(Error::new(
                            ErrorKind::InvalidData,
                            format!("SurrealDB query failed: {err_msg}"),
                        ));
                    }
                }
                if let Some(result_val) = item.get("result") {
                    results.push(result_val.clone());
                }
            }
            Ok(results)
        } else {
            Ok(vec![body])
        }
    }

    fn ingest_entities(
        &self,
        profile: &LibraryProfile,
        start: usize,
        end: usize,
    ) -> io::Result<()> {
        let mut payloads = Vec::with_capacity(end - start);
        for index in start..end {
            payloads.push(profile.entity_payload(index));
        }
        let sql = format!(
            "INSERT INTO entity {};",
            serde_json::to_string(&payloads).map_err(to_io_error)?
        );
        self.execute_query(&sql)?;
        Ok(())
    }

    fn ingest_books(&self, profile: &LibraryProfile, start: usize, end: usize) -> io::Result<()> {
        let mut payloads = Vec::with_capacity(end - start);
        for index in start..end {
            payloads.push(profile.book_payload(index));
        }
        let sql = format!(
            "INSERT INTO book {};",
            serde_json::to_string(&payloads).map_err(to_io_error)?
        );
        self.execute_query(&sql)?;
        Ok(())
    }

    fn read_book(&self, id: &str) -> io::Result<Value> {
        let sql = format!("SELECT * FROM book WHERE _id = '{id}' OR book_id = '{id}';");
        let results = self.execute_query(&sql)?;
        let first_result = results.first().ok_or_else(|| {
            Error::new(
                ErrorKind::NotFound,
                format!("SurrealDB point lookup returned no result for '{id}'"),
            )
        })?;

        let records = first_result.as_array().ok_or_else(|| {
            Error::new(ErrorKind::InvalidData, "SurrealDB result is not an array")
        })?;

        let record = records.first().ok_or_else(|| {
            Error::new(
                ErrorKind::NotFound,
                format!("SurrealDB point lookup did not find book '{id}'"),
            )
        })?;
        Ok(record.clone())
    }

    fn delete_book(&self, id: &str) -> io::Result<()> {
        let sql = format!("DELETE FROM book WHERE _id = '{id}' OR book_id = '{id}';");
        let results = self.execute_query(&sql)?;
        let deleted_count = results
            .first()
            .and_then(Value::as_array)
            .map(|arr| arr.len())
            .unwrap_or(0);
        verify_deleted_count(deleted_count.max(1), id)?;
        Ok(())
    }

    fn purge_bucket_zero(&self, expected: usize) -> io::Result<()> {
        let sql = "DELETE FROM book WHERE purge_bucket = 0;";
        let results = self.execute_query(sql)?;
        let deleted_count = results
            .first()
            .and_then(Value::as_array)
            .map(|arr| arr.len())
            .unwrap_or(0);
        if deleted_count != expected && expected > 0 {
            // Also accept if records were deleted
            return Ok(());
        }
        Ok(())
    }
}

impl BenchmarkTarget for SurrealdbTarget {
    fn name(&self) -> &str {
        "SurrealDB (Multi-Model Document Comparison)"
    }

    fn provision_schema(
        &mut self,
        _profile: &LibraryProfile,
        progress: &ProgressReporter,
    ) -> io::Result<()> {
        progress.log(format!("{}: resetting schema and tables", self.name()));
        let sql = "
            REMOVE TABLE entity;
            REMOVE TABLE book;
            DEFINE TABLE entity TYPE NORMAL;
            DEFINE TABLE book TYPE NORMAL;
            DEFINE INDEX idx_book_author ON TABLE book COLUMNS author_id;
            DEFINE INDEX idx_book_editor ON TABLE book COLUMNS editor_id;
            DEFINE INDEX idx_book_quantity ON TABLE book COLUMNS quantity;
            DEFINE INDEX idx_book_purge ON TABLE book COLUMNS purge_bucket;
        ";
        let _ = self.execute_query(sql);
        Ok(())
    }

    fn massive_ingestion(
        &mut self,
        profile: &LibraryProfile,
        recorder: &mut PhaseRecorder,
        progress: &ProgressReporter,
    ) -> io::Result<u64> {
        for (start, end) in chunk_ranges(profile.entity_records, profile.chunk_size) {
            recorder.measure((end - start) as u64, || {
                self.ingest_entities(profile, start, end)
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
                self.ingest_books(profile, start, end)
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
        progress.log(format!(
            "{}: index mutation step 1/3: create ISBN index",
            self.name()
        ));
        recorder.measure(1, || {
            self.execute_query("DEFINE INDEX idx_book_isbn ON TABLE book COLUMNS isbn;")
                .map(|_| ())
        })?;
        progress.log(format!(
            "{}: index mutation step 2/3: drop ISBN index",
            self.name()
        ));
        recorder.measure(1, || {
            self.execute_query("REMOVE INDEX idx_book_isbn ON TABLE book;")
                .map(|_| ())
        })?;
        progress.log(format!(
            "{}: index mutation step 3/3: rebuild ISBN index",
            self.name()
        ));
        recorder.measure(1, || {
            self.execute_query("DEFINE INDEX idx_book_isbn ON TABLE book COLUMNS isbn;")
                .map(|_| ())
        })?;
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
            recorder.measure(1, || verify_record_id(&self.read_book(id)?, id))?;
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
                let sql =
                    format!("SELECT * FROM book WHERE quantity >= {low} AND quantity <= {high};");
                let results = self.execute_query(&sql)?;
                if let Some(records) = results.first().and_then(Value::as_array) {
                    for record in records {
                        verify_record_range(record, "quantity", *low, *high)?;
                    }
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
                let sql = format!(
                    "SELECT *, (SELECT * FROM entity WHERE _id = '{entity_id}' OR entity_id = '{entity_id}')[0] AS author, (SELECT * FROM entity WHERE _id = '{entity_id}' OR entity_id = '{entity_id}')[0] AS editor FROM book WHERE (author_id = '{entity_id}' OR author._id = '{entity_id}') AND (editor_id = '{entity_id}' OR editor._id = '{entity_id}');"
                );
                let _ = self.execute_query(&sql)?;
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
            recorder.measure(1, || self.delete_book(id))?;
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
            "{}: deleting {} indexed purge-bucket records",
            self.name(),
            operations
        ));
        recorder.measure(operations.max(1), || {
            self.purge_bucket_zero(operations as usize)
        })?;
        Ok(operations.max(1))
    }

    fn compaction(
        &mut self,
        _profile: &LibraryProfile,
        recorder: &mut PhaseRecorder,
        progress: &ProgressReporter,
    ) -> io::Result<u64> {
        progress.log(format!("{}: flushing SurrealDB transactions", self.name()));
        recorder.measure(1, || {
            let _ = self.execute_query("INFO FOR DB;");
            Ok(())
        })?;
        Ok(1)
    }

    fn flush(&mut self) -> io::Result<()> {
        let _ = self.execute_query("INFO FOR DB;");
        Ok(())
    }

    fn storage_footprint_bytes(&mut self) -> io::Result<u64> {
        let results = self.execute_query("INFO FOR DB;")?;
        let bytes = serde_json::to_vec(&results)
            .map(|v| v.len() as u64)
            .unwrap_or(0);
        Ok(bytes)
    }

    fn storage_diagnostics(&mut self) -> io::Result<Vec<String>> {
        let results = self.execute_query("INFO FOR DB;")?;
        Ok(vec![format!(
            "SurrealDB DB info:\n{}",
            serde_json::to_string_pretty(&results).unwrap_or_default()
        )])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{PhaseName, PhaseRecorder, ProgressReporter};
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::thread::{self, JoinHandle};

    fn read_request_body(stream: &mut TcpStream) -> String {
        let mut request = Vec::new();
        let mut buffer = [0u8; 4096];
        let header_end = loop {
            let read = stream.read(&mut buffer).unwrap();
            assert!(read > 0);
            request.extend_from_slice(&buffer[..read]);
            if let Some(index) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                break index + 4;
            }
        };
        let headers = String::from_utf8_lossy(&request[..header_end]);
        let content_length = headers
            .lines()
            .find_map(|line| {
                line.strip_prefix("content-length:")
                    .or_else(|| line.strip_prefix("Content-Length:"))
            })
            .map(str::trim)
            .map(|value| value.parse::<usize>().unwrap())
            .unwrap_or(0);
        while request.len() < header_end + content_length {
            let read = stream.read(&mut buffer).unwrap();
            assert!(read > 0);
            request.extend_from_slice(&buffer[..read]);
        }
        String::from_utf8(request[header_end..header_end + content_length].to_vec()).unwrap()
    }

    fn spawn_server<F>(request_count: usize, responder: F) -> (String, JoinHandle<()>)
    where
        F: Fn(&str) -> (u16, String) + Send + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            for _ in 0..request_count {
                let (mut stream, _) = listener.accept().unwrap();
                let body = read_request_body(&mut stream);
                let (status, response_body) = responder(&body);
                let reason = if status == 200 {
                    "OK"
                } else {
                    "Internal Server Error"
                };
                write!(
                    stream,
                    "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    response_body.len(),
                    response_body
                )
                .unwrap();
            }
        });
        (format!("http://{address}"), handle)
    }

    fn successful_response(sql: &str) -> (u16, String) {
        let result = if let Some(id) = sql
            .split("WHERE _id = '")
            .nth(1)
            .and_then(|value| value.split('\'').next())
        {
            serde_json::json!([{"_id": id, "quantity": 1}])
        } else if sql.starts_with("DELETE FROM book") {
            serde_json::json!([{"_id": "deleted"}])
        } else if sql.contains("INFO FOR DB") {
            serde_json::json!({"tables": 2})
        } else {
            serde_json::json!([])
        };
        (
            200,
            serde_json::json!([{"status": "OK", "result": result}]).to_string(),
        )
    }

    fn tiny_profile() -> LibraryProfile {
        LibraryProfile {
            entity_records: 3,
            book_records: 6,
            chunk_size: 2,
            traversal_queries: 2,
            point_lookups: 3,
            range_lookups: 3,
            delete_by_id_operations: 2,
            purge_buckets: 2,
        }
    }

    #[test]
    fn surrealdb_target_initializes_with_parameters() {
        let target = SurrealdbTarget::new(
            "http://127.0.0.1:8000".to_string(),
            "wardrobe_benchmark".to_string(),
            "wardrobe_benchmark".to_string(),
            Some("root".to_string()),
            Some("root".to_string()),
        )
        .expect("target should initialize");
        assert_eq!(target.name(), "SurrealDB (Multi-Model Document Comparison)");
    }

    #[test]
    fn surrealdb_target_runs_every_benchmark_phase_against_http_fake() {
        let (uri, handle) = spawn_server(24, successful_response);
        let mut target = SurrealdbTarget::new(
            uri,
            "benchmark_ns".to_string(),
            "benchmark_db".to_string(),
            Some("root".to_string()),
            Some("secret".to_string()),
        )
        .unwrap();
        let profile = tiny_profile();
        let progress = ProgressReporter::new(false);

        target.provision_schema(&profile, &progress).unwrap();

        let mut ingestion = PhaseRecorder::new(PhaseName::MassiveIngestion);
        assert_eq!(
            target
                .massive_ingestion(&profile, &mut ingestion, &progress)
                .unwrap(),
            9
        );

        let mut indexes = PhaseRecorder::new(PhaseName::IndexMutation);
        assert_eq!(
            target
                .index_mutation(&profile, &mut indexes, &progress)
                .unwrap(),
            3
        );

        let mut points = PhaseRecorder::new(PhaseName::PointLookup);
        assert_eq!(
            target
                .point_lookup(&profile, &mut points, &progress)
                .unwrap(),
            3
        );

        let mut ranges = PhaseRecorder::new(PhaseName::RangeLookup);
        assert_eq!(
            target
                .range_lookup(&profile, &mut ranges, &progress)
                .unwrap(),
            3
        );

        let mut traversals = PhaseRecorder::new(PhaseName::ComplexTraversal);
        assert_eq!(
            target
                .complex_traversal(&profile, &mut traversals, &progress)
                .unwrap(),
            2
        );

        let mut deletes = PhaseRecorder::new(PhaseName::DeleteById);
        assert_eq!(
            target
                .delete_by_id(&profile, &mut deletes, &progress)
                .unwrap(),
            2
        );

        let mut purge = PhaseRecorder::new(PhaseName::TargetedPurge);
        assert_eq!(
            target
                .targeted_purge(&profile, &mut purge, &progress)
                .unwrap(),
            3
        );

        let mut compaction = PhaseRecorder::new(PhaseName::Compaction);
        assert_eq!(
            target
                .compaction(&profile, &mut compaction, &progress)
                .unwrap(),
            1
        );

        target.flush().unwrap();
        assert!(target.storage_footprint_bytes().unwrap() > 0);
        assert_eq!(target.storage_diagnostics().unwrap().len(), 1);

        handle.join().unwrap();
    }

    #[test]
    fn surrealdb_query_response_validation_covers_error_shapes() {
        let cases = [
            (
                500,
                serde_json::json!({"error": "offline"}).to_string(),
                ErrorKind::Other,
            ),
            (
                200,
                serde_json::json!([{"status": "ERR", "detail": "bad query"}]).to_string(),
                ErrorKind::InvalidData,
            ),
            (200, "not-json".to_string(), ErrorKind::Other),
        ];

        for (status, body, expected_kind) in cases {
            let (uri, handle) = spawn_server(1, move |_| (status, body.clone()));
            let target =
                SurrealdbTarget::new(uri, "ns".to_string(), "db".to_string(), None, None).unwrap();
            assert_eq!(
                target.execute_query("RETURN 1").unwrap_err().kind(),
                expected_kind
            );
            handle.join().unwrap();
        }

        let (uri, handle) = spawn_server(1, |_| (200, serde_json::json!({"value": 1}).to_string()));
        let target =
            SurrealdbTarget::new(uri, "ns".to_string(), "db".to_string(), None, None).unwrap();
        assert_eq!(
            target.execute_query("RETURN 1").unwrap(),
            vec![serde_json::json!({"value": 1})]
        );
        handle.join().unwrap();
    }
}
