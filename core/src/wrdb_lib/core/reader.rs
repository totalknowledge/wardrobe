use super::storage_format::{BsonBinaryFormat, StorageFormat};
use serde_json::Value;
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Mutex;

pub struct DatabaseReader {
    reader: Mutex<BufReader<File>>,
}

impl DatabaseReader {
    pub fn open_drawer<P: AsRef<Path>>(file_path: P) -> std::io::Result<Self> {
        let file = File::open(file_path)?;
        Ok(Self {
            reader: Mutex::new(BufReader::new(file)),
        })
    }

    pub fn close(self) -> std::io::Result<()> {
        let reader = self
            .reader
            .into_inner()
            .map_err(|_| std::io::Error::other("DatabaseReader lock was poisoned during close"))?;
        drop(reader.into_inner());
        Ok(())
    }

    pub fn read_record_at_offset(&self, byte_offset: u64) -> std::io::Result<Option<Value>> {
        if let Some(record_bytes) = self.read_raw_bytes_at_offset(byte_offset)? {
            return BsonBinaryFormat::deserialize_record(&record_bytes);
        }

        Ok(None)
    }

    pub fn read_records_at_offsets<I>(&self, offsets: I) -> std::io::Result<Vec<Value>>
    where
        I: IntoIterator<Item = u64>,
    {
        let mut reader = self.reader.lock().map_err(|_| {
            std::io::Error::other("DatabaseReader lock was poisoned during batch read")
        })?;
        let file_len = reader.get_ref().metadata()?.len();
        let mut records = Vec::new();

        for offset in offsets {
            if let Some(record_bytes) =
                Self::read_raw_bytes_at_offset_locked(&mut reader, file_len, offset)?
            {
                if let Some(record) = BsonBinaryFormat::deserialize_record(&record_bytes)? {
                    records.push(record);
                }
            }
        }

        Ok(records)
    }

    pub fn read_raw_bytes_at_offset(&self, byte_offset: u64) -> std::io::Result<Option<Vec<u8>>> {
        let mut reader = self.reader.lock().map_err(|_| {
            std::io::Error::other("DatabaseReader lock was poisoned during raw read")
        })?;
        let file_len = reader.get_ref().metadata()?.len();
        Self::read_raw_bytes_at_offset_locked(&mut reader, file_len, byte_offset)
    }

    fn read_raw_bytes_at_offset_locked(
        reader: &mut BufReader<File>,
        file_len: u64,
        byte_offset: u64,
    ) -> std::io::Result<Option<Vec<u8>>> {
        if byte_offset >= file_len {
            return Ok(None);
        }

        let header_len = BsonBinaryFormat::frame_header_len();
        if byte_offset + header_len as u64 > file_len {
            let remaining = file_len - byte_offset;
            let probe_len = remaining.min(4) as usize;
            let mut probe = vec![0u8; probe_len];
            reader.seek(SeekFrom::Start(byte_offset))?;
            reader.read_exact(&mut probe)?;
            if probe.as_slice() != &b"WRDB"[..probe_len] {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "Legacy newline-delimited records are no longer supported; migrate to WRDB binary frames",
                ));
            }

            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "WRDB binary frame header exceeds file length",
            ));
        }

        reader.seek(SeekFrom::Start(byte_offset))?;
        let mut header = vec![0u8; header_len];
        reader.read_exact(&mut header)?;

        let slot_size = BsonBinaryFormat::parse_slot_size(&header)?.ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Legacy newline-delimited records are no longer supported; migrate to WRDB binary frames",
            )
        })?;
        if byte_offset + slot_size as u64 > file_len {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "Binary frame exceeds file length",
            ));
        }

        let mut slot = vec![0u8; slot_size];
        slot[..header_len].copy_from_slice(&header);
        reader.read_exact(&mut slot[header_len..])?;
        Ok(Some(slot))
    }

    pub fn stream_with_offsets<F>(&self, mut processing_closure: F) -> std::io::Result<()>
    where
        F: FnMut(u64, &[u8]),
    {
        let mut reader = self
            .reader
            .lock()
            .map_err(|_| std::io::Error::other("DatabaseReader lock was poisoned during stream"))?;
        reader.seek(SeekFrom::Start(0))?;
        let file_len = reader.get_ref().metadata()?.len();
        if file_len == 0 {
            return Ok(());
        }
        let mut track_offset = 0u64;
        while track_offset < file_len {
            reader.seek(SeekFrom::Start(track_offset))?;
            let header_len = BsonBinaryFormat::frame_header_len();
            if track_offset + header_len as u64 > file_len {
                let remaining = file_len - track_offset;
                let probe_len = remaining.min(4) as usize;
                let mut probe = vec![0u8; probe_len];
                reader.seek(SeekFrom::Start(track_offset))?;
                reader.read_exact(&mut probe)?;
                if probe.as_slice() != &b"WRDB"[..probe_len] {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "Legacy newline-delimited records are no longer supported; migrate to WRDB binary frames",
                    ));
                }

                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "WRDB binary frame header exceeds file length",
                ));
            }

            reader.seek(SeekFrom::Start(track_offset))?;
            let mut header = vec![0u8; header_len];
            reader.read_exact(&mut header)?;
            let slot_size = BsonBinaryFormat::parse_slot_size(&header)?.ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "Legacy newline-delimited records are no longer supported; migrate to WRDB binary frames",
                )
            })?;
            if track_offset + slot_size as u64 > file_len {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "Binary frame exceeds file length",
                ));
            }

            reader.seek(SeekFrom::Start(track_offset))?;
            let mut slot = vec![0u8; slot_size];
            reader.read_exact(&mut slot)?;
            processing_closure(track_offset, &slot);
            track_offset += slot_size as u64;
        }

        Ok(())
    }
}
