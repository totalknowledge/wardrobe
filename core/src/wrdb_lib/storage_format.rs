use bson::Document;
use serde_json::Value;
use std::io::{Error, ErrorKind};

const BSON_FRAME_MAGIC: [u8; 4] = *b"WRDB";
const BSON_FRAME_VERSION: u8 = 1;
const BSON_FLAG_TOMBSTONE: u8 = 0b0000_0001;
const BSON_FRAME_HEADER_LEN: usize = 16;
const BSON_FRAME_SLOT_LEN_RANGE: std::ops::Range<usize> = 12..16;
const BSON_FRAME_PAYLOAD_LEN_RANGE: std::ops::Range<usize> = 8..12;

pub trait StorageFormat {
    fn serialize_record(value: &Value) -> std::io::Result<Vec<u8>>;
    fn deserialize_record(bytes: &[u8]) -> std::io::Result<Option<Value>>;
    fn is_tombstone(bytes: &[u8]) -> bool;
}

pub struct BsonBinaryFormat;

impl BsonBinaryFormat {
    pub fn frame_header_len() -> usize {
        BSON_FRAME_HEADER_LEN
    }

    pub fn is_binary_frame(bytes: &[u8]) -> bool {
        bytes.len() >= BSON_FRAME_HEADER_LEN && bytes[..4] == BSON_FRAME_MAGIC
    }

    pub fn parse_slot_size(bytes: &[u8]) -> std::io::Result<Option<usize>> {
        if !Self::is_binary_frame(bytes) {
            return Ok(None);
        }

        let slot_len = u32::from_be_bytes(
            bytes[BSON_FRAME_SLOT_LEN_RANGE.clone()]
                .try_into()
                .map_err(|_| Error::new(ErrorKind::InvalidData, "Invalid BSON frame header"))?,
        ) as usize;
        if slot_len < BSON_FRAME_HEADER_LEN {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "Invalid BSON frame slot length",
            ));
        }

        Ok(Some(slot_len))
    }

    pub fn with_slot_size(encoded: &[u8], slot_size: usize) -> std::io::Result<Vec<u8>> {
        if !Self::is_binary_frame(encoded) {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "Cannot rewrite slot size for non-BSON frame",
            ));
        }
        if slot_size < encoded.len() || slot_size > u32::MAX as usize {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "Invalid slot size for BSON frame",
            ));
        }

        let mut rewritten = encoded.to_vec();
        rewritten[BSON_FRAME_SLOT_LEN_RANGE].copy_from_slice(&(slot_size as u32).to_be_bytes());
        Ok(rewritten)
    }

    pub fn tombstone_frame(slot_size: usize) -> std::io::Result<Vec<u8>> {
        if slot_size < BSON_FRAME_HEADER_LEN || slot_size > u32::MAX as usize {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "Invalid tombstone slot size",
            ));
        }

        let mut encoded = Vec::with_capacity(BSON_FRAME_HEADER_LEN);
        encoded.extend_from_slice(&BSON_FRAME_MAGIC);
        encoded.push(BSON_FRAME_VERSION);
        encoded.push(BSON_FLAG_TOMBSTONE);
        encoded.extend_from_slice(&0u16.to_be_bytes());
        encoded.extend_from_slice(&0u32.to_be_bytes());
        encoded.extend_from_slice(&(slot_size as u32).to_be_bytes());
        Ok(encoded)
    }

    fn parse_frame(bytes: &[u8]) -> std::io::Result<Option<(u8, usize, usize)>> {
        if !Self::is_binary_frame(bytes) {
            return Ok(None);
        }

        let version = bytes[4];
        if version != BSON_FRAME_VERSION {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "Unsupported BSON frame version",
            ));
        }

        let flags = bytes[5];
        let payload_len = u32::from_be_bytes(
            bytes[BSON_FRAME_PAYLOAD_LEN_RANGE.clone()]
                .try_into()
                .map_err(|_| Error::new(ErrorKind::InvalidData, "Invalid BSON frame payload"))?,
        ) as usize;
        let slot_len = u32::from_be_bytes(
            bytes[BSON_FRAME_SLOT_LEN_RANGE.clone()]
                .try_into()
                .map_err(|_| Error::new(ErrorKind::InvalidData, "Invalid BSON frame slot"))?,
        ) as usize;
        let frame_len = BSON_FRAME_HEADER_LEN
            .checked_add(payload_len)
            .ok_or_else(|| Error::new(ErrorKind::InvalidData, "BSON frame length overflow"))?;

        if slot_len < frame_len || slot_len > bytes.len() {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "Corrupt BSON frame slot bounds",
            ));
        }

        Ok(Some((flags, frame_len, slot_len)))
    }
}

