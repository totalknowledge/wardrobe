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

pub struct NativeBinaryIndexFormat;

const WIDX_FRAME_MAGIC: [u8; 4] = *b"WIDX";
const WIDX_FRAME_VERSION: u8 = 1;
const WIDX_FLAG_TOMBSTONE: u8 = 0b0000_0001;
const WIDX_FRAME_HEADER_LEN: usize = 16;

fn write_varint(val: u64, buf: &mut Vec<u8>) {
    let mut temp = val;
    loop {
        let mut byte = (temp & 0x7F) as u8;
        temp >>= 7;
        if temp != 0 {
            byte |= 0x80;
        }
        buf.push(byte);
        if temp == 0 {
            break;
        }
    }
}

fn read_varint(bytes: &[u8], offset: &mut usize) -> std::io::Result<u64> {
    let mut result = 0u64;
    let mut shift = 0;
    loop {
        if *offset >= bytes.len() {
            return Err(Error::new(ErrorKind::UnexpectedEof, "EOF reading varint"));
        }
        let byte = bytes[*offset];
        *offset += 1;
        result |= ((byte & 0x7F) as u64) << shift;
        if (byte & 0x80) == 0 {
            break;
        }
        shift += 7;
        if shift >= 64 {
            return Err(Error::new(ErrorKind::InvalidData, "Varint overflow"));
        }
    }
    Ok(result)
}

impl NativeBinaryIndexFormat {
    pub fn frame_header_len() -> usize {
        WIDX_FRAME_HEADER_LEN
    }

    pub fn is_binary_frame(bytes: &[u8]) -> bool {
        bytes.len() >= WIDX_FRAME_HEADER_LEN && bytes[..4] == WIDX_FRAME_MAGIC
    }

    pub fn is_tombstone(bytes: &[u8]) -> bool {
        if bytes.len() >= WIDX_FRAME_HEADER_LEN && bytes[..4] == WIDX_FRAME_MAGIC {
            let version = bytes[4];
            if version == WIDX_FRAME_VERSION {
                let flags = bytes[5];
                return flags & WIDX_FLAG_TOMBSTONE != 0;
            }
        }
        false
    }

    pub fn parse_slot_size(bytes: &[u8]) -> std::io::Result<Option<usize>> {
        if !Self::is_binary_frame(bytes) {
            return Ok(None);
        }

        let version = bytes[4];
        if version != WIDX_FRAME_VERSION {
            return Err(Error::new(ErrorKind::InvalidData, "Unsupported WIDX version"));
        }

        let slot_len = u32::from_be_bytes(
            bytes[12..16]
                .try_into()
                .map_err(|_| Error::new(ErrorKind::InvalidData, "Invalid WIDX slot size"))?
        ) as usize;

        Ok(Some(slot_len))
    }

