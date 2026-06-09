use serde_json::Value;
use std::io::{Error, ErrorKind};

pub trait StorageFormat {
    fn serialize_record(value: &Value) -> std::io::Result<Vec<u8>>;
    fn deserialize_record(bytes: &[u8]) -> std::io::Result<Option<Value>>;
    fn tombstone_marker() -> &'static [u8];

    fn is_tombstone(bytes: &[u8]) -> bool {
        trim_record_bytes(bytes).starts_with(Self::tombstone_marker())
    }
}

pub struct PlainTextJsonFormat;

impl StorageFormat for PlainTextJsonFormat {
    fn serialize_record(value: &Value) -> std::io::Result<Vec<u8>> {
        serde_json::to_vec(value).map_err(|err| Error::new(ErrorKind::InvalidData, err))
    }

    fn deserialize_record(bytes: &[u8]) -> std::io::Result<Option<Value>> {
        let normalized = trim_record_bytes(bytes);
        if normalized.is_empty() || Self::is_tombstone(normalized) {
            return Ok(None);
        }

        Ok(serde_json::from_slice(normalized).ok())
    }

    fn tombstone_marker() -> &'static [u8] {
        b"!!DEAD!!"
    }
}

fn trim_record_bytes(bytes: &[u8]) -> &[u8] {
    let mut end = bytes.len();
    while end > 0 && bytes[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    &bytes[..end]
}
