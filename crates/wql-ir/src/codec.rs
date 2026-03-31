// Zigzag encode/decode intentionally reinterprets sign bits.
#![allow(clippy::cast_sign_loss, clippy::cast_possible_wrap)]

pub use crate::types::Program;
#[cfg(feature = "regex")]
use crate::types::FLAG_REGEX_REQUIRED;
use crate::types::{
    ArmAction, ArmMatch, DefaultAction, DispatchArm, Encoding, Instruction, ProgramHeader,
    WireType, HEADER_SIZE, MAGIC, VERSION,
};

use alloc::vec::Vec;

// ═══════════════════════════════════════════════════════════════════════
// DecodeError
// ═══════════════════════════════════════════════════════════════════════

/// Errors produced by [`decode`] and [`Program::from_bytes`].
#[derive(Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// Buffer too short to contain a valid header.
    TooShort,
    /// Magic bytes do not match [`MAGIC`].
    BadMagic,
    /// Header version is not [`VERSION`].
    UnsupportedVersion(u16),
    /// `bytecode_len` does not match the remaining buffer length.
    LengthMismatch,
    /// Unknown opcode encountered at the given bytecode offset.
    UnknownOpcode { offset: usize, opcode: u8 },
    /// A varint or byte sequence extends past end of bytecode.
    UnexpectedEof,
    /// A `FRAME`/`RECURSE` target is out of bounds or not a `LABEL`.
    InvalidTarget(u32),
    /// A `DISPATCH` arm had an empty action list.
    EmptyArmActions,
    /// A register index >= 16 was encountered.
    RegisterOutOfRange(u8),
    /// `BYTES_MATCHES` encountered but `regex` feature is not enabled.
    RegexNotSupported,
    /// A varint encoding is malformed (too many continuation bytes).
    MalformedVarint,
    /// A varint value overflows the target type (e.g. u64 → u32/usize).
    Overflow,
}

impl core::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::TooShort => write!(f, "buffer too short for header"),
            Self::BadMagic => write!(f, "bad magic bytes"),
            Self::UnsupportedVersion(v) => write!(f, "unsupported version {v}"),
            Self::LengthMismatch => write!(f, "bytecode_len does not match buffer"),
            Self::UnknownOpcode { offset, opcode } => {
                write!(f, "unknown opcode 0x{opcode:02X} at offset {offset}")
            }
            Self::UnexpectedEof => write!(f, "unexpected end of bytecode"),
            Self::InvalidTarget(t) => write!(f, "invalid target offset {t}"),
            Self::EmptyArmActions => write!(f, "DISPATCH arm has zero actions"),
            Self::RegisterOutOfRange(r) => write!(f, "register {r} out of range (max 15)"),
            Self::RegexNotSupported => write!(f, "BYTES_MATCHES requires regex feature"),
            Self::MalformedVarint => write!(f, "malformed varint encoding"),
            Self::Overflow => write!(f, "varint value overflows target type"),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Program::from_bytes
// ═══════════════════════════════════════════════════════════════════════

impl<'a> Program<'a> {
    /// Parse and validate the header. Does **not** decode individual
    /// instructions — use [`InstructionIter`] or [`decode`] for that.
    ///
    /// # Errors
    ///
    /// Returns [`DecodeError`] if the header is invalid or the buffer
    /// length does not match `bytecode_len`.
    pub fn from_bytes(buf: &'a [u8]) -> Result<Self, DecodeError> {
        if buf.len() < HEADER_SIZE {
            return Err(DecodeError::TooShort);
        }
        if buf[..4] != MAGIC {
            return Err(DecodeError::BadMagic);
        }
        let version = u16::from_le_bytes([buf[4], buf[5]]);
        if version != VERSION {
            return Err(DecodeError::UnsupportedVersion(version));
        }
        let register_count = buf[6];
        let max_frame_depth = buf[7];
        let flags = u16::from_le_bytes([buf[8], buf[9]]);
        let bytecode_len = u32::from_le_bytes([buf[10], buf[11], buf[12], buf[13]]);
        let expected = HEADER_SIZE + bytecode_len as usize;
        if buf.len() != expected {
            return Err(DecodeError::LengthMismatch);
        }
        Ok(Self {
            header: ProgramHeader {
                version,
                register_count,
                max_frame_depth,
                flags,
                bytecode_len,
            },
            bytecode: &buf[HEADER_SIZE..],
        })
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Varint helpers
// ═══════════════════════════════════════════════════════════════════════

fn zigzag_encode(n: i64) -> u64 {
    ((n << 1) ^ (n >> 63)) as u64
}

fn zigzag_decode(n: u64) -> i64 {
    ((n >> 1) as i64) ^ -((n & 1) as i64)
}

fn uvarint_size(value: u64) -> usize {
    if value == 0 {
        return 1;
    }
    let bits = 64 - value.leading_zeros() as usize;
    bits.div_ceil(7)
}

fn svarint_size(value: i64) -> usize {
    uvarint_size(zigzag_encode(value))
}

// ───────────────────────── Reader

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn is_empty(&self) -> bool {
        self.pos >= self.buf.len()
    }

    fn read_u8(&mut self) -> Result<u8, DecodeError> {
        if self.pos >= self.buf.len() {
            return Err(DecodeError::UnexpectedEof);
        }
        let v = self.buf[self.pos];
        self.pos += 1;
        Ok(v)
    }

    fn read_uvarint(&mut self) -> Result<u64, DecodeError> {
        let mut result: u64 = 0;
        let mut shift: u32 = 0;
        loop {
            let byte = self.read_u8()?;
            result |= u64::from(byte & 0x7F) << shift;
            if byte & 0x80 == 0 {
                return Ok(result);
            }
            shift += 7;
            if shift >= 64 {
                return Err(DecodeError::MalformedVarint);
            }
        }
    }

    fn read_svarint(&mut self) -> Result<i64, DecodeError> {
        self.read_uvarint().map(zigzag_decode)
    }

    /// Read a uvarint and convert to usize, rejecting overflow.
    fn read_uvarint_as_usize(&mut self) -> Result<usize, DecodeError> {
        let v = self.read_uvarint()?;
        usize::try_from(v).map_err(|_| DecodeError::Overflow)
    }

    /// Read a uvarint and convert to u32, rejecting overflow.
    fn read_uvarint_as_u32(&mut self) -> Result<u32, DecodeError> {
        let v = self.read_uvarint()?;
        u32::try_from(v).map_err(|_| DecodeError::Overflow)
    }

    /// Remaining bytes in the buffer from current position.
    fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    fn read_bytes_vec(&mut self) -> Result<Vec<u8>, DecodeError> {
        let len = self.read_uvarint_as_usize()?;
        if self.pos + len > self.buf.len() {
            return Err(DecodeError::UnexpectedEof);
        }
        let v = self.buf[self.pos..self.pos + len].to_vec();
        self.pos += len;
        Ok(v)
    }

    fn skip_bytes(&mut self) -> Result<(), DecodeError> {
        let len = self.read_uvarint_as_usize()?;
        if self.pos + len > self.buf.len() {
            return Err(DecodeError::UnexpectedEof);
        }
        self.pos += len;
        Ok(())
    }
}

// ───────────────────────── Writer

struct Writer {
    buf: Vec<u8>,
}

impl Writer {
    fn new() -> Self {
        Self { buf: Vec::new() }
    }

