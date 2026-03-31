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
    /// Access the program header.
    #[must_use]
    pub fn header(&self) -> &ProgramHeader {
        &self.header
    }

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
/// `output` must be at least `input.len() + 5 * max_frame_depth` bytes to
/// accommodate the temporary gap used when rewriting length prefixes in
/// nested sub-messages.
///
/// # Errors
/// Returns `RuntimeError::OutputBufferTooSmall` if `output` is too small,
/// or `RuntimeError::MalformedInput` if the input is not valid protobuf.
pub fn project(
    program: &LoadedProgram,
    input: &[u8],
    output: &mut [u8],
) -> Result<usize, RuntimeError> {
    let required = input.len() + 5 * usize::from(program.header.max_frame_depth);
    if output.len() < required {
        return Err(RuntimeError::OutputBufferTooSmall);
    }
    let mut vm = Vm::new(
        &program.instructions,
        &program.label_table,
        program.header.max_frame_depth,
    );
    let (_, written) = vm.execute(0, input, output, 0)?;
    Ok(written)
}

/// Run a filter-only `program` against `input`, returning whether it matches.
///
/// Programs with FRAME (nested predicates) still need an output buffer for
/// the length-prefix rewrite during recursion. The buffer is allocated
/// internally when needed.
///
/// # Errors
/// Returns `RuntimeError::MalformedInput` if the input is not valid protobuf,
/// or `RuntimeError::StackUnderflow` if the program's bool stack is malformed.
pub fn filter(program: &LoadedProgram, input: &[u8]) -> Result<bool, RuntimeError> {
    let mut vm = Vm::new(
        &program.instructions,
        &program.label_table,
        program.header.max_frame_depth,
    );
    if program.header.max_frame_depth == 0 {
        let mut output = [];
        let (predicate, _) = vm.execute(0, input, &mut output, 0)?;
        Ok(predicate)
    } else {
        let required = input.len() + 5 * usize::from(program.header.max_frame_depth);
        let mut output = alloc::vec![0u8; required];
        let (predicate, _) = vm.execute(0, input, &mut output, 0)?;
        Ok(predicate)
    }
}

/// Project and filter `input` through `program`.
///
/// Returns `Some(bytes_written)` if the predicate is true, `None` if false.
/// `output` must be at least `input.len() + 5 * max_frame_depth` bytes.
///
/// # Errors
/// Returns `RuntimeError::OutputBufferTooSmall` if `output` is too small,
/// or `RuntimeError::MalformedInput` if the input is not valid protobuf.
pub fn project_and_filter(
    program: &LoadedProgram,
    input: &[u8],
    output: &mut [u8],
) -> Result<Option<usize>, RuntimeError> {
    let required = input.len() + 5 * usize::from(program.header.max_frame_depth);
    if output.len() < required {
        return Err(RuntimeError::OutputBufferTooSmall);
    }
    let mut vm = Vm::new(
        &program.instructions,
        &program.label_table,
        program.header.max_frame_depth,
    );
    let (predicate, written) = vm.execute(0, input, output, 0)?;
    Ok(if predicate { Some(written) } else { None })
}
