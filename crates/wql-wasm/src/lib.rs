#![cfg_attr(target_arch = "wasm32", no_std)]

#[cfg(target_arch = "wasm32")]
extern crate alloc;

// WASM program shell defined in Block 7.

// Minimal allocator and panic handler required for no_std cdylib on wasm32.
// Both are replaced with real implementations in Block 7.
#[cfg(target_arch = "wasm32")]
#[allow(unsafe_code)]
mod wasm_compat {
    use core::alloc::{GlobalAlloc, Layout};

    struct BumpAllocator;

    unsafe impl GlobalAlloc for BumpAllocator {
        unsafe fn alloc(&self, _layout: Layout) -> *mut u8 {
            core::ptr::null_mut()
        }
        unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {}
    }

    #[global_allocator]
    static ALLOC: BumpAllocator = BumpAllocator;

    #[panic_handler]
    fn panic(_info: &core::panic::PanicInfo) -> ! {
        loop {}
    }
}
