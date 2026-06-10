use super::storage_format::{PlainTextJsonFormat, StorageFormat};
use serde_json::Value;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

pub struct DatabaseReader {
    file_path: PathBuf,
}

impl DatabaseReader {
    pub fn open_drawer<P: AsRef<Path>>(file_path: P) -> std::io::Result<Self> {
        let file_path = file_path.as_ref().to_path_buf();
        File::open(&file_path)?;
        Ok(Self { file_path })
    }

    pub fn read_record_at_offset(&self, byte_offset: u64) -> std::io::Result<Option<Value>> {
        if let Some(record_bytes) = self.read_raw_bytes_at_offset(byte_offset)? {
            return PlainTextJsonFormat::deserialize_record(&record_bytes);
        }

        Ok(None)
    }

    pub fn read_raw_bytes_at_offset(&self, byte_offset: u64) -> std::io::Result<Option<Vec<u8>>> {
        let mut drawer_file = File::open(&self.file_path)?;
        let file_len = drawer_file.metadata()?.len();
        if byte_offset >= file_len {
            return Ok(None);
        }

        drawer_file.seek(SeekFrom::Start(byte_offset))?;
        let mut byte_buffer = Vec::new();
        let mut single_byte_chunk = [0u8; 1];

        loop {
            let bytes_read = drawer_file.read(&mut single_byte_chunk)?;
            if bytes_read == 0 {
                break;
            }
            byte_buffer.push(single_byte_chunk[0]);
            if single_byte_chunk[0] == b'\n' {
                break;
            }
        }

        if byte_buffer.is_empty() {
            Ok(None)
        } else {
            Ok(Some(byte_buffer))
        }
    }

    pub fn stream_with_offsets<F>(&self, mut processing_closure: F) -> std::io::Result<()>
    where
        F: FnMut(u64, &str),
    {
        let mut drawer_file = File::open(&self.file_path)?;
        drawer_file.seek(SeekFrom::Start(0))?;
        let reader = BufReader::new(drawer_file);
        let mut track_offset = 0u64;

        for line_entry in reader.lines() {
            let line_content = line_entry?;
            let line_len_with_newline = line_content.len() + 1;

            processing_closure(track_offset, &line_content);
            track_offset += line_len_with_newline as u64;
        }

        Ok(())
    }
}
