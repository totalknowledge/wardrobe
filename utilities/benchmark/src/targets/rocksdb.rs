use super::{verify_deleted_count, verify_record_id, verify_record_range, BenchmarkTarget};
use crate::config::LibraryProfile;
use crate::engine::{report_record_progress, PhaseRecorder, ProgressReporter};
use crate::utils::{chunk_ranges, file_size_or_zero, sync_file_if_exists, to_io_error};
use rocksdb::{ColumnFamilyDescriptor, Options, WriteBatch, DB};
use serde_json::Value;
use std::collections::HashSet;
use std::fs;
use std::io::{self, Error, ErrorKind};
use std::path::{Path, PathBuf};

const CF_ENTITIES: &str = "entities";
const CF_BOOKS: &str = "books";
const CF_AUTHOR_BOOKS: &str = "author_books";
const CF_EDITOR_BOOKS: &str = "editor_books";
const CF_QUANTITY_BOOKS: &str = "quantity_books";
const CF_PURGE_BUCKET_BOOKS: &str = "purge_bucket_books";
const CF_ISBN_BOOKS: &str = "isbn_books";

const ALL_CFS: &[&str] = &[
    CF_ENTITIES,
    CF_BOOKS,
    CF_AUTHOR_BOOKS,
    CF_EDITOR_BOOKS,
    CF_QUANTITY_BOOKS,
    CF_PURGE_BUCKET_BOOKS,
    CF_ISBN_BOOKS,
];

pub(crate) struct RocksdbTarget {
    db: DB,
    path: PathBuf,
}

