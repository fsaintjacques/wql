#![no_std]

extern crate alloc;

pub mod codec;
pub mod types;

pub use codec::DecodeError;
#[cfg(feature = "alloc")]
pub use codec::{decode, encode, InstructionIter};
pub use types::*;
