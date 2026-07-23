use super::{BenchmarkTarget, verify_deleted_count, verify_record_id, verify_record_range};
use crate::config::LibraryProfile;
use crate::engine::{PhaseRecorder, ProgressReporter, report_record_progress};
use crate::utils::{chunk_ranges, file_size_or_zero, sync_file_if_exists, to_io_error};
use redb::{
    Database, MultimapTableDefinition, ReadableDatabase, ReadableTable, TableDefinition,
    WriteTransaction,
};
use serde_json::Value;
use std::collections::HashSet;
use std::fs;
use std::io::{self, Error, ErrorKind};
use std::path::PathBuf;

const ENTITIES: TableDefinition<&str, &[u8]> = TableDefinition::new("entities");
const BOOKS: TableDefinition<&str, &[u8]> = TableDefinition::new("books");
const AUTHOR_BOOKS: MultimapTableDefinition<&str, &str> =
    MultimapTableDefinition::new("author_books");
const EDITOR_BOOKS: MultimapTableDefinition<&str, &str> =
    MultimapTableDefinition::new("editor_books");
const QUANTITY_BOOKS: MultimapTableDefinition<u64, &str> =
    MultimapTableDefinition::new("quantity_books");
const PURGE_BUCKET_BOOKS: MultimapTableDefinition<u64, &str> =
    MultimapTableDefinition::new("purge_bucket_books");
const ISBN_BOOKS: MultimapTableDefinition<&str, &str> = MultimapTableDefinition::new("isbn_books");

pub(crate) struct RedbTarget {
    database: Database,
    path: PathBuf,
}