impl RocksdbTarget {
    pub(crate) fn new(path: PathBuf) -> io::Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let db = open_db(&path)?;
        Ok(Self { db, path })
    }

    fn cf_handle(&self, name: &str) -> io::Result<&rocksdb::ColumnFamily> {
        self.db.cf_handle(name).ok_or_else(|| {
            Error::new(
                ErrorKind::NotFound,
                format!("RocksDB column family '{name}' not found"),
            )
        })
    }

    fn ingest_entities(
        &self,
        profile: &LibraryProfile,
        start: usize,
        end: usize,
    ) -> io::Result<()> {
        let cf = self.cf_handle(CF_ENTITIES)?;
        let mut batch = WriteBatch::default();
        for index in start..end {
            let id = profile.entity_id(index);
            let payload = serde_json::to_vec(&profile.entity_payload(index)).map_err(to_io_error)?;
            batch.put_cf(cf, id.as_bytes(), &payload);
        }
        self.db.write(batch).map_err(to_io_error)
    }

    fn ingest_books(&self, profile: &LibraryProfile, start: usize, end: usize) -> io::Result<()> {
        let cf_books = self.cf_handle(CF_BOOKS)?;
        let cf_author = self.cf_handle(CF_AUTHOR_BOOKS)?;
        let cf_editor = self.cf_handle(CF_EDITOR_BOOKS)?;
        let cf_quantity = self.cf_handle(CF_QUANTITY_BOOKS)?;
        let cf_purge = self.cf_handle(CF_PURGE_BUCKET_BOOKS)?;

        let mut batch = WriteBatch::default();
        for index in start..end {
            let payload = profile.book_payload(index);
            let id = json_string_field(&payload, "_id")?;
            let author_id = json_string_field(&payload, "author_id")?;
            let editor_id = json_string_field(&payload, "editor_id")?;
            let quantity = json_u64_field(&payload, "quantity")?;
            let purge_bucket = json_u64_field(&payload, "purge_bucket")?;
            let encoded = serde_json::to_vec(&payload).map_err(to_io_error)?;

            batch.put_cf(cf_books, id.as_bytes(), &encoded);
            batch.put_cf(cf_author, index_key(author_id, id).as_bytes(), b"");
            batch.put_cf(cf_editor, index_key(editor_id, id).as_bytes(), b"");
            batch.put_cf(cf_quantity, numeric_index_key(quantity, id).as_bytes(), b"");
            batch.put_cf(cf_purge, numeric_index_key(purge_bucket, id).as_bytes(), b"");
        }
        self.db.write(batch).map_err(to_io_error)
    }

    fn build_isbn_index(&self) -> io::Result<()> {
        self.drop_isbn_index()?;
        let cf_books = self.cf_handle(CF_BOOKS)?;
        let cf_isbn = self.cf_handle(CF_ISBN_BOOKS)?;
        let mut batch = WriteBatch::default();

        let iter = self.db.iterator_cf(cf_books, rocksdb::IteratorMode::Start);
        for item in iter {
            let (id_bytes, payload_bytes) = item.map_err(to_io_error)?;
            let id = std::str::from_utf8(&id_bytes).map_err(to_io_error)?;
            let book = decode_json(&payload_bytes)?;
            let isbn = json_string_field(&book, "isbn")?;
            batch.put_cf(cf_isbn, index_key(isbn, id).as_bytes(), b"");
        }

        self.db.write(batch).map_err(to_io_error)
    }

    fn drop_isbn_index(&self) -> io::Result<()> {
        let cf_isbn = self.cf_handle(CF_ISBN_BOOKS)?;
        let mut batch = WriteBatch::default();
        let iter = self.db.iterator_cf(cf_isbn, rocksdb::IteratorMode::Start);
        for item in iter {
            let (key, _) = item.map_err(to_io_error)?;
            batch.delete_cf(cf_isbn, key);
        }
        self.db.write(batch).map_err(to_io_error)
    }

    fn read_book(&self, id: &str) -> io::Result<Value> {
        let cf_books = self.cf_handle(CF_BOOKS)?;
        let payload = self
            .db
            .get_cf(cf_books, id.as_bytes())
            .map_err(to_io_error)?
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::NotFound,
                    format!("RocksDB point lookup did not find book '{id}'"),
                )
            })?;
        decode_json(&payload)
    }

    fn delete_book(&self, id: &str) -> io::Result<()> {
        let cf_books = self.cf_handle(CF_BOOKS)?;
        let payload = self
            .db
            .get_cf(cf_books, id.as_bytes())
            .map_err(to_io_error)?;
        let removed = payload.is_some();
        verify_deleted_count(usize::from(removed), id)?;
        let book = decode_json(&payload.unwrap())?;

        let mut batch = WriteBatch::default();
        batch.delete_cf(cf_books, id.as_bytes());
        remove_book_indexes(&self.db, &mut batch, id, &book)?;
        self.db.write(batch).map_err(to_io_error)?;

        if self.db.get_cf(cf_books, id.as_bytes()).map_err(to_io_error)?.is_none() {
            Ok(())
        } else {
            Err(Error::new(
                ErrorKind::InvalidData,
                format!("RocksDB delete-by-ID left book '{id}' behind"),
            ))
        }
    }

    fn purge_bucket_zero(&self, expected: usize) -> io::Result<()> {
        let cf_purge = self.cf_handle(CF_PURGE_BUCKET_BOOKS)?;
        let cf_books = self.cf_handle(CF_BOOKS)?;
        let prefix = numeric_prefix(0);

        let mut ids = Vec::new();
        let iter = self.db.prefix_iterator_cf(cf_purge, prefix.as_bytes());
        for item in iter {
            let (key_bytes, _) = item.map_err(to_io_error)?;
            let key_str = std::str::from_utf8(&key_bytes).map_err(to_io_error)?;
            if !key_str.starts_with(&prefix) {
                break;
            }
            if let Some((_, book_id)) = key_str.split_once(':') {
                ids.push(book_id.to_string());
            }
        }

        let mut removed_books = Vec::with_capacity(ids.len());
        for id in &ids {
            if let Some(payload) = self.db.get_cf(cf_books, id.as_bytes()).map_err(to_io_error)? {
                removed_books.push((id.clone(), decode_json(&payload)?));
            }
        }

        if removed_books.len() != expected {
            return Err(Error::new(
                ErrorKind::InvalidData,
                format!(
                    "RocksDB targeted purge expected to remove {expected} records, removed {}",
                    removed_books.len()
                ),
            ));
        }

        let mut batch = WriteBatch::default();
        for (id, book) in &removed_books {
            batch.delete_cf(cf_books, id.as_bytes());
            remove_book_indexes(&self.db, &mut batch, id, book)?;
        }
        self.db.write(batch).map_err(to_io_error)
    }
}

impl BenchmarkTarget for RocksdbTarget {
    fn name(&self) -> &str {
        "RocksDB (Embedded Key-Value Mode)"
    }

