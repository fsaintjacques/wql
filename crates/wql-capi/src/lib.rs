//! C FFI layer for WQL.
//!
//! Exposes the WQL compiler and runtime as a C-compatible shared/static library.
//! All functions use `catch_unwind` for panic safety.
//!
// Reserved padding fields are public for C ABI stability.
#![allow(clippy::pub_underscore_fields)]
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

/// Program contains predicate logic.
pub const WQL_PROGRAM_FILTER: u8 = 0x01;
/// Program contains projection logic.
pub const WQL_PROGRAM_PROJECT: u8 = 0x02;

/// Program metadata returned by `wql_program_info`.
///
/// Zero-initialize before calling `wql_program_info`. New fields will be
/// appended into `_reserved`; existing fields are stable.
#[repr(C)]
pub struct wql_program_info_t {
    /// Bitmask of `WQL_PROGRAM_FILTER` and/or `WQL_PROGRAM_PROJECT`.
    pub program_type: u8,
    pub instruction_count: u32,
    pub register_count: u8,
    pub max_frame_depth: u8,
    pub _reserved: [u8; 24],
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

/// Populate `out` with metadata about a loaded program.
///
/// The caller should zero-initialize `out` before calling. This function
/// never fails.
///
/// # Safety
///
/// - `program` must be a valid pointer from `wql_program_load`.
/// - `out` must point to a valid `wql_program_info_t`.
#[no_mangle]
pub unsafe extern "C" fn wql_program_info(
    program: *const wql_program_t,
    out: *mut wql_program_info_t,
) {
    if program.is_null() || out.is_null() {
        return;
    }
    let prog = unsafe { &(*program).inner };
    let header = prog.header();
    let info = unsafe { &mut *out };
    info.program_type = 0;
    if header.has_predicate() {
        info.program_type |= WQL_PROGRAM_FILTER;
    }
    if header.has_projection() {
        info.program_type |= WQL_PROGRAM_PROJECT;
    }
    #[allow(clippy::cast_possible_truncation)]
    {
        info.instruction_count = prog.instruction_count() as u32;
    }
    info.register_count = header.register_count;
    info.max_frame_depth = header.max_frame_depth;
}

/// Result of `wql_eval`. Zero-initialize before calling.
///
/// New fields will be appended into `_reserved`; existing fields are stable.
#[repr(C)]
pub struct wql_eval_result_t {
    /// Bytes written to the output buffer (0 when the program has no projection).
    pub output_len: usize,
    /// Whether the record passed the predicate (`true` when no predicate).
    pub matched: bool,
    pub _reserved: [u8; 7],
}

// ═══════════════════════════════════════════════════════════════════════
// Execution
// ═══════════════════════════════════════════════════════════════════════

/// Evaluate a WQL program against input bytes.
///
/// Returns 0 on success, -1 on error. On success, `*result` is populated.
/// On error, `*errmsg` (if non-null) is set.
///
/// For filter-only programs, pass `output = NULL` / `output_len = 0`.
/// For project-only programs, `result->matched` is always `true`.
///
/// **Buffer sizing:** `output_len >= input_len` is always sufficient.
///
/// # Safety
///
/// - `program` must be a valid pointer from `wql_program_load`.
/// - `input` must point to `input_len` valid bytes.
/// - `output` (if non-null) must point to `output_len` writable bytes.
/// - `result` must point to a valid `wql_eval_result_t`.
/// - `errmsg`, if non-null, must point to a valid `*mut c_char` location.
#[no_mangle]
pub unsafe extern "C" fn wql_eval(
    program: *const wql_program_t,
    input: *const u8,
    input_len: usize,
    output: *mut u8,
    output_len: usize,
    result: *mut wql_eval_result_t,
    errmsg: *mut *mut std::ffi::c_char,
) -> i32 {
    if result.is_null() {
        set_errmsg(errmsg, "null result pointer");
        return -1;
    }

    let ret = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
        || -> Result<wql_runtime::EvalResult, String> {
            let prog = unsafe { &(*program).inner };
            let input_buf = unsafe { safe_slice(input, input_len) }?;
            let output_buf = unsafe { safe_slice_mut(output, output_len) }?;
            prog.eval(input_buf, output_buf).map_err(|e| format!("{e}"))
        },
    ));

    match ret {
        Ok(Ok(eval_result)) => {
            let out = unsafe { &mut *result };
            out.output_len = eval_result.output_len;
            out.matched = eval_result.matched;
            0
        }
        Ok(Err(msg)) => {
            set_errmsg(errmsg, &msg);
            -1
        }
        Err(_) => {
            set_errmsg(errmsg, "internal panic during eval");
            -1
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
        let slice = unsafe { core::slice::from_raw_parts_mut(bytes.data, bytes.len) };
        drop::<Box<[u8]>>(Box::from(slice));
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
