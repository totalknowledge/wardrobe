use super::storage_format::{BsonBinaryFormat, StorageFormat};
use serde_json::Value;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
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
            return BsonBinaryFormat::deserialize_record(&record_bytes);
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
        let mut frame_probe = [0u8; 4];
        drawer_file.read_exact(&mut frame_probe)?;

        if frame_probe == *b"WRDB" {
            drawer_file.seek(SeekFrom::Start(byte_offset))?;
            let mut header = vec![0u8; BsonBinaryFormat::frame_header_len()];
            drawer_file.read_exact(&mut header)?;

            let slot_size = BsonBinaryFormat::parse_slot_size(&header)?.ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "Binary frame missing slot size",
                )
            })?;
            if byte_offset + slot_size as u64 > file_len {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "Binary frame exceeds file length",
                ));
            }

            drawer_file.seek(SeekFrom::Start(byte_offset))?;
            let mut slot = vec![0u8; slot_size];
            drawer_file.read_exact(&mut slot)?;
            return Ok(Some(slot));
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
        F: FnMut(u64, &[u8]),
    {
        let mut drawer_file = File::open(&self.file_path)?;
        drawer_file.seek(SeekFrom::Start(0))?;
        let file_len = drawer_file.metadata()?.len();
        if file_len == 0 {
            return Ok(());
        }
        let mut track_offset = 0u64;
        while track_offset < file_len {
            drawer_file.seek(SeekFrom::Start(track_offset))?;
            let mut probe = [0u8; 4];
            let probe_read = drawer_file.read(&mut probe)?;
            if probe_read == 0 {
                break;
            }

            if probe_read == 4 && probe == *b"WRDB" {
                drawer_file.seek(SeekFrom::Start(track_offset))?;
                let mut header = vec![0u8; BsonBinaryFormat::frame_header_len()];
                drawer_file.read_exact(&mut header)?;
                let slot_size = BsonBinaryFormat::parse_slot_size(&header)?.ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "Binary frame missing slot size",
                    )
                })?;
                if track_offset + slot_size as u64 > file_len {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "Binary frame exceeds file length",
                    ));
                }

                drawer_file.seek(SeekFrom::Start(track_offset))?;
                let mut slot = vec![0u8; slot_size];
                drawer_file.read_exact(&mut slot)?;
                processing_closure(track_offset, &slot);
                track_offset += slot_size as u64;
            } else {
                drawer_file.seek(SeekFrom::Start(track_offset))?;
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
                    break;
                }
                processing_closure(track_offset, &byte_buffer);
                track_offset += byte_buffer.len() as u64;
            }
        }

        Ok(())
    }
}