    fn put_u8(&mut self, v: u8) {
        self.buf.push(v);
    }

    fn put_uvarint(&mut self, mut value: u64) {
        loop {
            let mut byte = (value & 0x7F) as u8;
            value >>= 7;
            if value != 0 {
                byte |= 0x80;
            }
            self.buf.push(byte);
            if value == 0 {
                break;
            }
        }
    }

    fn put_svarint(&mut self, value: i64) {
        self.put_uvarint(zigzag_encode(value));
    }

    fn put_bytes(&mut self, b: &[u8]) {
        self.put_uvarint(b.len() as u64);
        self.buf.extend_from_slice(b);
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Opcodes
// ═══════════════════════════════════════════════════════════════════════

const OP_DISPATCH: u8 = 0x00;
const OP_LABEL: u8 = 0x01;
const OP_COPY: u8 = 0x02;
const OP_SKIP: u8 = 0x03;
const OP_DECODE: u8 = 0x04;
const OP_CMP_EQ: u8 = 0x05;
const OP_CMP_NEQ: u8 = 0x06;
const OP_CMP_LT: u8 = 0x07;
const OP_CMP_LTE: u8 = 0x08;
const OP_CMP_GT: u8 = 0x09;
const OP_CMP_GTE: u8 = 0x0A;
const OP_CMP_LEN_EQ: u8 = 0x0B;
const OP_BYTES_STARTS: u8 = 0x0C;
const OP_BYTES_ENDS: u8 = 0x0D;
const OP_BYTES_CONTAINS: u8 = 0x0E;
const OP_BYTES_MATCHES: u8 = 0x0F;
const OP_IN_SET: u8 = 0x10;
const OP_IS_SET: u8 = 0x11;
const OP_AND: u8 = 0x12;
const OP_OR: u8 = 0x13;
const OP_NOT: u8 = 0x14;
const OP_RETURN: u8 = 0x15;

// ═══════════════════════════════════════════════════════════════════════
// Encode
// ═══════════════════════════════════════════════════════════════════════

fn check_reg(reg: u8) {
    debug_assert!(reg < 16, "register index {reg} out of range (max 15)");
}

/// Compute the byte size of an encoded instruction, given current label offsets.
/// `label_offsets` may be empty on the first iteration of label resolution.
fn instruction_size(instr: &Instruction, label_offsets: &[u32]) -> usize {
    fn resolve(label_offsets: &[u32], idx: u32) -> u64 {
        label_offsets.get(idx as usize).copied().unwrap_or(0).into()
    }

    match instr {
        Instruction::Label
        | Instruction::Copy
        | Instruction::Skip
        | Instruction::And
        | Instruction::Or
        | Instruction::Not
        | Instruction::Return => 1,

        Instruction::Decode { .. } => 3,
        Instruction::IsSet { .. } => 2,

        Instruction::CmpEq { imm, .. }
        | Instruction::CmpNeq { imm, .. }
        | Instruction::CmpLt { imm, .. }
        | Instruction::CmpLte { imm, .. }
        | Instruction::CmpGt { imm, .. }
        | Instruction::CmpGte { imm, .. } => 1 + 1 + svarint_size(*imm),

        Instruction::CmpLenEq { bytes, .. }
        | Instruction::BytesStarts { bytes, .. }
        | Instruction::BytesEnds { bytes, .. }
        | Instruction::BytesContains { bytes, .. } => {
            1 + 1 + uvarint_size(bytes.len() as u64) + bytes.len()
        }

        #[cfg(feature = "regex")]
        Instruction::BytesMatches { pattern, .. } => {
            1 + 1 + uvarint_size(pattern.len() as u64) + pattern.len()
        }

        Instruction::InSet { values, .. } => {
            1 + 1
                + uvarint_size(values.len() as u64)
                + values.iter().map(|v| svarint_size(*v)).sum::<usize>()
        }

        Instruction::Dispatch { default, arms } => {
            let mut s = 1 + 1; // opcode + default_kind
            if let DefaultAction::Recurse(idx) = default {
                s += uvarint_size(resolve(label_offsets, *idx));
            }
            s += uvarint_size(arms.len() as u64);
            for arm in arms {
                s += 1; // match_kind
                match &arm.match_ {
                    ArmMatch::Field(n) => s += uvarint_size(u64::from(*n)),
                    ArmMatch::FieldAndWireType(n, _) => {
                        s += uvarint_size(u64::from(*n)) + 1;
                    }
                }
                s += uvarint_size(arm.actions.len() as u64);
                for action in &arm.actions {
                    s += 1; // action_kind
                    match action {
                        ArmAction::Copy | ArmAction::Skip => {}
                        ArmAction::Decode { .. } => s += 2,
                        ArmAction::Frame(idx) => {
                            s += uvarint_size(resolve(label_offsets, *idx));
                        }
                    }
                }
            }
            s
        }
    }
}

/// Iteratively resolve label indices to absolute bytecode byte offsets.
#[allow(clippy::cast_possible_truncation)] // bytecode << 4 GB
fn resolve_label_offsets(instructions: &[Instruction]) -> Vec<u32> {
    let mut offsets: Vec<u32> = Vec::new();
    loop {
        let mut new_offsets: Vec<u32> = Vec::new();
        let mut pos: u32 = 0;
        for instr in instructions {
            if matches!(instr, Instruction::Label) {
                new_offsets.push(pos);
            }
            pos += instruction_size(instr, &offsets) as u32;
        }
        if new_offsets == offsets {
            return offsets;
        }
        offsets = new_offsets;
    }
}

#[allow(clippy::too_many_lines)]
fn encode_instruction(w: &mut Writer, instr: &Instruction, label_offsets: &[u32]) {
    fn resolve(label_offsets: &[u32], idx: u32) -> u64 {
        u64::from(label_offsets[idx as usize])
    }

    match instr {
        Instruction::Label => w.put_u8(OP_LABEL),
        Instruction::Copy => w.put_u8(OP_COPY),
        Instruction::Skip => w.put_u8(OP_SKIP),
        Instruction::And => w.put_u8(OP_AND),
        Instruction::Or => w.put_u8(OP_OR),
        Instruction::Not => w.put_u8(OP_NOT),
        Instruction::Return => w.put_u8(OP_RETURN),

        Instruction::Decode { reg, encoding } => {
            check_reg(*reg);
            w.put_u8(OP_DECODE);
            w.put_u8(*reg);
            w.put_u8(*encoding as u8);
        }

        Instruction::IsSet { reg } => {
            check_reg(*reg);
            w.put_u8(OP_IS_SET);
            w.put_u8(*reg);
        }

        Instruction::CmpEq { reg, imm } => {
            check_reg(*reg);
            w.put_u8(OP_CMP_EQ);
            w.put_u8(*reg);
            w.put_svarint(*imm);
        }
        Instruction::CmpNeq { reg, imm } => {
            check_reg(*reg);
            w.put_u8(OP_CMP_NEQ);
            w.put_u8(*reg);
            w.put_svarint(*imm);
        }
        Instruction::CmpLt { reg, imm } => {
            check_reg(*reg);
            w.put_u8(OP_CMP_LT);
            w.put_u8(*reg);
            w.put_svarint(*imm);
        }
        Instruction::CmpLte { reg, imm } => {
            check_reg(*reg);
            w.put_u8(OP_CMP_LTE);
            w.put_u8(*reg);
            w.put_svarint(*imm);
        }
        Instruction::CmpGt { reg, imm } => {
            check_reg(*reg);
            w.put_u8(OP_CMP_GT);
            w.put_u8(*reg);
            w.put_svarint(*imm);
        }
        Instruction::CmpGte { reg, imm } => {
            check_reg(*reg);
            w.put_u8(OP_CMP_GTE);
            w.put_u8(*reg);
            w.put_svarint(*imm);
        }

        Instruction::CmpLenEq { reg, bytes } => {
            check_reg(*reg);
            w.put_u8(OP_CMP_LEN_EQ);
            w.put_u8(*reg);
            w.put_bytes(bytes);
        }
        Instruction::BytesStarts { reg, bytes } => {
            check_reg(*reg);
            w.put_u8(OP_BYTES_STARTS);
            w.put_u8(*reg);
            w.put_bytes(bytes);
        }
        Instruction::BytesEnds { reg, bytes } => {
            check_reg(*reg);
            w.put_u8(OP_BYTES_ENDS);
            w.put_u8(*reg);
            w.put_bytes(bytes);
        }
        Instruction::BytesContains { reg, bytes } => {
            check_reg(*reg);
            w.put_u8(OP_BYTES_CONTAINS);
            w.put_u8(*reg);
            w.put_bytes(bytes);
        }

        #[cfg(feature = "regex")]
        Instruction::BytesMatches { reg, pattern } => {
            check_reg(*reg);
            w.put_u8(OP_BYTES_MATCHES);
            w.put_u8(*reg);
            w.put_bytes(pattern);
        }

        Instruction::InSet { reg, values } => {
            check_reg(*reg);
            w.put_u8(OP_IN_SET);
            w.put_u8(*reg);
            w.put_uvarint(values.len() as u64);
            for v in values {
                w.put_svarint(*v);
            }
        }

        Instruction::Dispatch { default, arms } => {
            w.put_u8(OP_DISPATCH);
            match default {
                DefaultAction::Skip => w.put_u8(0),
                DefaultAction::Copy => w.put_u8(1),
                DefaultAction::Recurse(idx) => {
                    w.put_u8(2);
                    w.put_uvarint(resolve(label_offsets, *idx));
                }
            }
            w.put_uvarint(arms.len() as u64);
            for arm in arms {
                match &arm.match_ {
                    ArmMatch::Field(n) => {
                        w.put_u8(0);
                        w.put_uvarint(u64::from(*n));
                    }
                    ArmMatch::FieldAndWireType(n, wt) => {
                        w.put_u8(1);
                        w.put_uvarint(u64::from(*n));
                        w.put_u8(*wt as u8);
                    }
                }
                w.put_uvarint(arm.actions.len() as u64);
                for action in &arm.actions {
                    match action {
                        ArmAction::Copy => w.put_u8(0),
                        ArmAction::Skip => w.put_u8(1),
                        ArmAction::Decode { reg, encoding } => {
                            check_reg(*reg);
                            w.put_u8(2);
                            w.put_u8(*reg);
                            w.put_u8(*encoding as u8);
                        }
                        ArmAction::Frame(idx) => {
                            w.put_u8(3);
                            w.put_uvarint(resolve(label_offsets, *idx));
                        }
                    }
                }
            }
        }
    }
}

fn compute_register_count(instructions: &[Instruction]) -> u8 {
    let mut max_reg: Option<u8> = None;
    let mut update = |r: u8| {
        max_reg = Some(max_reg.map_or(r, |m| m.max(r)));
    };

    for instr in instructions {
        match instr {
            Instruction::Decode { reg, .. }
            | Instruction::CmpEq { reg, .. }
            | Instruction::CmpNeq { reg, .. }
            | Instruction::CmpLt { reg, .. }
            | Instruction::CmpLte { reg, .. }
            | Instruction::CmpGt { reg, .. }
            | Instruction::CmpGte { reg, .. }
            | Instruction::CmpLenEq { reg, .. }
            | Instruction::BytesStarts { reg, .. }
            | Instruction::BytesEnds { reg, .. }
            | Instruction::BytesContains { reg, .. }
            | Instruction::InSet { reg, .. }
            | Instruction::IsSet { reg, .. } => update(*reg),

            #[cfg(feature = "regex")]
            Instruction::BytesMatches { reg, .. } => update(*reg),

            Instruction::Dispatch { arms, .. } => {
                for arm in arms {
                    for action in &arm.actions {
                        if let ArmAction::Decode { reg, .. } = action {
                            update(*reg);
                        }
                    }
                }
            }

            _ => {}
        }
    }

    max_reg.map_or(0, |r| r + 1)
}

#[allow(clippy::cast_possible_truncation)] // capped at 255
fn compute_max_frame_depth(instructions: &[Instruction]) -> u8 {
    // Count distinct labels that are referenced by Frame or Recurse actions.
    // This is a tighter upper bound than counting all labels.
    let mut referenced_labels = alloc::collections::BTreeSet::new();
    for instr in instructions {
        if let Instruction::Dispatch { default, arms } = instr {
            if let DefaultAction::Recurse(idx) = default {
                referenced_labels.insert(*idx);
            }
            for arm in arms {
                for action in &arm.actions {
                    if let ArmAction::Frame(idx) = action {
                        referenced_labels.insert(*idx);
                    }
                }
            }
        }
    }
    referenced_labels.len().min(255) as u8
}

fn compute_flags(instructions: &[Instruction]) -> u16 {
    use crate::types::{FLAG_HAS_PREDICATE, FLAG_HAS_PROJECTION};

    let mut flags = 0u16;

    #[cfg(feature = "regex")]
    {
        for instr in instructions {
            if matches!(instr, Instruction::BytesMatches { .. }) {
                flags |= FLAG_REGEX_REQUIRED;
                break;
            }
        }
    }

    for instr in instructions {
        match instr {
            Instruction::Dispatch { default, arms } => {
                // Projection: default Copy/Recurse, or arms with Copy/Frame
                if matches!(default, DefaultAction::Copy | DefaultAction::Recurse(_)) {
                    flags |= FLAG_HAS_PROJECTION;
                }
                for arm in arms {
                    for action in &arm.actions {
                        match action {
                            ArmAction::Copy | ArmAction::Frame(_) => {
                                flags |= FLAG_HAS_PROJECTION;
                            }
                            ArmAction::Decode { .. } => flags |= FLAG_HAS_PREDICATE,
                            ArmAction::Skip => {}
                        }
                    }
                }
            }
            Instruction::CmpEq { .. }
            | Instruction::CmpNeq { .. }
            | Instruction::CmpLt { .. }
            | Instruction::CmpLte { .. }
            | Instruction::CmpGt { .. }
            | Instruction::CmpGte { .. }
            | Instruction::CmpLenEq { .. }
            | Instruction::BytesStarts { .. }
            | Instruction::BytesEnds { .. }
            | Instruction::BytesContains { .. }
            | Instruction::InSet { .. }
            | Instruction::IsSet { .. }
            | Instruction::And
            | Instruction::Or
            | Instruction::Not => flags |= FLAG_HAS_PREDICATE,
            _ => {}
        }
    }

    flags
}

/// Encode a sequence of instructions into a complete WVM program binary.
///
/// `register_count`, `max_frame_depth`, and `flags` are computed
/// automatically by scanning the instruction list.
///
/// Target `u32` values in [`DefaultAction::Recurse`] and
/// [`ArmAction::Frame`] are **label indices** (0-based among
/// `Instruction::Label` entries). The encoder resolves these to
/// absolute byte offsets in the bytecode.
///
/// # Panics
///
/// Panics (in debug builds) if any register index >= 16 or if a label
/// index references a non-existent label.
#[must_use]
#[allow(clippy::cast_possible_truncation)] // bytecode << 4 GB
pub fn encode(instructions: &[Instruction]) -> Vec<u8> {
    let label_offsets = resolve_label_offsets(instructions);
    let register_count = compute_register_count(instructions);
    let max_frame_depth = compute_max_frame_depth(instructions);
    let flags = compute_flags(instructions);

    let mut w = Writer::new();
    for instr in instructions {
        encode_instruction(&mut w, instr, &label_offsets);
    }
    let bytecode = w.buf;

    let mut out = Vec::with_capacity(HEADER_SIZE + bytecode.len());
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.push(register_count);
    out.push(max_frame_depth);
    out.extend_from_slice(&flags.to_le_bytes());
    out.extend_from_slice(&(bytecode.len() as u32).to_le_bytes());
    out.extend_from_slice(&bytecode);
    out
}

// ═══════════════════════════════════════════════════════════════════════
// Decode
// ═══════════════════════════════════════════════════════════════════════

fn check_reg_decode(reg: u8) -> Result<(), DecodeError> {
    if reg >= 16 {
        Err(DecodeError::RegisterOutOfRange(reg))
    } else {
        Ok(())
    }
}

/// Decode a single instruction from the reader, returning raw byte
/// offsets for any FRAME/RECURSE targets (not yet resolved to label
/// indices).
#[allow(clippy::too_many_lines)]
fn decode_instruction(r: &mut Reader<'_>, start_offset: usize) -> Result<Instruction, DecodeError> {
    let opcode = r.read_u8()?;
    match opcode {
        OP_LABEL => Ok(Instruction::Label),
        OP_COPY => Ok(Instruction::Copy),
        OP_SKIP => Ok(Instruction::Skip),
        OP_AND => Ok(Instruction::And),
        OP_OR => Ok(Instruction::Or),
        OP_NOT => Ok(Instruction::Not),
        OP_RETURN => Ok(Instruction::Return),

        OP_DECODE => {
            let reg = r.read_u8()?;
            check_reg_decode(reg)?;
            let enc = r.read_u8()?;
            let encoding = Encoding::from_u8(enc).ok_or(DecodeError::UnknownOpcode {
                offset: r.pos - 1,
                opcode: enc,
            })?;
            Ok(Instruction::Decode { reg, encoding })
        }

        OP_IS_SET => {
            let reg = r.read_u8()?;
            check_reg_decode(reg)?;
            Ok(Instruction::IsSet { reg })
        }

        OP_CMP_EQ => {
            let reg = r.read_u8()?;
            check_reg_decode(reg)?;
            Ok(Instruction::CmpEq {
                reg,
                imm: r.read_svarint()?,
            })
        }
        OP_CMP_NEQ => {
            let reg = r.read_u8()?;
            check_reg_decode(reg)?;
            Ok(Instruction::CmpNeq {
                reg,
                imm: r.read_svarint()?,
            })
        }
        OP_CMP_LT => {
            let reg = r.read_u8()?;
            check_reg_decode(reg)?;
            Ok(Instruction::CmpLt {
                reg,
                imm: r.read_svarint()?,
            })
        }
        OP_CMP_LTE => {
            let reg = r.read_u8()?;
            check_reg_decode(reg)?;
            Ok(Instruction::CmpLte {
                reg,
                imm: r.read_svarint()?,
            })
        }
        OP_CMP_GT => {
            let reg = r.read_u8()?;
            check_reg_decode(reg)?;
            Ok(Instruction::CmpGt {
                reg,
                imm: r.read_svarint()?,
            })
        }
        OP_CMP_GTE => {
            let reg = r.read_u8()?;
            check_reg_decode(reg)?;
            Ok(Instruction::CmpGte {
                reg,
                imm: r.read_svarint()?,
            })
        }

        OP_CMP_LEN_EQ => {
            let reg = r.read_u8()?;
            check_reg_decode(reg)?;
            Ok(Instruction::CmpLenEq {
                reg,
                bytes: r.read_bytes_vec()?,
            })
        }
        OP_BYTES_STARTS => {
            let reg = r.read_u8()?;
            check_reg_decode(reg)?;
            Ok(Instruction::BytesStarts {
                reg,
                bytes: r.read_bytes_vec()?,
            })
        }
        OP_BYTES_ENDS => {
            let reg = r.read_u8()?;
            check_reg_decode(reg)?;
            Ok(Instruction::BytesEnds {
                reg,
                bytes: r.read_bytes_vec()?,
            })
        }
        OP_BYTES_CONTAINS => {
            let reg = r.read_u8()?;
            check_reg_decode(reg)?;
            Ok(Instruction::BytesContains {
                reg,
                bytes: r.read_bytes_vec()?,
            })
        }

        OP_BYTES_MATCHES => {
            #[cfg(not(feature = "regex"))]
            {
                // Skip operands so the offset is correct in error context,
                // but return the feature-gate error.
                let reg = r.read_u8()?;
                let _ = reg;
                r.skip_bytes()?;
                Err(DecodeError::RegexNotSupported)
            }
            #[cfg(feature = "regex")]
            {
                let reg = r.read_u8()?;
                check_reg_decode(reg)?;
                Ok(Instruction::BytesMatches {
                    reg,
                    pattern: r.read_bytes_vec()?,
                })
            }
        }

        OP_IN_SET => {
            let reg = r.read_u8()?;
            check_reg_decode(reg)?;
            let count = r.read_uvarint_as_usize()?;
            // Cap capacity hint: each svarint is >= 1 byte.
            let cap = count.min(r.remaining());
            let mut values = Vec::with_capacity(cap);
            for _ in 0..count {
                values.push(r.read_svarint()?);
            }
            Ok(Instruction::InSet { reg, values })
        }

        OP_DISPATCH => decode_dispatch(r),

        _ => Err(DecodeError::UnknownOpcode {
            offset: start_offset,
            opcode,
        }),
    }
}

fn decode_dispatch(r: &mut Reader<'_>) -> Result<Instruction, DecodeError> {
    let default_kind = r.read_u8()?;
    let default = match default_kind {
        0 => DefaultAction::Skip,
        1 => DefaultAction::Copy,
        2 => {
            let target = r.read_uvarint_as_u32()?;
            DefaultAction::Recurse(target)
        }
        _ => {
            return Err(DecodeError::UnknownOpcode {
                offset: r.pos - 1,
                opcode: default_kind,
            });
        }
    };

    let arm_count = r.read_uvarint_as_usize()?;
    // Cap capacity: each arm is >= 4 bytes (match_kind + field_num + action_count + action).
    let arm_cap = arm_count.min(r.remaining() / 4);
    let mut arms = Vec::with_capacity(arm_cap);
    for _ in 0..arm_count {
        let match_kind = r.read_u8()?;
        let field_num = r.read_uvarint_as_u32()?;
        let match_ = match match_kind {
            0 => ArmMatch::Field(field_num),
            1 => {
                let wt_byte = r.read_u8()?;
                let wt = WireType::from_u8(wt_byte).ok_or(DecodeError::UnknownOpcode {
                    offset: r.pos - 1,
                    opcode: wt_byte,
                })?;
                ArmMatch::FieldAndWireType(field_num, wt)
            }
            _ => {
                return Err(DecodeError::UnknownOpcode {
                    offset: r.pos - 1,
                    opcode: match_kind,
                });
            }
        };

        let action_count = r.read_uvarint_as_usize()?;
        if action_count == 0 {
            return Err(DecodeError::EmptyArmActions);
        }
        // Cap capacity: each action is >= 1 byte.
        let action_cap = action_count.min(r.remaining());
        let mut actions = Vec::with_capacity(action_cap);
        for _ in 0..action_count {
            let ak = r.read_u8()?;
            let action = match ak {
                0 => ArmAction::Copy,
                1 => ArmAction::Skip,
                2 => {
                    let reg = r.read_u8()?;
                    check_reg_decode(reg)?;
                    let enc = r.read_u8()?;
                    let encoding = Encoding::from_u8(enc).ok_or(DecodeError::UnknownOpcode {
                        offset: r.pos - 1,
                        opcode: enc,
                    })?;
                    ArmAction::Decode { reg, encoding }
                }
                3 => {
                    let target = r.read_uvarint_as_u32()?;
                    ArmAction::Frame(target)
                }
                _ => {
                    return Err(DecodeError::UnknownOpcode {
                        offset: r.pos - 1,
                        opcode: ak,
                    });
                }
            };
            actions.push(action);
        }
        arms.push(DispatchArm { match_, actions });
    }

    Ok(Instruction::Dispatch { default, arms })
}

/// Resolve raw byte-offset targets in decoded instructions back to label
/// indices, and validate that all targets point to LABEL opcodes.
fn resolve_targets_to_labels(
    instructions: &mut [Instruction],
    label_byte_offsets: &[(usize, u32)], // (instruction_byte_offset, label_index)
) -> Result<(), DecodeError> {
    let lookup = |byte_offset: u32| -> Result<u32, DecodeError> {
        for &(off, idx) in label_byte_offsets {
            if u32::try_from(off).ok() == Some(byte_offset) {
                return Ok(idx);
            }
        }
        Err(DecodeError::InvalidTarget(byte_offset))
    };

    for instr in instructions.iter_mut() {
        if let Instruction::Dispatch { default, arms } = instr {
            if let DefaultAction::Recurse(ref mut target) = default {
                *target = lookup(*target)?;
            }
            for arm in arms {
                for action in &mut arm.actions {
                    if let ArmAction::Frame(ref mut target) = action {
                        *target = lookup(*target)?;
                    }
                }
            }
        }
    }
    Ok(())
}

/// Decode a complete WVM program binary into a header and instruction list.
///
/// Target `u32` values in [`DefaultAction::Recurse`] and
/// [`ArmAction::Frame`] are returned as **label indices** (matching the
/// convention expected by [`encode`]).
///
/// # Errors
///
/// Returns [`DecodeError`] for any header or bytecode validation failure.
pub fn decode(buf: &[u8]) -> Result<(ProgramHeader, Vec<Instruction>), DecodeError> {
    let program = Program::from_bytes(buf)?;
    let mut r = Reader::new(program.bytecode);
    let mut instructions = Vec::new();
    let mut label_map: Vec<(usize, u32)> = Vec::new(); // (byte_offset, label_index)
    let mut label_index = 0u32;

    while !r.is_empty() {
        let off = r.pos;
        let instr = decode_instruction(&mut r, off)?;
        if matches!(instr, Instruction::Label) {
            label_map.push((off, label_index));
            label_index += 1;
        }
        instructions.push(instr);
    }

    resolve_targets_to_labels(&mut instructions, &label_map)?;
    Ok((program.header, instructions))
}

// ═══════════════════════════════════════════════════════════════════════
// InstructionIter
// ═══════════════════════════════════════════════════════════════════════

/// Iterator over instructions in a bytecode slice.
///
/// Target `u32` values are returned as **raw byte offsets** (as stored in
/// the binary). Use [`decode`] if you need label-index-based targets.
pub struct InstructionIter<'a> {
    reader: Reader<'a>,
}

impl<'a> InstructionIter<'a> {
    /// Create an iterator starting at the beginning of `bytecode`.
    #[must_use]
    pub fn new(bytecode: &'a [u8]) -> Self {
        Self {
            reader: Reader::new(bytecode),
        }
    }

