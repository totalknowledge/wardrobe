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
    filter
        .as_object()
        .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "Filter root must be a JSON object"))
}

pub(crate) fn record_matches_filter(
    record: &Value,
    filter_map: &Map<String, Value>,
    drawer_namespace: Option<&str>,
) -> bool {
    let Value::Object(record_map) = record else {
        return false;
    };

    filter_map.iter().all(|(field_name, expected_value)| {
        record_map.get(field_name).is_some_and(|actual_value| {
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

pub(crate) fn apply_query_modifiers(records: &mut Vec<Value>, modifiers: Option<&QueryModifiers>) {
    let Some(modifiers) = modifiers else {
        return;
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
}

fn compare_records_by_field(
    left: &Value,
    right: &Value,
    field_name: &str,
    direction: OrderDirection,
    sort_type: Option<SortableType>,
) -> Ordering {
    let Some(sort_type) = sort_type else {
        return Ordering::Equal;
    };

    let left_value = left.get(field_name);
    let right_value = right.get(field_name);

    match (
        left_value.and_then(|value| sortable_value(value, sort_type)),
        right_value.and_then(|value| sortable_value(value, sort_type)),
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
    }
}

fn sort_type_for_records(records: &[Value], field_name: &str) -> Option<SortableType> {
    records.iter().find_map(|record| {
        record.get(field_name).and_then(|value| match value {
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

fn id_only_reference(map: &Map<String, Value>) -> Option<&str> {
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
