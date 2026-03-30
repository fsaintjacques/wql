use crate::lexer::Span;

/// An error produced during parsing of a WQL source string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub kind: ParseErrorKind,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseErrorKind {
    /// Unexpected character in source (not part of any valid token).
    UnexpectedChar(char),
    /// String literal was not closed before end of input.
    UnterminatedString,
    /// Unknown escape sequence in string literal (e.g. `\q`).
    InvalidEscape(char),
    /// Integer literal overflowed i64.
    InvalidIntLiteral,
    /// Expected a specific token kind, found something else.
    Expected {
        expected: &'static str,
        found: String,
    },
    /// Unexpected end of input.
    UnexpectedEof,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "error at byte {}..{}: ", self.span.start, self.span.end)?;
        match &self.kind {
            ParseErrorKind::UnexpectedChar(c) => write!(f, "unexpected character '{c}'"),
            ParseErrorKind::UnterminatedString => write!(f, "unterminated string literal"),
            ParseErrorKind::InvalidEscape(c) => write!(f, "invalid escape sequence '\\{c}'"),
            ParseErrorKind::InvalidIntLiteral => write!(f, "integer literal out of range"),
            ParseErrorKind::Expected { expected, found } => {
                write!(f, "expected {expected}, found {found}")
            }
            ParseErrorKind::UnexpectedEof => write!(f, "unexpected end of input"),
        }
    }
}

impl std::error::Error for ParseError {}

// ═══════════════════════════════════════════════════════════════════════
// CompileError
// ═══════════════════════════════════════════════════════════════════════

/// Errors produced during WQL compilation.
#[derive(Debug)]
pub enum CompileError {
    /// Source failed to parse (wraps `ParseError`).
    Parse(ParseError),

    /// Named field not found in the schema's message descriptor.
    UnresolvedField { field: String, span: Span },

    /// Literal type does not match the field's proto type.
    TypeError {
        field: String,
        expected: &'static str,
        actual: &'static str,
        span: Span,
    },

    /// Root message type or nested message type not found in the schema.
    InvalidMessageType { type_name: String },

    /// Schema-bound mode requires `root_message` in `CompileOptions`.
    MissingRootMessage,

    /// Schema-free mode encountered a named field reference.
    NamedFieldWithoutSchema { field: String, span: Span },

    /// Program requires more than 16 registers.
    TooManyRegisters,

    /// Failed to decode the `FileDescriptorSet` bytes.
    InvalidSchema(String),

    /// Unsupported comparison (e.g. ordering on bool/string).
    UnsupportedComparison {
        op: &'static str,
        literal_type: &'static str,
    },

    /// `matches` predicate requires the `regex` feature.
    RegexNotEnabled,

    /// Same field path used with conflicting encodings.
    ConflictingEncoding { field: String },
}

impl From<ParseError> for CompileError {
    fn from(e: ParseError) -> Self {
        Self::Parse(e)
    }
}

impl std::fmt::Display for CompileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Parse(e) => write!(f, "{e}"),
            Self::UnresolvedField { field, span } => {
                write!(
                    f,
                    "unresolved field '{field}' at byte {}..{}",
                    span.start, span.end
                )
            }
            Self::TypeError {
                field,
                expected,
                actual,
                span,
            } => {
                write!(
                    f,
                    "type error for field '{field}' at byte {}..{}: expected {expected}, found {actual}",
                    span.start, span.end
                )
            }
            Self::InvalidMessageType { type_name } => {
                write!(f, "message type '{type_name}' not found in schema")
            }
            Self::MissingRootMessage => {
                write!(
                    f,
                    "schema-bound mode requires root_message in CompileOptions"
                )
            }
            Self::NamedFieldWithoutSchema { field, span } => {
                write!(
                    f,
                    "named field '{field}' at byte {}..{} requires a schema",
                    span.start, span.end
                )
            }
            Self::TooManyRegisters => write!(f, "program requires more than 16 registers"),
            Self::InvalidSchema(msg) => write!(f, "invalid schema: {msg}"),
            Self::UnsupportedComparison { op, literal_type } => {
                write!(f, "unsupported comparison: {op} on {literal_type}")
            }
            Self::RegexNotEnabled => {
                write!(f, "matches predicate requires the regex feature")
            }
            Self::ConflictingEncoding { field } => {
                write!(f, "field '{field}' used with conflicting encodings")
            }
        }
    }
}

impl std::error::Error for CompileError {}
