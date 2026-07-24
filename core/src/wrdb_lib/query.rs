use crate::wrdb_lib::command::PaginationMetadata;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::cmp::Ordering;
use std::io::{Error, ErrorKind, Result};

use super::pointer;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderDirection {
    Ascending,
    Descending,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryModifiers {
    pub order_by: Option<String>,
    pub order_direction: Option<OrderDirection>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
    pub cursor: Option<String>,
    pub page: Option<usize>,
    pub page_size: Option<usize>,
}

pub(crate) const DEFAULT_PAGE_SIZE: usize = 100;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct QueryRecords {
    pub(crate) records: Vec<Value>,
    pub(crate) pagination: Option<PaginationMetadata>,
}

#[derive(Debug, Serialize, Deserialize)]
struct QueryCursor {
    order_by: String,
    order_direction: OrderDirection,
    id: String,
}

enum SortableValue<'a> {
    Bool(bool),
    Number(f64),
    String(&'a str),
}

#[derive(Clone, Copy)]
enum SortableType {
    Bool,
    Number,
    String,
}

impl SortableValue<'_> {
    fn compare_same_type(&self, other: Self) -> Option<Ordering> {
        match (self, other) {
            (Self::Bool(left), Self::Bool(right)) => Some(left.cmp(&right)),
            (Self::Number(left), Self::Number(right)) => left.partial_cmp(&right),
            (Self::String(left), Self::String(right)) => Some(left.cmp(&right)),
            _ => None,
        }
    }
}

pub(crate) fn filter_map(filter: &Value) -> Result<&Map<String, Value>> {
    let map = filter
        .as_object()
        .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "Filter root must be a JSON object"))?;
    validate_filter_map(map)?;
    Ok(map)
}

fn validate_filter_map(map: &Map<String, Value>) -> Result<()> {
    for (key, val) in map {
        if key == "$in" {
            if !val.is_array() {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "$in operator requires a JSON array payload",
                ));
            }
        } else if key.starts_with('$') {
            match key.as_str() {
                "$gt" | "$gte" | "$lt" | "$lte" | "$eq" | "$ne" | "$in" => {}
                _ => {
                    return Err(Error::new(
                        ErrorKind::InvalidInput,
                        format!("Unknown query operator: {key}"),
                    ));
                }
            }
        }
        if let Value::Object(nested) = val {
            validate_filter_map(nested)?;
        }
    }
    Ok(())
}

pub(crate) fn resolve_field_path<'a>(value: &'a Value, field_path: &str) -> Option<&'a Value> {
    field_path
        .split('.')
        .try_fold(value, |current, segment| current.as_object()?.get(segment))
}

pub(crate) fn record_matches_filter(
    record: &Value,
    filter_map: &Map<String, Value>,
    drawer_namespace: Option<&str>,
) -> bool {
    let Value::Object(_) = record else {
        return false;
    };

    filter_map.iter().all(|(field_name, expected_value)| {
        resolve_field_path(record, field_name).is_some_and(|actual_value| {
            field_matches_filter(field_name, actual_value, expected_value, drawer_namespace)
        })
    })
}

fn field_matches_filter(
    field_name: &str,
    actual_value: &Value,
    expected_value: &Value,
    drawer_namespace: Option<&str>,
) -> bool {
    match expected_value {
        Value::String(expected_string) => actual_value
            .as_str()
            .is_some_and(|actual_string| matches_string_filter(actual_string, expected_string)),
        Value::Object(expected_map) => {
            if expected_map.keys().any(|key| key.starts_with('$')) {
                return scalar_matches_operator_filter(
                    field_name,
                    actual_value,
                    expected_map,
                    drawer_namespace,
                );
            }

            if let Some(reference_id) = id_only_reference(expected_map) {
                let relationship_drawer = relationship_drawer_name(field_name);
                let normalized_pointer = pointer::normalize_reference_pointer_for_namespace(
                    &relationship_drawer,
                    reference_id,
                    drawer_namespace,
                );
                return actual_value.as_str() == Some(normalized_pointer.as_str());
            }

            let Value::Object(actual_map) = actual_value else {
                return false;
            };

            expected_map.iter().all(|(nested_field, nested_expected)| {
                actual_map.get(nested_field).is_some_and(|nested_actual| {
                    field_matches_filter(
                        nested_field,
                        nested_actual,
                        nested_expected,
                        drawer_namespace,
                    )
                })
            })
        }
        Value::Array(expected_array) => {
            let Value::Array(actual_array) = actual_value else {
                return false;
            };

            actual_array.len() == expected_array.len()
                && actual_array.iter().zip(expected_array.iter()).all(
                    |(actual_item, expected_item)| {
                        field_matches_filter(
                            field_name,
                            actual_item,
                            expected_item,
                            drawer_namespace,
                        )
                    },
                )
        }
        _ => actual_value == expected_value,
    }
}

