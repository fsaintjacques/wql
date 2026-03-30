use crate::error::RuntimeError;
use crate::wire::WireScanner;
use wql_ir::{ArmAction, ArmMatch, DefaultAction, Instruction};

/// Decoded register value.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum RegisterValue {
    Int(i64),
    Bytes(alloc::vec::Vec<u8>),
}

pub(crate) struct Vm<'a> {
    instructions: &'a [Instruction],
    label_table: &'a [usize],

    /// R0–R15. None = not set.
    registers: [Option<RegisterValue>; 16],

    /// Predicate bool stack.
    bool_stack: alloc::vec::Vec<bool>,
}

impl<'a> Vm<'a> {
    pub fn new(instructions: &'a [Instruction], label_table: &'a [usize]) -> Self {
        Self {
            instructions,
            label_table,
            registers: Default::default(),
            bool_stack: alloc::vec::Vec::new(),
        }
    }

    /// Execute starting at `start_pc` over the given input window,
    /// writing to `output` at `output_cursor`.
    /// Returns `(predicate, output_bytes_written)`.
    pub fn execute(
        &mut self,
        start_pc: usize,
        input: &[u8],
        output: &mut [u8],
        output_cursor: usize,
    ) -> Result<(bool, usize), RuntimeError> {
        let mut pc = start_pc;
        let mut cursor = output_cursor;

        while pc < self.instructions.len() {
            match &self.instructions[pc] {
                Instruction::Dispatch { default, arms } => {
                    let scanner = WireScanner::new(input);
                    for field_result in scanner {
                        let field = field_result?;

                        // Find first matching arm.
                        let matched_arm = arms.iter().find(|arm| match &arm.match_ {
                            ArmMatch::Field(num) => field.field_num == *num,
                            ArmMatch::FieldAndWireType(num, wt) => {
                                field.field_num == *num && field.wire_type == *wt
                            }
                        });

                        if let Some(arm) = matched_arm {
                            for action in &arm.actions {
                                match action {
                                    ArmAction::Copy => {
                                        cursor =
                                            copy_field(output, cursor, &field)?;
                                    }
                                    // Skip, Frame, Decode — no output in this chunk.
                                    ArmAction::Skip
                                    | ArmAction::Frame(_)
                                    | ArmAction::Decode { .. } => {}
                                }
                            }
                        } else {
                            match default {
                                DefaultAction::Copy => {
                                    cursor = copy_field(output, cursor, &field)?;
                                }
                                DefaultAction::Skip | DefaultAction::Recurse(_) => {}
                            }
                        }
                    }
                    pc += 1;
                }
                Instruction::Return => {
                    let predicate = self.bool_stack.last().copied().unwrap_or(true);
                    return Ok((predicate, cursor - output_cursor));
                }
                // Label is a no-op marker used for jump targets.
                Instruction::Label => {
                    pc += 1;
                }
                // Predicate and other instructions — no-ops in this chunk.
                _ => {
                    pc += 1;
                }
            }
        }

        // Implicit return at end of instruction stream.
        let predicate = self.bool_stack.last().copied().unwrap_or(true);
        Ok((predicate, cursor - output_cursor))
    }
}

/// Copy a wire field (tag + value) to the output buffer. Returns the new cursor.
fn copy_field(
    output: &mut [u8],
    cursor: usize,
    field: &crate::wire::WireField<'_>,
) -> Result<usize, RuntimeError> {
    let tag_len = field.tag_bytes.len();
    let val_len = field.value_bytes.len();
    let total = tag_len + val_len;
    let end = cursor + total;
    if end > output.len() {
        return Err(RuntimeError::OutputBufferTooSmall);
    }
    output[cursor..cursor + tag_len].copy_from_slice(field.tag_bytes);
    output[cursor + tag_len..end].copy_from_slice(field.value_bytes);
    Ok(end)
}
