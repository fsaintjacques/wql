#![no_std]
#![allow(unsafe_code)]

extern crate alloc;

use core::cell::UnsafeCell;
use core::slice;
use dlmalloc::GlobalDlmalloc;
use wql_runtime::LoadedProgram;

#[global_allocator]
static ALLOC: GlobalDlmalloc = GlobalDlmalloc;

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    core::arch::wasm32::unreachable()
}

static PROGRAM_BYTES: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/program.wqlbc"));

struct SyncCell(UnsafeCell<Option<LoadedProgram>>);
unsafe impl Sync for SyncCell {}

static PROGRAM: SyncCell = SyncCell(UnsafeCell::new(None));

/// WASM is single-threaded; no data race is possible.
fn program() -> &'static LoadedProgram {
    unsafe {
        (*PROGRAM.0.get()).get_or_insert_with(|| {
            LoadedProgram::from_bytes(PROGRAM_BYTES).expect("embedded program is invalid")
        })
    }
}

/// Evaluate the sealed WQL program on protobuf wire bytes.
///
/// # Returns
/// * `>= 0` — predicate matched; value is the number of bytes written to output
/// * `-1`   — predicate did not match; output contents are undefined
/// * `-2`   — runtime error
///
/// # Safety
///
/// Caller must ensure `in_ptr..in_ptr+in_len` and `out_ptr..out_ptr+out_len`
/// are valid, non-overlapping regions in WASM linear memory.
#[no_mangle]
pub extern "C" fn wql_eval(in_ptr: u32, in_len: u32, out_ptr: u32, out_len: u32) -> i64 {
    let input = unsafe { slice::from_raw_parts(in_ptr as *const u8, in_len as usize) };
    let output = unsafe { slice::from_raw_parts_mut(out_ptr as *mut u8, out_len as usize) };

    match program().eval(input, output) {
        Ok(result) => {
            if result.matched {
                result.output_len as i64
            } else {
                -1
            }
        }
        Err(_) => -2,
    }
}