impl RedbTarget {
    pub(crate) fn new(path: PathBuf) -> io::Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let database = Database::create(&path).map_err(to_io_error)?;
        Ok(Self { database, path })
    }

    fn ingest_entities(
        &self,
        profile: &LibraryProfile,
        start: usize,
        end: usize,
    ) -> io::Result<()> {
        let transaction = self.database.begin_write().map_err(to_io_error)?;
        {
            let mut entities = transaction.open_table(ENTITIES).map_err(to_io_error)?;
            for index in start..end {
                let id = profile.entity_id(index);
                let payload =
                    serde_json::to_vec(&profile.entity_payload(index)).map_err(to_io_error)?;
                entities
                    .insert(id.as_str(), payload.as_slice())
                    .map_err(to_io_error)?;
            }
        }
        transaction.commit().map_err(to_io_error)
    }

    fn ingest_books(&self, profile: &LibraryProfile, start: usize, end: usize) -> io::Result<()> {
        let transaction = self.database.begin_write().map_err(to_io_error)?;
        {
            let mut books = transaction.open_table(BOOKS).map_err(to_io_error)?;
            let mut author_books = transaction
                .open_multimap_table(AUTHOR_BOOKS)
                .map_err(to_io_error)?;
            let mut editor_books = transaction
                .open_multimap_table(EDITOR_BOOKS)
                .map_err(to_io_error)?;
            let mut quantity_books = transaction
                .open_multimap_table(QUANTITY_BOOKS)
                .map_err(to_io_error)?;
            let mut purge_bucket_books = transaction
                .open_multimap_table(PURGE_BUCKET_BOOKS)
                .map_err(to_io_error)?;

            for index in start..end {
                let payload = profile.book_payload(index);
                let id = json_string_field(&payload, "_id")?;
                let author_id = json_string_field(&payload, "author_id")?;
                let editor_id = json_string_field(&payload, "editor_id")?;
                let quantity = json_u64_field(&payload, "quantity")?;
                let purge_bucket = json_u64_field(&payload, "purge_bucket")?;
                let encoded = serde_json::to_vec(&payload).map_err(to_io_error)?;

                books.insert(id, encoded.as_slice()).map_err(to_io_error)?;
                author_books.insert(author_id, id).map_err(to_io_error)?;
                editor_books.insert(editor_id, id).map_err(to_io_error)?;
                quantity_books.insert(quantity, id).map_err(to_io_error)?;
                purge_bucket_books
                    .insert(purge_bucket, id)
                    .map_err(to_io_error)?;
            }
        }
        transaction.commit().map_err(to_io_error)
    }

    fn build_isbn_index(&self) -> io::Result<()> {
        let transaction = self.database.begin_write().map_err(to_io_error)?;
        transaction
            .delete_multimap_table(ISBN_BOOKS)
            .map_err(to_io_error)?;
        {
            let books = transaction.open_table(BOOKS).map_err(to_io_error)?;
            let mut isbn_books = transaction
                .open_multimap_table(ISBN_BOOKS)
                .map_err(to_io_error)?;
            for entry in books.iter().map_err(to_io_error)? {
                let (id, payload) = entry.map_err(to_io_error)?;
                let book = decode_json(payload.value())?;
                isbn_books
                    .insert(json_string_field(&book, "isbn")?, id.value())
                    .map_err(to_io_error)?;
            }
        }
        transaction.commit().map_err(to_io_error)
    }

    fn drop_isbn_index(&self) -> io::Result<()> {
        let transaction = self.database.begin_write().map_err(to_io_error)?;
        transaction
            .delete_multimap_table(ISBN_BOOKS)
            .map_err(to_io_error)?;
        transaction.commit().map_err(to_io_error)
    }

    fn read_book(&self, id: &str) -> io::Result<Value> {
        let transaction = self.database.begin_read().map_err(to_io_error)?;
        let books = transaction.open_table(BOOKS).map_err(to_io_error)?;
        let payload = books.get(id).map_err(to_io_error)?.ok_or_else(|| {
            Error::new(
                ErrorKind::NotFound,
                format!("redb point lookup did not find book '{id}'"),
            )
        })?;
        decode_json(payload.value())
    }

    fn delete_book(&self, id: &str) -> io::Result<()> {
        let transaction = self.database.begin_write().map_err(to_io_error)?;
        let removed = {
            let mut books = transaction.open_table(BOOKS).map_err(to_io_error)?;
            books
                .remove(id)
                .map_err(to_io_error)?
                .map(|payload| decode_json(payload.value()))
                .transpose()?
        };
        verify_deleted_count(usize::from(removed.is_some()), id)?;
        let book = removed.as_ref().expect("verified removed redb book");
        remove_book_indexes(&transaction, id, book)?;
        transaction.commit().map_err(to_io_error)?;

        let read = self.database.begin_read().map_err(to_io_error)?;
        let books = read.open_table(BOOKS).map_err(to_io_error)?;
        if books.get(id).map_err(to_io_error)?.is_none() {
            Ok(())
        } else {
            Err(Error::new(
                ErrorKind::InvalidData,
                format!("redb delete-by-ID left book '{id}' behind"),
            ))
        }
    }

    fn purge_bucket_zero(&self, expected: usize) -> io::Result<()> {
        let transaction = self.database.begin_write().map_err(to_io_error)?;
        let ids = {
            let mut purge_bucket_books = transaction
                .open_multimap_table(PURGE_BUCKET_BOOKS)
                .map_err(to_io_error)?;
            purge_bucket_books
                .remove_all(0)
                .map_err(to_io_error)?
                .map(|result| {
                    result
                        .map(|guard| guard.value().to_string())
                        .map_err(to_io_error)
                })
                .collect::<io::Result<Vec<_>>>()?
        };
        let mut removed_books = Vec::with_capacity(ids.len());
        {
            let mut books = transaction.open_table(BOOKS).map_err(to_io_error)?;
            for id in &ids {
                if let Some(payload) = books.remove(id.as_str()).map_err(to_io_error)? {
                    removed_books.push((id.clone(), decode_json(payload.value())?));
                }
            }
        }
        if removed_books.len() != expected {
            return Err(Error::new(
                ErrorKind::InvalidData,
                format!(
                    "redb targeted purge expected to remove {expected} records, removed {}",
                    removed_books.len()
                ),
            ));
        }
        for (id, book) in &removed_books {
            remove_book_indexes(&transaction, id, book)?;
        }
        transaction.commit().map_err(to_io_error)
    }
}

impl BenchmarkTarget for RedbTarget {
    fn name(&self) -> &str {
        "redb (Pure Rust Embedded Key-Value Mode)"
    }