    /// Current byte offset within the bytecode.
    #[must_use]
    pub fn offset(&self) -> usize {
        self.reader.pos
    }

    /// Seek to an absolute byte offset.
    ///
    /// # Errors
    ///
    /// Returns [`DecodeError::UnexpectedEof`] if `offset > bytecode.len()`.
    pub fn seek(&mut self, offset: usize) -> Result<(), DecodeError> {
        if offset > self.reader.buf.len() {
            return Err(DecodeError::UnexpectedEof);
        }
        self.reader.pos = offset;
        Ok(())
    }
}

impl Iterator for InstructionIter<'_> {
    type Item = Result<Instruction, DecodeError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.reader.is_empty() {
            return None;
        }
        let off = self.reader.pos;
        Some(decode_instruction(&mut self.reader, off))
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::FLAG_REGEX_REQUIRED;
    use alloc::vec;

    /// Helper: encode → decode round-trip, assert equality.
    fn roundtrip(instructions: &[Instruction]) {
        let encoded = encode(instructions);
        let (_, decoded) = decode(&encoded).expect("decode failed");
        assert_eq!(instructions, decoded.as_slice());
    }

    /// Helper: encode, verify header + bytecode bytes, then decode round-trip.
    fn roundtrip_with_bytes(instructions: &[Instruction], expected_bytecode: &[u8]) {
        let encoded = encode(instructions);
        let bytecode = &encoded[HEADER_SIZE..];
        assert_eq!(bytecode, expected_bytecode, "bytecode mismatch");
        let (_, decoded) = decode(&encoded).expect("decode failed");
        assert_eq!(instructions, decoded.as_slice());
    }

