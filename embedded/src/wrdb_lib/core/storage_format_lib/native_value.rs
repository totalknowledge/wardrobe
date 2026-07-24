use super::varint::{read_varint, write_varint};
use serde_json::Value;
use std::io::{Error, ErrorKind};

pub(super) fn serialize_native_value(val: &Value, bytes: &mut Vec<u8>) -> std::io::Result<()> {
    match val {
        Value::Null => {
            bytes.push(0x00);
        }
        Value::Bool(b) => {
            if *b {
                bytes.push(0x02);
            } else {
                bytes.push(0x01);
            }
        }
        Value::Number(num) => {
            if let Some(i) = num.as_i64() {
                bytes.push(0x03);
                bytes.extend_from_slice(&i.to_le_bytes());
            } else if let Some(u) = num.as_u64() {
                bytes.push(0x04);
                bytes.extend_from_slice(&u.to_le_bytes());
            } else if let Some(f) = num.as_f64() {
                bytes.push(0x05);
                bytes.extend_from_slice(&f.to_le_bytes());
            } else {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "Unsupported number type",
                ));
            }
        }
        Value::String(s) => {
            bytes.push(0x06);
            let s_bytes = s.as_bytes();
            write_varint(s_bytes.len() as u64, bytes);
            bytes.extend_from_slice(s_bytes);
        }
        Value::Array(arr) => {
            bytes.push(0x09);
            write_varint(arr.len() as u64, bytes);
            for value in arr {
                serialize_native_value(value, bytes)?;
            }
        }
        Value::Object(map) => {
            bytes.push(0x0A);
            write_varint(map.len() as u64, bytes);
            for (key, value) in map {
                write_varint(key.len() as u64, bytes);
                bytes.extend_from_slice(key.as_bytes());
                serialize_native_value(value, bytes)?;
            }
        }
    }
    Ok(())
}