    pub fn with_slot_size(encoded: &[u8], slot_size: usize) -> std::io::Result<Vec<u8>> {
        if !Self::is_binary_frame(encoded) {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "Cannot rewrite slot size for non-WIDX frame",
            ));
        }
        if slot_size < encoded.len() || slot_size > u32::MAX as usize {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "Invalid slot size for WIDX frame",
            ));
        }

        let mut rewritten = encoded.to_vec();
        rewritten[12..16].copy_from_slice(&(slot_size as u32).to_be_bytes());
        Ok(rewritten)
    }

    pub fn tombstone_frame(slot_size: usize) -> std::io::Result<Vec<u8>> {
        if slot_size < WIDX_FRAME_HEADER_LEN || slot_size > u32::MAX as usize {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "Invalid tombstone slot size",
            ));
        }

        let mut encoded = Vec::with_capacity(WIDX_FRAME_HEADER_LEN);
        encoded.extend_from_slice(&WIDX_FRAME_MAGIC);
        encoded.push(WIDX_FRAME_VERSION);
        encoded.push(WIDX_FLAG_TOMBSTONE);
        encoded.extend_from_slice(&0u16.to_be_bytes());
        encoded.extend_from_slice(&0u32.to_be_bytes());
        encoded.extend_from_slice(&(slot_size as u32).to_be_bytes());
        Ok(encoded)
    }

    pub fn serialize_index_entry(
        value: &Value,
        field_name_map: &std::collections::BTreeMap<String, String>,
    ) -> std::io::Result<Vec<u8>> {
        let obj = value.as_object().ok_or_else(|| {
            Error::new(ErrorKind::InvalidInput, "Index entry must be an object")
        })?;

        let field_token = obj.get("f").and_then(|v| v.as_str()).ok_or_else(|| {
            Error::new(ErrorKind::InvalidInput, "Index entry missing field token 'f'")
        })?;

        let key = obj.get("k").and_then(|v| v.as_str()).ok_or_else(|| {
            Error::new(ErrorKind::InvalidInput, "Index entry missing key 'k'")
        })?;

        let offset_val = obj.get("o").ok_or_else(|| {
            Error::new(ErrorKind::InvalidInput, "Index entry missing offset 'o'")
        })?;

        let is_secondary = offset_val.is_array();

        let has_block_meta = obj.get("l").is_some()
            && obj.get("c").is_some()
            && obj.get("x").is_some()
            && obj.get("s").is_some();

        let field_id = field_name_map
            .keys()
            .position(|k| k == field_token)
            .or_else(|| {
                field_name_map
                    .iter()
                    .position(|(_k, v)| v == field_token)
            })
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::InvalidInput,
                    format!("Field token/name '{}' not registered in metadata", field_token),
                )
            })?;

        let mut entry_header: u8 = 0;
        if is_secondary {
            entry_header |= 0b0000_0001;
        }
        if has_block_meta {
            entry_header |= 0b0000_0010;
        }

        let mut field_id_bytes = Vec::new();
        if field_id < 16 {
            entry_header |= (field_id as u8) << 4;
        } else if field_id < 256 {
            entry_header |= 0b0000_0100;
            field_id_bytes.push(field_id as u8);
        } else {
            entry_header |= 0b0001_0100;
            field_id_bytes.extend_from_slice(&(field_id as u16).to_be_bytes());
        }

        let key_bytes = key.as_bytes();
        let key_len = key_bytes.len();
        let mut key_len_bytes = Vec::new();
        if key_len < 256 {
            key_len_bytes.push(key_len as u8);
        } else if key_len <= u16::MAX as usize {
            entry_header |= 0b0000_1000;
            key_len_bytes.extend_from_slice(&(key_len as u16).to_be_bytes());
        } else {
            return Err(Error::new(ErrorKind::InvalidInput, "Key length exceeds u16::MAX"));
        }

        let mut payload = Vec::new();
        payload.push(entry_header);
        payload.extend_from_slice(&field_id_bytes);
        payload.extend_from_slice(&key_len_bytes);
        payload.extend_from_slice(key_bytes);

        if !is_secondary {
            let offset = offset_val.as_u64().ok_or_else(|| {
                Error::new(ErrorKind::InvalidInput, "Primary index offset must be integer")
            })?;
            write_varint(offset, &mut payload);
        } else {
            let offset_arr = offset_val.as_array().unwrap();
            write_varint(offset_arr.len() as u64, &mut payload);

            let mut offsets = Vec::new();
            for item in offset_arr {
                let off = item.as_u64().ok_or_else(|| {
                    Error::new(ErrorKind::InvalidInput, "Secondary offset must be integer")
                })?;
                offsets.push(off);
            }
            offsets.sort_unstable();

            let mut last = 0u64;
            for &offset in &offsets {
                let delta = offset - last;
                write_varint(delta, &mut payload);
                last = offset;
            }
        }

        if has_block_meta {
            let len = obj.get("l").and_then(|v| v.as_u64()).ok_or_else(|| {
                Error::new(ErrorKind::InvalidInput, "Block length missing or invalid")
            })?;
            let size_class = obj.get("c").and_then(|v| v.as_u64()).ok_or_else(|| {
                Error::new(ErrorKind::InvalidInput, "Block size class missing or invalid")
            })?;
            let crc = obj.get("x").and_then(|v| v.as_u64()).ok_or_else(|| {
                Error::new(ErrorKind::InvalidInput, "Block CRC missing or invalid")
            })?;
            let status = obj.get("s").and_then(|v| v.as_u64()).ok_or_else(|| {
                Error::new(ErrorKind::InvalidInput, "Block status missing or invalid")
            })?;

            write_varint(len, &mut payload);
            write_varint(size_class, &mut payload);
            payload.extend_from_slice(&(crc as u32).to_be_bytes());
            payload.push(status as u8);
        }

        let frame_len = WIDX_FRAME_HEADER_LEN
            .checked_add(payload.len())
            .ok_or_else(|| Error::new(ErrorKind::InvalidData, "WIDX frame length overflow"))?;
        if frame_len > u32::MAX as usize {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "WIDX payload exceeds maximum supported size",
            ));
        }

        let mut encoded = Vec::with_capacity(frame_len);
        encoded.extend_from_slice(&WIDX_FRAME_MAGIC);
        encoded.push(WIDX_FRAME_VERSION);
        encoded.push(0);
        encoded.extend_from_slice(&0u16.to_be_bytes());
        encoded.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        encoded.extend_from_slice(&(frame_len as u32).to_be_bytes());
        encoded.extend_from_slice(&payload);
        Ok(encoded)
    }

    pub fn deserialize_index_entry(
        bytes: &[u8],
        field_name_map: &std::collections::BTreeMap<String, String>,
    ) -> std::io::Result<Option<Value>> {
        if bytes.is_empty() {
            return Ok(None);
        }

        if !Self::is_binary_frame(bytes) {
            return Err(Error::new(ErrorKind::InvalidData, "Invalid WIDX magic bytes"));
        }

        let version = bytes[4];
        if version != WIDX_FRAME_VERSION {
            return Err(Error::new(ErrorKind::InvalidData, "Unsupported WIDX version"));
        }

        let flags = bytes[5];
        if flags & WIDX_FLAG_TOMBSTONE != 0 {
            return Ok(None);
        }

        let payload_len = u32::from_be_bytes(
            bytes[8..12]
                .try_into()
                .map_err(|_| Error::new(ErrorKind::InvalidData, "Invalid WIDX payload len"))?
        ) as usize;

        let frame_len = WIDX_FRAME_HEADER_LEN + payload_len;
        if frame_len > bytes.len() {
            return Err(Error::new(ErrorKind::InvalidData, "WIDX frame bounds overflow"));
        }

        let payload = &bytes[WIDX_FRAME_HEADER_LEN..frame_len];
        let mut cursor = 0usize;

        if payload.is_empty() {
            return Err(Error::new(ErrorKind::InvalidData, "Empty WIDX payload"));
        }

        let entry_header = payload[cursor];
        cursor += 1;

        let is_secondary = (entry_header & 0b0000_0001) != 0;
        let has_block_meta = (entry_header & 0b0000_0010) != 0;
        let has_field_id_extra = (entry_header & 0b0000_0100) != 0;
        let key_encoding = (entry_header & 0b0000_1000) != 0;

        let field_id = if !has_field_id_extra {
            ((entry_header >> 4) & 0x0F) as usize
        } else {
            let is_u16 = (entry_header & 0b0001_0000) != 0;
            if !is_u16 {
                if cursor >= payload.len() {
                    return Err(Error::new(ErrorKind::UnexpectedEof, "EOF reading field ID byte"));
                }
                let id = payload[cursor] as usize;
                cursor += 1;
                id
            } else {
                if cursor + 2 > payload.len() {
                    return Err(Error::new(ErrorKind::UnexpectedEof, "EOF reading field ID word"));
                }
                let id = u16::from_be_bytes(payload[cursor..cursor+2].try_into().unwrap()) as usize;
                cursor += 2;
                id
            }
        };

        let field_token = field_name_map
            .keys()
            .nth(field_id)
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::InvalidData,
                    format!("Field ID {} out of range in metadata", field_id),
                )
            })?;

        let key_len = if !key_encoding {
            if cursor >= payload.len() {
                return Err(Error::new(ErrorKind::UnexpectedEof, "EOF reading key length"));
            }
            let len = payload[cursor] as usize;
            cursor += 1;
            len
        } else {
            if cursor + 2 > payload.len() {
                return Err(Error::new(ErrorKind::UnexpectedEof, "EOF reading key length u16"));
            }
            let len = u16::from_be_bytes(payload[cursor..cursor+2].try_into().unwrap()) as usize;
            cursor += 2;
            len
        };

        if cursor + key_len > payload.len() {
            return Err(Error::new(ErrorKind::UnexpectedEof, "EOF reading key string"));
        }
        let key = std::str::from_utf8(&payload[cursor..cursor+key_len])
            .map_err(|err| Error::new(ErrorKind::InvalidData, err.to_string()))?;
        cursor += key_len;

        let offset_value = if !is_secondary {
            let offset = read_varint(payload, &mut cursor)?;
            Value::from(offset)
        } else {
            let count = read_varint(payload, &mut cursor)? as usize;
            let mut offsets = Vec::with_capacity(count);
            let mut last = 0u64;
            for _ in 0..count {
                let delta = read_varint(payload, &mut cursor)?;
                let val = last + delta;
                offsets.push(Value::from(val));
                last = val;
            }
            Value::Array(offsets)
        };

        let mut obj = serde_json::Map::new();
        obj.insert("f".to_string(), Value::String(field_token.clone()));
        obj.insert("k".to_string(), Value::String(key.to_string()));
        obj.insert("o".to_string(), offset_value);

        if has_block_meta {
            let len = read_varint(payload, &mut cursor)?;
            let size_class = read_varint(payload, &mut cursor)?;
            if cursor + 4 > payload.len() {
                return Err(Error::new(ErrorKind::UnexpectedEof, "EOF reading block CRC"));
            }
            let crc = u32::from_be_bytes(payload[cursor..cursor+4].try_into().unwrap());
            cursor += 4;
            if cursor >= payload.len() {
                return Err(Error::new(ErrorKind::UnexpectedEof, "EOF reading block status"));
            }
            let status = payload[cursor];

            obj.insert("l".to_string(), Value::from(len));
            obj.insert("c".to_string(), Value::from(size_class));
            obj.insert("x".to_string(), Value::from(crc));
            obj.insert("s".to_string(), Value::from(status));
        }

        Ok(Some(Value::Object(obj)))
    }
}