    // ─────────────────────────────────── Varint internals

    #[test]
    fn varint_roundtrip() {
        let cases: &[(u64, &[u8])] = &[
            (0, &[0x00]),
            (1, &[0x01]),
            (127, &[0x7F]),
            (128, &[0x80, 0x01]),
            (300, &[0xAC, 0x02]),
            (16384, &[0x80, 0x80, 0x01]),
        ];
        for &(val, expected) in cases {
            let mut w = Writer::new();
            w.put_uvarint(val);
            assert_eq!(&w.buf, expected, "encode uvarint {val}");
            let mut r = Reader::new(expected);
            assert_eq!(r.read_uvarint().unwrap(), val, "decode uvarint {val}");
        }
    }

    #[test]
    fn svarint_roundtrip() {
        let cases: &[(i64, &[u8])] = &[
            (0, &[0x00]),
            (-1, &[0x01]),
            (1, &[0x02]),
            (-2, &[0x03]),
            (2147483647, &[0xFE, 0xFF, 0xFF, 0xFF, 0x0F]),
            (-2147483648, &[0xFF, 0xFF, 0xFF, 0xFF, 0x0F]),
        ];
        for &(val, expected) in cases {
            let mut w = Writer::new();
            w.put_svarint(val);
            assert_eq!(&w.buf, expected, "encode svarint {val}");
            let mut r = Reader::new(expected);
            assert_eq!(r.read_svarint().unwrap(), val, "decode svarint {val}");
        }
    }