fn scalar_matches_operator_filter(
    field_name: &str,
    actual_value: &Value,
    expected_map: &Map<String, Value>,
    drawer_namespace: Option<&str>,
) -> bool {
    expected_map.iter().all(|(operator, expected_value)| {
        match operator.as_str() {
            "$in" => {
                let Value::Array(candidates) = expected_value else {
                    return false;
                };
                candidates.iter().any(|candidate| {
                    candidate_matches_value(field_name, actual_value, candidate, drawer_namespace)
                })
            }
            "$ne" => compare_scalar_values(actual_value, expected_value)
                .map_or(true, |ordering| ordering != Ordering::Equal),
            "$gt" => compare_scalar_values(actual_value, expected_value)
                .is_some_and(|ordering| ordering == Ordering::Greater),
            "$gte" => compare_scalar_values(actual_value, expected_value)
                .is_some_and(|ordering| matches!(ordering, Ordering::Greater | Ordering::Equal)),
            "$lt" => compare_scalar_values(actual_value, expected_value)
                .is_some_and(|ordering| ordering == Ordering::Less),
            "$lte" => compare_scalar_values(actual_value, expected_value)
                .is_some_and(|ordering| matches!(ordering, Ordering::Less | Ordering::Equal)),
            "$eq" => compare_scalar_values(actual_value, expected_value)
                .is_some_and(|ordering| ordering == Ordering::Equal),
            _ => false,
        }
    })
}

fn candidate_matches_value(
    field_name: &str,
    actual_value: &Value,
    candidate: &Value,
    drawer_namespace: Option<&str>,
) -> bool {
    if actual_value == candidate {
        return true;
    }

    let target_drawer = if field_name == "_id" {
        drawer_namespace.unwrap_or("").to_string()
    } else {
        relationship_drawer_name(field_name)
    };

    if let Value::Object(cand_map) = candidate {
        if let Some(reference_id) = id_only_reference(cand_map) {
            let normalized_pointer = pointer::normalize_reference_pointer_for_namespace(
                &target_drawer,
                reference_id,
                drawer_namespace,
            );
            return actual_value.as_str() == Some(normalized_pointer.as_str());
        }
    }

    if let (Value::String(actual_str), Value::String(cand_str)) = (actual_value, candidate) {
        if matches_string_filter(actual_str, cand_str) {
            return true;
        }

        let cand_clean_id = pointer::try_parse_pointer(cand_str)
            .map(|(_, id)| id)
            .unwrap_or_else(|| cand_str.clone());
        let actual_clean_id = pointer::try_parse_pointer(actual_str)
            .map(|(_, id)| id)
            .unwrap_or_else(|| actual_str.clone());

        if actual_clean_id == cand_clean_id {
            return true;
        }

        let normalized_cand = pointer::normalize_reference_pointer_for_namespace(
            &target_drawer,
            cand_str,
            drawer_namespace,
        );
        let normalized_actual = pointer::normalize_reference_pointer_for_namespace(
            &target_drawer,
            actual_str,
            drawer_namespace,
        );
        return normalized_actual == normalized_cand;
    }

    compare_scalar_values(actual_value, candidate).is_some_and(|o| o == Ordering::Equal)
}

fn compare_scalar_values(actual_value: &Value, expected_value: &Value) -> Option<Ordering> {
    match (actual_value, expected_value) {
        (Value::Null, Value::Null) => Some(Ordering::Equal),
        (Value::Bool(actual), Value::Bool(expected)) => Some(actual.cmp(expected)),
        (Value::Number(actual), Value::Number(expected)) => {
            actual.as_f64()?.partial_cmp(&expected.as_f64()?)
        }
        (Value::String(actual), Value::String(expected)) => Some(actual.cmp(expected)),
        _ => None,
    }
}

