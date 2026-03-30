#![no_std]

extern crate alloc;

pub mod error;
pub(crate) mod wire;

#[cfg(test)]
pub(crate) mod test_utils;

pub use error::RuntimeError;