    // ─────────────────────────────────── DISPATCH round-trips

    #[test]
    fn roundtrip_dispatch_skip() {
        let instructions = vec![
            Instruction::Dispatch {
                default: DefaultAction::Skip,
                arms: vec![DispatchArm {
                    match_: ArmMatch::Field(1),
                    actions: vec![ArmAction::Copy],
                }],
            },
            Instruction::Return,
        ];
        // DISPATCH: 00 00 01 00 01 01 00
        // RETURN:   15
        roundtrip_with_bytes(
            &instructions,
            &[0x00, 0x00, 0x01, 0x00, 0x01, 0x01, 0x00, 0x15],
        );
    }

    #[test]
    fn roundtrip_dispatch_copy() {
        let instructions = vec![
            Instruction::Dispatch {
                default: DefaultAction::Copy,
                arms: vec![DispatchArm {
                    match_: ArmMatch::Field(2),
                    actions: vec![ArmAction::Copy],
                }],
            },
            Instruction::Return,
        ];
        // DISPATCH: 00 01 01 00 02 01 00
        // RETURN:   15
        roundtrip_with_bytes(
            &instructions,
            &[0x00, 0x01, 0x01, 0x00, 0x02, 0x01, 0x00, 0x15],
        );
    }

    #[test]
    fn roundtrip_dispatch_recurse() {
        let instructions = vec![
            Instruction::Label, // label 0, at byte offset 0
            Instruction::Dispatch {
                default: DefaultAction::Recurse(0), // → label 0
                arms: vec![DispatchArm {
                    match_: ArmMatch::Field(1),
                    actions: vec![ArmAction::Copy],
                }],
            },
            Instruction::Return,
        ];
        // LABEL:    01
        // DISPATCH: 00 02 00 01 00 01 01 00  (recurse target=0x00)
        // RETURN:   15
        roundtrip_with_bytes(
            &instructions,
            &[0x01, 0x00, 0x02, 0x00, 0x01, 0x00, 0x01, 0x01, 0x00, 0x15],
        );
    }