pub(super) fn deserialize_native_value(bytes: &[u8], offset: &mut usize) -> std::io::Result<Value> {
    if *offset >= bytes.len() {
        return Err(Error::new(
            ErrorKind::UnexpectedEof,
            "EOF reading value type",
        ));
    }
    let type_byte = bytes[*offset];
    *offset += 1;

    match type_byte {
        0x00 => Ok(Value::Null),
        0x01 => Ok(Value::Bool(false)),
        0x02 => Ok(Value::Bool(true)),
        0x03 => {
            if *offset + 8 > bytes.len() {
                return Err(Error::new(ErrorKind::UnexpectedEof, "EOF reading i64"));
            }
            let i = i64::from_le_bytes(bytes[*offset..*offset + 8].try_into().unwrap());
            *offset += 8;
            Ok(Value::Number(i.into()))
        }
        0x04 => {
            if *offset + 8 > bytes.len() {
                return Err(Error::new(ErrorKind::UnexpectedEof, "EOF reading u64"));
            }
            let u = u64::from_le_bytes(bytes[*offset..*offset + 8].try_into().unwrap());
            *offset += 8;
            Ok(Value::Number(u.into()))
        }
        0x05 => {
            if *offset + 8 > bytes.len() {
                return Err(Error::new(ErrorKind::UnexpectedEof, "EOF reading f64"));
            }
            let f = f64::from_le_bytes(bytes[*offset..*offset + 8].try_into().unwrap());
            *offset += 8;
            if let Some(num) = serde_json::Number::from_f64(f) {
                Ok(Value::Number(num))
            } else {
                Err(Error::new(ErrorKind::InvalidData, "Invalid f64 number"))
            }
        }
        0x06 => {
            let len = read_varint(bytes, offset)? as usize;
            if *offset + len > bytes.len() {
                return Err(Error::new(ErrorKind::UnexpectedEof, "EOF reading string"));
            }
            let s = std::str::from_utf8(&bytes[*offset..*offset + len])
                .map_err(|err| Error::new(ErrorKind::InvalidData, err.to_string()))?;
            *offset += len;
            Ok(Value::String(s.to_string()))
        }
        0x07 => {
            let len = read_varint(bytes, offset)? as usize;
            if *offset + len > bytes.len() {
                return Err(Error::new(ErrorKind::UnexpectedEof, "EOF reading array"));
            }
            let arr: Vec<Value> = serde_json::from_slice(&bytes[*offset..*offset + len])
                .map_err(|err| Error::new(ErrorKind::InvalidData, err.to_string()))?;
            *offset += len;
            Ok(Value::Array(arr))
        }
        0x08 => {
            let len = read_varint(bytes, offset)? as usize;
            if *offset + len > bytes.len() {
                return Err(Error::new(ErrorKind::UnexpectedEof, "EOF reading object"));
            }
            let obj: serde_json::Map<String, Value> =
                serde_json::from_slice(&bytes[*offset..*offset + len])
                    .map_err(|err| Error::new(ErrorKind::InvalidData, err.to_string()))?;
            *offset += len;
            Ok(Value::Object(obj))
        }
        0x09 => {
            let count = read_varint(bytes, offset)? as usize;
            let mut values = Vec::with_capacity(count.min(bytes.len().saturating_sub(*offset)));
            for _ in 0..count {
                values.push(deserialize_native_value(bytes, offset)?);
            }
            Ok(Value::Array(values))
        }
        0x0A => {
            let count = read_varint(bytes, offset)? as usize;
            let mut map = serde_json::Map::new();
            for _ in 0..count {
                let key_len = read_varint(bytes, offset)? as usize;
                let Some(key_end) = offset.checked_add(key_len) else {
                    return Err(Error::new(ErrorKind::InvalidData, "Object key overflow"));
                };
                if key_end > bytes.len() {
                    return Err(Error::new(
                        ErrorKind::UnexpectedEof,
                        "EOF reading object key",
                    ));
                }
                let key = std::str::from_utf8(&bytes[*offset..key_end])
                    .map_err(|err| Error::new(ErrorKind::InvalidData, err.to_string()))?;
                *offset = key_end;
                let value = deserialize_native_value(bytes, offset)?;
                map.insert(key.to_string(), value);
            }
            Ok(Value::Object(map))
        }
        _ => Err(Error::new(
            ErrorKind::InvalidData,
            format!("Unknown value type byte: 0x{:02X}", type_byte),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn native_nested_values_round_trip_without_json_blobs() {
        let value = json!({
            "attributes": {
                "strength": 18,
                "proficiencies": ["athletics", {"name": "arcana"}]
            }
        });
        let mut bytes = Vec::new();

        serialize_native_value(&value, &mut bytes).expect("nested value should serialize");

        assert_eq!(bytes.first(), Some(&0x0A));
        let mut offset = 0;
        let decoded =
            deserialize_native_value(&bytes, &mut offset).expect("nested value should deserialize");
        assert_eq!(decoded, value);
        assert_eq!(offset, bytes.len());
    }

    #[test]
    fn version_two_json_encoded_nested_values_remain_readable() {
        let value = json!({"strength": 18, "tags": ["hero", "mage"]});
        let json_bytes = serde_json::to_vec(value.as_object().expect("object")).expect("json");
        let mut bytes = vec![0x08];
        write_varint(json_bytes.len() as u64, &mut bytes);
        bytes.extend_from_slice(&json_bytes);
        let mut offset = 0;

        let decoded = deserialize_native_value(&bytes, &mut offset)
            .expect("legacy object should deserialize");

        assert_eq!(decoded, value);
        assert_eq!(offset, bytes.len());
    }

    #[test]
    fn version_two_json_encoded_arrays_remain_readable() {
        let value = json!(["hero", {"strength": 18}]);
        let json_bytes = serde_json::to_vec(value.as_array().expect("array")).expect("json");
        let mut bytes = vec![0x07];
        write_varint(json_bytes.len() as u64, &mut bytes);
        bytes.extend_from_slice(&json_bytes);
        let mut offset = 0;

        let decoded =
            deserialize_native_value(&bytes, &mut offset).expect("legacy array should deserialize");

        assert_eq!(decoded, value);
        assert_eq!(offset, bytes.len());
    }

    #[test]
    fn truncated_native_object_key_is_rejected() {
        let mut bytes = vec![0x0A];
        write_varint(1, &mut bytes);
        write_varint(4, &mut bytes);
        bytes.extend_from_slice(b"ab");
        let mut offset = 0;

        let error =
            deserialize_native_value(&bytes, &mut offset).expect_err("truncated key should fail");

        assert_eq!(error.kind(), ErrorKind::UnexpectedEof);
    }

    #[test]
    fn malformed_native_values_return_data_errors() {
        let malformed_values = vec![
            Vec::new(),
            vec![0x03],
            vec![0x04],
            vec![0x05],
            vec![0x06, 0x01, 0xff],
            vec![0x07, 0x01, b'['],
            vec![0x08, 0x01, b'{'],
            vec![0x09, 0x01],
            vec![0x0A, 0x01, 0x01, 0xff],
            vec![0x0A, 0x01, 0x01, b'a'],
            vec![0xff],
        ];

        for bytes in malformed_values {
            let mut offset = 0;
            assert!(deserialize_native_value(&bytes, &mut offset).is_err());
        }

        let mut invalid_float = vec![0x05];
        invalid_float.extend_from_slice(&f64::NAN.to_le_bytes());
        let mut offset = 0;
        assert!(deserialize_native_value(&invalid_float, &mut offset).is_err());

        let mut overflowing_key = vec![0x0A];
        write_varint(1, &mut overflowing_key);
        write_varint(u64::MAX, &mut overflowing_key);
        let mut offset = 0;
        assert_eq!(
            deserialize_native_value(&overflowing_key, &mut offset)
                .expect_err("overflowing object key should fail")
                .kind(),
            ErrorKind::InvalidData
        );
    }
}
