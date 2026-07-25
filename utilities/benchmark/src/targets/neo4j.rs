use super::{
    BenchmarkTarget, book_value, read_default_neo4j_credentials, verify_deleted_count,
    verify_record_range,
};
use crate::config::{
    DEFAULT_NEO4J_PASSWORD, DEFAULT_NEO4J_PASSWORD_ENV, DEFAULT_NEO4J_USER, DEFAULT_NEO4J_USER_ENV,
    LibraryProfile,
};
use crate::engine::{PhaseRecorder, ProgressReporter, report_record_progress};
use crate::utils::{chunk_ranges, to_io_error};
use neo4rs::{BoltList, BoltType, Graph, query};
use serde_json::Value;
use std::collections::HashMap;
use std::env;
use std::io::{self, Error, ErrorKind};
use tokio::runtime::Runtime;

pub(crate) const NEO4J_BULK_ENTITY_UPSERT_QUERY: &str = r#"
UNWIND $rows AS row
MERGE (e:BenchNode:Entity {bench_marker: $marker, id: row.id})
SET e.display_name = row.display_name,
    e.role = row.role,
    e.cohort = row.cohort
"#;
pub(crate) const NEO4J_BULK_BOOK_UPSERT_QUERY: &str = r#"
UNWIND $rows AS row
MERGE (b:BenchNode:Book {bench_marker: $marker, id: row.id})
SET b.isbn = row.isbn,
    b.title = row.title,
    b.author_id = row.author_id,
    b.editor_id = row.editor_id,
    b.branch = row.branch,
    b.quantity = row.quantity,
    b.purge_bucket = row.purge_bucket
WITH b, row, $marker AS marker
MATCH (author:BenchNode:Entity {bench_marker: marker, id: row.author_id})
MATCH (editor:BenchNode:Entity {bench_marker: marker, id: row.editor_id})
MERGE (b)-[:AUTHORED_BY]->(author)
MERGE (b)-[:EDITED_BY]->(editor)
"#;
pub(crate) const NEO4J_STORE_SIZE_BYTES_QUERY: &str = r#"
CALL dbms.queryJmx("org.neo4j:*") YIELD name, attributes
WHERE name CONTAINS "Store file sizes"
  AND (name CONTAINS ("database=" + $database) OR name CONTAINS "instance=kernel")
RETURN coalesce(max(toInteger(attributes.TotalStoreSize.value)), 0) AS storage_bytes
"#;

pub(crate) struct Neo4jTarget {
    graph: Graph,
    runtime: Runtime,
    database: String,
    marker: String,
}

impl Neo4jTarget {
    pub(crate) fn new(
        uri: String,
        database: String,
        user: String,
        password_env: String,
        marker: String,
    ) -> io::Result<Self> {
        let fallback_credentials = read_default_neo4j_credentials()?;
        let resolved_user = if user == DEFAULT_NEO4J_USER {
            env::var(DEFAULT_NEO4J_USER_ENV)
                .ok()
                .or_else(|| fallback_credentials.user.clone())
                .unwrap_or(user)
        } else {
            user
        };
        let password = match env::var(&password_env) {
            Ok(value) => value,
            Err(env::VarError::NotPresent) if password_env == DEFAULT_NEO4J_PASSWORD_ENV => {
                fallback_credentials
                    .password
                    .unwrap_or_else(|| DEFAULT_NEO4J_PASSWORD.to_string())
            }
            Err(env::VarError::NotPresent) => {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    format!("Neo4j password environment variable '{password_env}' is not set"),
                ));
            }
            Err(env::VarError::NotUnicode(_)) => {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    format!(
                        "Neo4j password environment variable '{password_env}' is not valid UTF-8"
                    ),
                ));
            }
        };
        let runtime = Runtime::new().map_err(to_io_error)?;
        let graph = runtime.block_on(async {
            let config = neo4rs::ConfigBuilder::default()
                .uri(uri)
                .user(resolved_user)
                .password(password)
                .db(database.clone())
                .build()
                .map_err(to_io_error)?;
            Graph::connect(config).await.map_err(to_io_error)
        })?;
        Ok(Self {
            graph,
            runtime,
            database,
            marker,
        })
    }

    fn run_query(&self, q: neo4rs::Query) -> io::Result<()> {
        self.runtime
            .block_on(async { self.graph.run(q).await.map_err(to_io_error) })
    }

    fn count_query(&self, q: neo4rs::Query, field: &str) -> io::Result<u64> {
        self.runtime.block_on(async {
            let mut stream = self.graph.execute(q).await.map_err(to_io_error)?;
            if let Some(row) = stream.next().await.map_err(to_io_error)? {
                let count: i64 = row.get(field).map_err(to_io_error)?;
                return Ok(u64::try_from(count).unwrap_or(0));
            }
            Ok(0)
        })
    }

    fn query_barrier(&self) -> io::Result<()> {
        self.runtime.block_on(async {
            let mut stream = self
                .graph
                .execute(query("RETURN 1 AS ok"))
                .await
                .map_err(to_io_error)?;
            while let Some(_row) = stream.next().await.map_err(to_io_error)? {}
            Ok(())
        })
    }

    fn checkpoint_or_barrier(&self, progress: Option<&ProgressReporter>) -> io::Result<()> {
        match self.run_query(query("CALL db.checkpoint()")) {
            Ok(()) => Ok(()),
            Err(error) if neo4j_checkpoint_is_unavailable(&error) => {
                if let Some(progress) = progress {
                    progress.log(format!(
                        "{}: Neo4j db.checkpoint() unavailable; using query barrier",
                        self.name()
                    ));
                }
                self.query_barrier()
            }
            Err(error) => Err(error),
        }
    }

    fn entity_rows(&self, profile: &LibraryProfile, start: usize, end: usize) -> BoltType {
        neo4j_entity_rows(profile, start, end)
    }

    fn book_rows(&self, profile: &LibraryProfile, start: usize, end: usize) -> BoltType {
        neo4j_book_rows(profile, start, end)
    }
}

