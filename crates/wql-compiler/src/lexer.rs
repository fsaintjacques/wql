use crate::error::{ParseError, ParseErrorKind};

/// Byte offset range in the source string: [start, end).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    #[must_use]
    pub fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    /// Merge two spans into one covering both.
    #[must_use]
    pub fn merge(self, other: Span) -> Span {
        Span {
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    // ── Literals ──
    IntLit(i64),
    StringLit(String),

    // ── Identifier ──
    Ident(String),

    // ── Reserved keywords ──
    Where,
    Select,

    // ── Contextual keywords ──
    And,
    Or,
    Not,
    In,
    Exists,
    Has,
    StartsWith,
    EndsWith,
    Contains,
    Matches,
    True,
    False,

    // ── Punctuation ──
    LBrace,
    RBrace,
    LParen,
    RParen,
    LBracket,
    RBracket,
    Comma,
    Dot,
    Hash,
    Minus,
    DotDot,
    Ellipsis,

    // ── Operators ──
    EqEq,
    BangEq,
    Lt,
    Lte,
    Gt,
    Gte,
    AmpAmp,
    PipePipe,
    Bang,

    // ── End ──
    Eof,
}

impl TokenKind {
    /// Human-readable description for error messages.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            TokenKind::IntLit(n) => format!("integer '{n}'"),
            TokenKind::StringLit(s) => format!("string \"{s}\""),
            TokenKind::Ident(s) => format!("identifier '{s}'"),
            TokenKind::Where => "'WHERE'".into(),
            TokenKind::Select => "'SELECT'".into(),
            TokenKind::And => "'AND'".into(),
            TokenKind::Or => "'OR'".into(),
            TokenKind::Not => "'NOT'".into(),
            TokenKind::In => "'in'".into(),
            TokenKind::Exists => "'exists'".into(),
            TokenKind::Has => "'has'".into(),
            TokenKind::StartsWith => "'starts_with'".into(),
            TokenKind::EndsWith => "'ends_with'".into(),
            TokenKind::Contains => "'contains'".into(),
            TokenKind::Matches => "'matches'".into(),
            TokenKind::True => "'true'".into(),
            TokenKind::False => "'false'".into(),
            TokenKind::LBrace => "'{'".into(),
            TokenKind::RBrace => "'}'".into(),
            TokenKind::LParen => "'('".into(),
            TokenKind::RParen => "')'".into(),
            TokenKind::LBracket => "'['".into(),
            TokenKind::RBracket => "']'".into(),
            TokenKind::Comma => "','".into(),
            TokenKind::Dot => "'.'".into(),
            TokenKind::Hash => "'#'".into(),
            TokenKind::Minus => "'-'".into(),
            TokenKind::DotDot => "'..'".into(),
            TokenKind::Ellipsis => "'...'".into(),
            TokenKind::EqEq => "'=='".into(),
            TokenKind::BangEq => "'!='".into(),
            TokenKind::Lt => "'<'".into(),
            TokenKind::Lte => "'<='".into(),
            TokenKind::Gt => "'>'".into(),
            TokenKind::Gte => "'>='".into(),
            TokenKind::AmpAmp => "'&&'".into(),
            TokenKind::PipePipe => "'||'".into(),
            TokenKind::Bang => "'!'".into(),
            TokenKind::Eof => "end of input".into(),
        }
    }
}

fn keyword_lookup(s: &str) -> Option<TokenKind> {
    match s {
        "WHERE" => Some(TokenKind::Where),
        "SELECT" => Some(TokenKind::Select),
        "AND" => Some(TokenKind::And),
        "OR" => Some(TokenKind::Or),
        "NOT" => Some(TokenKind::Not),
        "in" => Some(TokenKind::In),
        "exists" => Some(TokenKind::Exists),
        "has" => Some(TokenKind::Has),
        "starts_with" => Some(TokenKind::StartsWith),
        "ends_with" => Some(TokenKind::EndsWith),
        "contains" => Some(TokenKind::Contains),
        "matches" => Some(TokenKind::Matches),
        "true" => Some(TokenKind::True),
        "false" => Some(TokenKind::False),
        _ => None,
    }
}

pub struct Lexer<'a> {
    source: &'a str,
    bytes: &'a [u8],
    pos: usize,
    peeked: Option<Token>,
}

impl<'a> Lexer<'a> {
    /// Creates a new lexer for the given WQL source string.
    ///
    /// # Errors
    ///
    /// Individual calls to [`peek`](Self::peek) and [`next_token`](Self::next_token)
    /// return errors for invalid tokens.
    #[must_use]
    pub fn new(source: &'a str) -> Self {
        Self {
            source,
            bytes: source.as_bytes(),
            pos: 0,
            peeked: None,
        }
    }

