//! C FFI layer for WQL.
//!
//! Exposes the WQL compiler and runtime as a C-compatible shared/static library.
//! All functions use `catch_unwind` for panic safety.
//!
//! Ownership model:
//! - `wql_compile*` returns a `wql_bytes_t` that the caller must free with `wql_bytes_free`.
//! - `wql_project*` / `wql_filter` write into caller-provided buffers — no Rust-side allocation.
//! - Error messages are heap-allocated strings freed with `wql_errmsg_free`.

#![allow(non_camel_case_types)]

use std::ptr;
use std::slice;

// ═══════════════════════════════════════════════════════════════════════
// Types
// ═══════════════════════════════════════════════════════════════════════

/// Opaque handle to a loaded WQL program.
pub struct wql_program_t {
    inner: wql_runtime::LoadedProgram,
}

/// Owned byte buffer returned by `wql_compile*`.
/// The caller must free with `wql_bytes_free`.
#[repr(C)]
pub struct wql_bytes_t {
    pub data: *mut u8,
    pub len: usize,
}

// ═══════════════════════════════════════════════════════════════════════
// Compile
// ═══════════════════════════════════════════════════════════════════════

/// Compile a WQL query to bytecode (schema-free mode).
///
/// Returns a `wql_bytes_t` with the bytecode. On error, `data` is null and
/// `*errmsg` (if non-null) is set to a heap-allocated error string that
/// the caller must free with `wql_errmsg_free`.
///
/// # Safety
///
/// - `query` must be a valid UTF-8 C string (null-terminated).
/// - `errmsg`, if non-null, must point to a valid `*mut c_char` location.
#[no_mangle]
pub unsafe extern "C" fn wql_compile(
    query: *const std::ffi::c_char,
    errmsg: *mut *mut std::ffi::c_char,
) -> wql_bytes_t {
    wql_compile_with_schema(query, ptr::null(), 0, ptr::null(), errmsg)
}

/// Compile a WQL query to bytecode with an optional schema.
///
/// - `schema_ptr`/`schema_len`: serialized `FileDescriptorSet`. Pass null/0 for schema-free.
/// - `root_message`: fully-qualified message type (null for schema-free).
///
/// # Safety
///
/// - `query` must be a valid UTF-8 C string (null-terminated).
/// - `schema_ptr` (if non-null) must point to `schema_len` valid bytes.
/// - `root_message` (if non-null) must be a valid UTF-8 C string.
/// - `errmsg`, if non-null, must point to a valid `*mut c_char` location.
#[no_mangle]
pub unsafe extern "C" fn wql_compile_with_schema(
    query: *const std::ffi::c_char,
    schema_ptr: *const u8,
    schema_len: usize,
    root_message: *const std::ffi::c_char,
    errmsg: *mut *mut std::ffi::c_char,
) -> wql_bytes_t {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
        || -> Result<Vec<u8>, String> {
            let query_str = unsafe { cstr_to_str(query) }?;

            let schema = if schema_ptr.is_null() {
                None
            } else {
                Some(unsafe { safe_slice(schema_ptr, schema_len) }?)
            };

            let root_msg = if root_message.is_null() {
                None
            } else {
                Some(unsafe { cstr_to_str(root_message) }?)
            };

            let opts = wql_compiler::CompileOptions {
                schema,
                root_message: root_msg,
            };

            wql_compiler::compile(query_str, &opts).map_err(|e| format!("{e}"))
        },
    ));

    match result {
        Ok(Ok(bytecode)) => vec_to_wql_bytes(bytecode),
        Ok(Err(msg)) => {
            set_errmsg(errmsg, &msg);
            wql_bytes_t {
                data: ptr::null_mut(),
                len: 0,
            }
        }
        Err(_panic) => {
            set_errmsg(errmsg, "internal panic during compilation");
            wql_bytes_t {
                data: ptr::null_mut(),
                len: 0,
            }
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Program lifecycle
// ═══════════════════════════════════════════════════════════════════════

/// Load a compiled WQL program from bytecode.
///
/// Returns a pointer to a `wql_program_t`, or null on error.
///
/// # Safety
///
/// - `bytecode` must point to `len` valid bytes.
/// - `errmsg`, if non-null, must point to a valid `*mut c_char` location.
#[no_mangle]
pub unsafe extern "C" fn wql_program_load(
    bytecode: *const u8,
    len: usize,
    errmsg: *mut *mut std::ffi::c_char,
) -> *mut wql_program_t {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
        || -> Result<Box<wql_program_t>, String> {
            let buf = unsafe { safe_slice(bytecode, len) }?;
            let program =
                wql_runtime::LoadedProgram::from_bytes(buf).map_err(|e| format!("{e}"))?;
            Ok(Box::new(wql_program_t { inner: program }))
        },
    ));

    match result {
        Ok(Ok(boxed)) => Box::into_raw(boxed),
        Ok(Err(msg)) => {
            set_errmsg(errmsg, &msg);
            ptr::null_mut()
        }
        Err(_) => {
            set_errmsg(errmsg, "internal panic during program load");
            ptr::null_mut()
        }
    }
}