impl BenchmarkTarget for Neo4jTarget {
    fn name(&self) -> &str {
        "Neo4j (Graph Database Base Comparison)"
    }

    fn provision_schema(
        &mut self,
        _profile: &LibraryProfile,
        progress: &ProgressReporter,
    ) -> io::Result<()> {
        progress.log(format!(
            "{}: clearing benchmark nodes in database '{}'",
            self.name(),
            self.database
        ));
        self.run_query(
            query("MATCH (n:BenchNode {bench_marker: $marker}) DETACH DELETE n")
                .param("marker", self.marker.clone()),
        )?;

        progress.log(format!(
            "{}: creating Neo4j constraints/indexes",
            self.name()
        ));
        self.run_query(query(
            "CREATE CONSTRAINT IF NOT EXISTS FOR (e:Entity) REQUIRE (e.bench_marker, e.id) IS UNIQUE",
        ))?;
        self.run_query(query(
            "CREATE CONSTRAINT IF NOT EXISTS FOR (b:Book) REQUIRE (b.bench_marker, b.id) IS UNIQUE",
        ))?;
        self.run_query(query("CREATE INDEX IF NOT EXISTS FOR (b:Book) ON (b.isbn)"))?;
        self.run_query(query(
            "CREATE INDEX IF NOT EXISTS FOR (b:Book) ON (b.purge_bucket)",
        ))?;
        self.run_query(query(
            "CREATE INDEX IF NOT EXISTS FOR (b:Book) ON (b.quantity)",
        ))?;
        self.flush()
    }

