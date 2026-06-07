use serde_json::Value;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};

pub struct DatabaseReader {
    drawer_file: File,
}

impl DatabaseReader {
    pub fn open_drawer<P: AsRef<std::path::Path>>(file_path: P) -> std::io::Result<Self> {
        let drawer_file = File::open(file_path)?;
        Ok(Self { drawer_file })
    }

    pub fn read_record_at_offset(&mut self, byte_offset: u64) -> std::io::Result<Option<Value>> {
        let file_len = self.drawer_file.metadata()?.len();
        if byte_offset >= file_len {
            return Ok(None);
        }

        self.drawer_file.seek(SeekFrom::Start(byte_offset))?;
        let reader = BufReader::new(&mut self.drawer_file);

        if let Some(line_result) = reader.lines().next() {
            let clear_line = line_result?;
            let normalized_line = clear_line.trim_end();

            if normalized_line.starts_with("!!DEAD!!") || normalized_line.is_empty() {
                return Ok(None);
            }

            if let Ok(parsed_json) = serde_json::from_str::<Value>(normalized_line) {
                return Ok(Some(parsed_json));
            }
        }

        Ok(None)
    }

    pub fn read_raw_bytes_at_offset(
        &mut self,
        byte_offset: u64,
    ) -> std::io::Result<Option<Vec<u8>>> {
        let file_len = self.drawer_file.metadata()?.len();
        if byte_offset >= file_len {
            return Ok(None);
        }

        self.drawer_file.seek(SeekFrom::Start(byte_offset))?;
        let mut byte_buffer = Vec::new();
        let mut single_byte_chunk = [0u8; 1];

        loop {
            let bytes_read = self.drawer_file.read(&mut single_byte_chunk)?;
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

    pub fn stream_with_offsets<F>(&mut self, mut processing_closure: F) -> std::io::Result<()>
    where
        F: FnMut(u64, &str),
    {
        self.drawer_file.seek(SeekFrom::Start(0))?;
        let reader = BufReader::new(&mut self.drawer_file);
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
