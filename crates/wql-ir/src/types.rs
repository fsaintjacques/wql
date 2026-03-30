#![allow(clippy::module_name_repetitions)]

use alloc::vec::Vec;

// ───────────────────────────────────────────────── Primitives

/// Protobuf wire type identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum WireType {
    Varint = 0,
    I64 = 1,
    Len = 2,
    I32 = 5,
}

impl WireType {
    /// Convert a raw byte to a `WireType`, returning `None` for unknown values.
    #[must_use]
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Varint),
            1 => Some(Self::I64),
            2 => Some(Self::Len),
            5 => Some(Self::I32),
            _ => None,
        }
    }
}

/// Decoding mode for the `DECODE` instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Encoding {
    /// Unsigned varint; stored as `i64` (reinterpret bit pattern).
    Varint = 0,
    /// Zigzag-encoded varint; stored as `i64`.
    Sint = 1,
    /// 4-byte little-endian fixed; stored as `i64`.
    I32 = 2,
    /// 8-byte little-endian fixed; stored as `i64`.
    I64 = 3,
    /// Length-prefixed bytes; stored as bytes.
    Len = 4,
}

impl Encoding {
    /// Convert a raw byte to an `Encoding`, returning `None` for unknown values.
    #[must_use]
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Varint),
            1 => Some(Self::Sint),
            2 => Some(Self::I32),
            3 => Some(Self::I64),
            4 => Some(Self::Len),
            _ => None,
        }
    }
}

// ───────────────────────────────────────────────── DISPATCH components

/// Default action applied to any field not matched by a `DISPATCH` arm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DefaultAction {
    /// Consume the field bytes; emit nothing.
    Skip,
    /// Emit tag + raw value bytes verbatim to the output buffer.
    Copy,
    /// If `wire_type == LEN`: push new scan window, re-run the program at
    /// the target label, emit tag + reframed sub-output.
    /// If `wire_type != LEN`: skip.
    ///
    /// The `u32` is a **label index** (0-based among `Instruction::Label`
    /// entries in the instruction list). The encoder resolves this to an
    /// absolute byte offset in the bytecode.
    Recurse(u32),
}

/// Field match within a single `DISPATCH` arm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArmMatch {
    /// Match any field with the given field number regardless of wire type.
    Field(u32),
    /// Match only when both the field number and wire type equal the given values.
    FieldAndWireType(u32, WireType),
}

/// A single action inside a `DISPATCH` arm's action sequence.
///
/// Actions are executed left-to-right for a matched field. At most one
/// `Frame` action may appear per arm; if present it must be last.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArmAction {
    /// Emit current tag + raw value bytes to the output buffer.
    Copy,
    /// Consume the field bytes; emit nothing.
    Skip,
    /// Decode the current field into register `reg` using `encoding`.
    Decode { reg: u8, encoding: Encoding },
    /// Enter sub-message scope. The `u32` is a **label index**.
    Frame(u32),
}

/// A single arm within a `DISPATCH` instruction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchArm {
    pub match_: ArmMatch,
    /// One or more actions executed when this arm matches.
    pub actions: Vec<ArmAction>,
}

// ───────────────────────────────────────────────── Instruction

/// The complete WVM instruction set (22 instructions).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Instruction {
    // ── Control ──
    Dispatch {
        default: DefaultAction,
        arms: Vec<DispatchArm>,
    },
    Label,

    // ── Leaf actions ──
    Copy,
    Skip,
    Decode {
        reg: u8,
        encoding: Encoding,
    },

    // ── Predicate: integer comparisons ──
    CmpEq {
        reg: u8,
        imm: i64,
    },
    CmpNeq {
        reg: u8,
        imm: i64,
    },
    CmpLt {
        reg: u8,
        imm: i64,
    },
    CmpLte {
        reg: u8,
        imm: i64,
    },
    CmpGt {
        reg: u8,
        imm: i64,
    },
    CmpGte {
        reg: u8,
        imm: i64,
    },

    // ── Predicate: bytes comparisons ──
    CmpLenEq {
        reg: u8,
        bytes: Vec<u8>,
    },
    BytesStarts {
        reg: u8,
        bytes: Vec<u8>,
    },
    BytesEnds {
        reg: u8,
        bytes: Vec<u8>,
    },
    BytesContains {
        reg: u8,
        bytes: Vec<u8>,
    },

    /// RE2 regex match. Programs using this instruction set
    /// `FLAG_REGEX_REQUIRED` in the program header.
    #[cfg(feature = "regex")]
    BytesMatches {
        reg: u8,
        pattern: Vec<u8>,
    },

    // ── Predicate: set / existence ──
    InSet {
        reg: u8,
        values: Vec<i64>,
    },
    IsSet {
        reg: u8,
    },

    // ── Logic ──
    And,
    Or,
    Not,

    // ── Return ──
    Return,
}

// ───────────────────────────────────────────────── Program

/// Magic bytes at the start of every WVM program: `b"WQL\x00"`.
pub const MAGIC: [u8; 4] = [0x57, 0x51, 0x4C, 0x00];

/// Current bytecode format version.
pub const VERSION: u16 = 1;

/// Header flag: program contains at least one `BYTES_MATCHES` instruction.
pub const FLAG_REGEX_REQUIRED: u16 = 0x0001;

/// Size of the fixed program header in bytes.
pub const HEADER_SIZE: usize = 14;

/// Fixed-size program header (14 bytes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgramHeader {
    pub version: u16,
    pub register_count: u8,
    pub max_frame_depth: u8,
    pub flags: u16,
    pub bytecode_len: u32,
}

/// A parsed, validated WVM program that borrows its backing byte slice.
#[derive(Debug)]
pub struct Program<'a> {
    pub header: ProgramHeader,
    pub bytecode: &'a [u8],
}