    fn massive_ingestion(
        &mut self,
        profile: &LibraryProfile,
        recorder: &mut PhaseRecorder,
        progress: &ProgressReporter,
    ) -> io::Result<u64> {
        for (start, end) in chunk_ranges(profile.entity_records, profile.chunk_size) {
            let rows = self.entity_rows(profile, start, end);
            recorder.measure((end - start) as u64, || {
                self.run_query(
                    query(NEO4J_BULK_ENTITY_UPSERT_QUERY)
                        .param("marker", self.marker.clone())
                        .param("rows", rows),
                )
            })?;
            report_record_progress(
                progress,
                &format!("{}: entities ingested", self.name()),
                end,
                profile.entity_records,
            );
        }

        for (start, end) in chunk_ranges(profile.book_records, profile.chunk_size) {
            let rows = self.book_rows(profile, start, end);
            recorder.measure((end - start) as u64, || {
                self.run_query(
                    query(NEO4J_BULK_BOOK_UPSERT_QUERY)
                        .param("marker", self.marker.clone())
                        .param("rows", rows),
                )
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
        for (index, (label, cypher)) in [
            (
                "create bench_isbn_idx",
                "CREATE INDEX bench_isbn_idx IF NOT EXISTS FOR (b:Book) ON (b.isbn)",
            ),
            ("drop bench_isbn_idx", "DROP INDEX bench_isbn_idx IF EXISTS"),
            (
                "recreate bench_isbn_idx",
                "CREATE INDEX bench_isbn_idx IF NOT EXISTS FOR (b:Book) ON (b.isbn)",
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
            recorder.measure(1, || self.run_query(query(cypher)))?;
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
                self.runtime.block_on(async {
                    let mut stream = self
                        .graph
                        .execute(
                            query(
                                "MATCH (b:BenchNode:Book {bench_marker: $marker, id: $id}) RETURN b.id AS id, b.isbn AS isbn, b.title AS title, b.author_id AS author_id, b.editor_id AS editor_id, b.branch AS branch, b.quantity AS quantity, b.purge_bucket AS purge_bucket",
                            )
                            .param("marker", self.marker.clone())
                            .param("id", id.clone()),
                        )
                        .await
                        .map_err(to_io_error)?;
                    let mut found = 0_usize;
                    while let Some(row) = stream.next().await.map_err(to_io_error)? {
                        let actual_id: String = row.get("id").map_err(to_io_error)?;
                        let _record = book_value(
                            actual_id.clone(),
                            row.get("isbn").map_err(to_io_error)?,
                            row.get("title").map_err(to_io_error)?,
                            row.get("author_id").map_err(to_io_error)?,
                            row.get("editor_id").map_err(to_io_error)?,
                            row.get("branch").map_err(to_io_error)?,
                            row.get("quantity").map_err(to_io_error)?,
                            row.get("purge_bucket").map_err(to_io_error)?,
                        );
                        if actual_id != *id {
                            return Err(Error::new(
                                ErrorKind::InvalidData,
                                format!("Expected Neo4j book id '{id}', got '{actual_id}'"),
                            ));
                        }
                        found = found.saturating_add(1);
                    }
                    if found == 1 {
                        Ok(())
                    } else {
                        Err(Error::new(
                            ErrorKind::NotFound,
                            format!("Neo4j point lookup found {found} records for book '{id}'"),
                        ))
                    }
                })
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
                self.runtime.block_on(async {
                    let mut stream = self
                        .graph
                        .execute(
                            query(
                                "MATCH (b:BenchNode:Book {bench_marker: $marker}) WHERE b.quantity >= $low AND b.quantity <= $high RETURN b.id AS id, b.isbn AS isbn, b.title AS title, b.author_id AS author_id, b.editor_id AS editor_id, b.branch AS branch, b.quantity AS quantity, b.purge_bucket AS purge_bucket",
                            )
                            .param("marker", self.marker.clone())
                            .param("low", *low)
                            .param("high", *high),
                        )
                        .await
                        .map_err(to_io_error)?;
                    while let Some(row) = stream.next().await.map_err(to_io_error)? {
                        let actual_id: String = row.get("id").map_err(to_io_error)?;
                        let record = book_value(
                            actual_id,
                            row.get("isbn").map_err(to_io_error)?,
                            row.get("title").map_err(to_io_error)?,
                            row.get("author_id").map_err(to_io_error)?,
                            row.get("editor_id").map_err(to_io_error)?,
                            row.get("branch").map_err(to_io_error)?,
                            row.get("quantity").map_err(to_io_error)?,
                            row.get("purge_bucket").map_err(to_io_error)?,
                        );
                        verify_record_range(&record, "quantity", *low, *high)?;
                    }
                    Ok(())
                })
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
                self.runtime.block_on(async {
                    let mut stream = self
                        .graph
                        .execute(
                            query(
                                "MATCH (author:BenchNode:Entity {bench_marker: $marker, id: $entity_id})<-[:AUTHORED_BY]-(b:BenchNode:Book)-[:EDITED_BY]->(editor:BenchNode:Entity {bench_marker: $marker, id: $entity_id}) RETURN b.id AS book_id, b.isbn AS isbn, b.title AS title, b.author_id AS author_id, b.editor_id AS editor_id, b.branch AS branch, b.quantity AS quantity, b.purge_bucket AS purge_bucket, author.id AS author_entity_id, author.display_name AS author_display_name, author.role AS author_role, author.cohort AS author_cohort, editor.id AS editor_entity_id, editor.display_name AS editor_display_name, editor.role AS editor_role, editor.cohort AS editor_cohort",
                            )
                            .param("marker", self.marker.clone())
                            .param("entity_id", entity_id),
                        )
                        .await
                        .map_err(to_io_error)?;
                    while let Some(_row) = stream.next().await.map_err(to_io_error)? {}
                    Ok(())
                })
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
                let deleted = self.count_query(
                    query(
                        "MATCH (b:BenchNode:Book {bench_marker: $marker, id: $id}) WITH collect(b) AS nodes FOREACH (node IN nodes | DETACH DELETE node) RETURN size(nodes) AS deleted_count",
                    )
                    .param("marker", self.marker.clone())
                    .param("id", id.clone()),
                    "deleted_count",
                )?;
                verify_deleted_count(deleted as usize, id)?;
                let remaining = self.count_query(
                    query(
                        "MATCH (b:BenchNode:Book {bench_marker: $marker, id: $id}) RETURN count(b) AS remaining_count",
                    )
                    .param("marker", self.marker.clone())
                    .param("id", id.clone()),
                    "remaining_count",
                )?;
                if remaining == 0 {
                    Ok(())
                } else {
                    Err(Error::new(
                        ErrorKind::InvalidData,
                        format!("Neo4j delete-by-ID left book '{id}' behind"),
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
            self.run_query(
                query(
                    "MATCH (b:BenchNode:Book {bench_marker: $marker, purge_bucket: 0}) DETACH DELETE b",
                )
                .param("marker", self.marker.clone()),
            )
        })?;
        Ok(operations.max(1))
    }

    fn compaction(
        &mut self,
        _profile: &LibraryProfile,
        recorder: &mut PhaseRecorder,
        progress: &ProgressReporter,
    ) -> io::Result<u64> {
        progress.log(format!(
            "{}: issuing Neo4j checkpoint or query barrier",
            self.name()
        ));
        recorder.measure(1, || self.checkpoint_or_barrier(Some(progress)))?;
        Ok(1)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.checkpoint_or_barrier(None)
    }

    fn storage_footprint_bytes(&mut self) -> io::Result<u64> {
        self.count_query(
            query(NEO4J_STORE_SIZE_BYTES_QUERY).param("database", self.database.clone()),
            "storage_bytes",
        )
    }

    fn storage_diagnostics(&mut self) -> io::Result<Vec<String>> {
        let node_count = self.count_query(
            query("MATCH (n:BenchNode {bench_marker: $marker}) RETURN count(n) AS node_count")
                .param("marker", self.marker.clone()),
            "node_count",
        )?;
        let relationship_count = self.count_query(
            query(
                "MATCH (:BenchNode {bench_marker: $marker})-[r]->(:BenchNode {bench_marker: $marker}) RETURN count(r) AS relationship_count",
            )
            .param("marker", self.marker.clone()),
            "relationship_count",
        )?;
        Ok(vec![
            format!(
                "Benchmark scope marker '{}' in database '{}' contains {} nodes and {} relationships",
                self.marker, self.database, node_count, relationship_count
            ),
            "Neo4j storage metric reports database store size bytes from JMX; logical node and relationship counts are diagnostic only.".to_string(),
        ])
    }
}

pub(crate) fn neo4j_checkpoint_is_unavailable(error: &io::Error) -> bool {
    let message = error.to_string();
    message.contains("ProcedureNotFound") && message.contains("db.checkpoint")
}

pub(crate) fn neo4j_entity_rows(profile: &LibraryProfile, start: usize, end: usize) -> BoltType {
    let rows = (start..end)
        .map(|index| {
            let payload = profile.entity_payload(index);
            neo4j_row([
                ("id", neo4j_string_field(&payload, "_id")),
                ("display_name", neo4j_string_field(&payload, "display_name")),
                ("role", neo4j_string_field(&payload, "role")),
                ("cohort", neo4j_i64_field(&payload, "cohort")),
            ])
        })
        .collect::<Vec<_>>();
    BoltType::List(BoltList::from(rows))
}

pub(crate) fn neo4j_book_rows(profile: &LibraryProfile, start: usize, end: usize) -> BoltType {
    let rows = (start..end)
        .map(|index| {
            let payload = profile.book_payload(index);
            neo4j_row([
                ("id", neo4j_string_field(&payload, "_id")),
                ("isbn", neo4j_string_field(&payload, "isbn")),
                ("title", neo4j_string_field(&payload, "title")),
                ("author_id", neo4j_string_field(&payload, "author_id")),
                ("editor_id", neo4j_string_field(&payload, "editor_id")),
                ("branch", neo4j_string_field(&payload, "branch")),
                ("quantity", neo4j_i64_field(&payload, "quantity")),
                ("purge_bucket", neo4j_i64_field(&payload, "purge_bucket")),
            ])
        })
        .collect::<Vec<_>>();
    BoltType::List(BoltList::from(rows))
}

pub(crate) fn neo4j_row<const N: usize>(fields: [(&str, BoltType); N]) -> BoltType {
    let values = fields
        .into_iter()
        .map(|(key, value)| (key.to_string(), value))
        .collect::<HashMap<_, _>>();
    BoltType::from(values)
}

pub(crate) fn neo4j_string_field(payload: &Value, field: &str) -> BoltType {
    BoltType::from(payload[field].as_str().unwrap_or_default().to_string())
}

pub(crate) fn neo4j_i64_field(payload: &Value, field: &str) -> BoltType {
    BoltType::from(payload[field].as_u64().unwrap_or_default() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{PhaseName, PhaseRecorder, ProgressReporter};

    #[test]
    fn neo4j_target_covers_credentials_and_empty_work_paths() {
        let missing_password = format!("WARDROBE_BENCH_NEO4J_MISSING_{}", std::process::id());
        let error = Neo4jTarget::new(
            "127.0.0.1:1".to_string(),
            "neo4j".to_string(),
            "neo4j".to_string(),
            missing_password.clone(),
            "test".to_string(),
        )
        .err()
        .expect("custom missing password variable should fail");
        assert_eq!(error.kind(), ErrorKind::InvalidInput);
        assert!(error.to_string().contains(&missing_password));

        let mut target = Neo4jTarget::new(
            "127.0.0.1:1".to_string(),
            "neo4j".to_string(),
            DEFAULT_NEO4J_USER.to_string(),
            DEFAULT_NEO4J_PASSWORD_ENV.to_string(),
            "benchmark-test".to_string(),
        )
        .expect("Neo4j connection pool creation should be lazy");
        let profile = LibraryProfile {
            entity_records: 0,
            book_records: 0,
            chunk_size: 1,
            traversal_queries: 0,
            point_lookups: 0,
            range_lookups: 0,
            delete_by_id_operations: 0,
            purge_buckets: 1,
        };
        let progress = ProgressReporter::new(false);

        assert_eq!(target.name(), "Neo4j (Graph Database Base Comparison)");
        assert_eq!(
            target.entity_rows(&profile, 0, 0),
            BoltType::List(BoltList::new())
        );
        assert_eq!(
            target.book_rows(&profile, 0, 0),
            BoltType::List(BoltList::new())
        );
        assert_eq!(
            target
                .massive_ingestion(
                    &profile,
                    &mut PhaseRecorder::new(PhaseName::MassiveIngestion),
                    &progress,
                )
                .expect("empty ingestion should not contact Neo4j"),
            0
        );
        assert_eq!(
            target
                .point_lookup(
                    &profile,
                    &mut PhaseRecorder::new(PhaseName::PointLookup),
                    &progress,
                )
                .expect("empty point lookup should not contact Neo4j"),
            0
        );
        assert_eq!(
            target
                .range_lookup(
                    &profile,
                    &mut PhaseRecorder::new(PhaseName::RangeLookup),
                    &progress,
                )
                .expect("empty range lookup should not contact Neo4j"),
            0
        );
        assert_eq!(
            target
                .complex_traversal(
                    &profile,
                    &mut PhaseRecorder::new(PhaseName::ComplexTraversal),
                    &progress,
                )
                .expect("empty traversal should not contact Neo4j"),
            0
        );
        assert_eq!(
            target
                .delete_by_id(
                    &profile,
                    &mut PhaseRecorder::new(PhaseName::DeleteById),
                    &progress,
                )
                .expect("empty deletion should not contact Neo4j"),
            0
        );
    }
}
