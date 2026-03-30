use crate::error::RuntimeError;
use crate::wire::{self, WireField, WireScanner};
use wql_ir::{ArmAction, ArmMatch, DefaultAction, Encoding, Instruction, WireType};

/// Hard cap on frame nesting to guard against malformed programs.
const MAX_FRAME_DEPTH_CAP: u8 = 64;

/// Decoded register value.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum RegisterValue {
    Int(i64),
    Bytes(alloc::vec::Vec<u8>),
}

pub(crate) struct Vm<'a> {
    instructions: &'a [Instruction],
    label_table: &'a [usize],
    max_frame_depth: u8,

    /// R0–R15. None = not set.
    registers: [Option<RegisterValue>; 16],

    /// Predicate bool stack.
    bool_stack: alloc::vec::Vec<bool>,

    /// Current FRAME nesting depth.
    frame_depth: u8,
}

impl<'a> Vm<'a> {
    pub fn new(
        instructions: &'a [Instruction],
        label_table: &'a [usize],
        max_frame_depth: u8,
    ) -> Self {
        Self {
            instructions,
            label_table,
            max_frame_depth: max_frame_depth.min(MAX_FRAME_DEPTH_CAP),
            registers: Default::default(),
            bool_stack: alloc::vec::Vec::new(),
            frame_depth: 0,
        }
    }

    /// Get a register value, returning `None` for out-of-bounds indices.
    fn reg(&self, idx: u8) -> Option<&RegisterValue> {
        self.registers.get(idx as usize).and_then(|r| r.as_ref())
    }

