use std::env;
use std::fs;
use std::path::PathBuf;

/// Fixed-size slot embedded in the WASM binary. Layout:
///
///   [0..16)     sentinel: "WQLSLOT!WQLSLOT!"
///   [16..8192)  program area: valid WQL bytecode + zero padding
///
/// `wqlc wasm` finds the sentinel and patches the program area.
const SLOT_SIZE: usize = 8192;
const SENTINEL: &[u8; 16] = b"WQLSLOT!WQLSLOT!";

fn main() {
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let dst = out_dir.join("program_slot.bin");

    let mut slot = vec![0u8; SLOT_SIZE];

    // Sentinel at the start (used by wqlc to find the slot in the binary).
    slot[..16].copy_from_slice(SENTINEL);

    // Valid minimal WQL program at offset 16 so the compiler generates
    // real code (prevents LTO from proving the slot is always invalid).
    let program_offset = 16;
    // Header (14 bytes)
    slot[program_offset..program_offset + 4].copy_from_slice(b"WQL\x00"); // magic
    slot[program_offset + 4..program_offset + 6].copy_from_slice(&1u16.to_le_bytes()); // version
    slot[program_offset + 6] = 0; // register_count
    slot[program_offset + 7] = 0; // max_frame_depth
    slot[program_offset + 8..program_offset + 10].copy_from_slice(&0u16.to_le_bytes()); // flags
    slot[program_offset + 10..program_offset + 14].copy_from_slice(&1u32.to_le_bytes()); // bytecode_len=1
                                                                                         // Bytecode: single RETURN instruction
    slot[program_offset + 14] = 0x15; // OP_RETURN

    fs::write(&dst, slot).unwrap();
}
