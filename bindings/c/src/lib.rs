use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int};
use std::ptr;
use wardrobe_core::{AlterRequest, Command, StatusRequest, WardrobeClient, WardrobeEngine};

#[repr(C)]
pub struct WardrobeCStatus {
    pub ok: c_int,
    pub database_count: usize,
}

#[unsafe(no_mangle)]
pub extern "C" fn wardrobe_cabi_version() -> *const c_char {
    static VERSION: &[u8] = b"wardrobe-cabi-0.26.724\0";
    VERSION.as_ptr().cast()
}

#[unsafe(no_mangle)]
pub extern "C" fn wardrobe_cabi_free_string(value: *mut c_char) {
    if value.is_null() {
        return;
    }
    unsafe {
        let _ = CString::from_raw(value);
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn wardrobe_cabi_duplicate_path(value: *const c_char) -> *mut c_char {
    if value.is_null() {
        return ptr::null_mut();
    }

    let value = match unsafe { CStr::from_ptr(value) }.to_str() {
        Ok(value) => value,
        Err(_) => return ptr::null_mut(),
    };

    match CString::new(value) {
        Ok(s) => s.into_raw(),
        Err(_) => ptr::null_mut(),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn wardrobe_cabi_relationship_command(
    drawer_name: *const c_char,
    field_name: *const c_char,
    target_drawer: *const c_char,
) -> *mut c_char {
    if drawer_name.is_null() || field_name.is_null() || target_drawer.is_null() {
        return ptr::null_mut();
    }

    let drawer_name = match unsafe { CStr::from_ptr(drawer_name) }.to_str() {
        Ok(value) => value,
        Err(_) => return ptr::null_mut(),
    };
    let field_name = match unsafe { CStr::from_ptr(field_name) }.to_str() {
        Ok(value) => value,
        Err(_) => return ptr::null_mut(),
    };
    let target_drawer = match unsafe { CStr::from_ptr(target_drawer) }.to_str() {
        Ok(value) => value,
        Err(_) => return ptr::null_mut(),
    };
    let command = Command::Alter(AlterRequest::relationship(
        drawer_name,
        field_name,
        target_drawer,
    ));
    let command_json = match serde_json::to_string(&command) {
        Ok(value) => value,
        Err(_) => return ptr::null_mut(),
    };

    match CString::new(command_json) {
        Ok(value) => value.into_raw(),
        Err(_) => ptr::null_mut(),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn wardrobe_cabi_status_databases(target: *const c_char) -> WardrobeCStatus {
    if target.is_null() {
        return WardrobeCStatus {
            ok: 0,
            database_count: 0,
        };
    }

    let target = match unsafe { CStr::from_ptr(target) }.to_str() {
        Ok(value) => value,
        Err(_) => {
            return WardrobeCStatus {
                ok: 0,
                database_count: 0,
            };
        }
    };

    let result = if target.starts_with("wardrobe://") || target.contains("://") {
        WardrobeClient::open(target).and_then(|client| client.status(StatusRequest::databases()))
    } else {
        WardrobeEngine::open(target).and_then(|engine| engine.status(StatusRequest::databases()))
    };

    match result {
        Ok(databases) => WardrobeCStatus {
            ok: 1,
            database_count: databases.len(),
        },
        _ => WardrobeCStatus {
            ok: 0,
            database_count: 0,
        },
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn wardrobe_cabi_execute_command(
    target: *const c_char,
    command_json: *const c_char,
) -> *mut c_char {
    if target.is_null() || command_json.is_null() {
        return ptr::null_mut();
    }

    let target_str = match unsafe { CStr::from_ptr(target) }.to_str() {
        Ok(s) => s,
        Err(_) => return ptr::null_mut(),
    };

    let command_str = match unsafe { CStr::from_ptr(command_json) }.to_str() {
        Ok(s) => s,
        Err(_) => return ptr::null_mut(),
    };

    let command: Command = match serde_json::from_str(command_str) {
        Ok(cmd) => cmd,
        Err(e) => {
            let err_msg = format!(
                "{{\"error\":\"Failed to deserialize command JSON: {}\"}}",
                e
            );
            return CString::new(err_msg).unwrap().into_raw();
        }
    };

    let engine = match WardrobeEngine::open(target_str) {
        Ok(eng) => eng,
        Err(e) => {
            let err_msg = format!("{{\"error\":\"Failed to open engine: {}\"}}", e);
            return CString::new(err_msg).unwrap().into_raw();
        }
    };

    let result = match engine.execute_command(command) {
        Ok(res) => res,
        Err(e) => {
            let err_msg = format!("{{\"error\":\"Execution failed: {}\"}}", e);
            return CString::new(err_msg).unwrap().into_raw();
        }
    };

    let result_json = match serde_json::to_string(&result) {
        Ok(json) => json,
        Err(e) => {
            let err_msg = format!(
                "{{\"error\":\"Failed to serialize command result: {}\"}}",
                e
            );
            return CString::new(err_msg).unwrap().into_raw();
        }
    };

    CString::new(result_json).unwrap().into_raw()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temporary_storage_path(test_name: &str) -> std::path::PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!("wardrobe_c_{test_name}_{nonce}"))
    }

    #[test]
    fn relationship_command_matches_the_canonical_command_contract() {
        let drawer_name = CString::new("w/b/character").expect("drawer");
        let field_name = CString::new("item_map").expect("field");
        let target_drawer = CString::new("w/b/item").expect("target");

        let command_json = wardrobe_cabi_relationship_command(
            drawer_name.as_ptr(),
            field_name.as_ptr(),
            target_drawer.as_ptr(),
        );

        assert!(!command_json.is_null());
        let serialized = unsafe { CStr::from_ptr(command_json) }
            .to_str()
            .expect("utf-8");
        let command: Command = serde_json::from_str(serialized).expect("command");
        assert_eq!(
            command,
            Command::Alter(AlterRequest::relationship(
                "w/b/character",
                "item_map",
                "w/b/item"
            ))
        );
        wardrobe_cabi_free_string(command_json);
    }

    #[test]
    fn version_duplicate_and_null_inputs_follow_the_c_contract() {
        let version = unsafe { CStr::from_ptr(wardrobe_cabi_version()) }
            .to_str()
            .expect("version");
        assert_eq!(version, "wardrobe-cabi-0.26.724");
        assert!(wardrobe_cabi_duplicate_path(ptr::null()).is_null());
        wardrobe_cabi_free_string(ptr::null_mut());

        let path = CString::new("./wardrobe").expect("path");
        let duplicate = wardrobe_cabi_duplicate_path(path.as_ptr());
        assert!(!duplicate.is_null());
        assert_eq!(
            unsafe { CStr::from_ptr(duplicate) }.to_str().expect("utf-8"),
            "./wardrobe"
        );
        wardrobe_cabi_free_string(duplicate);

        let invalid_utf8 = [0xff_u8, 0];
        assert!(wardrobe_cabi_duplicate_path(invalid_utf8.as_ptr().cast()).is_null());
        assert_eq!(
            wardrobe_cabi_status_databases(ptr::null()).ok,
            0
        );
        assert!(
            wardrobe_cabi_execute_command(ptr::null(), ptr::null()).is_null()
        );
    }

    #[test]
    fn embedded_status_and_command_execution_return_c_owned_results() {
        let storage_path = temporary_storage_path("execute");
        let target = CString::new(storage_path.to_string_lossy().as_bytes()).expect("target");

        let status = wardrobe_cabi_status_databases(target.as_ptr());
        assert_eq!(status.ok, 1);
        assert_eq!(status.database_count, 0);

        let invalid_command = CString::new("{invalid").expect("command");
        let invalid_result =
            wardrobe_cabi_execute_command(target.as_ptr(), invalid_command.as_ptr());
        assert!(!invalid_result.is_null());
        assert!(
            unsafe { CStr::from_ptr(invalid_result) }
                .to_str()
                .expect("utf-8")
                .contains("Failed to deserialize")
        );
        wardrobe_cabi_free_string(invalid_result);

        let command = Command::Status(StatusRequest::databases().into_request());
        let command_json =
            CString::new(serde_json::to_string(&command).expect("serialize")).expect("command");
        let result = wardrobe_cabi_execute_command(target.as_ptr(), command_json.as_ptr());
        assert!(!result.is_null());
        let parsed: serde_json::Value = serde_json::from_str(
            unsafe { CStr::from_ptr(result) }
                .to_str()
                .expect("utf-8"),
        )
        .expect("result");
        assert!(parsed["status"].is_array());
        wardrobe_cabi_free_string(result);

        std::fs::remove_dir_all(storage_path).expect("storage cleanup");
    }

    #[test]
    fn relationship_command_rejects_null_and_invalid_utf8_arguments() {
        let drawer = CString::new("character").expect("drawer");
        let field = CString::new("item_map").expect("field");
        let target = CString::new("item").expect("target");
        let invalid_utf8 = [0xff_u8, 0];

        assert!(
            wardrobe_cabi_relationship_command(ptr::null(), field.as_ptr(), target.as_ptr())
                .is_null()
        );
        assert!(
            wardrobe_cabi_relationship_command(
                invalid_utf8.as_ptr().cast(),
                field.as_ptr(),
                target.as_ptr(),
            )
            .is_null()
        );
        assert!(
            wardrobe_cabi_relationship_command(
                drawer.as_ptr(),
                invalid_utf8.as_ptr().cast(),
                target.as_ptr(),
            )
            .is_null()
        );
        assert!(
            wardrobe_cabi_relationship_command(
                drawer.as_ptr(),
                field.as_ptr(),
                invalid_utf8.as_ptr().cast(),
            )
            .is_null()
        );
    }
}
