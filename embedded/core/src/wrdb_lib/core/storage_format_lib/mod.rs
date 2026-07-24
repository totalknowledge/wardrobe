use serde_json::Value;

pub trait StorageFormat {
    fn serialize_record(value: &Value) -> std::io::Result<Vec<u8>>;
    fn deserialize_record(bytes: &[u8]) -> std::io::Result<Option<Value>>;
    fn is_tombstone(bytes: &[u8]) -> bool;
}

mod bson;
mod native_record;
mod native_value;
mod varint;
mod widx;

pub use bson::BsonBinaryFormat;
pub use widx::NativeBinaryIndexFormat;