/// Free a loaded WQL program.
///
/// # Safety
///
/// `program` must be a pointer returned by `wql_program_load`, or null.
#[no_mangle]
pub unsafe extern "C" fn wql_program_free(program: *mut wql_program_t) {
    if !program.is_null() {
        drop(unsafe { Box::from_raw(program) });
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Execution
// ═══════════════════════════════════════════════════════════════════════

/// Run a filter (predicate-only) program on input bytes.
///
/// Returns 1 if the record passes, 0 if filtered out, -1 on error.
///
/// # Safety
///
/// - `program` must be a valid pointer from `wql_program_load`.
/// - `input` must point to `input_len` valid bytes.
/// - `errmsg`, if non-null, must point to a valid `*mut c_char` location.
#[no_mangle]
pub unsafe extern "C" fn wql_filter(
    program: *const wql_program_t,
    input: *const u8,
    input_len: usize,
    errmsg: *mut *mut std::ffi::c_char,
) -> i32 {
    let result =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<bool, String> {
            let prog = unsafe { &(*program).inner };
            let input_buf = unsafe { safe_slice(input, input_len) }?;
            wql_runtime::filter(prog, input_buf).map_err(|e| format!("{e}"))
        }));

    match result {
        Ok(Ok(true)) => 1,
        Ok(Ok(false)) => 0,
        Ok(Err(msg)) => {
            set_errmsg(errmsg, &msg);
            -1
        }
        Err(_) => {
            set_errmsg(errmsg, "internal panic during filter");
            -1
        }
    }
}

/// Run a projection program. Writes projected output into the caller's buffer.
///
/// Returns the number of bytes written to `output`, or -1 on error.
/// If the output buffer is too small, returns -1 and sets `*errmsg`.
///
/// **Buffer sizing:** projection output is always <= `input_len` bytes
/// (fields are stripped, never added). Passing `output_len >= input_len`
/// guarantees the buffer is large enough.
///
/// # Safety
///
/// - `program` must be a valid pointer from `wql_program_load`.
/// - `input` must point to `input_len` valid bytes.
/// - `output` must point to `output_len` writable bytes.
/// - `errmsg`, if non-null, must point to a valid `*mut c_char` location.
#[no_mangle]
pub unsafe extern "C" fn wql_project(
    program: *const wql_program_t,
    input: *const u8,
    input_len: usize,
    output: *mut u8,
    output_len: usize,
    errmsg: *mut *mut std::ffi::c_char,
) -> i64 {
    let result =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<usize, String> {
            let prog = unsafe { &(*program).inner };
            let input_buf = unsafe { safe_slice(input, input_len) }?;
            let output_buf = unsafe { safe_slice_mut(output, output_len) }?;
            wql_runtime::project(prog, input_buf, output_buf).map_err(|e| format!("{e}"))
        }));

    match result {
        #[allow(clippy::cast_possible_wrap)]
        Ok(Ok(n)) => n as i64,
        Ok(Err(msg)) => {
            set_errmsg(errmsg, &msg);
            -1
        }
        Err(_) => {
            set_errmsg(errmsg, "internal panic during project");
            -1
        }
    }
}

