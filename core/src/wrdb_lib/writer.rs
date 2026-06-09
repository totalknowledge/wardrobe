use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::Path;

use super::storage_format::{PlainTextJsonFormat, StorageFormat};

const PADDING_BUFFER_SIZE: usize = 512;

pub struct DatabaseWriter {
    file_handle: File,
}

impl DatabaseWriter {
    pub fn open_drawer<P: AsRef<Path>>(file_path: P) -> std::io::Result<Self> {
        let file_handle = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(file_path)?;

        Ok(Self { file_handle })
    }

    pub fn current_length(&self) -> std::io::Result<u64> {
        Ok(self.file_handle.metadata()?.len())
    }

    pub fn append_record(
        &mut self,
        record_payload: &[u8],
        alignment_chunk_size: usize,
    ) -> std::io::Result<u64> {
        let starting_offset = self.file_handle.seek(SeekFrom::End(0))?;

        self.write_aligned_payload(record_payload, alignment_chunk_size)?;

        Ok(starting_offset)
    }

    pub fn append_aligned_index(
        &mut self,
        index_payload: &[u8],
        alignment_chunk_size: usize,
    ) -> std::io::Result<u64> {
        let starting_offset = self.file_handle.seek(SeekFrom::End(0))?;

        self.write_aligned_payload(index_payload, alignment_chunk_size)?;

        Ok(starting_offset)
    }

    pub fn overwrite_at_offset(
        &mut self,
        byte_offset: u64,
        replacement_payload: &[u8],
        alignment_chunk_size: usize,
    ) -> std::io::Result<()> {
        self.file_handle.seek(SeekFrom::Start(byte_offset))?;

        self.write_aligned_payload(replacement_payload, alignment_chunk_size)
    }

    pub fn write_tombstone_at_offset(
        &mut self,
        byte_offset: u64,
        alignment_chunk_size: usize,
    ) -> std::io::Result<()> {
        self.file_handle.seek(SeekFrom::Start(byte_offset))?;

        let tombstone_prefix = PlainTextJsonFormat::tombstone_marker();
        let remaining_padding = alignment_chunk_size - 1 - tombstone_prefix.len();

        self.file_handle.write_all(tombstone_prefix)?;
        self.write_padding(remaining_padding)?;
        self.file_handle.write_all(b"\n")?;
        self.file_handle.flush()?;

        Ok(())
    }

    fn write_aligned_payload(
        &mut self,
        payload: &[u8],
        alignment_chunk_size: usize,
    ) -> std::io::Result<()> {
        let current_length = payload.len() + 1;

        let padding_needed = if current_length % alignment_chunk_size == 0 {
            0
        } else {
            alignment_chunk_size - (current_length % alignment_chunk_size)
        };

        self.file_handle.write_all(payload)?;
        self.write_padding(padding_needed)?;
        self.file_handle.write_all(b"\n")?;
        self.file_handle.flush()?;

        Ok(())
    }

    fn write_padding(&mut self, mut padding_needed: usize) -> std::io::Result<()> {
        let padding_buffer = [b' '; PADDING_BUFFER_SIZE];

        while padding_needed > 0 {
            let chunk_size = padding_needed.min(padding_buffer.len());
            self.file_handle.write_all(&padding_buffer[..chunk_size])?;
            padding_needed -= chunk_size;
        }

        Ok(())
    }
}
