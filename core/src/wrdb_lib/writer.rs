use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::Path;

use super::storage_format::BsonBinaryFormat;

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

    pub fn rewrite_all(&mut self, contents: &[u8]) -> std::io::Result<()> {
        self.file_handle.set_len(0)?;
        self.file_handle.seek(SeekFrom::Start(0))?;
        self.file_handle.write_all(contents)?;
        self.file_handle.flush()?;
        self.file_handle.sync_all()?;

        Ok(())
    }

    pub fn sync_all(&mut self) -> std::io::Result<()> {
        self.file_handle.sync_all()?;
        Ok(())
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
        if alignment_chunk_size < BsonBinaryFormat::frame_header_len() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Alignment chunk size is too small for BSON tombstone frame",
            ));
        }

        self.file_handle.seek(SeekFrom::Start(byte_offset))?;

        let tombstone_frame = BsonBinaryFormat::tombstone_frame(alignment_chunk_size)?;
        let remaining_padding = alignment_chunk_size - tombstone_frame.len();
        self.file_handle.write_all(&tombstone_frame)?;
        self.write_padding(remaining_padding)?;
        self.file_handle.flush()?;

        Ok(())
    }

    fn write_aligned_payload(
        &mut self,
        payload: &[u8],
        alignment_chunk_size: usize,
    ) -> std::io::Result<()> {
        if alignment_chunk_size == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Alignment chunk size must be greater than zero",
            ));
        }

        if !BsonBinaryFormat::is_binary_frame(payload) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "WRDB binary frame expected; legacy text storage is no longer supported",
            ));
        }

        let slot_size = if alignment_chunk_size < payload.len() {
            let remainder = payload.len() % alignment_chunk_size;
            if remainder == 0 {
                payload.len()
            } else {
                payload.len() + (alignment_chunk_size - remainder)
            }
        } else {
            alignment_chunk_size
        };

        let payload = BsonBinaryFormat::with_slot_size(payload, slot_size)?;
        let padding_needed = slot_size - payload.len();
        self.file_handle.write_all(&payload)?;
        self.write_padding(padding_needed)?;
        self.file_handle.flush()?;
        Ok(())
    }

    fn write_padding(&mut self, padding_needed: usize) -> std::io::Result<()> {
        let mut padding_needed = padding_needed;
        let padding_buffer = [0u8; PADDING_BUFFER_SIZE];

        while padding_needed > 0 {
            let chunk_size = padding_needed.min(padding_buffer.len());
            self.file_handle.write_all(&padding_buffer[..chunk_size])?;
            padding_needed -= chunk_size;
        }

        Ok(())
    }
}