    fn provision_schema(
        &mut self,
        _profile: &LibraryProfile,
        progress: &ProgressReporter,
    ) -> io::Result<()> {
        progress.log(format!("{}: resetting typed tables", self.name()));
        let transaction = self.database.begin_write().map_err(to_io_error)?;
        transaction.delete_table(ENTITIES).map_err(to_io_error)?;
        transaction.delete_table(BOOKS).map_err(to_io_error)?;
        transaction
            .delete_multimap_table(AUTHOR_BOOKS)
            .map_err(to_io_error)?;
        transaction
            .delete_multimap_table(EDITOR_BOOKS)
            .map_err(to_io_error)?;
        transaction
            .delete_multimap_table(QUANTITY_BOOKS)
            .map_err(to_io_error)?;
        transaction
            .delete_multimap_table(PURGE_BUCKET_BOOKS)
            .map_err(to_io_error)?;
        transaction
            .delete_multimap_table(ISBN_BOOKS)
            .map_err(to_io_error)?;
        {
            transaction.open_table(ENTITIES).map_err(to_io_error)?;
            transaction.open_table(BOOKS).map_err(to_io_error)?;
            transaction
                .open_multimap_table(AUTHOR_BOOKS)
                .map_err(to_io_error)?;
            transaction
                .open_multimap_table(EDITOR_BOOKS)
                .map_err(to_io_error)?;
            transaction
                .open_multimap_table(QUANTITY_BOOKS)
                .map_err(to_io_error)?;
            transaction
                .open_multimap_table(PURGE_BUCKET_BOOKS)
                .map_err(to_io_error)?;
        }
        transaction.commit().map_err(to_io_error)?;
        self.flush()
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
            "{}: index mutation step 1/3: create ISBN multimap",
            self.name()
        ));
        recorder.measure(1, || self.build_isbn_index())?;
        progress.log(format!(
            "{}: index mutation step 2/3: drop ISBN multimap",
            self.name()
        ));
        recorder.measure(1, || self.drop_isbn_index())?;
        progress.log(format!(
            "{}: index mutation step 3/3: rebuild ISBN multimap",
            self.name()
        ));
        recorder.measure(1, || self.build_isbn_index())?;
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
                let transaction = self.database.begin_read().map_err(to_io_error)?;
                let quantity_books = transaction
                    .open_multimap_table(QUANTITY_BOOKS)
                    .map_err(to_io_error)?;
                let books = transaction.open_table(BOOKS).map_err(to_io_error)?;
                for entry in quantity_books
                    .range((*low as u64)..=(*high as u64))
                    .map_err(to_io_error)?
                {
                    let (_, ids) = entry.map_err(to_io_error)?;
                    for id in ids {
                        let id = id.map_err(to_io_error)?;
                        let payload = books
                            .get(id.value())
                            .map_err(to_io_error)?
                            .ok_or_else(|| missing_indexed_book(id.value()))?;
                        let book = decode_json(payload.value())?;
                        verify_record_range(&book, "quantity", *low, *high)?;
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
                let transaction = self.database.begin_read().map_err(to_io_error)?;
                let author_books = transaction
                    .open_multimap_table(AUTHOR_BOOKS)
                    .map_err(to_io_error)?;
                let editor_books = transaction
                    .open_multimap_table(EDITOR_BOOKS)
                    .map_err(to_io_error)?;
                let books = transaction.open_table(BOOKS).map_err(to_io_error)?;
                let entities = transaction.open_table(ENTITIES).map_err(to_io_error)?;
                let author_ids = author_books
                    .get(entity_id.as_str())
                    .map_err(to_io_error)?
                    .map(|result| {
                        result
                            .map(|guard| guard.value().to_string())
                            .map_err(to_io_error)
                    })
                    .collect::<io::Result<HashSet<_>>>()?;
                let editor_ids = editor_books
                    .get(entity_id.as_str())
                    .map_err(to_io_error)?
                    .map(|result| {
                        result
                            .map(|guard| guard.value().to_string())
                            .map_err(to_io_error)
                    })
                    .collect::<io::Result<HashSet<_>>>()?;

                for id in author_ids.intersection(&editor_ids) {
                    let book = books
                        .get(id.as_str())
                        .map_err(to_io_error)?
                        .ok_or_else(|| missing_indexed_book(id))
                        .and_then(|guard| decode_json(guard.value()))?;
                    let author_id = json_string_field(&book, "author_id")?;
                    let editor_id = json_string_field(&book, "editor_id")?;
                    let author = entities
                        .get(author_id)
                        .map_err(to_io_error)?
                        .ok_or_else(|| missing_indexed_entity(author_id))
                        .and_then(|guard| decode_json(guard.value()))?;
                    let editor = entities
                        .get(editor_id)
                        .map_err(to_io_error)?
                        .ok_or_else(|| missing_indexed_entity(editor_id))
                        .and_then(|guard| decode_json(guard.value()))?;
                    let mut materialized = book;
                    let object = materialized.as_object_mut().ok_or_else(|| {
                        Error::new(ErrorKind::InvalidData, "redb book is not a JSON object")
                    })?;
                    object.insert("author".to_string(), author);
                    object.insert("editor".to_string(), editor);
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
        progress.log(format!("{}: running native compaction", self.name()));
        recorder.measure(1, || {
            self.database.compact().map(|_| ()).map_err(to_io_error)
        })?;
        Ok(1)
    }

    fn flush(&mut self) -> io::Result<()> {
        sync_file_if_exists(&self.path)
    }

    fn storage_footprint_bytes(&mut self) -> io::Result<u64> {
        file_size_or_zero(&self.path)
    }

    fn storage_diagnostics(&mut self) -> io::Result<Vec<String>> {
        let transaction = self.database.begin_write().map_err(to_io_error)?;
        let stats = transaction.stats().map_err(to_io_error)?;
        Ok(vec![format!(
            "redb: {} allocated pages, {} bytes of stored keys and values, {} metadata bytes, {} fragmented bytes",
            stats.allocated_pages(),
            stats.stored_bytes(),
            stats.metadata_bytes(),
            stats.fragmented_bytes(),
        )])
    }
}

fn remove_book_indexes(transaction: &WriteTransaction, id: &str, book: &Value) -> io::Result<()> {
    let author_id = json_string_field(book, "author_id")?;
    let editor_id = json_string_field(book, "editor_id")?;
    let quantity = json_u64_field(book, "quantity")?;
    let purge_bucket = json_u64_field(book, "purge_bucket")?;
    let isbn = json_string_field(book, "isbn")?;
    transaction
        .open_multimap_table(AUTHOR_BOOKS)
        .map_err(to_io_error)?
        .remove(author_id, id)
        .map_err(to_io_error)?;
    transaction
        .open_multimap_table(EDITOR_BOOKS)
        .map_err(to_io_error)?
        .remove(editor_id, id)
        .map_err(to_io_error)?;
    transaction
        .open_multimap_table(QUANTITY_BOOKS)
        .map_err(to_io_error)?
        .remove(quantity, id)
        .map_err(to_io_error)?;
    transaction
        .open_multimap_table(PURGE_BUCKET_BOOKS)
        .map_err(to_io_error)?
        .remove(purge_bucket, id)
        .map_err(to_io_error)?;
    transaction
        .open_multimap_table(ISBN_BOOKS)
        .map_err(to_io_error)?
        .remove(isbn, id)
        .map_err(to_io_error)?;
    Ok(())
}

fn decode_json(bytes: &[u8]) -> io::Result<Value> {
    serde_json::from_slice(bytes).map_err(to_io_error)
}

fn json_string_field<'a>(value: &'a Value, field: &str) -> io::Result<&'a str> {
    value.get(field).and_then(Value::as_str).ok_or_else(|| {
        Error::new(
            ErrorKind::InvalidData,
            format!("redb record is missing string field '{field}'"),
        )
    })
}