/// Run a combined filter+projection program. Writes output into the caller's buffer.
///
/// Returns:
/// -  `>= 0`: record passed; value is bytes written to `output`.
/// -  `-1`: record was filtered out (not an error).
/// -  `-2`: error; `*errmsg` is set.
///
/// **Buffer sizing:** see `wql_project` — `output_len >= input_len` is sufficient.
///
/// # Safety
///
/// - `program` must be a valid pointer from `wql_program_load`.
/// - `input` must point to `input_len` valid bytes.
/// - `output` must point to `output_len` writable bytes.
/// - `errmsg`, if non-null, must point to a valid `*mut c_char` location.
#[no_mangle]
pub unsafe extern "C" fn wql_project_and_filter(
    program: *const wql_program_t,
    input: *const u8,
    input_len: usize,
    output: *mut u8,
    output_len: usize,
    errmsg: *mut *mut std::ffi::c_char,
) -> i64 {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
        || -> Result<Option<usize>, String> {
            let prog = unsafe { &(*program).inner };
            let input_buf = unsafe { safe_slice(input, input_len) }?;
            let output_buf = unsafe { safe_slice_mut(output, output_len) }?;
            wql_runtime::project_and_filter(prog, input_buf, output_buf).map_err(|e| format!("{e}"))
        },
    ));

    match result {
        #[allow(clippy::cast_possible_wrap)]
        Ok(Ok(Some(n))) => n as i64,
        Ok(Ok(None)) => -1,
        Ok(Err(msg)) => {
            set_errmsg(errmsg, &msg);
            -2
        }
        Err(_) => {
            set_errmsg(errmsg, "internal panic during project_and_filter");
            -2
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Free helpers
// ═══════════════════════════════════════════════════════════════════════

/// Free a `wql_bytes_t` returned by `wql_compile*`.
///
/// # Safety
///
/// `bytes.data` must be a pointer returned by `wql_compile*`, or null.
#[no_mangle]
pub unsafe extern "C" fn wql_bytes_free(bytes: wql_bytes_t) {
    if !bytes.data.is_null() {
        drop(unsafe { Vec::from_raw_parts(bytes.data, bytes.len, bytes.len) });
    }
}

/// Free an error message string returned via `errmsg` parameters.
///
/// # Safety
///
/// `msg` must be a pointer set by a `wql_*` function, or null.
#[no_mangle]
pub unsafe extern "C" fn wql_errmsg_free(msg: *mut std::ffi::c_char) {
    if !msg.is_null() {
        drop(unsafe { std::ffi::CString::from_raw(msg) });
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Internal helpers
// ═══════════════════════════════════════════════════════════════════════

/// Safe slice from a C pointer + length. Returns an empty slice for len=0.
/// Returns Err if ptr is null but len > 0 (caller bug).
unsafe fn safe_slice<'a>(ptr: *const u8, len: usize) -> Result<&'a [u8], String> {
    if len == 0 {
        Ok(&[])
    } else if ptr.is_null() {
        Err("null pointer with non-zero length".into())
    } else {
        Ok(unsafe { slice::from_raw_parts(ptr, len) })
    }
}

/// Safe mutable slice from a C pointer + length.
/// Returns Err if ptr is null but len > 0 (caller bug).
unsafe fn safe_slice_mut<'a>(ptr: *mut u8, len: usize) -> Result<&'a mut [u8], String> {
    if len == 0 {
        Ok(&mut [])
    } else if ptr.is_null() {
        Err("null output pointer with non-zero length".into())
    } else {
        Ok(unsafe { slice::from_raw_parts_mut(ptr, len) })
    }
}

unsafe fn cstr_to_str<'a>(p: *const std::ffi::c_char) -> Result<&'a str, String> {
    if p.is_null() {
        return Err("null pointer".into());
    }
    unsafe { std::ffi::CStr::from_ptr(p) }
        .to_str()
        .map_err(|e| format!("invalid UTF-8: {e}"))
}

fn set_errmsg(errmsg: *mut *mut std::ffi::c_char, msg: &str) {
    if !errmsg.is_null() {
        // Truncate at first NUL to guarantee CString::new succeeds.
        let safe_msg = msg.split('\0').next().unwrap_or("unknown error");
        if let Ok(c) = std::ffi::CString::new(safe_msg) {
            unsafe { *errmsg = c.into_raw() };
        }
    }
}

fn vec_to_wql_bytes(mut v: Vec<u8>) -> wql_bytes_t {
    v.shrink_to_fit();
    let data = v.as_mut_ptr();
    let len = v.len();
    std::mem::forget(v);
    wql_bytes_t { data, len }
}