    #[test]
    fn roundtrip_frame() {
        // DISPATCH at offset 0, LABEL at offset N (forward reference)
        let instructions = vec![
            Instruction::Dispatch {
                default: DefaultAction::Skip,
                arms: vec![DispatchArm {
                    match_: ArmMatch::Field(3),
                    actions: vec![ArmAction::Frame(0)], // → label 0
                }],
            },
            Instruction::Label, // label 0
            Instruction::Dispatch {
                default: DefaultAction::Skip,
                arms: vec![DispatchArm {
                    match_: ArmMatch::Field(1),
                    actions: vec![ArmAction::Copy],
                }],
            },
            Instruction::Return,
        ];
        // DISPATCH: 00 00 01 00 03 01 03 08  (frame target=8, where LABEL is)
        // LABEL:    01
        // DISPATCH: 00 00 01 00 01 01 00
        // RETURN:   15
        roundtrip_with_bytes(
            &instructions,
            &[
                0x00, 0x00, 0x01, 0x00, 0x03, 0x01, 0x03,
                0x08, // DISPATCH with FRAME(offset=8)
                0x01, // LABEL
                0x00, 0x00, 0x01, 0x00, 0x01, 0x01, 0x00, // inner DISPATCH
                0x15, // RETURN
            ],
        );
    }

    // ─────────────────────────────────── DECODE round-trips

    #[test]
    fn roundtrip_decode_varint() {
        let instructions = vec![
            Instruction::Dispatch {
                default: DefaultAction::Skip,
                arms: vec![DispatchArm {
                    match_: ArmMatch::Field(2),
                    actions: vec![ArmAction::Decode {
                        reg: 0,
                        encoding: Encoding::Varint,
                    }],
                }],
            },
            Instruction::Return,
        ];
        roundtrip(&instructions);
    }

    #[test]
    fn roundtrip_decode_sint() {
        roundtrip(&[
            Instruction::Dispatch {
                default: DefaultAction::Skip,
                arms: vec![DispatchArm {
                    match_: ArmMatch::Field(2),
                    actions: vec![ArmAction::Decode {
                        reg: 0,
                        encoding: Encoding::Sint,
                    }],
                }],
            },
            Instruction::Return,
        ]);
    }

    #[test]
    fn roundtrip_decode_i32() {
        roundtrip(&[
            Instruction::Dispatch {
                default: DefaultAction::Skip,
                arms: vec![DispatchArm {
                    match_: ArmMatch::Field(2),
                    actions: vec![ArmAction::Decode {
                        reg: 0,
                        encoding: Encoding::I32,
                    }],
                }],
            },
            Instruction::Return,
        ]);
    }

    #[test]
    fn roundtrip_decode_i64() {
        roundtrip(&[
            Instruction::Dispatch {
                default: DefaultAction::Skip,
                arms: vec![DispatchArm {
                    match_: ArmMatch::Field(2),
                    actions: vec![ArmAction::Decode {
                        reg: 0,
                        encoding: Encoding::I64,
                    }],
                }],
            },
            Instruction::Return,
        ]);
    }

    #[test]
    fn roundtrip_decode_len() {
        roundtrip(&[
            Instruction::Dispatch {
                default: DefaultAction::Skip,
                arms: vec![DispatchArm {
                    match_: ArmMatch::Field(2),
                    actions: vec![ArmAction::Decode {
                        reg: 0,
                        encoding: Encoding::Len,
                    }],
                }],
            },
            Instruction::Return,
        ]);
    }

    #[test]
    fn roundtrip_arm_decode_copy() {
        roundtrip(&[
            Instruction::Dispatch {
                default: DefaultAction::Skip,
                arms: vec![DispatchArm {
                    match_: ArmMatch::Field(1),
                    actions: vec![
                        ArmAction::Decode {
                            reg: 0,
                            encoding: Encoding::Varint,
                        },
                        ArmAction::Copy,
                    ],
                }],
            },
            Instruction::Return,
        ]);
    }

    #[test]
    fn roundtrip_field_and_wire_type() {
        roundtrip(&[
            Instruction::Dispatch {
                default: DefaultAction::Skip,
                arms: vec![DispatchArm {
                    match_: ArmMatch::FieldAndWireType(5, WireType::Len),
                    actions: vec![ArmAction::Copy],
                }],
            },
            Instruction::Return,
        ]);
    }

    // ─────────────────────────────────── Comparison round-trips

    #[test]
    fn roundtrip_cmp_eq() {
        roundtrip_with_bytes(
            &[Instruction::CmpEq { reg: 0, imm: 42 }, Instruction::Return],
            &[OP_CMP_EQ, 0x00, 0x54, OP_RETURN], // zigzag(42) = 84 = 0x54
        );
    }

    #[test]
    fn roundtrip_cmp_neq() {
        roundtrip(&[Instruction::CmpNeq { reg: 0, imm: 0 }, Instruction::Return]);
    }