fn json_u64_field(value: &Value, field: &str) -> io::Result<u64> {
    value.get(field).and_then(Value::as_u64).ok_or_else(|| {
        Error::new(
            ErrorKind::InvalidData,
            format!("redb record is missing unsigned integer field '{field}'"),
        )
    })
}

fn missing_indexed_book(id: &str) -> Error {
    Error::new(
        ErrorKind::InvalidData,
        format!("redb secondary index references missing book '{id}'"),
    )
}

fn missing_indexed_entity(id: &str) -> Error {
    Error::new(
        ErrorKind::InvalidData,
        format!("redb book references missing entity '{id}'"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn redb_record_helpers_reject_invalid_data() {
        let invalid_json = decode_json(b"not-json").expect_err("invalid JSON should fail");
        assert_eq!(invalid_json.kind(), ErrorKind::Other);

        let record = json!({"string": 7, "unsigned": "7"});
        let string_error =
            json_string_field(&record, "string").expect_err("non-string field should fail");
        assert_eq!(string_error.kind(), ErrorKind::InvalidData);
        let unsigned_error =
            json_u64_field(&record, "unsigned").expect_err("non-integer field should fail");
        assert_eq!(unsigned_error.kind(), ErrorKind::InvalidData);

        assert_eq!(
            missing_indexed_book("book-missing").kind(),
            ErrorKind::InvalidData
        );
        assert_eq!(
            missing_indexed_entity("entity-missing").kind(),
            ErrorKind::InvalidData
        );
    }
}
