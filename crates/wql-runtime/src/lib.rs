#![no_std]

extern crate alloc;

use alloc::vec::Vec;

pub mod error;
pub(crate) mod vm;
pub(crate) mod wire;

#[cfg(test)]
pub(crate) mod test_utils;
#[cfg(test)]
mod vm_tests;

pub use error::RuntimeError;

use vm::Vm;
use wql_ir::{Instruction, ProgramHeader};

/// A decoded, ready-to-execute WVM program.
pub struct LoadedProgram {
    header: ProgramHeader,
    instructions: Vec<Instruction>,
    /// `label_index` → `instruction_index` in `instructions`.
    label_table: Vec<usize>,
}

impl LoadedProgram {
    /// Decode a WVM program from its binary representation.
    ///
    /// # Errors
    /// Returns `DecodeError` if the binary is malformed or invalid.
    pub fn from_bytes(buf: &[u8]) -> Result<Self, wql_ir::DecodeError> {
        let (header, instructions) = wql_ir::decode(buf)?;

        let label_table = instructions
            .iter()
            .enumerate()
            .filter(|(_, instr)| matches!(instr, Instruction::Label))
            .map(|(i, _)| i)
            .collect();

        Ok(Self {
            header,
            instructions,
            label_table,
        })
    }
}

/// Project `input` through `program`, writing selected fields to `output`.
///
/// Returns the number of bytes written to `output`.
/// `output` must be at least as large as `input`.
///
/// # Errors
/// Returns `RuntimeError::OutputBufferTooSmall` if `output` is shorter than
/// `input`, or `RuntimeError::MalformedInput` if the input is not valid protobuf.
pub fn project(
    program: &LoadedProgram,
    input: &[u8],
    output: &mut [u8],
) -> Result<usize, RuntimeError> {
    if output.len() < input.len() {
        return Err(RuntimeError::OutputBufferTooSmall);
    }
    let mut vm = Vm::new(&program.instructions, &program.label_table);
    let (_, written) = vm.execute(0, input, output, 0)?;
    Ok(written)
}