pub(crate) fn matches_string_filter(actual_value: &str, expected_filter: &str) -> bool {
    if !expected_filter.contains('%') {
        return actual_value == expected_filter;
    }

    let actual_bytes = actual_value.as_bytes();
    let filter_bytes = expected_filter.as_bytes();
    let mut actual_index = 0usize;
    let mut filter_index = 0usize;
    let mut wildcard_index = None;
    let mut wildcard_match_start = 0usize;

    while actual_index < actual_bytes.len() {
        if filter_index < filter_bytes.len()
            && filter_bytes[filter_index] == actual_bytes[actual_index]
        {
            actual_index += 1;
            filter_index += 1;
        } else if filter_index < filter_bytes.len() && filter_bytes[filter_index] == b'%' {
            wildcard_index = Some(filter_index);
            filter_index += 1;
            wildcard_match_start = actual_index;
        } else if let Some(last_wildcard_index) = wildcard_index {
            filter_index = last_wildcard_index + 1;
            wildcard_match_start += 1;
            actual_index = wildcard_match_start;
        } else {
            return false;
        }
    }

    while filter_index < filter_bytes.len() && filter_bytes[filter_index] == b'%' {
        filter_index += 1;
    }

    filter_index == filter_bytes.len()
}

pub(crate) fn apply_query_modifiers(
    records: &mut Vec<Value>,
    modifiers: Option<&QueryModifiers>,
) -> Result<Option<PaginationMetadata>> {
    let Some(modifiers) = modifiers else {
        return Ok(None);
    };

    if let Some(order_by) = modifiers.order_by.as_deref() {
        let direction = modifiers
            .order_direction
            .unwrap_or(OrderDirection::Ascending);
        let sort_type = sort_type_for_records(records, order_by);
        records.sort_by(|left, right| {
            compare_records_by_field(left, right, order_by, direction, sort_type)
        });
    }

    if modifiers.cursor.is_some() || modifiers.page.is_some() || modifiers.page_size.is_some() {
        return apply_cursor_or_page_pagination(records, modifiers);
    }

    let offset = modifiers.offset.unwrap_or(0);
    if offset > 0 {
        if offset >= records.len() {
            records.clear();
        } else {
            records.drain(0..offset);
        }
    }

    if let Some(limit) = modifiers.limit {
        records.truncate(limit);
    }

    Ok(None)
}

fn apply_cursor_or_page_pagination(
    records: &mut Vec<Value>,
    modifiers: &QueryModifiers,
) -> Result<Option<PaginationMetadata>> {
    if modifiers.limit.is_some() || modifiers.offset.is_some() {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "cursor and page pagination cannot be combined with limit or offset",
        ));
    }

    let order_by = modifiers.order_by.as_deref().ok_or_else(|| {
        Error::new(
            ErrorKind::InvalidInput,
            "cursor and page pagination require an order_by option",
        )
    })?;
    let order_direction = modifiers
        .order_direction
        .unwrap_or(OrderDirection::Ascending);
    let page_size = modifiers.page_size.unwrap_or(DEFAULT_PAGE_SIZE);
    if page_size == 0 {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "page_size must be greater than zero",
        ));
    }

    let (start, page) = if let Some(raw_cursor) = modifiers.cursor.as_deref() {
        if modifiers.page.is_some() {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "cursor pagination cannot be combined with page",
            ));
        }
        let cursor = serde_json::from_str::<QueryCursor>(raw_cursor).map_err(|_| {
            Error::new(
                ErrorKind::InvalidInput,
                "cursor must be a valid Wardrobe pagination cursor",
            )
        })?;
        if cursor.order_by != order_by || cursor.order_direction != order_direction {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "cursor ordering does not match the requested order_by and order_direction",
            ));
        }
        let cursor_index = records
            .iter()
            .position(|record| record_id(record).is_some_and(|id| id == cursor.id))
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::InvalidInput,
                    "cursor record is not present in the query result",
                )
            })?;
        (cursor_index + 1, None)
    } else {
        let page = modifiers.page.unwrap_or(1);
        if page == 0 {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "page must be greater than zero",
            ));
        }
        let start = page
            .checked_sub(1)
            .and_then(|page_index| page_index.checked_mul(page_size))
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::InvalidInput,
                    "page and page_size exceed the supported range",
                )
            })?;
        (start, Some(page))
    };

    let mut page_records = if start >= records.len() {
        Vec::new()
    } else {
        records.drain(start..).collect()
    };
    let has_more = page_records.len() > page_size;
    page_records.truncate(page_size);
    let next_cursor = if has_more {
        let last_record = page_records.last().ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidData,
                "pagination produced no record for a non-empty page",
            )
        })?;
        Some(encode_cursor(last_record, order_by, order_direction)?)
    } else {
        None
    };
    *records = page_records;

    Ok(Some(PaginationMetadata {
        next_cursor,
        has_more,
        page,
        page_size,
    }))
}

