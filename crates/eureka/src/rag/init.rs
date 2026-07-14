#![allow(unsafe_code)]
//! One-time `sqlite-vec` extension registration.
//!
//! `sqlite-vec` implements vector similarity search inside SQLite. It must be
//! registered as an auto-extension before any `rusqlite::Connection` is opened
//! so that every subsequent connection automatically loads the extension.
//!
//! The registration uses `sqlite3_auto_extension`, a raw C API — one `unsafe`
//! block is unavoidable. It is isolated to this file and guarded by `OnceLock`
//! so it only executes once per process.

use rusqlite::ffi::sqlite3_auto_extension;
use sqlite_vec::sqlite3_vec_init;

static REGISTERED: std::sync::OnceLock<()> = std::sync::OnceLock::new();

/// Register the `sqlite-vec` extension for all subsequent SQLite connections
/// in this process.
///
/// Safe to call multiple times — the actual registration happens exactly once.
/// Must be called before the first `rusqlite::Connection::open`.
pub fn register_sqlite_vec() {
    REGISTERED.get_or_init(|| {
        // SAFETY: `sqlite3_vec_init` is a valid SQLite extension entry point.
        // `sqlite3_auto_extension` stores a function pointer that SQLite calls
        // for every new connection; the transmute reinterprets the void pointer
        // as the exact signature SQLite expects, which is sound for this specific
        // function.
        unsafe {
            sqlite3_auto_extension(Some(std::mem::transmute::<
                *const (),
                unsafe extern "C" fn(
                    *mut rusqlite::ffi::sqlite3,
                    *mut *mut ::std::os::raw::c_char,
                    *const rusqlite::ffi::sqlite3_api_routines,
                ) -> ::std::os::raw::c_int,
            >(sqlite3_vec_init as *const ())));
        }
    });
}