#[cfg(test)]
mod widx_tests {
    use super::*;
    use serde_json::json;
    use std::collections::BTreeMap;

    #[test]
    fn widx_frame_roundtrip_serialize_and_deserialize() {
        let mut field_name_map = BTreeMap::new();
        field_name_map.insert("_".to_string(), "_id".to_string());
        field_name_map.insert("a".to_string(), "age".to_string());

        let val = json!({
            "f": "a",
            "k": "Bob",
            "o": 123456u64,
            "l": 200u64,
            "c": 256u64,
            "x": 9999u64,
            "s": 1u64
        });

        let encoded = NativeBinaryIndexFormat::serialize_index_entry(&val, &field_name_map).expect("serialize");
        assert!(NativeBinaryIndexFormat::is_binary_frame(&encoded));
        let decoded = NativeBinaryIndexFormat::deserialize_index_entry(&encoded, &field_name_map).expect("deserialize").unwrap();
        assert_eq!(decoded["f"], "a");
        assert_eq!(decoded["k"], "Bob");
        assert_eq!(decoded["o"], 123456u64);
        assert_eq!(decoded["l"], 200u64);
        assert_eq!(decoded["c"], 256u64);
        assert_eq!(decoded["x"], 9999u64);
        assert_eq!(decoded["s"], 1u64);
    }
}