fn encode_cursor(record: &Value, order_by: &str, order_direction: OrderDirection) -> Result<String> {
    let id = record_id(record).ok_or_else(|| {
        Error::new(
            ErrorKind::InvalidData,
            "cursor pagination requires every record to have a string _id",
        )
    })?;
    serde_json::to_string(&QueryCursor {
        order_by: order_by.to_string(),
        order_direction,
        id: id.to_string(),
    })
    .map_err(|_| Error::new(ErrorKind::InvalidData, "failed to encode pagination cursor"))
}

fn record_id(record: &Value) -> Option<&str> {
    record.get("_id").and_then(Value::as_str)
}

fn compare_records_by_field(
    left: &Value,
    right: &Value,
    field_name: &str,
    direction: OrderDirection,
    sort_type: Option<SortableType>,
) -> Ordering {
    let field_ordering = match sort_type {
        Some(sort_type) => match (
            resolve_field_path(left, field_name).and_then(|value| sortable_value(value, sort_type)),
            resolve_field_path(right, field_name)
                .and_then(|value| sortable_value(value, sort_type)),
        ) {
        (Some(left_sortable), Some(right_sortable)) => {
            match left_sortable.compare_same_type(right_sortable) {
                Some(ordering) => match direction {
                    OrderDirection::Ascending => ordering,
                    OrderDirection::Descending => ordering.reverse(),
                },
                None => Ordering::Equal,
            }
        }
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
        },
        None => Ordering::Equal,
    };

    if field_ordering == Ordering::Equal {
        compare_records_by_id(left, right, direction)
    } else {
        field_ordering
    }
}

fn compare_records_by_id(left: &Value, right: &Value, direction: OrderDirection) -> Ordering {
    let ordering = match (record_id(left), record_id(right)) {
        (Some(left_id), Some(right_id)) => left_id.cmp(right_id),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    };
    match direction {
        OrderDirection::Ascending => ordering,
        OrderDirection::Descending => ordering.reverse(),
    }
}

fn sort_type_for_records(records: &[Value], field_name: &str) -> Option<SortableType> {
    records.iter().find_map(|record| {
        resolve_field_path(record, field_name).and_then(|value| match value {
            Value::Bool(_) => Some(SortableType::Bool),
            Value::Number(_) => Some(SortableType::Number),
            Value::String(_) => Some(SortableType::String),
            _ => None,
        })
    })
}

fn sortable_value(value: &Value, sort_type: SortableType) -> Option<SortableValue<'_>> {
    match (value, sort_type) {
        (Value::Bool(value), SortableType::Bool) => Some(SortableValue::Bool(*value)),
        (Value::Number(value), SortableType::Number) => value.as_f64().map(SortableValue::Number),
        (Value::String(value), SortableType::String) => Some(SortableValue::String(value)),
        _ => None,
    }
}

pub(crate) fn id_only_reference(map: &Map<String, Value>) -> Option<&str> {
    if map.len() == 1 {
        map.get("_id").and_then(|value| value.as_str())
    } else {
        None
    }
}