impl StorageFormat for BsonBinaryFormat {
    fn serialize_record(value: &Value) -> std::io::Result<Vec<u8>> {
        let document = bson::to_document(value)
            .map_err(|err| Error::new(ErrorKind::InvalidInput, err.to_string()))?;
        let bson_payload = bson::to_vec(&document)
            .map_err(|err| Error::new(ErrorKind::InvalidData, err.to_string()))?;
        let frame_len = BSON_FRAME_HEADER_LEN
            .checked_add(bson_payload.len())
            .ok_or_else(|| Error::new(ErrorKind::InvalidData, "BSON frame length overflow"))?;
        if frame_len > u32::MAX as usize {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "BSON payload exceeds maximum supported size",
            ));
        }

        let mut encoded = Vec::with_capacity(frame_len);
        encoded.extend_from_slice(&BSON_FRAME_MAGIC);
        encoded.push(BSON_FRAME_VERSION);
        encoded.push(0);
        encoded.extend_from_slice(&0u16.to_be_bytes());
        encoded.extend_from_slice(&(bson_payload.len() as u32).to_be_bytes());
        encoded.extend_from_slice(&(frame_len as u32).to_be_bytes());
        encoded.extend_from_slice(&bson_payload);
        Ok(encoded)
    }

    fn deserialize_record(bytes: &[u8]) -> std::io::Result<Option<Value>> {
        if let Some((flags, frame_len, _slot_len)) = Self::parse_frame(bytes)? {
            if flags & BSON_FLAG_TOMBSTONE != 0 {
                return Ok(None);
            }

            let payload = &bytes[BSON_FRAME_HEADER_LEN..frame_len];
            let document = bson::from_slice::<Document>(payload)
                .map_err(|err| Error::new(ErrorKind::InvalidData, err.to_string()))?;
            let value = serde_json::to_value(document)
                .map_err(|err| Error::new(ErrorKind::InvalidData, err.to_string()))?;
            return Ok(Some(value));
        }

        Err(Error::new(
            ErrorKind::InvalidData,
            "WRDB binary frame expected; legacy text storage is no longer supported",
        ))
    }

    fn is_tombstone(bytes: &[u8]) -> bool {
        if let Ok(Some((flags, _frame_len, _slot_len))) = Self::parse_frame(bytes) {
            return flags & BSON_FLAG_TOMBSTONE != 0;
        }

        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn bson_frame_roundtrip_serialize_and_deserialize() {
        let val = json!({"a": 1});
        let encoded = BsonBinaryFormat::serialize_record(&val).expect("serialize");
        assert!(BsonBinaryFormat::is_binary_frame(&encoded));
        let decoded = BsonBinaryFormat::deserialize_record(&encoded).expect("deserialize");
        assert_eq!(decoded.unwrap()["a"], 1);
    }

    #[test]
    fn parse_slot_size_for_non_bson_returns_none() {
        let bytes = b"not a frame".to_vec();
        let slot = BsonBinaryFormat::parse_slot_size(&bytes).expect("parse should not fail");
        assert!(slot.is_none());
    }

    #[test]
    fn with_slot_size_errors_for_non_bson_frame() {
        let res = BsonBinaryFormat::with_slot_size(b"nope", 1024);
        assert!(res.is_err());
    }

    #[test]
    fn tombstone_frame_is_detected_as_tombstone() {
        let slot = BsonBinaryFormat::frame_header_len();
        let t = BsonBinaryFormat::tombstone_frame(slot).expect("tombstone should create");
        assert!(BsonBinaryFormat::is_tombstone(&t));
    }

    #[test]
    fn parse_frame_invalid_version_is_error() {
        let mut frame = BsonBinaryFormat::serialize_record(&json!({"x":1})).expect("serialize");
        frame[4] = 0xff; // bad version
        let res = BsonBinaryFormat::parse_frame(&frame);
        assert!(res.is_err());
    }
}
