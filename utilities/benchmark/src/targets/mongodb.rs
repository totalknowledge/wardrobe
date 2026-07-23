use super::{BenchmarkTarget, verify_deleted_count};
use crate::config::{BOOK_DRAWER, ENTITY_DRAWER, LibraryProfile};
use crate::engine::{PhaseRecorder, ProgressReporter, report_record_progress};
use crate::utils::{chunk_ranges, to_io_error};
use mongodb::IndexModel;
use mongodb::bson::{Bson, Document, doc};
use mongodb::sync::{Client as MongoClient, Collection};
use std::io::{self, Error, ErrorKind};

pub(crate) struct MongoTarget {
    client: MongoClient,
    database: String,
}

impl MongoTarget {
    pub(crate) fn new(mongo_uri: String, database: String) -> io::Result<Self> {
        let client = MongoClient::with_uri_str(&mongo_uri).map_err(to_io_error)?;
        client
            .database("admin")
            .run_command(doc! { "ping": 1 }, None)
            .map_err(to_io_error)?;
        Ok(Self { client, database })
    }

    fn entities(&self) -> Collection<Document> {
        self.client
            .database(&self.database)
            .collection::<Document>("entities")
    }

    fn books(&self) -> Collection<Document> {
        self.client
            .database(&self.database)
            .collection::<Document>("books")
    }

    fn insert_documents(&self, drawer: &str, documents: Vec<Document>) -> io::Result<()> {
        if documents.is_empty() {
            return Ok(());
        }
        if drawer == ENTITY_DRAWER {
            self.entities()
                .insert_many(documents, None)
                .map(|_| ())
                .map_err(to_io_error)
        } else {
            self.books()
                .insert_many(documents, None)
                .map(|_| ())
                .map_err(to_io_error)
        }
    }
}

impl BenchmarkTarget for MongoTarget {
    fn name(&self) -> &str {
        "MongoDB (Document Store Base Comparison)"
    }

    fn provision_schema(
        &mut self,
        _profile: &LibraryProfile,
        progress: &ProgressReporter,
    ) -> io::Result<()> {
        progress.log(format!(
            "{}: dropping and recreating MongoDB collections in '{}'",
            self.name(),
            self.database
        ));
        let database = self.client.database(&self.database);
        database.drop(None).map_err(to_io_error)?;
        database
            .create_collection("entities", None)
            .map_err(to_io_error)?;
        database
            .create_collection("books", None)
            .map_err(to_io_error)?;
        self.books()
            .create_index(
                IndexModel::builder().keys(doc! { "quantity": 1 }).build(),
                None,
            )
            .map_err(to_io_error)?;
        progress.log(format!(
            "{}: MongoDB collections are ready; requesting disk sync",
            self.name()
        ));
        self.flush()
    }

