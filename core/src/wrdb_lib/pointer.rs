use std::io::{Error, ErrorKind, Result};

use serde_json::Value;

use super::storage::StorageLocator;

pub(crate) fn is_pointer(value: &str) -> bool {
    try_parse_pointer_parts(value).is_some()
}

pub(crate) fn try_parse_pointer(pointer: &str) -> Option<(String, String)> {
    let (drawer_name, record_key) = try_parse_pointer_parts(pointer)?;
    Some((drawer_name.to_string(), record_key.to_string()))
}

pub(crate) fn try_parse_pointer_parts(pointer: &str) -> Option<(&str, &str)> {
    let clean_pointer = pointer.strip_prefix('@')?;
    let (drawer_name, record_key) = clean_pointer.split_once(':')?;
    let record_key = record_key.strip_prefix("lnk_").unwrap_or(record_key);

    if drawer_name.is_empty() || record_key.is_empty() || record_key.contains(':') {
        return None;
    }

    Some((drawer_name, record_key))
}

pub(crate) fn parse_pointer(pointer: &str) -> Result<(String, String)> {
    try_parse_pointer(pointer).ok_or_else(|| {
        Error::new(
            ErrorKind::InvalidData,
            format!("Malformed pointer reference encountered: {}", pointer),
        )
    })
}

pub(crate) fn clean_primary_key_token(value: &str) -> String {
    if let Some((_, record_key)) = try_parse_pointer_parts(value) {
        return record_key.to_string();
    }

    value
        .trim_start_matches('@')
        .strip_prefix("lnk_")
        .unwrap_or_else(|| value.trim_start_matches('@'))
        .to_string()
}

pub(crate) fn format_pointer(drawer_name: &str, record_key: &str) -> String {
    format!(
        "@{}:{}",
        drawer_name.trim_start_matches('@'),
        clean_primary_key_token(record_key)
    )
}

pub(crate) fn normalize_reference_pointer(drawer_name: &str, reference_id: &str) -> String {
    if let Some((pointer_drawer, pointer_key)) = try_parse_pointer(reference_id) {
        return format_pointer(&pointer_drawer, &pointer_key);
    }

    format_pointer(drawer_name, &clean_primary_key_token(reference_id))
}

pub(crate) fn normalize_reference_pointer_for_namespace(
    drawer_name: &str,
    reference_id: &str,
    drawer_namespace: Option<&str>,
) -> String {
    if drawer_namespace.is_none() {
        return normalize_reference_pointer(drawer_name, reference_id);
    }

    if let Some((pointer_drawer, pointer_key)) = try_parse_pointer(reference_id) {
        let physical_pointer_drawer = scoped_drawer_name(&pointer_drawer, drawer_namespace);
        return format_pointer(&physical_pointer_drawer, &pointer_key);
    }

    let physical_drawer_name = scoped_drawer_name(drawer_name, drawer_namespace);
    normalize_reference_pointer(&physical_drawer_name, reference_id)
}

pub(crate) fn normalize_primary_key(
    physical_drawer_name: &str,
    logical_drawer_name: &str,
    existing_id: &str,
) -> String {
    if let Some((pointer_drawer, pointer_key)) = try_parse_pointer(existing_id) {
        if pointer_drawer == logical_drawer_name || pointer_drawer == physical_drawer_name {
            return pointer_key;
        }
    }

    clean_primary_key_token(existing_id)
}

pub(crate) fn scoped_drawer_name(drawer_name: &str, drawer_namespace: Option<&str>) -> String {
    let Some(namespace) = drawer_namespace else {
        return drawer_name.to_string();
    };

    let prefix = format!("{namespace}_");
    if drawer_name.starts_with(&prefix) {
        drawer_name.to_string()
    } else {
        format!("{prefix}{drawer_name}")
    }
}

pub(crate) fn scoped_pointer(pointer: &str, drawer_namespace: Option<&str>) -> String {
    let Some((drawer_name, record_key)) = try_parse_pointer(pointer) else {
        return pointer.to_string();
    };

    if drawer_namespace.is_none() {
        return format_pointer(&drawer_name, &record_key);
    }

    let physical_drawer_name = scoped_drawer_name(&drawer_name, drawer_namespace);
    format_pointer(&physical_drawer_name, &record_key)
}

pub(crate) fn locator_to_pointer(locator: StorageLocator) -> String {
    match locator {
        StorageLocator::Explicit { drawer, id } => format_pointer(&drawer, &id),
        StorageLocator::Inline(pointer) => pointer,
    }
}

pub(crate) fn inline_pointer_drawer_names(value: &Value) -> Vec<String> {
    let mut drawer_names = Vec::new();
    collect_inline_pointer_drawer_names(value, &mut drawer_names);
    drawer_names.sort();
    drawer_names.dedup();
    drawer_names
}

pub(crate) fn collect_pointer_strings(value: &Value, pointers: &mut Vec<String>) {
    match value {
        Value::String(pointer) if is_pointer(pointer) => {
            pointers.push(pointer.to_string());
        }
        Value::Array(values) => {
            for value in values {
                collect_pointer_strings(value, pointers);
            }
        }
        Value::Object(map) => {
            for value in map.values() {
                collect_pointer_strings(value, pointers);
            }
        }
        _ => {}
    }
}

fn collect_inline_pointer_drawer_names(value: &Value, drawer_names: &mut Vec<String>) {
    match value {
        Value::String(pointer) => {
            if let Some((drawer_name, _)) = try_parse_pointer(pointer) {
                drawer_names.push(drawer_name);
            }
        }
        Value::Array(values) => {
            for value in values {
                collect_inline_pointer_drawer_names(value, drawer_names);
            }
        }
        Value::Object(map) => {
            for value in map.values() {
                collect_inline_pointer_drawer_names(value, drawer_names);
            }
        }
        _ => {}
    }
}