    #[test]
    fn roundtrip_cmp_lt() {
        roundtrip(&[Instruction::CmpLt { reg: 0, imm: 100 }, Instruction::Return]);
    }

    #[test]
    fn roundtrip_cmp_lte() {
        roundtrip(&[
            Instruction::CmpLte { reg: 0, imm: 100 },
            Instruction::Return,
        ]);
    }

    #[test]
    fn roundtrip_cmp_gt() {
        roundtrip(&[Instruction::CmpGt { reg: 0, imm: 100 }, Instruction::Return]);
    }

    #[test]
    fn roundtrip_cmp_gte() {
        // Negative immediate tests zigzag encoding
        roundtrip_with_bytes(
            &[Instruction::CmpGte { reg: 0, imm: -1 }, Instruction::Return],
            &[OP_CMP_GTE, 0x00, 0x01, OP_RETURN], // zigzag(-1) = 1
        );
    }

    // ─────────────────────────────────── Bytes comparison round-trips

    #[test]
    fn roundtrip_cmp_len_eq() {
        roundtrip_with_bytes(
            &[
                Instruction::CmpLenEq {
                    reg: 1,
                    bytes: b"NYC".to_vec(),
                },
                Instruction::Return,
            ],
            &[OP_CMP_LEN_EQ, 0x01, 0x03, b'N', b'Y', b'C', OP_RETURN],
        );
    }

    #[test]
    fn roundtrip_bytes_starts() {
        roundtrip_with_bytes(
            &[
                Instruction::BytesStarts {
                    reg: 1,
                    bytes: b"pre".to_vec(),
                },
                Instruction::Return,
            ],
            &[OP_BYTES_STARTS, 0x01, 0x03, b'p', b'r', b'e', OP_RETURN],
        );
    }

    #[test]
    fn roundtrip_bytes_ends() {
        roundtrip(&[
            Instruction::BytesEnds {
                reg: 1,
                bytes: b"suf".to_vec(),
            },
            Instruction::Return,
        ]);
    }

    #[test]
    fn roundtrip_bytes_contains() {
        roundtrip(&[
            Instruction::BytesContains {
                reg: 1,
                bytes: b"mid".to_vec(),
            },
            Instruction::Return,
        ]);
    }

    // ─────────────────────────────────── Set / existence round-trips

    #[test]
    fn roundtrip_in_set() {
        roundtrip_with_bytes(
            &[
                Instruction::InSet {
                    reg: 0,
                    values: vec![1, 2, 3],
                },
                Instruction::Return,
            ],
            &[
                OP_IN_SET, 0x00, 0x03, // reg=0, count=3
                0x02, 0x04, 0x06, // zigzag(1)=2, zigzag(2)=4, zigzag(3)=6
                OP_RETURN,
            ],
        );
    }

    #[test]
    fn roundtrip_in_set_negative() {
        roundtrip(&[
            Instruction::InSet {
                reg: 0,
                values: vec![-10, -1, 0, 1, 10],
            },
            Instruction::Return,
        ]);
    }

    #[test]
    fn roundtrip_in_set_empty() {
        roundtrip(&[
            Instruction::InSet {
                reg: 0,
                values: vec![],
            },
            Instruction::Return,
        ]);
    }

    #[test]
    fn roundtrip_is_set() {
        roundtrip_with_bytes(
            &[Instruction::IsSet { reg: 0 }, Instruction::Return],
            &[OP_IS_SET, 0x00, OP_RETURN],
        );
    }

    // ─────────────────────────────────── Logic round-trips

    #[test]
    fn roundtrip_and_or_not() {
        roundtrip_with_bytes(
            &[
                Instruction::And,
                Instruction::Or,
                Instruction::Not,
                Instruction::Return,
            ],
            &[OP_AND, OP_OR, OP_NOT, OP_RETURN],
        );
    }

    // ─────────────────────────────────── Standalone Copy / Skip

    #[test]
    fn roundtrip_copy_skip() {
        roundtrip_with_bytes(
            &[Instruction::Copy, Instruction::Skip, Instruction::Return],
            &[OP_COPY, OP_SKIP, OP_RETURN],
        );
    }

    // ─────────────────────────────────── Header validation

    #[test]
    fn roundtrip_header_fields() {
        let instructions = vec![
            Instruction::Dispatch {
                default: DefaultAction::Skip,
                arms: vec![DispatchArm {
                    match_: ArmMatch::Field(1),
                    actions: vec![ArmAction::Decode {
                        reg: 3,
                        encoding: Encoding::Varint,
                    }],
                }],
            },
            Instruction::CmpEq { reg: 3, imm: 42 },
            Instruction::Return,
        ];
        let encoded = encode(&instructions);
        let (header, _) = decode(&encoded).unwrap();
        assert_eq!(header.version, VERSION);
        assert_eq!(header.register_count, 4); // reg 3 → count = 4
        assert_eq!(header.flags, 0x0004); // HAS_PREDICATE only (Decode + CmpEq, no Copy/Frame)
    }

    #[test]
    fn header_frame_depth() {
        let instructions = vec![
            Instruction::Dispatch {
                default: DefaultAction::Skip,
                arms: vec![DispatchArm {
                    match_: ArmMatch::Field(1),
                    actions: vec![ArmAction::Frame(0)],
                }],
            },
            Instruction::Label,
            Instruction::Dispatch {
                default: DefaultAction::Skip,
                arms: vec![DispatchArm {
                    match_: ArmMatch::Field(2),
                    actions: vec![ArmAction::Copy],
                }],
            },
            Instruction::Return,
        ];
        let encoded = encode(&instructions);
        let (header, _) = decode(&encoded).unwrap();
        // 1 Frame action referencing 1 distinct label → max_frame_depth = 1
        assert_eq!(header.max_frame_depth, 1);
    }

    // ─────────────────────────────────── Error cases

    #[test]
    fn decode_bad_magic() {
        let mut buf = encode(&[Instruction::Return]);
        buf[0] = 0xFF;
        assert_eq!(decode(&buf), Err(DecodeError::BadMagic));
    }

    #[test]
    fn decode_bad_version() {
        let mut buf = encode(&[Instruction::Return]);
        buf[4] = 99;
        buf[5] = 0;
        assert_eq!(decode(&buf), Err(DecodeError::UnsupportedVersion(99)));
    }

    #[test]
    fn decode_length_mismatch() {
        let mut buf = encode(&[Instruction::Return]);
        // Append an extra byte to cause mismatch
        buf.push(0xFF);
        assert_eq!(decode(&buf), Err(DecodeError::LengthMismatch));
    }

    #[test]
    fn decode_unknown_opcode() {
        let mut buf = encode(&[Instruction::Return]);
        // Replace RETURN opcode with unknown
        buf[HEADER_SIZE] = 0xFF;
        assert_eq!(
            decode(&buf),
            Err(DecodeError::UnknownOpcode {
                offset: 0,
                opcode: 0xFF
            })
        );
    }

