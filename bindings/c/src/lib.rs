use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int};
use std::ptr;
use wardrobe_core::{Command, StatusRequest, WardrobeClient, WardrobeEngine};

#[repr(C)]
pub struct WardrobeCStatus {
    pub ok: c_int,
    pub database_count: usize,
}

#[unsafe(no_mangle)]
pub extern "C" fn wardrobe_cabi_version() -> *const c_char {
    static VERSION: &[u8] = b"wardrobe-cabi-0.1.0\0";
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
        Ok(wardrobe_core::StatusResult::Databases(databases)) => WardrobeCStatus {
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