    /// Execute starting at `start_pc` over the given input window,
    /// writing to `output` at `output_cursor`.
    /// Returns `(predicate, output_bytes_written)`.
    #[allow(clippy::too_many_lines)]
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
                                        cursor = copy_field(output, cursor, &field)?;
                                    }
                                    ArmAction::Frame(label_idx) => {
                                        cursor =
                                            self.execute_frame(*label_idx, &field, output, cursor)?;
                                    }
                                    ArmAction::Skip => {}
                                    ArmAction::Decode { reg, encoding } => {
                                        self.decode_field(&field, *reg, *encoding);
                                    }
                                }
                            }
                        } else {
                            match default {
                                DefaultAction::Copy => {
                                    cursor = copy_field(output, cursor, &field)?;
                                }
                                DefaultAction::Skip => {}
                                DefaultAction::Recurse(label_idx) => {
                                    if field.wire_type == WireType::Len {
                                        cursor =
                                            self.execute_frame(*label_idx, &field, output, cursor)?;
                                    }
                                }
                            }
                        }
                    }
                    pc += 1;
                }
                Instruction::Return => {
                    let predicate = self.bool_stack.last().copied().unwrap_or(true);
                    return Ok((predicate, cursor - output_cursor));
                }

                // ── Predicate: integer comparisons ──
                Instruction::CmpEq { reg, imm } => {
                    let result = matches!(
                        self.reg(*reg),
                        Some(RegisterValue::Int(v)) if *v == *imm
                    );
                    self.bool_stack.push(result);
                    pc += 1;
                }
                Instruction::CmpNeq { reg, imm } => {
                    let result = match self.reg(*reg) {
                        Some(RegisterValue::Int(v)) => *v != *imm,
                        // Unset or type mismatch: not equal.
                        _ => true,
                    };
                    self.bool_stack.push(result);
                    pc += 1;
                }
                Instruction::CmpLt { reg, imm } => {
                    let result = matches!(
                        self.reg(*reg),
                        Some(RegisterValue::Int(v)) if *v < *imm
                    );
                    self.bool_stack.push(result);
                    pc += 1;
                }
                Instruction::CmpLte { reg, imm } => {
                    let result = matches!(
                        self.reg(*reg),
                        Some(RegisterValue::Int(v)) if *v <= *imm
                    );
                    self.bool_stack.push(result);
                    pc += 1;
                }
                Instruction::CmpGt { reg, imm } => {
                    let result = matches!(
                        self.reg(*reg),
                        Some(RegisterValue::Int(v)) if *v > *imm
                    );
                    self.bool_stack.push(result);
                    pc += 1;
                }
                Instruction::CmpGte { reg, imm } => {
                    let result = matches!(
                        self.reg(*reg),
                        Some(RegisterValue::Int(v)) if *v >= *imm
                    );
                    self.bool_stack.push(result);
                    pc += 1;
                }

                // ── Predicate: bytes comparisons ──
                Instruction::CmpLenEq { reg, bytes } => {
                    let result = matches!(
                        self.reg(*reg),
                        Some(RegisterValue::Bytes(v)) if v == bytes
                    );
                    self.bool_stack.push(result);
                    pc += 1;
                }
                Instruction::BytesStarts { reg, bytes } => {
                    let result = matches!(
                        self.reg(*reg),
                        Some(RegisterValue::Bytes(v)) if v.starts_with(bytes)
                    );
                    self.bool_stack.push(result);
                    pc += 1;
                }
                Instruction::BytesEnds { reg, bytes } => {
                    let result = matches!(
                        self.reg(*reg),
                        Some(RegisterValue::Bytes(v)) if v.ends_with(bytes)
                    );
                    self.bool_stack.push(result);
                    pc += 1;
                }
                Instruction::BytesContains { reg, bytes } => {
                    let result = if bytes.is_empty() {
                        matches!(self.reg(*reg), Some(RegisterValue::Bytes(_)))
                    } else {
                        matches!(
                            self.reg(*reg),
                            Some(RegisterValue::Bytes(v))
                                if v.windows(bytes.len()).any(|w| w == bytes.as_slice())
                        )
                    };
                    self.bool_stack.push(result);
                    pc += 1;
                }

                #[cfg(feature = "regex")]
                Instruction::BytesMatches { .. } => {
                    panic!("BYTES_MATCHES not implemented");
                }

                // ── Predicate: set / existence ──
                Instruction::InSet { reg, values } => {
                    let result = matches!(
                        self.reg(*reg),
                        Some(RegisterValue::Int(v)) if values.contains(v)
                    );
                    self.bool_stack.push(result);
                    pc += 1;
                }
                Instruction::IsSet { reg } => {
                    let result = self.reg(*reg).is_some();
                    self.bool_stack.push(result);
                    pc += 1;
                }

                // ── Logic ──
                Instruction::And => {
                    let b = self.bool_stack.pop().ok_or(RuntimeError::StackUnderflow)?;
                    let a = self.bool_stack.pop().ok_or(RuntimeError::StackUnderflow)?;
                    self.bool_stack.push(a && b);
                    pc += 1;
                }
                Instruction::Or => {
                    let b = self.bool_stack.pop().ok_or(RuntimeError::StackUnderflow)?;
                    let a = self.bool_stack.pop().ok_or(RuntimeError::StackUnderflow)?;
                    self.bool_stack.push(a || b);
                    pc += 1;
                }
                Instruction::Not => {
                    let a = self.bool_stack.pop().ok_or(RuntimeError::StackUnderflow)?;
                    self.bool_stack.push(!a);
                    pc += 1;
                }

                // Label, Copy, Skip, Decode outside of Dispatch — advance PC.
                _ => {
                    pc += 1;
                }
            }
        }

        // Implicit return at end of instruction stream.
        let predicate = self.bool_stack.last().copied().unwrap_or(true);
        Ok((predicate, cursor - output_cursor))
    }

    /// Decode a wire field value into a register.
    #[allow(clippy::cast_possible_wrap)] // intentional u64→i64 reinterpret for varint/zigzag
    fn decode_field(&mut self, field: &WireField<'_>, reg: u8, encoding: Encoding) {
        if reg as usize >= self.registers.len() {
            return;
        }
        let value = match encoding {
            Encoding::Varint => wire::read_varint(field.value_bytes, 0)
                .ok()
                .map(|(v, _)| RegisterValue::Int(v as i64)),
            Encoding::Sint => wire::read_varint(field.value_bytes, 0).ok().map(|(n, _)| {
                let decoded = ((n >> 1) as i64) ^ -((n & 1) as i64);
                RegisterValue::Int(decoded)
            }),
            Encoding::I32 => {
                if field.value_bytes.len() >= 4 {
                    let v = i32::from_le_bytes([
                        field.value_bytes[0],
                        field.value_bytes[1],
                        field.value_bytes[2],
                        field.value_bytes[3],
                    ]);
                    Some(RegisterValue::Int(i64::from(v)))
                } else {
                    None
                }
            }
            Encoding::I64 => {
                if field.value_bytes.len() >= 8 {
                    let v = i64::from_le_bytes([
                        field.value_bytes[0],
                        field.value_bytes[1],
                        field.value_bytes[2],
                        field.value_bytes[3],
                        field.value_bytes[4],
                        field.value_bytes[5],
                        field.value_bytes[6],
                        field.value_bytes[7],
                    ]);
                    Some(RegisterValue::Int(v))
                } else {
                    None
                }
            }
            Encoding::Len => Some(RegisterValue::Bytes(field.len_payload.to_vec())),
        };

        if let Some(v) = value {
            self.registers[reg as usize] = Some(v);
        }
    }

    /// Enter a sub-message scope via FRAME. Writes tag + gap-and-shift
    /// length prefix + recursively projected sub-output.
    fn execute_frame(
        &mut self,
        label_idx: u32,
        field: &WireField<'_>,
        output: &mut [u8],
        cursor: usize,
    ) -> Result<usize, RuntimeError> {
        if field.wire_type != WireType::Len {
            return Ok(cursor);
        }

        if self.frame_depth > self.max_frame_depth {
            return Err(RuntimeError::FrameDepthExceeded);
        }
        self.frame_depth += 1;

        // Write tag bytes + reserve 5 bytes for the length varint gap.
        let tag_len = field.tag_bytes.len();
        let sub_start = cursor + tag_len + 5;
        if sub_start > output.len() {
            self.frame_depth -= 1;
            return Err(RuntimeError::OutputBufferTooSmall);
        }
        output[cursor..cursor + tag_len].copy_from_slice(field.tag_bytes);
        let tag_end = cursor + tag_len;

        // Recurse into the sub-message.
        let target_pc = self.label_table[label_idx as usize];
        let result = self.execute(target_pc, field.len_payload, output, sub_start);
        self.frame_depth -= 1;
        let (_, sub_written) = result?;

        // Encode sub_written as a varint.
        #[allow(clippy::cast_possible_truncation)]
        let sub_written_u32 = sub_written as u32;
        let varint_len = write_varint(output, tag_end, sub_written_u32);

        // Shift sub-output left if the varint is shorter than 5 bytes.
        if varint_len < 5 {
            output.copy_within(sub_start..sub_start + sub_written, tag_end + varint_len);
        }

        Ok(tag_end + varint_len + sub_written)
    }
}

/// Copy a wire field (tag + value) to the output buffer. Returns the new cursor.
fn copy_field(
    output: &mut [u8],
    cursor: usize,
    field: &WireField<'_>,
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

/// Encode a u32 as a varint into `buf[pos..]`. Returns bytes written (1–5).
fn write_varint(buf: &mut [u8], pos: usize, mut value: u32) -> usize {
    let mut i = pos;
    loop {
        let mut byte = (value & 0x7F) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        buf[i] = byte;
        i += 1;
        if value == 0 {
            break;
        }
    }
    i - pos
}
