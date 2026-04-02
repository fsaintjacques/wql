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

/// Result of evaluating a WQL program against an input record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EvalResult {
    /// Bytes written to the output buffer (0 when the program has no projection).
    pub output_len: usize,
    /// Whether the record passed the predicate (`true` when the program has no predicate).
    pub matched: bool,
}

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

    /// Number of decoded instructions in the program.
    #[must_use]
    pub fn instruction_count(&self) -> usize {
        self.instructions.len()
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

    /// Evaluate this program against `input`.
    ///
    /// The program header determines what happens:
    /// - **Filter-only**: `output` is unused; pass `&mut []`.
    /// - **Project-only**: `matched` is always `true`.
    /// - **Filter+project**: both fields populated.
    ///
    /// # Buffer sizing
    ///
    /// When the program has projection, `output` must be at least
    /// `input.len() + 5 * max_frame_depth` bytes.  For filter-only programs
    /// an empty slice is sufficient; an internal buffer is allocated when
    /// `max_frame_depth > 0`.
    ///
    /// # Errors
    /// Returns `RuntimeError::OutputBufferTooSmall` if `output` is too small
    /// for a program with projection, or `RuntimeError::MalformedInput` if
    /// the input is not valid protobuf.
    pub fn eval(&self, input: &[u8], output: &mut [u8]) -> Result<EvalResult, RuntimeError> {
        let depth = self.header.max_frame_depth;
        let required = input.len() + 5 * usize::from(depth);

        if output.len() >= required {
            // Output buffer is large enough — use it directly.
            let mut vm = Vm::new(&self.instructions, &self.label_table, depth);
            let (predicate, written) = vm.execute(0, input, output, 0)?;
            Ok(EvalResult {
                output_len: written,
                matched: predicate,
            })
        } else if input.is_empty() {
            // Empty input — nothing to scan, no buffer needed.
            Ok(EvalResult {
                output_len: 0,
                matched: true,
            })
        } else {
            // Output buffer too small (or not provided).
            // Allocate scratch internally; projected output is discarded.
            let mut scratch = alloc::vec![0u8; required];
            let mut vm = Vm::new(&self.instructions, &self.label_table, depth);
            let (predicate, _) = vm.execute(0, input, &mut scratch, 0)?;
            Ok(EvalResult {
                output_len: 0,
                matched: predicate,
            })
        }
    }
}
