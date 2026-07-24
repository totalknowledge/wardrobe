use super::storage_format::{BsonBinaryFormat, NativeBinaryIndexFormat};
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

    pub fn read_record_at_offset(
        &self,
        byte_offset: u64,
        field_name_map: Option<&std::collections::BTreeMap<String, String>>,
    ) -> std::io::Result<Option<Value>> {
        if let Some(record_bytes) = self.read_raw_bytes_at_offset(byte_offset)? {
            return BsonBinaryFormat::deserialize_record_with_map(&record_bytes, field_name_map);
        }

        Ok(None)
    }

    pub fn read_records_at_offsets<I>(
        &self,
        offsets: I,
        field_name_map: Option<&std::collections::BTreeMap<String, String>>,
    ) -> std::io::Result<Vec<Value>>
    where
        I: IntoIterator<Item = u64>,
    {
        let mut reader = self.reader.lock().map_err(|_| {
            std::io::Error::other("DatabaseReader lock was poisoned during batch read")
        })?;
        let file_len = reader.get_ref().metadata()?.len();

        let mut offset_list: Vec<u64> = offsets.into_iter().collect();
        offset_list.sort_unstable();

        let mut records = Vec::with_capacity(offset_list.len());
        let mut header = [0u8; 16];
        let mut slot_buf = Vec::new();
        let mut current_pos = 0u64;
        let mut is_first = true;

        for offset in offset_list {
            if offset >= file_len {
                continue;
            }
            if offset + 16 > file_len {
                continue;
            }

            if is_first || offset != current_pos {
                reader.seek(SeekFrom::Start(offset))?;
                is_first = false;
            }

            reader.read_exact(&mut header)?;

            let slot_size = if BsonBinaryFormat::is_binary_frame(&header) {
                BsonBinaryFormat::parse_slot_size(&header)?.unwrap()
            } else if NativeBinaryIndexFormat::is_binary_frame(&header) {
                NativeBinaryIndexFormat::parse_slot_size(&header)?.unwrap()
            } else {
                continue;
            };

            if offset + slot_size as u64 > file_len {
                continue;
            }

            slot_buf.resize(slot_size, 0);
            slot_buf[..16].copy_from_slice(&header);
            reader.read_exact(&mut slot_buf[16..])?;

            if let Some(record) =
                BsonBinaryFormat::deserialize_record_with_map(&slot_buf, field_name_map)?
            {
                records.push(record);
            }

            current_pos = offset + slot_size as u64;
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

        let header_len = 16;
        if byte_offset + header_len as u64 > file_len {
            let remaining = file_len - byte_offset;
            let probe_len = remaining.min(4) as usize;
            let mut probe = vec![0u8; probe_len];
            reader.seek(SeekFrom::Start(byte_offset))?;
            reader.read_exact(&mut probe)?;
            let matches_magic = (probe.as_slice() == &b"WRDB"[..probe_len])
                || (probe.as_slice() == &b"WIDX"[..probe_len]);
            if !matches_magic {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "Legacy newline-delimited records are no longer supported; migrate to WRDB/WIDX binary frames",
                ));
            }

            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "WRDB/WIDX binary frame header exceeds file length",
            ));
        }

        reader.seek(SeekFrom::Start(byte_offset))?;
        let mut header = vec![0u8; header_len];
        reader.read_exact(&mut header)?;

        let slot_size = if BsonBinaryFormat::is_binary_frame(&header) {
            BsonBinaryFormat::parse_slot_size(&header)?.unwrap()
        } else if NativeBinaryIndexFormat::is_binary_frame(&header) {
            NativeBinaryIndexFormat::parse_slot_size(&header)?.unwrap()
        } else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Legacy newline-delimited records are no longer supported; migrate to WRDB/WIDX binary frames",
            ));
        };
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
            let header_len = 16;
            if track_offset + header_len as u64 > file_len {
                let remaining = file_len - track_offset;
                let probe_len = remaining.min(4) as usize;
                let mut probe = vec![0u8; probe_len];
                reader.seek(SeekFrom::Start(track_offset))?;
                reader.read_exact(&mut probe)?;
                let matches_magic = (probe.as_slice() == &b"WRDB"[..probe_len])
                    || (probe.as_slice() == &b"WIDX"[..probe_len]);
                if !matches_magic {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "Legacy newline-delimited records are no longer supported; migrate to WRDB/WIDX binary frames",
                    ));
                }

                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "WRDB/WIDX binary frame header exceeds file length",
                ));
            }

            reader.seek(SeekFrom::Start(track_offset))?;
            let mut header = vec![0u8; header_len];
            reader.read_exact(&mut header)?;
            let slot_size = if BsonBinaryFormat::is_binary_frame(&header) {
                BsonBinaryFormat::parse_slot_size(&header)?.unwrap()
            } else if NativeBinaryIndexFormat::is_binary_frame(&header) {
                NativeBinaryIndexFormat::parse_slot_size(&header)?.unwrap()
            } else {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "Legacy newline-delimited records are no longer supported; migrate to WRDB/WIDX binary frames",
                ));
            };
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