    fn massive_ingestion(
        &mut self,
        profile: &LibraryProfile,
        recorder: &mut PhaseRecorder,
        progress: &ProgressReporter,
    ) -> io::Result<u64> {
        for (start, end) in chunk_ranges(profile.entity_records, profile.chunk_size) {
            let documents = mongo_documents(profile, ENTITY_DRAWER, start, end)?;
            recorder.measure((end - start) as u64, || {
                self.insert_documents(ENTITY_DRAWER, documents)
            })?;
            report_record_progress(
                progress,
                &format!("{}: entities ingested", self.name()),
                end,
                profile.entity_records,
            );
        }
        for (start, end) in chunk_ranges(profile.book_records, profile.chunk_size) {
            let documents = mongo_documents(profile, BOOK_DRAWER, start, end)?;
            recorder.measure((end - start) as u64, || {
                self.insert_documents(BOOK_DRAWER, documents)
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
        for (index, (label, operation)) in [
            ("create isbn_1", "create"),
            ("drop isbn_1", "drop"),
            ("recreate isbn_1", "create"),
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
                if operation == "drop" {
                    self.books()
                        .drop_index("isbn_1", None)
                        .map(|_| ())
                        .map_err(to_io_error)
                } else {
                    self.books()
                        .create_index(IndexModel::builder().keys(doc! { "isbn": 1 }).build(), None)
                        .map(|_| ())
                        .map_err(to_io_error)
                }
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
        for (index, id) in ids.iter().enumerate() {
            recorder.measure(1, || {
                let document = self
                    .books()
                    .find_one(doc! { "_id": id }, None)
                    .map_err(to_io_error)?
                    .ok_or_else(|| {
                        Error::new(
                            ErrorKind::NotFound,
                            format!("MongoDB point lookup did not find book '{id}'"),
                        )
                    })?;
                match document.get_str("_id") {
                    Ok(actual_id) if actual_id == id => Ok(()),
                    Ok(actual_id) => Err(Error::new(
                        ErrorKind::InvalidData,
                        format!("Expected MongoDB book id '{id}', got '{actual_id}'"),
                    )),
                    Err(error) => Err(to_io_error(error)),
                }
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
                let cursor = self
                    .books()
                    .find(doc! { "quantity": { "$gte": low, "$lte": high } }, None)
                    .map_err(to_io_error)?;
                for document in cursor {
                    let document = document.map_err(to_io_error)?;
                    verify_mongo_record_range(&document, "quantity", *low, *high)?;
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
                let cursor = self
                    .books()
                    .aggregate(mongo_materialized_book_pipeline(&entity_id), None)
                    .map_err(to_io_error)?;
                for document in cursor {
                    let _record = document.map_err(to_io_error)?;
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
                let result = self
                    .books()
                    .delete_one(doc! { "_id": id }, None)
                    .map_err(to_io_error)?;
                verify_deleted_count(result.deleted_count as usize, id)?;
                let remaining = self
                    .books()
                    .count_documents(doc! { "_id": id }, None)
                    .map_err(to_io_error)?;
                if remaining == 0 {
                    Ok(())
                } else {
                    Err(Error::new(
                        ErrorKind::InvalidData,
                        format!("MongoDB delete-by-ID left book '{id}' behind"),
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
            self.books()
                .delete_many(doc! { "purge_bucket": 0_i64 }, None)
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
        progress.log(format!(
            "{}: running compact/validate fallback on books",
            self.name()
        ));
        recorder.measure(1, || {
            let database = self.client.database(&self.database);
            database
                .run_command(doc! { "compact": "books", "force": true }, None)
                .or_else(|_| {
                    database.run_command(doc! { "validate": "books", "full": false }, None)
                })
                .map(|_| ())
                .map_err(to_io_error)
        })?;
        Ok(1)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.client
            .database("admin")
            .run_command(doc! { "fsync": 1 }, None)
            .map(|_| ())
            .map_err(to_io_error)
    }

    fn storage_footprint_bytes(&mut self) -> io::Result<u64> {
        let stats = self
            .client
            .database(&self.database)
            .run_command(doc! { "dbStats": 1 }, None)
            .map_err(to_io_error)?;
        Ok(bson_number_to_u64(stats.get("storageSize")).unwrap_or(0))
    }
}

pub(crate) fn bson_number_to_u64(value: Option<&Bson>) -> Option<u64> {
    match value? {
        Bson::Int32(value) => u64::try_from(*value).ok(),
        Bson::Int64(value) => u64::try_from(*value).ok(),
        Bson::Double(value) if value.is_finite() && *value >= 0.0 => Some(*value as u64),
        _ => None,
    }
}

pub(crate) fn verify_mongo_record_range(
    record: &Document,
    field: &str,
    low: i64,
    high: i64,
) -> io::Result<()> {
    let actual = record.get_i64(field).map_err(to_io_error)?;
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

pub(crate) fn mongo_materialized_book_pipeline(entity_id: &str) -> Vec<Document> {
    vec![
        doc! { "$match": { "author_id": entity_id, "editor_id": entity_id } },
        doc! {
            "$lookup": {
                "from": "entities",
                "localField": "author_id",
                "foreignField": "_id",
                "as": "author"
            }
        },
        doc! { "$unwind": "$author" },
        doc! {
            "$lookup": {
                "from": "entities",
                "localField": "editor_id",
                "foreignField": "_id",
                "as": "editor"
            }
        },
        doc! { "$unwind": "$editor" },
        doc! {
            "$project": {
                "_id": 1,
                "book_id": 1,
                "isbn": 1,
                "title": 1,
                "author_id": 1,
                "editor_id": 1,
                "branch": 1,
                "quantity": 1,
                "purge_bucket": 1,
                "author": {
                    "_id": "$author._id",
                    "entity_id": "$author.entity_id",
                    "display_name": "$author.display_name",
                    "role": "$author.role",
                    "cohort": "$author.cohort"
                },
                "editor": {
                    "_id": "$editor._id",
                    "entity_id": "$editor.entity_id",
                    "display_name": "$editor.display_name",
                    "role": "$editor.role",
                    "cohort": "$editor.cohort"
                }
            }
        },
    ]
}

pub(crate) fn mongo_documents(
    profile: &LibraryProfile,
    drawer: &str,
    start: usize,
    end: usize,
) -> io::Result<Vec<Document>> {
    (start..end)
        .map(|index| {
            let payload = if drawer == ENTITY_DRAWER {
                profile.entity_payload(index)
            } else {
                profile.book_payload(index)
            };
            mongodb::bson::to_document(&payload).map_err(to_io_error)
        })
        .collect()
}
