use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int};
use std::ptr;
use wardrobe_core::{StatusRequest, WardrobeClient, WardrobeEngine};

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