    #[test]
    fn decode_invalid_target() {
        // Manually construct a DISPATCH with a FRAME pointing to offset 99
        // (which is not a LABEL)
        let mut w = Writer::new();
        // DISPATCH
        w.put_u8(OP_DISPATCH);
        w.put_u8(0); // default = SKIP
        w.put_uvarint(1); // 1 arm
        w.put_u8(0); // match = Field
        w.put_uvarint(1); // field_num = 1
        w.put_uvarint(1); // 1 action
        w.put_u8(3); // FRAME
        w.put_uvarint(99); // target = 99 (invalid)
                           // RETURN
        w.put_u8(OP_RETURN);

        let bytecode = w.buf;
        let mut out = Vec::with_capacity(HEADER_SIZE + bytecode.len());
        out.extend_from_slice(&MAGIC);
        out.extend_from_slice(&VERSION.to_le_bytes());
        out.push(0); // register_count
        out.push(0); // max_frame_depth
        out.extend_from_slice(&0u16.to_le_bytes()); // flags
        out.extend_from_slice(&(bytecode.len() as u32).to_le_bytes());
        out.extend_from_slice(&bytecode);

        assert_eq!(decode(&out), Err(DecodeError::InvalidTarget(99)));
    }

    #[test]
    fn decode_register_out_of_range() {
        // Manually construct a CMP_EQ with reg=16
        let mut w = Writer::new();
        w.put_u8(OP_CMP_EQ);
        w.put_u8(16); // out of range
        w.put_svarint(0);
        w.put_u8(OP_RETURN);

        let bytecode = w.buf;
        let mut out = Vec::with_capacity(HEADER_SIZE + bytecode.len());
        out.extend_from_slice(&MAGIC);
        out.extend_from_slice(&VERSION.to_le_bytes());
        out.push(0);
        out.push(0);
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&(bytecode.len() as u32).to_le_bytes());
        out.extend_from_slice(&bytecode);

        assert_eq!(decode(&out), Err(DecodeError::RegisterOutOfRange(16)));
    }

    #[test]
    fn decode_too_short() {
        assert_eq!(decode(&[0x57, 0x51]), Err(DecodeError::TooShort));
    }

    #[test]
    fn decode_empty_arm_actions() {
        let mut w = Writer::new();
        w.put_u8(OP_DISPATCH);
        w.put_u8(0); // default = SKIP
        w.put_uvarint(1); // 1 arm
        w.put_u8(0); // match = Field
        w.put_uvarint(1); // field_num
        w.put_uvarint(0); // 0 actions (invalid!)
        w.put_u8(OP_RETURN);

        let bytecode = w.buf;
        let mut out = Vec::with_capacity(HEADER_SIZE + bytecode.len());
        out.extend_from_slice(&MAGIC);
        out.extend_from_slice(&VERSION.to_le_bytes());
        out.push(0);
        out.push(0);
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&(bytecode.len() as u32).to_le_bytes());
        out.extend_from_slice(&bytecode);

        assert_eq!(decode(&out), Err(DecodeError::EmptyArmActions));
    }

    // ─────────────────────────────── InstructionIter

    #[test]
    fn instruction_iter_seek() {
        let instructions = vec![
            Instruction::Label, // label 0, offset 0
            Instruction::Dispatch {
                default: DefaultAction::Skip,
                arms: vec![DispatchArm {
                    match_: ArmMatch::Field(1),
                    actions: vec![ArmAction::Copy],
                }],
            },
            Instruction::Return,
        ];
        let encoded = encode(&instructions);
        let program = Program::from_bytes(&encoded).unwrap();

        let mut iter = InstructionIter::new(program.bytecode);
        // Read first instruction (LABEL at offset 0)
        let first = iter.next().unwrap().unwrap();
        assert_eq!(first, Instruction::Label);
        assert_eq!(iter.offset(), 1);

        // Seek back to 0
        iter.seek(0).unwrap();
        assert_eq!(iter.offset(), 0);
        let again = iter.next().unwrap().unwrap();
        assert_eq!(again, Instruction::Label);
    }

    #[test]
    fn instruction_iter_seek_out_of_bounds() {
        let encoded = encode(&[Instruction::Return]);
        let program = Program::from_bytes(&encoded).unwrap();
        let mut iter = InstructionIter::new(program.bytecode);
        assert_eq!(iter.seek(999), Err(DecodeError::UnexpectedEof));
    }

    // ─────────────────────────────── Full program round-trip

    #[test]
    fn roundtrip_complex_program() {
        // A program that projects fields 1 and 3 from a message,
        // recursing into field 2 as a sub-message.
        let instructions = vec![
            Instruction::Dispatch {
                default: DefaultAction::Skip,
                arms: vec![
                    DispatchArm {
                        match_: ArmMatch::Field(1),
                        actions: vec![ArmAction::Copy],
                    },
                    DispatchArm {
                        match_: ArmMatch::Field(2),
                        actions: vec![ArmAction::Frame(0)],
                    },
                    DispatchArm {
                        match_: ArmMatch::Field(3),
                        actions: vec![
                            ArmAction::Decode {
                                reg: 0,
                                encoding: Encoding::Varint,
                            },
                            ArmAction::Copy,
                        ],
                    },
                ],
            },
            Instruction::CmpGt { reg: 0, imm: 100 },
            Instruction::Return,
            Instruction::Label, // label 0 (sub-message handler)
            Instruction::Dispatch {
                default: DefaultAction::Skip,
                arms: vec![DispatchArm {
                    match_: ArmMatch::Field(1),
                    actions: vec![ArmAction::Copy],
                }],
            },
            Instruction::Return,
        ];
        roundtrip(&instructions);

        // Verify header
        let encoded = encode(&instructions);
        let (header, _) = decode(&encoded).unwrap();
        assert_eq!(header.register_count, 1);
        assert_eq!(header.flags, 0x0004 | 0x0002); // HAS_PREDICATE | HAS_PROJECTION
    }

    // ─────────────────────────────── BYTES_MATCHES (regex feature)

    #[cfg(feature = "regex")]
    #[test]
    fn roundtrip_bytes_matches() {
        roundtrip(&[
            Instruction::BytesMatches {
                reg: 0,
                pattern: b"^foo.*bar$".to_vec(),
            },
            Instruction::Return,
        ]);

        // Verify FLAG_REGEX_REQUIRED is set
        let encoded = encode(&[
            Instruction::BytesMatches {
                reg: 0,
                pattern: b"test".to_vec(),
            },
            Instruction::Return,
        ]);
        let (header, _) = decode(&encoded).unwrap();
        assert_ne!(header.flags & FLAG_REGEX_REQUIRED, 0);
    }

    #[cfg(not(feature = "regex"))]
    #[test]
    fn decode_regex_not_supported() {
        // Manually construct bytecode with BYTES_MATCHES opcode
        let mut w = Writer::new();
        w.put_u8(OP_BYTES_MATCHES);
        w.put_u8(0); // reg
        w.put_bytes(b"pattern");
        w.put_u8(OP_RETURN);

        let bytecode = w.buf;
        let mut out = Vec::with_capacity(HEADER_SIZE + bytecode.len());
        out.extend_from_slice(&MAGIC);
        out.extend_from_slice(&VERSION.to_le_bytes());
        out.push(0);
        out.push(0);
        out.extend_from_slice(&FLAG_REGEX_REQUIRED.to_le_bytes());
        out.extend_from_slice(&(bytecode.len() as u32).to_le_bytes());
        out.extend_from_slice(&bytecode);

        assert_eq!(decode(&out), Err(DecodeError::RegexNotSupported));
    }
}
