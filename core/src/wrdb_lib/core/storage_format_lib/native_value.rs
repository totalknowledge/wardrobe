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
            bytes.push(0x07);
            let json_bytes = serde_json::to_vec(arr)
                .map_err(|err| Error::new(ErrorKind::InvalidInput, err.to_string()))?;
            write_varint(json_bytes.len() as u64, bytes);
            bytes.extend_from_slice(&json_bytes);
        }
        Value::Object(map) => {
            bytes.push(0x08);
            let json_bytes = serde_json::to_vec(map)
                .map_err(|err| Error::new(ErrorKind::InvalidInput, err.to_string()))?;
            write_varint(json_bytes.len() as u64, bytes);
            bytes.extend_from_slice(&json_bytes);
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
        _ => Err(Error::new(
            ErrorKind::InvalidData,
            format!("Unknown value type byte: 0x{:02X}", type_byte),
        )),
    }
}
