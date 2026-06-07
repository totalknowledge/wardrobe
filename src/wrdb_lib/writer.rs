use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::Path;

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
        serialized_record: &str,
        alignment_chunk_size: usize,
    ) -> std::io::Result<u64> {
        let starting_offset = self.file_handle.seek(SeekFrom::End(0))?;

        let raw_bytes = serialized_record.trim_end().as_bytes();
        let current_length = raw_bytes.len() + 1;

        let padding_needed = if current_length % alignment_chunk_size == 0 {
            0
        } else {
            alignment_chunk_size - (current_length % alignment_chunk_size)
        };

        self.file_handle.write_all(raw_bytes)?;
        self.file_handle.write_all(&vec![b' '; padding_needed])?;
        self.file_handle.write_all(b"\n")?;
        self.file_handle.flush()?;

        Ok(starting_offset)
    }

    pub fn append_aligned_index(
        &mut self,
        serialized_index: &str,
        alignment_chunk_size: usize,
    ) -> std::io::Result<u64> {
        let starting_offset = self.file_handle.seek(SeekFrom::End(0))?;

        let raw_bytes = serialized_index.trim_end().as_bytes();
        let current_length = raw_bytes.len() + 1;

        let padding_needed = if current_length % alignment_chunk_size == 0 {
            0
        } else {
            alignment_chunk_size - (current_length % alignment_chunk_size)
        };

        self.file_handle.write_all(raw_bytes)?;
        self.file_handle.write_all(&vec![b' '; padding_needed])?;
        self.file_handle.write_all(b"\n")?;
        self.file_handle.flush()?;

        Ok(starting_offset)
    }

    pub fn overwrite_at_offset(
        &mut self,
        byte_offset: u64,
        replacement_payload: &str,
        alignment_chunk_size: usize,
    ) -> std::io::Result<()> {
        self.file_handle.seek(SeekFrom::Start(byte_offset))?;

        let raw_bytes = replacement_payload.trim_end().as_bytes();
        let current_length = raw_bytes.len() + 1;

        let padding_needed = if current_length % alignment_chunk_size == 0 {
            0
        } else {
            alignment_chunk_size - (current_length % alignment_chunk_size)
        };

        self.file_handle.write_all(raw_bytes)?;
        self.file_handle.write_all(&vec![b' '; padding_needed])?;
        self.file_handle.write_all(b"\n")?;
        self.file_handle.flush()?;

        Ok(())
    }

    pub fn write_tombstone_at_offset(
        &mut self,
        byte_offset: u64,
        alignment_chunk_size: usize,
    ) -> std::io::Result<()> {
        self.file_handle.seek(SeekFrom::Start(byte_offset))?;

        let tombstone_prefix = "!!DEAD!!";
        let remaining_padding = alignment_chunk_size - 1 - tombstone_prefix.len();

        self.file_handle.write_all(tombstone_prefix.as_bytes())?;
        self.file_handle.write_all(&vec![b' '; remaining_padding])?;
        self.file_handle.write_all(b"\n")?;
        self.file_handle.flush()?;

        Ok(())
    }
}
