use libloading::{Library, Symbol};
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int};
use std::path::PathBuf;

#[repr(C)]
struct WardrobeCStatus {
    ok: c_int,
    database_count: usize,
}

fn main() {
    let lib_name = if cfg!(windows) {
        "wardrobe_c.dll"
    } else if cfg!(target_os = "macos") {
        "libwardrobe_c.dylib"
    } else {
        "libwardrobe_c.so"
    };

    let library_path = executable_directory().join(lib_name);
    let library = unsafe { Library::new(&library_path) }.unwrap_or_else(|_| {
        panic!(
            "sample should load wardrobe_c from {}",
            library_path.display()
        )
    });

    unsafe {
        let version: Symbol<unsafe extern "C" fn() -> *const c_char> = library
            .get(b"wardrobe_cabi_version")
            .expect("version symbol should load");
        let status: Symbol<unsafe extern "C" fn(*const c_char) -> WardrobeCStatus> = library
            .get(b"wardrobe_cabi_status_databases")
            .expect("status symbol should load");
        let duplicate: Symbol<unsafe extern "C" fn(*const c_char) -> *mut c_char> = library
            .get(b"wardrobe_cabi_duplicate_path")
            .expect("duplicate symbol should load");
        let free_string: Symbol<unsafe extern "C" fn(*mut c_char)> = library
            .get(b"wardrobe_cabi_free_string")
            .expect("free symbol should load");

        let version_ptr = version();
        let version = CStr::from_ptr(version_ptr)
            .to_str()
            .expect("version should be utf-8");
        println!("C ABI version: {version}");

        let path = CString::new("./wardrobe").expect("path should be valid");
        let duplicated = duplicate(path.as_ptr());
        assert!(
            !duplicated.is_null(),
            "duplicate path should return a string"
        );
        free_string(duplicated);

        let result = status(path.as_ptr());
        println!("Database count: {}", result.database_count);
    }
}

fn executable_directory() -> PathBuf {
    std::env::current_exe()
        .expect("current executable should be available")
        .parent()
        .expect("executable should have a parent directory")
        .to_path_buf()
}
