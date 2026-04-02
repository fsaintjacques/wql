#![no_std]

extern crate alloc;

pub mod codec;
pub mod types;

pub use codec::{decode, encode, encode_with_flags, DecodeError, InstructionIter};
pub use types::*;
