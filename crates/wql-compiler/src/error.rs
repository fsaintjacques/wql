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
