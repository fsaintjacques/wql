use core::fmt;

/// Errors that can occur during WVM program execution.
#[derive(Debug, PartialEq, Eq)]
pub enum RuntimeError {
    /// Input protobuf bytes are malformed (truncated field, bad varint, unknown wire type).
    MalformedInput,
    /// Output buffer is too small (must be >= `input.len()`).
    OutputBufferTooSmall,
    /// Bool stack underflow (AND/OR/NOT with insufficient operands).
    StackUnderflow,
    /// Program decoding failed.
    Decode(wql_ir::DecodeError),
    /// FRAME nesting exceeded the program's declared `max_frame_depth`.
    FrameDepthExceeded,
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MalformedInput => f.write_str("malformed protobuf input"),
            Self::OutputBufferTooSmall => f.write_str("output buffer too small"),
            Self::StackUnderflow => f.write_str("bool stack underflow"),
            Self::Decode(e) => write!(f, "program decode error: {e}"),
            Self::FrameDepthExceeded => f.write_str("frame depth exceeded"),
        }
    }
}

impl From<wql_ir::DecodeError> for RuntimeError {
    fn from(e: wql_ir::DecodeError) -> Self {
        Self::Decode(e)
    }
}