    /// Return the next token without consuming it.
    ///
    /// # Errors
    ///
    /// Returns `ParseError` if the next token is invalid.
    ///
    /// # Panics
    ///
    /// Cannot panic — the `expect` is guarded by the preceding `is_none` check.
    pub fn peek(&mut self) -> Result<&Token, ParseError> {
        if self.peeked.is_none() {
            self.peeked = Some(self.lex_token()?);
        }
        // SAFETY: we just set `self.peeked` to `Some` above.
        Ok(self.peeked.as_ref().expect("just populated"))
    }

    /// Consume and return the next token.
    ///
    /// # Errors
    ///
    /// Returns `ParseError` if the next token is invalid.
    pub fn next_token(&mut self) -> Result<Token, ParseError> {
        if let Some(tok) = self.peeked.take() {
            return Ok(tok);
        }
        self.lex_token()
    }

    fn skip_whitespace(&mut self) {
        while self.pos < self.bytes.len() {
            match self.bytes[self.pos] {
                b' ' | b'\t' | b'\n' | b'\r' => self.pos += 1,
                _ => break,
            }
        }
    }

    fn peek_byte(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn peek_byte_at(&self, offset: usize) -> Option<u8> {
        self.bytes.get(self.pos + offset).copied()
    }

    fn lex_token(&mut self) -> Result<Token, ParseError> {
        self.skip_whitespace();

        let start = self.pos;

        let Some(b) = self.peek_byte() else {
            return Ok(Token {
                kind: TokenKind::Eof,
                span: Span::new(start, start),
            });
        };

        match b {
            b'{' => Ok(self.single(start, TokenKind::LBrace)),
            b'}' => Ok(self.single(start, TokenKind::RBrace)),
            b'(' => Ok(self.single(start, TokenKind::LParen)),
            b')' => Ok(self.single(start, TokenKind::RParen)),
            b'[' => Ok(self.single(start, TokenKind::LBracket)),
            b']' => Ok(self.single(start, TokenKind::RBracket)),
            b',' => Ok(self.single(start, TokenKind::Comma)),
            b'#' => Ok(self.single(start, TokenKind::Hash)),
            b'-' => Ok(self.single(start, TokenKind::Minus)),

            b'.' => Ok(self.lex_dots(start)),
            b'!' => Ok(self.lex_bang(start)),
            b'<' => Ok(self.lex_lt(start)),
            b'>' => Ok(self.lex_gt(start)),

            b'=' => self.lex_eq(start),
            b'&' => self.lex_amp(start),
            b'|' => self.lex_pipe(start),
            b'"' => self.lex_string(start),
            b'0'..=b'9' => self.lex_integer(start),

            b'a'..=b'z' | b'A'..=b'Z' | b'_' => Ok(self.lex_ident(start)),

            _ => {
                let ch = self.source[self.pos..].chars().next().expect("non-empty");
                Err(ParseError {
                    kind: ParseErrorKind::UnexpectedChar(ch),
                    span: Span::new(start, start + ch.len_utf8()),
                })
            }
        }
    }

    fn single(&mut self, start: usize, kind: TokenKind) -> Token {
        self.pos += 1;
        Token {
            kind,
            span: Span::new(start, self.pos),
        }
    }

    fn lex_dots(&mut self, start: usize) -> Token {
        if self.peek_byte_at(1) == Some(b'.') && self.peek_byte_at(2) == Some(b'.') {
            self.pos += 3;
            Token { kind: TokenKind::Ellipsis, span: Span::new(start, self.pos) }
        } else if self.peek_byte_at(1) == Some(b'.') {
            self.pos += 2;
            Token { kind: TokenKind::DotDot, span: Span::new(start, self.pos) }
        } else {
            self.pos += 1;
            Token { kind: TokenKind::Dot, span: Span::new(start, self.pos) }
        }
    }

    fn lex_eq(&mut self, start: usize) -> Result<Token, ParseError> {
        if self.peek_byte_at(1) == Some(b'=') {
            self.pos += 2;
            Ok(Token { kind: TokenKind::EqEq, span: Span::new(start, self.pos) })
        } else {
            Err(ParseError {
                kind: ParseErrorKind::UnexpectedChar('='),
                span: Span::new(start, start + 1),
            })
        }
    }

    fn lex_bang(&mut self, start: usize) -> Token {
        if self.peek_byte_at(1) == Some(b'=') {
            self.pos += 2;
            Token { kind: TokenKind::BangEq, span: Span::new(start, self.pos) }
        } else {
            self.pos += 1;
            Token { kind: TokenKind::Bang, span: Span::new(start, self.pos) }
        }
    }

    fn lex_lt(&mut self, start: usize) -> Token {
        if self.peek_byte_at(1) == Some(b'=') {
            self.pos += 2;
            Token { kind: TokenKind::Lte, span: Span::new(start, self.pos) }
        } else {
            self.pos += 1;
            Token { kind: TokenKind::Lt, span: Span::new(start, self.pos) }
        }
    }

    fn lex_gt(&mut self, start: usize) -> Token {
        if self.peek_byte_at(1) == Some(b'=') {
            self.pos += 2;
            Token { kind: TokenKind::Gte, span: Span::new(start, self.pos) }
        } else {
            self.pos += 1;
            Token { kind: TokenKind::Gt, span: Span::new(start, self.pos) }
        }
    }

    fn lex_amp(&mut self, start: usize) -> Result<Token, ParseError> {
        if self.peek_byte_at(1) == Some(b'&') {
            self.pos += 2;
            Ok(Token { kind: TokenKind::AmpAmp, span: Span::new(start, self.pos) })
        } else {
            Err(ParseError {
                kind: ParseErrorKind::UnexpectedChar('&'),
                span: Span::new(start, start + 1),
            })
        }
    }

    fn lex_pipe(&mut self, start: usize) -> Result<Token, ParseError> {
        if self.peek_byte_at(1) == Some(b'|') {
            self.pos += 2;
            Ok(Token { kind: TokenKind::PipePipe, span: Span::new(start, self.pos) })
        } else {
            Err(ParseError {
                kind: ParseErrorKind::UnexpectedChar('|'),
                span: Span::new(start, start + 1),
            })
        }
    }

    fn lex_string(&mut self, start: usize) -> Result<Token, ParseError> {
        self.pos += 1; // skip opening quote
        let mut value = String::new();

        loop {
            if self.pos >= self.bytes.len() {
                return Err(ParseError {
                    kind: ParseErrorKind::UnterminatedString,
                    span: Span::new(start, self.pos),
                });
            }

            match self.bytes[self.pos] {
                b'"' => {
                    self.pos += 1;
                    return Ok(Token {
                        kind: TokenKind::StringLit(value),
                        span: Span::new(start, self.pos),
                    });
                }
                b'\\' => {
                    self.pos += 1;
                    if self.pos >= self.bytes.len() {
                        return Err(ParseError {
                            kind: ParseErrorKind::UnterminatedString,
                            span: Span::new(start, self.pos),
                        });
                    }
                    match self.bytes[self.pos] {
                        b'"' => value.push('"'),
                        b'\\' => value.push('\\'),
                        b'n' => value.push('\n'),
                        b't' => value.push('\t'),
                        b'r' => value.push('\r'),
                        b'0' => value.push('\0'),
                        b'x' => {
                            self.pos += 1;
                            if self.pos + 2 > self.bytes.len() {
                                return Err(ParseError {
                                    kind: ParseErrorKind::UnterminatedString,
                                    span: Span::new(start, self.pos),
                                });
                            }
                            let hi = hex_digit(self.bytes[self.pos]);
                            let lo = hex_digit(self.bytes[self.pos + 1]);
                            if let (Some(h), Some(l)) = (hi, lo) {
                                value.push((h << 4 | l) as char);
                                self.pos += 1; // second digit; outer += 1 below
                            } else {
                                return Err(ParseError {
                                    kind: ParseErrorKind::InvalidEscape('x'),
                                    span: Span::new(self.pos - 2, self.pos + 2),
                                });
                            }
                        }
                        other => {
                            return Err(ParseError {
                                kind: ParseErrorKind::InvalidEscape(char::from(other)),
                                span: Span::new(self.pos - 1, self.pos + 1),
                            });
                        }
                    }
                    self.pos += 1;
                }
                _ => {
                    // Decode a full UTF-8 character from the source slice.
                    let ch = self.source[self.pos..].chars().next().expect("non-empty");
                    value.push(ch);
                    self.pos += ch.len_utf8();
                }
            }
        }
    }

    fn lex_integer(&mut self, start: usize) -> Result<Token, ParseError> {
        while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_digit() {
            self.pos += 1;
        }
        let text = &self.source[start..self.pos];
        let value: i64 = text.parse().map_err(|_| ParseError {
            kind: ParseErrorKind::InvalidIntLiteral,
            span: Span::new(start, self.pos),
        })?;
        Ok(Token {
            kind: TokenKind::IntLit(value),
            span: Span::new(start, self.pos),
        })
    }

    fn lex_ident(&mut self, start: usize) -> Token {
        while self.pos < self.bytes.len()
            && (self.bytes[self.pos].is_ascii_alphanumeric() || self.bytes[self.pos] == b'_')
        {
            self.pos += 1;
        }
        let text = &self.source[start..self.pos];
        let kind = keyword_lookup(text).unwrap_or_else(|| TokenKind::Ident(text.to_string()));
        Token {
            kind,
            span: Span::new(start, self.pos),
        }
    }
}

fn hex_digit(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lex all tokens from source, collecting kinds (excluding Eof).
    fn lex_kinds(source: &str) -> Result<Vec<TokenKind>, ParseError> {
        let mut lexer = Lexer::new(source);
        let mut kinds = Vec::new();
        loop {
            let tok = lexer.next_token()?;
            if tok.kind == TokenKind::Eof {
                break;
            }
            kinds.push(tok.kind);
        }
        Ok(kinds)
    }

    /// Lex all tokens from source, collecting (kind, span) pairs (excluding Eof).
    fn lex_tokens(source: &str) -> Result<Vec<Token>, ParseError> {
        let mut lexer = Lexer::new(source);
        let mut tokens = Vec::new();
        loop {
            let tok = lexer.next_token()?;
            if tok.kind == TokenKind::Eof {
                break;
            }
            tokens.push(tok);
        }
        Ok(tokens)
    }

    #[test]
    fn lex_empty() {
        let mut lexer = Lexer::new("");
        assert_eq!(lexer.next_token().unwrap().kind, TokenKind::Eof);
    }

    #[test]
    fn lex_whitespace_only() {
        let mut lexer = Lexer::new("  \t\n  ");
        assert_eq!(lexer.next_token().unwrap().kind, TokenKind::Eof);
    }

    #[test]
    fn lex_projection_simple() {
        let kinds = lex_kinds("{ name, age }").unwrap();
        assert_eq!(
            kinds,
            vec![
                TokenKind::LBrace,
                TokenKind::Ident("name".into()),
                TokenKind::Comma,
                TokenKind::Ident("age".into()),
                TokenKind::RBrace,
            ]
        );
    }

    #[test]
    fn lex_field_number() {
        let kinds = lex_kinds("#42").unwrap();
        assert_eq!(kinds, vec![TokenKind::Hash, TokenKind::IntLit(42)]);
    }

    #[test]
    fn lex_operators() {
        let kinds = lex_kinds("== != < <= > >=").unwrap();
        assert_eq!(
            kinds,
            vec![
                TokenKind::EqEq,
                TokenKind::BangEq,
                TokenKind::Lt,
                TokenKind::Lte,
                TokenKind::Gt,
                TokenKind::Gte,
            ]
        );
    }

    #[test]
    fn lex_logical() {
        let kinds = lex_kinds("&& || !").unwrap();
        assert_eq!(
            kinds,
            vec![TokenKind::AmpAmp, TokenKind::PipePipe, TokenKind::Bang]
        );
    }

    #[test]
    fn lex_keywords() {
        let kinds = lex_kinds("WHERE SELECT AND OR NOT in exists has").unwrap();
        assert_eq!(
            kinds,
            vec![
                TokenKind::Where,
                TokenKind::Select,
                TokenKind::And,
                TokenKind::Or,
                TokenKind::Not,
                TokenKind::In,
                TokenKind::Exists,
                TokenKind::Has,
            ]
        );
    }

    #[test]
    fn lex_string_predicates() {
        let kinds = lex_kinds("starts_with ends_with contains matches").unwrap();
        assert_eq!(
            kinds,
            vec![
                TokenKind::StartsWith,
                TokenKind::EndsWith,
                TokenKind::Contains,
                TokenKind::Matches,
            ]
        );
    }

    #[test]
    fn lex_string_simple() {
        let kinds = lex_kinds(r#""hello""#).unwrap();
        assert_eq!(kinds, vec![TokenKind::StringLit("hello".into())]);
    }

    #[test]
    fn lex_string_escapes() {
        let kinds = lex_kinds(r#""a\"b\\c\n\t\x41""#).unwrap();
        assert_eq!(
            kinds,
            vec![TokenKind::StringLit("a\"b\\c\n\tA".into())]
        );
    }

    #[test]
    fn lex_dots() {
        let kinds = lex_kinds(". .. ...").unwrap();
        assert_eq!(
            kinds,
            vec![TokenKind::Dot, TokenKind::DotDot, TokenKind::Ellipsis]
        );
    }

    #[test]
    fn lex_dots_adjacent() {
        assert_eq!(lex_kinds("...").unwrap(), vec![TokenKind::Ellipsis]);
        assert_eq!(lex_kinds("..").unwrap(), vec![TokenKind::DotDot]);
        assert_eq!(
            lex_kinds("..name").unwrap(),
            vec![TokenKind::DotDot, TokenKind::Ident("name".into())]
        );
    }

    #[test]
    fn lex_integer() {
        let kinds = lex_kinds("0 42 999999").unwrap();
        assert_eq!(
            kinds,
            vec![
                TokenKind::IntLit(0),
                TokenKind::IntLit(42),
                TokenKind::IntLit(999999),
            ]
        );
    }

    #[test]
    fn lex_booleans() {
        let kinds = lex_kinds("true false").unwrap();
        assert_eq!(kinds, vec![TokenKind::True, TokenKind::False]);
    }

    #[test]
    fn lex_unterminated_string() {
        let err = lex_kinds(r#""hello"#).unwrap_err();
        assert_eq!(err.kind, ParseErrorKind::UnterminatedString);
    }

    #[test]
    fn lex_invalid_escape() {
        let err = lex_kinds(r#""\q""#).unwrap_err();
        assert_eq!(err.kind, ParseErrorKind::InvalidEscape('q'));
    }

    #[test]
    fn lex_unexpected_char() {
        let err = lex_kinds("@").unwrap_err();
        assert_eq!(err.kind, ParseErrorKind::UnexpectedChar('@'));
    }

    #[test]
    fn lex_combined() {
        let kinds = lex_kinds(r#"WHERE age > 18 SELECT { name }"#).unwrap();
        assert_eq!(
            kinds,
            vec![
                TokenKind::Where,
                TokenKind::Ident("age".into()),
                TokenKind::Gt,
                TokenKind::IntLit(18),
                TokenKind::Select,
                TokenKind::LBrace,
                TokenKind::Ident("name".into()),
                TokenKind::RBrace,
            ]
        );
    }

    #[test]
    fn lex_spans() {
        let tokens = lex_tokens("{ name }").unwrap();
        assert_eq!(tokens.len(), 3);
        assert_eq!(tokens[0].span, Span::new(0, 1)); // {
        assert_eq!(tokens[1].span, Span::new(2, 6)); // name
        assert_eq!(tokens[2].span, Span::new(7, 8)); // }
    }

    #[test]
    fn lex_peek_does_not_consume() {
        let mut lexer = Lexer::new("{ }");
        let peeked = lexer.peek().unwrap().kind.clone();
        assert_eq!(peeked, TokenKind::LBrace);
        let next = lexer.next_token().unwrap().kind;
        assert_eq!(next, TokenKind::LBrace);
        let next2 = lexer.next_token().unwrap().kind;
        assert_eq!(next2, TokenKind::RBrace);
    }

    #[test]
    fn lex_single_ampersand_error() {
        let err = lex_kinds("&").unwrap_err();
        assert_eq!(err.kind, ParseErrorKind::UnexpectedChar('&'));
    }

    #[test]
    fn lex_single_pipe_error() {
        let err = lex_kinds("|").unwrap_err();
        assert_eq!(err.kind, ParseErrorKind::UnexpectedChar('|'));
    }

    #[test]
    fn lex_single_eq_error() {
        let err = lex_kinds("=").unwrap_err();
        assert_eq!(err.kind, ParseErrorKind::UnexpectedChar('='));
    }

    #[test]
    fn lex_case_sensitivity() {
        // WHERE is a keyword, where is an identifier
        let kinds = lex_kinds("WHERE where").unwrap();
        assert_eq!(
            kinds,
            vec![TokenKind::Where, TokenKind::Ident("where".into())]
        );
    }

    #[test]
    fn lex_string_hex_escape() {
        let kinds = lex_kinds(r#""\x00\xff""#).unwrap();
        assert_eq!(
            kinds,
            vec![TokenKind::StringLit("\x00\u{ff}".into())]
        );
    }

    #[test]
    fn lex_string_null_escape() {
        let kinds = lex_kinds(r#""\0""#).unwrap();
        assert_eq!(kinds, vec![TokenKind::StringLit("\0".into())]);
    }

    #[test]
    fn lex_string_utf8() {
        // Multi-byte UTF-8: é = [0xC3, 0xA9], 日 = [0xE6, 0x97, 0xA5]
        let kinds = lex_kinds(r#""café 日本""#).unwrap();
        assert_eq!(kinds, vec![TokenKind::StringLit("café 日本".into())]);
    }
}