fn relationship_drawer_name(field_name: &str) -> String {
    if let Some(stem) = field_name.strip_suffix("ies") {
        return format!("{}y", stem);
    }

    if field_name.ends_with('s') && !field_name.ends_with("ss") {
        return field_name
            .strip_suffix('s')
            .unwrap_or(field_name)
            .to_string();
    }

    field_name.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn fixed_field_paths_resolve_nested_objects_and_filters() {
        let record = json!({
            "_id": "hero",
            "attributes": {"strength": 18}
        });
        let filter = json!({"attributes.strength": 18});

        assert_eq!(
            resolve_field_path(&record, "attributes.strength"),
            Some(&json!(18))
        );
        assert!(resolve_field_path(&record, "attributes.dexterity").is_none());
        assert!(record_matches_filter(
            &record,
            filter.as_object().expect("filter"),
            None
        ));
    }

    #[test]
    fn query_modifiers_sort_by_fixed_field_paths() {
        let mut records = vec![
            json!({"_id": "weak", "attributes": {"strength": 8}}),
            json!({"_id": "strong", "attributes": {"strength": 18}}),
            json!({"_id": "missing", "attributes": {}}),
        ];
        let modifiers = QueryModifiers {
            order_by: Some("attributes.strength".to_string()),
            order_direction: Some(OrderDirection::Descending),
            limit: None,
            offset: None,
            ..QueryModifiers::default()
        };

        apply_query_modifiers(&mut records, Some(&modifiers))
            .expect("modifiers should apply");

        assert_eq!(records[0]["_id"], "strong");
        assert_eq!(records[1]["_id"], "weak");
        assert_eq!(records[2]["_id"], "missing");
    }

    #[test]
    fn filters_cover_nested_objects_arrays_references_and_scalar_operators() {
        let record = json!({
            "_id": "hero",
            "name": "Bob the Brave",
            "level": 7,
            "guild": "@guild:heroes",
            "attributes": {"strength": 18},
            "tags": ["hero", "fighter"]
        });

        for filter in [
            json!({"name": "Bob%"}),
            json!({"name": "%Brave"}),
            json!({"name": "%the%"}),
            json!({"level": {"$gte": 7, "$lt": 8}}),
            json!({"guild": {"_id": "heroes"}}),
            json!({"attributes": {"strength": 18}}),
            json!({"tags": ["hero", "fighter"]}),
        ] {
            assert!(record_matches_filter(
                &record,
                filter.as_object().expect("filter"),
                None
            ));
        }

        for filter in [
            json!({"name": "Alice%"}),
            json!({"level": {"$gt": 7}}),
            json!({"level": {"$unknown": 7}}),
            json!({"attributes": {"strength": 10}}),
            json!({"tags": ["hero"]}),
            json!({"missing": true}),
        ] {
            assert!(!record_matches_filter(
                &record,
                filter.as_object().expect("filter"),
                None
            ));
        }
        assert!(!record_matches_filter(
            &json!("not an object"),
            json!({}).as_object().expect("filter"),
            None
        ));
    }

    #[test]
    fn query_modifiers_cover_bool_string_pagination_and_empty_results() {
        let mut bool_records = vec![
            json!({"_id": "false", "active": false}),
            json!({"_id": "true", "active": true}),
        ];
        apply_query_modifiers(
            &mut bool_records,
            Some(&QueryModifiers {
                order_by: Some("active".to_string()),
                order_direction: None,
                limit: Some(1),
                offset: Some(1),
                ..QueryModifiers::default()
            }),
        )
        .expect("modifiers should apply");
        assert_eq!(bool_records, vec![json!({"_id": "true", "active": true})]);

        let mut string_records = vec![
            json!({"_id": "b", "name": "Beta"}),
            json!({"_id": "a", "name": "Alpha"}),
        ];
        apply_query_modifiers(
            &mut string_records,
            Some(&QueryModifiers {
                order_by: Some("name".to_string()),
                order_direction: None,
                limit: None,
                offset: None,
                ..QueryModifiers::default()
            }),
        )
        .expect("modifiers should apply");
        assert_eq!(string_records[0]["_id"], "a");

        apply_query_modifiers(
            &mut string_records,
            Some(&QueryModifiers {
                order_by: None,
                order_direction: None,
                limit: None,
                offset: Some(10),
                ..QueryModifiers::default()
            }),
        )
        .expect("modifiers should apply");
        assert!(string_records.is_empty());
        apply_query_modifiers(&mut string_records, None).expect("no modifiers should apply");
    }
}