    fn provision_schema(
        &mut self,
        _profile: &LibraryProfile,
        progress: &ProgressReporter,
    ) -> io::Result<()> {
        progress.log(format!("{}: resetting column families", self.name()));
        for cf_name in ALL_CFS {
            let handle = self.cf_handle(cf_name)?;
            let iter = self.db.iterator_cf(handle, rocksdb::IteratorMode::Start);
            let mut batch = WriteBatch::default();
            for item in iter {
                let (key, _) = item.map_err(to_io_error)?;
                batch.delete_cf(handle, key);
            }
            self.db.write(batch).map_err(to_io_error)?;
        }
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
            "{}: index mutation step 1/3: create ISBN index",
            self.name()
        ));
        recorder.measure(1, || self.build_isbn_index())?;
        progress.log(format!(
            "{}: index mutation step 2/3: drop ISBN index",
            self.name()
        ));
        recorder.measure(1, || self.drop_isbn_index())?;
        progress.log(format!(
            "{}: index mutation step 3/3: rebuild ISBN index",
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
        let cf_quantity = self.cf_handle(CF_QUANTITY_BOOKS)?;
        let cf_books = self.cf_handle(CF_BOOKS)?;
        let bounds = profile.range_lookup_bounds();

        for (index, (low, high)) in bounds.iter().enumerate() {
            recorder.measure(1, || {
                let low_prefix = numeric_prefix(*low as u64);
                let iter = self.db.prefix_iterator_cf(cf_quantity, low_prefix.as_bytes());
                for item in iter {
                    let (key_bytes, _) = item.map_err(to_io_error)?;
                    let key_str = std::str::from_utf8(&key_bytes).map_err(to_io_error)?;
                    let Some((qty_str, book_id)) = key_str.split_once(':') else {
                        continue;
                    };
                    let qty: u64 = qty_str.parse().map_err(to_io_error)?;
                    if qty > (*high as u64) {
                        break;
                    }
                    if qty >= (*low as u64) {
                        let payload = self
                            .db
                            .get_cf(cf_books, book_id.as_bytes())
                            .map_err(to_io_error)?
                            .ok_or_else(|| missing_indexed_book(book_id))?;
                        let book = decode_json(&payload)?;
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
        let cf_author = self.cf_handle(CF_AUTHOR_BOOKS)?;
        let cf_editor = self.cf_handle(CF_EDITOR_BOOKS)?;
        let cf_books = self.cf_handle(CF_BOOKS)?;
        let cf_entities = self.cf_handle(CF_ENTITIES)?;

        for query_index in 0..profile.traversal_queries {
            let entity_id = profile.traversal_entity_id(query_index);
            recorder.measure(1, || {
                let prefix = format!("{entity_id}:");
                let mut author_ids = HashSet::new();
                let iter_author = self.db.prefix_iterator_cf(cf_author, prefix.as_bytes());
                for item in iter_author {
                    let (key_bytes, _) = item.map_err(to_io_error)?;
                    let key_str = std::str::from_utf8(&key_bytes).map_err(to_io_error)?;
                    if !key_str.starts_with(&prefix) {
                        break;
                    }
                    if let Some((_, book_id)) = key_str.split_once(':') {
                        author_ids.insert(book_id.to_string());
                    }
                }

                let mut editor_ids = HashSet::new();
                let iter_editor = self.db.prefix_iterator_cf(cf_editor, prefix.as_bytes());
                for item in iter_editor {
                    let (key_bytes, _) = item.map_err(to_io_error)?;
                    let key_str = std::str::from_utf8(&key_bytes).map_err(to_io_error)?;
                    if !key_str.starts_with(&prefix) {
                        break;
                    }
                    if let Some((_, book_id)) = key_str.split_once(':') {
                        editor_ids.insert(book_id.to_string());
                    }
                }

                for id in author_ids.intersection(&editor_ids) {
                    let book_payload = self
                        .db
                        .get_cf(cf_books, id.as_bytes())
                        .map_err(to_io_error)?
                        .ok_or_else(|| missing_indexed_book(id))?;
                    let book = decode_json(&book_payload)?;
                    let author_id = json_string_field(&book, "author_id")?;
                    let editor_id = json_string_field(&book, "editor_id")?;

                    let author_payload = self
                        .db
                        .get_cf(cf_entities, author_id.as_bytes())
                        .map_err(to_io_error)?
                        .ok_or_else(|| missing_indexed_entity(author_id))?;
                    let author = decode_json(&author_payload)?;

                    let editor_payload = self
                        .db
                        .get_cf(cf_entities, editor_id.as_bytes())
                        .map_err(to_io_error)?
                        .ok_or_else(|| missing_indexed_entity(editor_id))?;
                    let editor = decode_json(&editor_payload)?;

                    let mut materialized = book;
                    let object = materialized.as_object_mut().ok_or_else(|| {
                        Error::new(ErrorKind::InvalidData, "RocksDB book is not a JSON object")
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
            self.db
                .compact_range::<&[u8], &[u8]>(None, None);
            Ok(())
        })?;
        Ok(1)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.db.flush().map_err(to_io_error)?;
        sync_file_if_exists(&self.path)
    }

    fn storage_footprint_bytes(&mut self) -> io::Result<u64> {
        dir_size(&self.path)
    }

    fn storage_diagnostics(&mut self) -> io::Result<Vec<String>> {
        let stats = self
            .db
            .property_value("rocksdb.stats")
            .map_err(to_io_error)?
            .unwrap_or_else(|| "no stats".to_string());
        Ok(vec![format!("RocksDB stats:\n{stats}")])
    }
}

fn open_db(path: &Path) -> io::Result<DB> {
    let mut db_opts = Options::default();
    db_opts.create_if_missing(true);
    db_opts.create_missing_column_families(true);

    let cfs = match DB::list_cf(&db_opts, path) {
        Ok(cfs) => cfs,
        Err(_) => vec!["default".to_string()],
    };

    let mut descriptors = Vec::new();
    let mut known_cfs: HashSet<String> = cfs.into_iter().collect();
    known_cfs.insert("default".to_string());
    for cf in ALL_CFS {
        known_cfs.insert((*cf).to_string());
    }

    for cf in known_cfs {
        descriptors.push(ColumnFamilyDescriptor::new(cf, Options::default()));
    }

    DB::open_cf_descriptors(&db_opts, path, descriptors).map_err(to_io_error)
}

fn remove_book_indexes(db: &DB, batch: &mut WriteBatch, id: &str, book: &Value) -> io::Result<()> {
    let cf_author = db
        .cf_handle(CF_AUTHOR_BOOKS)
        .ok_or_else(|| Error::new(ErrorKind::NotFound, "CF author_books not found"))?;
    let cf_editor = db
        .cf_handle(CF_EDITOR_BOOKS)
        .ok_or_else(|| Error::new(ErrorKind::NotFound, "CF editor_books not found"))?;
    let cf_quantity = db
        .cf_handle(CF_QUANTITY_BOOKS)
        .ok_or_else(|| Error::new(ErrorKind::NotFound, "CF quantity_books not found"))?;
    let cf_purge = db
        .cf_handle(CF_PURGE_BUCKET_BOOKS)
        .ok_or_else(|| Error::new(ErrorKind::NotFound, "CF purge_bucket_books not found"))?;

    let author_id = json_string_field(book, "author_id")?;
    let editor_id = json_string_field(book, "editor_id")?;
    let quantity = json_u64_field(book, "quantity")?;
    let purge_bucket = json_u64_field(book, "purge_bucket")?;

    batch.delete_cf(cf_author, index_key(author_id, id).as_bytes());
    batch.delete_cf(cf_editor, index_key(editor_id, id).as_bytes());
    batch.delete_cf(cf_quantity, numeric_index_key(quantity, id).as_bytes());
    batch.delete_cf(cf_purge, numeric_index_key(purge_bucket, id).as_bytes());

    if let Ok(cf_isbn) = db
        .cf_handle(CF_ISBN_BOOKS)
        .ok_or_else(|| Error::new(ErrorKind::NotFound, "CF isbn_books not found"))
    {
        if let Ok(isbn) = json_string_field(book, "isbn") {
            batch.delete_cf(cf_isbn, index_key(isbn, id).as_bytes());
        }
    }

    Ok(())
}

fn index_key(prefix: &str, id: &str) -> String {
    format!("{prefix}:{id}")
}

fn numeric_prefix(value: u64) -> String {
    format!("{value:020}:")
}

fn numeric_index_key(value: u64, id: &str) -> String {
    format!("{value:020}:{id}")
}

fn decode_json(bytes: &[u8]) -> io::Result<Value> {
    serde_json::from_slice(bytes).map_err(to_io_error)
}

fn json_string_field<'a>(value: &'a Value, field: &str) -> io::Result<&'a str> {
    value.get(field).and_then(Value::as_str).ok_or_else(|| {
        Error::new(
            ErrorKind::InvalidData,
            format!("RocksDB record is missing string field '{field}'"),
        )
    })
}

fn json_u64_field(value: &Value, field: &str) -> io::Result<u64> {
    value.get(field).and_then(Value::as_u64).ok_or_else(|| {
        Error::new(
            ErrorKind::InvalidData,
            format!("RocksDB record is missing unsigned integer field '{field}'"),
        )
    })
}

fn missing_indexed_book(id: &str) -> Error {
    Error::new(
        ErrorKind::InvalidData,
        format!("RocksDB secondary index references missing book '{id}'"),
    )
}

fn missing_indexed_entity(id: &str) -> Error {
    Error::new(
        ErrorKind::InvalidData,
        format!("RocksDB book references missing entity '{id}'"),
    )
}

fn dir_size(path: &Path) -> io::Result<u64> {
    if !path.exists() {
        return Ok(0);
    }
    if path.is_file() {
        return file_size_or_zero(path);
    }
    let mut total = 0;
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let entry_path = entry.path();
        if entry_path.is_file() {
            total += file_size_or_zero(&entry_path)?;
        } else if entry_path.is_dir() {
            total += dir_size(&entry_path)?;
        }
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rocksdb_record_helpers_reject_invalid_data() {
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
