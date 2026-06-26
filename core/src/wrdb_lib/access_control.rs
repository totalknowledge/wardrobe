use super::diagnostics::split_structural_path;
use serde_json::{Value, json};
use std::fs;
use std::io::{Error, ErrorKind, Result};
use std::path::Path;

const ACCESS_CONTROL_FILE_NAME: &str = "_wardrobe_access_control.json";

pub(super) fn manage_user(root_directory: &Path, action: &str, payload: Value) -> Result<Value> {
    let normalized_action = action.replace('-', "_").to_ascii_lowercase();
    let mut registry = read_access_control_registry(root_directory)?;

    match normalized_action.as_str() {
        "add_user" | "add" | "create_user" => {
            let username = user_payload_username(&payload)?;
            let users = access_control_users_mut(&mut registry)?;
            let mut user_payload = payload;
            if let Value::Object(map) = &mut user_payload {
                map.insert("username".to_string(), Value::String(username.clone()));
            }
            users.insert(username.clone(), user_payload);
            write_access_control_registry(root_directory, &registry)?;
            Ok(json!({
                "ok": true,
                "action": "add_user",
                "username": username,
            }))
        }
        "grant_permission" | "revoke_permission" => {
            let username = permission_payload_username(&payload)?;
            let scope = permission_payload_scope(&payload)?;
            let users = access_control_users_mut(&mut registry)?;
            let user = users
                .entry(username.clone())
                .or_insert_with(|| json!({ "username": username.clone() }));
            let permissions = access_control_permissions_mut(user)?;

            if normalized_action == "grant_permission" {
                if !permissions
                    .iter()
                    .any(|permission| permission.as_str() == Some(scope.as_str()))
                {
                    permissions.push(Value::String(scope.clone()));
                }
            } else {
                permissions.retain(|permission| permission.as_str() != Some(scope.as_str()));
            }

            write_access_control_registry(root_directory, &registry)?;
            Ok(json!({
                "ok": true,
                "action": normalized_action,
                "username": username,
                "permission_scope": scope,
            }))
        }
        _ => {
            let username = payload
                .get("username")
                .or_else(|| payload.get("user"))
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string();
            let operations = access_control_operations_mut(&mut registry)?;
            operations.push(json!({
                "action": action,
                "username": username,
                "payload": payload,
            }));
            write_access_control_registry(root_directory, &registry)?;
            Ok(json!({
                "ok": true,
                "action": action,
                "username": username,
            }))
        }
    }
}

fn read_access_control_registry(root_directory: &Path) -> Result<Value> {
    let path = root_directory.join(ACCESS_CONTROL_FILE_NAME);
    if !path.exists() {
        return Ok(json!({ "users": {}, "operations": [] }));
    }
    let bytes = fs::read(path)?;
    serde_json::from_slice(&bytes).map_err(|error| {
        Error::new(
            ErrorKind::InvalidData,
            format!("Invalid access-control registry JSON: {error}"),
        )
    })
}

fn write_access_control_registry(root_directory: &Path, registry: &Value) -> Result<()> {
    fs::create_dir_all(root_directory)?;
    let bytes = serde_json::to_vec_pretty(registry).map_err(|error| {
        Error::new(
            ErrorKind::InvalidData,
            format!("Failed to serialize access-control registry: {error}"),
        )
    })?;
    fs::write(root_directory.join(ACCESS_CONTROL_FILE_NAME), bytes)
}

fn access_control_users_mut(registry: &mut Value) -> Result<&mut serde_json::Map<String, Value>> {
    if !registry.is_object() {
        *registry = json!({});
    }
    let object = registry.as_object_mut().ok_or_else(|| {
        Error::new(
            ErrorKind::InvalidData,
            "access-control registry must be a JSON object",
        )
    })?;
    let users = object.entry("users").or_insert_with(|| json!({}));
    if !users.is_object() {
        *users = json!({});
    }
    users.as_object_mut().ok_or_else(|| {
        Error::new(
            ErrorKind::InvalidData,
            "access-control users registry must be a JSON object",
        )
    })
}

fn access_control_operations_mut(registry: &mut Value) -> Result<&mut Vec<Value>> {
    if !registry.is_object() {
        *registry = json!({});
    }
    let object = registry.as_object_mut().ok_or_else(|| {
        Error::new(
            ErrorKind::InvalidData,
            "access-control registry must be a JSON object",
        )
    })?;
    let operations = object.entry("operations").or_insert_with(|| json!([]));
    if !operations.is_array() {
        *operations = json!([]);
    }
    operations.as_array_mut().ok_or_else(|| {
        Error::new(
            ErrorKind::InvalidData,
            "access-control operations registry must be a JSON array",
        )
    })
}

fn access_control_permissions_mut(user: &mut Value) -> Result<&mut Vec<Value>> {
    if !user.is_object() {
        *user = json!({});
    }
    let object = user.as_object_mut().ok_or_else(|| {
        Error::new(
            ErrorKind::InvalidData,
            "access-control user entry must be a JSON object",
        )
    })?;
    let permissions = object.entry("permissions").or_insert_with(|| json!([]));
    if !permissions.is_array() {
        *permissions = json!([]);
    }
    permissions.as_array_mut().ok_or_else(|| {
        Error::new(
            ErrorKind::InvalidData,
            "access-control permissions must be a JSON array",
        )
    })
}

fn user_payload_username(payload: &Value) -> Result<String> {
    let username = payload
        .get("username")
        .or_else(|| payload.get("user"))
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default();
    if username.is_empty() {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "user admin payload requires a non-empty username",
        ));
    }
    Ok(username.to_string())
}

fn permission_payload_username(payload: &Value) -> Result<String> {
    user_payload_username(payload)
}

fn permission_payload_scope(payload: &Value) -> Result<String> {
    if let Some(scope) = payload.get("permission_scope").and_then(Value::as_str) {
        return parse_permission_scope(scope);
    }
    if let Some(scope) = payload.get("scope").and_then(Value::as_object) {
        let path = scope
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let rights = scope
            .get("rights")
            .and_then(Value::as_str)
            .unwrap_or_default();
        return parse_permission_scope(&format!("{path}:{rights}"));
    }
    Err(Error::new(
        ErrorKind::InvalidInput,
        "permission payload requires a permission_scope",
    ))
}

fn parse_permission_scope(raw: &str) -> Result<String> {
    let raw = raw.trim();
    let mut parts = raw.split(':');
    let path_part = parts.next().unwrap_or_default().trim();
    let rights_part = parts.next().unwrap_or_default().trim();
    if parts.next().is_some() || path_part.is_empty() || rights_part.is_empty() {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "permission scope must use <path>:<rights>",
        ));
    }
    let segments = split_structural_path(path_part, "permission scope path")?;
    if segments.len() > 3 {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "permission scope path must identify a wardrobe, bay, or drawer",
        ));
    }
    let mut rights = String::new();
    for right in rights_part.chars().map(|right| right.to_ascii_lowercase()) {
        if !matches!(right, 'r' | 'u' | 'd' | 'i') {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "permission rights must contain only r, u, d, or i",
            ));
        }
        if rights.contains(right) {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                format!("permission right '{right}' cannot be repeated"),
            ));
        }
        rights.push(right);
    }
    if rights.is_empty() {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "permission scope requires at least one right",
        ));
    }
    Ok(format!("{}:{rights}", segments.join("/")))
}
