//! WASM template patching: find the sentinel slot in the pre-built template
//! and overwrite the program area with real WQL bytecode.

/// Pre-built WASM template containing the WVM runtime with a placeholder
/// program slot. The slot starts with a 16-byte sentinel (`WQLSLOT!` x2)
/// followed by a program area that we overwrite with the real bytecode.
const TEMPLATE: &[u8] = include_bytes!("../data/template.wasm");
const SENTINEL: &[u8; 16] = b"WQLSLOT!WQLSLOT!";
const SLOT_SIZE: usize = 8192;
const PROGRAM_OFFSET: usize = 16;

/// Maximum program size that fits in the template slot.
pub const MAX_PROGRAM_SIZE: usize = SLOT_SIZE - PROGRAM_OFFSET;

/// Patch the WASM template with the given WQL bytecode.
/// Returns the complete WASM module bytes ready to be written or loaded.
pub fn patch(bytecode: &[u8]) -> Result<Vec<u8>, String> {
    if bytecode.len() > MAX_PROGRAM_SIZE {
        return Err(format!(
            "program is {} bytes; maximum is {MAX_PROGRAM_SIZE}",
            bytecode.len()
        ));
    }

    let slot_pos = TEMPLATE
        .windows(SENTINEL.len())
        .position(|w| w == SENTINEL)
        .ok_or("sentinel not found in WASM template (template may be corrupt)")?;

    let mut wasm = TEMPLATE.to_vec();
    let program_start = slot_pos + PROGRAM_OFFSET;
    wasm[program_start..slot_pos + SLOT_SIZE].fill(0);
    wasm[program_start..program_start + bytecode.len()].copy_from_slice(bytecode);

    Ok(wasm)
}
