use super::bson::{
    BSON_FLAG_TOMBSTONE, BSON_FRAME_HEADER_LEN, BSON_FRAME_MAGIC, BSON_FRAME_VERSION,
    BSON_NATIVE_FRAME_VERSION, BsonBinaryFormat,
};
use super::native_value::{deserialize_native_value, serialize_native_value};
use super::varint::{read_varint, write_varint};
use bson::Document;
use serde_json::Value;
use std::collections::BTreeMap;
use std::io::{Error, ErrorKind};

const FIELD_TOKEN_ALPHABET: &[u8] =
    b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";

impl BsonBinaryFormat {
    pub fn serialize_native_record(
        value: &Value,
        field_name_map: &BTreeMap<String, String>,
    ) -> std::io::Result<Vec<u8>> {
        let record_object = value.as_object().ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidInput,
                "Record payload must be a JSON object",
            )
        })?;

        let ordered_tokens = Self::ordered_field_tokens(field_name_map);
        let field_count_at_write = ordered_tokens.len();
        let bitmap_len = (field_count_at_write + 7) / 8;
        let mut presence_bitmap = vec![0u8; bitmap_len];
        let mut values_payload = Vec::new();

        for (index, token) in ordered_tokens.iter().enumerate() {
            if let Some(val) = record_object.get(*token) {
                let byte_idx = index / 8;
                let bit_idx = index % 8;
                presence_bitmap[byte_idx] |= 1 << bit_idx;

                serialize_native_value(val, &mut values_payload)?;
            }
        }

        let mut header_and_bitmap = Vec::new();
        write_varint(field_count_at_write as u64, &mut header_and_bitmap);
        header_and_bitmap.extend_from_slice(&presence_bitmap);

        let payload_len = header_and_bitmap.len() + values_payload.len();
        let frame_len = BSON_FRAME_HEADER_LEN
            .checked_add(payload_len)
            .ok_or_else(|| Error::new(ErrorKind::InvalidData, "Frame length overflow"))?;

        if frame_len > u32::MAX as usize {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "Payload exceeds maximum supported size",
            ));
        }

        let mut encoded = Vec::with_capacity(frame_len);
        encoded.extend_from_slice(&BSON_FRAME_MAGIC);
        encoded.push(BSON_NATIVE_FRAME_VERSION);
        encoded.push(0);
        encoded.extend_from_slice(&0u16.to_be_bytes());
        encoded.extend_from_slice(&(payload_len as u32).to_be_bytes());
        encoded.extend_from_slice(&(frame_len as u32).to_be_bytes());
        encoded.extend_from_slice(&header_and_bitmap);
        encoded.extend_from_slice(&values_payload);

        Ok(encoded)
    }

    pub fn deserialize_record_with_map(
        bytes: &[u8],
        field_name_map: Option<&BTreeMap<String, String>>,
    ) -> std::io::Result<Option<Value>> {
        let Some((version, flags, frame_len, _slot_len)) = Self::parse_frame(bytes)? else {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "WRDB binary frame expected",
            ));
        };
        if flags & BSON_FLAG_TOMBSTONE != 0 {
            return Ok(None);
        }

        let payload = &bytes[BSON_FRAME_HEADER_LEN..frame_len];

        match version {
            BSON_FRAME_VERSION => {
                let document = bson::from_slice::<Document>(payload)
                    .map_err(|err| Error::new(ErrorKind::InvalidData, err.to_string()))?;
                let value = serde_json::to_value(document)
                    .map_err(|err| Error::new(ErrorKind::InvalidData, err.to_string()))?;
                Ok(Some(value))
            }
            BSON_NATIVE_FRAME_VERSION => {
                let Some(map) = field_name_map else {
                    return Err(Error::new(
                        ErrorKind::InvalidData,
                        "Cannot deserialize native record (version 2) without a field map",
                    ));
                };

                let mut offset = 0;
                let field_count_at_write = read_varint(payload, &mut offset)? as usize;
                let bitmap_len = (field_count_at_write + 7) / 8;

                if offset + bitmap_len > payload.len() {
                    return Err(Error::new(
                        ErrorKind::InvalidData,
                        "Truncated presence bitmap",
                    ));
                }

                let presence_bitmap = &payload[offset..offset + bitmap_len];
                let mut values_offset = offset + bitmap_len;

                let ordered_tokens = Self::ordered_field_tokens(map);
                let mut record_object = serde_json::Map::new();

                for index in 0..field_count_at_write {
                    let byte_idx = index / 8;
                    let bit_idx = index % 8;
                    let is_present = (presence_bitmap[byte_idx] & (1 << bit_idx)) != 0;

                    if is_present {
                        if values_offset >= payload.len() {
                            return Err(Error::new(
                                ErrorKind::InvalidData,
                                "Truncated native record payload values",
                            ));
                        }
                        let val = deserialize_native_value(payload, &mut values_offset)?;
                        if index < ordered_tokens.len() {
                            record_object.insert(ordered_tokens[index].clone(), val);
                        }
                    }
                }

                if values_offset != payload.len() {
                    return Err(Error::new(
                        ErrorKind::InvalidData,
                        "Native record payload contains trailing bytes",
                    ));
                }

                Ok(Some(Value::Object(record_object)))
            }
            _ => Err(Error::new(
                ErrorKind::InvalidData,
                format!("Unsupported BSON frame version: {}", version),
            )),
        }
    }

    fn ordered_field_tokens(field_name_map: &BTreeMap<String, String>) -> Vec<&String> {
        let mut ordered_tokens = field_name_map.keys().collect::<Vec<_>>();
        ordered_tokens.sort_by(|left, right| {
            match (
                Self::field_token_assignment_ordinal(left),
                Self::field_token_assignment_ordinal(right),
            ) {
                (Some(left_ordinal), Some(right_ordinal)) => left_ordinal
                    .cmp(&right_ordinal)
                    .then_with(|| left.cmp(right)),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => left.cmp(right),
            }
        });
        ordered_tokens
    }

    fn field_token_assignment_ordinal(field_token: &str) -> Option<usize> {
        if field_token == "_" {
            return Some(0);
        }

        let mut ordinal = 1usize;
        let base = FIELD_TOKEN_ALPHABET.len();
        let width = field_token.len();
        if width == 0 {
            return None;
        }

        for previous_width in 1..width {
            ordinal = ordinal.checked_add(base.checked_pow(previous_width as u32)?)?;
        }

        let mut token_ordinal = 0usize;
        for byte in field_token.as_bytes() {
            let digit = FIELD_TOKEN_ALPHABET
                .iter()
                .position(|alphabet_byte| alphabet_byte == byte)?;
            token_ordinal = token_ordinal.checked_mul(base)?.checked_add(digit)?;
        }

        ordinal.checked_add(token_ordinal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn version_two_record_with_json_encoded_object_value_remains_readable() {
        let field_name_map = BTreeMap::from([
            ("_".to_string(), "_id".to_string()),
            ("a".to_string(), "attributes".to_string()),
        ]);
        let mut payload = Vec::new();
        write_varint(2, &mut payload);
        payload.push(0b0000_0011);
        serialize_native_value(&json!("hero"), &mut payload).expect("id should serialize");
        let attributes = json!({"strength": 18, "tags": ["athletics"]});
        let json_bytes = serde_json::to_vec(attributes.as_object().expect("object")).expect("json");
        payload.push(0x08);
        write_varint(json_bytes.len() as u64, &mut payload);
        payload.extend_from_slice(&json_bytes);
        let frame_len = BSON_FRAME_HEADER_LEN + payload.len();
        let mut frame = Vec::new();
        frame.extend_from_slice(&BSON_FRAME_MAGIC);
        frame.push(BSON_NATIVE_FRAME_VERSION);
        frame.push(0);
        frame.extend_from_slice(&0u16.to_be_bytes());
        frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        frame.extend_from_slice(&(frame_len as u32).to_be_bytes());
        frame.extend_from_slice(&payload);

        let decoded = BsonBinaryFormat::deserialize_record_with_map(&frame, Some(&field_name_map))
            .expect("legacy frame should deserialize")
            .expect("legacy frame should contain a record");

        assert_eq!(decoded["_"], "hero");
        assert_eq!(decoded["a"], attributes);
    }
}
