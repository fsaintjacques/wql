#![no_std]
#![allow(unsafe_code)]

#[cfg(not(target_arch = "wasm32"))]
compile_error!(
    "wql-wasm must be compiled for wasm32 \
     (use: cargo build -p wql-wasm --target wasm32-unknown-unknown --profile release-wasm)"
);

#[cfg(target_feature = "atomics")]
compile_error!(
    "wql-wasm requires single-threaded WASM; the atomics target feature is not supported"
);

#[cfg(target_arch = "wasm32")]
mod wasm {
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

    /// Fixed-size slot: 16-byte sentinel + program area.
    /// See `build.rs` for layout.
    const SLOT_SIZE: usize = 8192;
    const PROGRAM_OFFSET: usize = 16;
    static PROGRAM_SLOT: [u8; SLOT_SIZE] =
        *include_bytes!(concat!(env!("OUT_DIR"), "/program_slot.bin"));

    /// Extract the valid program slice from the slot's program area.
    /// Reads `bytecode_len` from the WQL header to determine actual size.
    ///
    /// Uses `black_box` to prevent LTO from constant-folding the slot contents.
    /// The slot is patched after compilation by `wqlc wasm`; the optimizer must
    /// not specialize code paths based on the placeholder program.
    fn program_bytes() -> &'static [u8] {
        const HEADER_SIZE: usize = 14;
        let slot = core::hint::black_box(&PROGRAM_SLOT);
        let base = PROGRAM_OFFSET;
        let bytecode_len = u32::from_le_bytes([
            slot[base + 10],
            slot[base + 11],
            slot[base + 12],
            slot[base + 13],
        ]) as usize;
        &slot[base..base + HEADER_SIZE + bytecode_len]
    }

    /// Single-threaded lazy cell. Sound because WASM has no threads (guarded by
    /// the `compile_error!` above when `target_feature = "atomics"` is enabled).
    struct SyncCell(UnsafeCell<Option<LoadedProgram>>);
    unsafe impl Sync for SyncCell {}

    static PROGRAM: SyncCell = SyncCell(UnsafeCell::new(None));

    fn program() -> &'static LoadedProgram {
        unsafe {
            (*PROGRAM.0.get()).get_or_insert_with(|| {
                LoadedProgram::from_bytes(program_bytes()).expect("program slot not patched")
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
                    // Safe: on wasm32 usize is 32-bit, always fits in i64.
                    result.output_len as i64
                } else {
                    -1
                }
            }
            Err(_) => -2,
        }
    }
}
