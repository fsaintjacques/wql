# Block 4 — `wql-compiler` Part 1: Parser & AST

| | |
|---|---|
| **Status** | Draft |
| **Date** | 2026-03-30 |
| **Depends on** | Block 1 (workspace scaffold) |

---

## Goal

Implement the WQL surface syntax parser inside the `wql-compiler` crate. The parser consumes a WQL source string and produces a typed AST representing predicate expressions, projection expressions, or a combined `WHERE … SELECT …` form. The parser has **no dependency on `wql-ir`**, no schema knowledge, and no register allocation — it is a pure syntax-to-tree transformation with span-tracked error reporting.

---

## Deliverables

- `crates/wql-compiler/src/ast.rs`: all AST type definitions.
- `crates/wql-compiler/src/lexer.rs`: token types, `Span`, and `Lexer` iterator.
- `crates/wql-compiler/src/parser.rs`: recursive-descent parser producing `Query`.
- `crates/wql-compiler/src/error.rs`: `ParseError` with span information.
- `crates/wql-compiler/src/lib.rs`: public `parse(source: &str) -> Result<Query, ParseError>`.
- All tests pass: `cargo test -p wql-compiler`.
- `cargo clippy -p wql-compiler` passes with zero warnings.

---

## Implementation Chunks

Block 4 is split into three sequential chunks. Each chunk compiles and passes its own tests before the next begins.

| Chunk | Scope | Key files |
|-------|-------|-----------|
| 4a | Token types, Span, Lexer | `lexer.rs`, `error.rs` |
| 4b | AST types, projection parser | `ast.rs`, `parser.rs` (projection half) |
| 4c | Predicate parser, combined form, public API | `parser.rs` (predicate half), `lib.rs` |

---

## Surface Syntax Grammar

The grammar below uses EBNF notation. `'…'` denotes literal tokens. `(…)?` is optional, `(…)*` is zero-or-more, `(…)+` is one-or-more. `|` separates alternatives. Whitespace is insignificant between tokens (handled by the lexer).

```ebnf
(* ── Top-level ── *)

query           = projection
                | 'WHERE' predicate 'SELECT' projection
                | predicate ;

(* ── Projection ── *)

projection      = '{' projection_body '}' ;

projection_body = deep_copy_body
                | inclusion_body ;

deep_copy_body  = '..' exclusion* ;

inclusion_body  = ( inclusion_item ( ',' inclusion_item )* ','? )?
                  ( '...' )? ;

inclusion_item  = '..' field_ref                       (* deep field search *)
                | field_ref projection                 (* nested sub-message *)
                | field_ref ;                          (* flat include *)

exclusion       = '-' field_ref ;

(* ── Predicate ── *)

predicate       = or_expr ;

or_expr         = and_expr ( ( '||' | 'OR' ) and_expr )* ;

and_expr        = unary_expr ( ( '&&' | 'AND' ) unary_expr )* ;

unary_expr      = ( '!' | 'NOT' ) unary_expr
                | primary_expr ;

primary_expr    = '(' predicate ')'
                | 'exists' '(' field_path ')'
                | 'has' '(' field_path ')'
                | field_path 'starts_with' literal
                | field_path 'ends_with' literal
                | field_path 'contains' literal
                | field_path 'matches' literal
                | field_path 'in' '[' literal_list? ']'
                | field_path cmp_op literal ;

cmp_op          = '==' | '!=' | '<' | '<=' | '>' | '>=' ;

(* ── Shared ── *)

field_ref       = IDENT | '#' INT_LIT ;

field_path      = field_ref ( '.' field_ref )* ;

literal         = '-'? INT_LIT
                | STRING_LIT
                | 'true'
                | 'false' ;

literal_list    = literal ( ',' literal )* ','? ;
```

### Disambiguation rules

1. **`query` ambiguity.** The parser first tries to match `projection` (leading `{`), then `'WHERE' predicate 'SELECT' projection` (leading `WHERE`), then falls back to `predicate`. A bare predicate must not start with `{` or `WHERE`.
2. **`..` vs `...` in projection.** The lexer emits `DotDot` for `..` and `Ellipsis` for `...`. The two are always unambiguous in context: `...` appears only as the unknown-field-preservation trailer; `..` appears as the deep-copy prefix or before a `field_ref` in a deep search item.
3. **`inclusion_body` trailing comma.** A trailing comma before `}` or before `...` is permitted for ergonomics. `{ name, age, }` and `{ name, age, ... }` are both valid.
4. **Negative integer literals.** The lexer emits `Minus` and `IntLit` as separate tokens. The parser combines them when parsing a `literal` production (not a unary operator on an expression — literals are not expressions).
5. **Keyword identifiers.** `exists`, `has`, `starts_with`, `ends_with`, `contains`, `matches`, `in`, `true`, `false` are contextual keywords — they are recognized as keywords only in positions where the grammar expects them. A field named `exists` is valid: `exists == 1` parses as a comparison on a field called `exists`. The parser uses lookahead (the following token) to disambiguate: `exists(` triggers the presence-check production; `exists ==` triggers the field-path comparison production.
6. **`WHERE` and `SELECT` are reserved keywords.** They cannot be used as field names. This avoids ambiguity at the `query` level.

---

## File Tree

```
crates/wql-compiler/
└── src/
    ├── lib.rs       # public parse() entry point
    ├── ast.rs       # AST type definitions
    ├── lexer.rs     # Token, Span, Lexer
    ├── parser.rs    # recursive-descent Parser
    └── error.rs     # ParseError
```

---

# Chunk 4a — Token Types, Span & Lexer

## Goal

Implement the token type definitions, `Span` tracking, and a `Lexer` that converts a WQL source string into a sequence of `Token` values. The lexer handles all whitespace skipping, keyword recognition, string literal parsing (with escape sequences), integer literal parsing, and multi-character operator recognition.

## Deliverables

- `lexer.rs`: `Token`, `TokenKind`, `Span`, `Lexer`.
- `error.rs`: `ParseError` enum.
- 15+ lexer tests pass.

## `Span` — `lexer.rs`

```rust
/// Byte offset range in the source string: [start, end).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    /// Merge two spans into one covering both.
    pub fn merge(self, other: Span) -> Span {
        Span {
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
    }
}
```

## `Token` — `lexer.rs`

```rust
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
    And,        // AND
    Or,         // OR
    Not,        // NOT
    In,
    Exists,
    Has,
    StartsWith, // starts_with
    EndsWith,   // ends_with
    Contains,
    Matches,
    True,
    False,

    // ── Punctuation ──
    LBrace,     // {
    RBrace,     // }
    LParen,     // (
    RParen,     // )
    LBracket,   // [
    RBracket,   // ]
    Comma,      // ,
    Dot,        // .
    Hash,       // #
    Minus,      // -
    DotDot,     // ..
    Ellipsis,   // ...

    // ── Operators ──
    EqEq,       // ==
    BangEq,     // !=
    Lt,         // <
    Lte,        // <=
    Gt,         // >
    Gte,        // >=
    AmpAmp,     // &&
    PipePipe,   // ||
    Bang,       // !

    // ── End ──
    Eof,
}
```

### Keyword recognition

The lexer scans an identifier (`[a-zA-Z_][a-zA-Z0-9_]*`) and then checks a lookup table:

| Source text | TokenKind |
|-------------|-----------|
| `WHERE` | `Where` |
| `SELECT` | `Select` |
| `AND` | `And` |
| `OR` | `Or` |
| `NOT` | `Not` |
| `in` | `In` |
| `exists` | `Exists` |
| `has` | `Has` |
| `starts_with` | `StartsWith` |
| `ends_with` | `EndsWith` |
| `contains` | `Contains` |
| `matches` | `Matches` |
| `true` | `True` |
| `false` | `False` |
| anything else | `Ident(s)` |

Keywords are **case-sensitive**. `WHERE` is a keyword; `where` and `Where` are identifiers.

### Integer literals

Decimal only: `[0-9]+`. Parsed as `i64`. Overflow → `ParseError::InvalidIntLiteral`. Leading zeros are permitted (e.g. `007` parses as `7`). Negative sign is handled by the parser, not the lexer.

### String literals

Double-quoted: `"…"`. Supported escape sequences:

| Escape | Character |
|--------|-----------|
| `\"` | `"` |
| `\\` | `\` |
| `\n` | newline (0x0A) |
| `\t` | tab (0x09) |
| `\r` | carriage return (0x0D) |
| `\0` | null (0x00) |
| `\xHH` | hex byte (exactly 2 hex digits) |

Unterminated string → `ParseError::UnterminatedString`. Unknown escape → `ParseError::InvalidEscape`.

### Dot disambiguation

When the lexer encounters `.`:
1. If followed by `..` (two more dots): emit `Ellipsis`, consume 3 bytes.
2. If followed by `.` (one more dot): emit `DotDot`, consume 2 bytes.
3. Otherwise: emit `Dot`, consume 1 byte.

### Whitespace and comments

Whitespace (`' '`, `'\t'`, `'\n'`, `'\r'`) is skipped between tokens. No comment syntax in v1.

## `Lexer` — `lexer.rs`

```rust
pub struct Lexer<'a> {
    source: &'a str,
    pos: usize,
}

impl<'a> Lexer<'a> {
    pub fn new(source: &'a str) -> Self;

    /// Return the next token without consuming it.
    pub fn peek(&mut self) -> Result<&Token, ParseError>;

    /// Consume and return the next token.
    pub fn next_token(&mut self) -> Result<Token, ParseError>;
}
```

The lexer eagerly lexes one token ahead for `peek()` support. Internal state caches the peeked token.

## `ParseError` — `error.rs`

```rust
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
    /// `...` (ellipsis) must be the last element in a projection body.
    EllipsisNotLast,
    /// A `..` deep copy body cannot contain inclusion items.
    MixedDeepCopyAndInclusion,
    /// Empty action list or other structural error in projection.
    EmptyProjection,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Format as: "error at byte 42..45: expected '}', found 'EOF'"
        todo!()
    }
}

impl std::error::Error for ParseError {}
```

## Tests — `lexer.rs` `#[cfg(test)]`

| Test | Description |
|------|-------------|
| `lex_empty` | `""` → `[Eof]`. |
| `lex_whitespace_only` | `"  \t\n  "` → `[Eof]`. |
| `lex_projection_simple` | `"{ name, age }"` → `[LBrace, Ident("name"), Comma, Ident("age"), RBrace, Eof]`. |
| `lex_field_number` | `"#42"` → `[Hash, IntLit(42)]`. |
| `lex_operators` | `"== != < <= > >="` → `[EqEq, BangEq, Lt, Lte, Gt, Gte, Eof]`. |
| `lex_logical` | `"&& || !"` → `[AmpAmp, PipePipe, Bang, Eof]`. |
| `lex_keywords` | `"WHERE SELECT AND OR NOT in exists has"` → all keyword tokens. |
| `lex_string_predicates` | `"starts_with ends_with contains matches"` → `[StartsWith, EndsWith, Contains, Matches]`. |
| `lex_string_simple` | `r#""hello""#` → `[StringLit("hello")]`. |
| `lex_string_escapes` | `r#""a\"b\\c\n\t\x41""#` → `StringLit("a\"b\\c\n\tA")`. |
| `lex_dots` | `". .. ..."` → `[Dot, DotDot, Ellipsis]`. |
| `lex_dots_adjacent` | `"..."` → `[Ellipsis]`. `".."` → `[DotDot]`. `"..name"` → `[DotDot, Ident("name")]`. |
| `lex_integer` | `"0 42 999999"` → `[IntLit(0), IntLit(42), IntLit(999999)]`. |
| `lex_booleans` | `"true false"` → `[True, False]`. |
| `lex_unterminated_string` | `r#""hello"#` → `ParseError::UnterminatedString`. |
| `lex_invalid_escape` | `r#""\q""#` → `ParseError::InvalidEscape('q')`. |
| `lex_unexpected_char` | `"@"` → `ParseError::UnexpectedChar('@')`. |
| `lex_combined` | `r#"WHERE age > 18 SELECT { name }"#` → `[Where, Ident("age"), Gt, IntLit(18), Select, LBrace, Ident("name"), RBrace, Eof]`. |
| `lex_spans` | Verify spans are correct for `"{ name }"`: `LBrace` span `0..1`, `Ident` span `2..6`, `RBrace` span `7..8`. |

## Verification

```sh
cargo test -p wql-compiler
cargo clippy -p wql-compiler
```

---

# Chunk 4b — AST Types & Projection Parser

## Goal

Define all AST types and implement the projection-parsing subset of the recursive-descent parser. After this chunk, `{ name, age }`, `{ address { city, ... }, ... }`, `{ .. -payload }`, and `{ ..name }` all parse correctly to AST nodes.

## Deliverables

- `ast.rs`: all AST type definitions (projection + predicate + query).
- `parser.rs`: `Parser` struct, projection parsing methods.
- 12+ projection-parsing tests pass.

## AST Types — `ast.rs`

```rust
use crate::lexer::Span;

/// A complete WQL query as parsed from source.
#[derive(Debug, Clone, PartialEq)]
pub enum Query {
    /// Pure projection: `{ name, age }`
    Projection(Projection),
    /// Pure predicate: `age > 18`
    Predicate(Predicate),
    /// Combined: `WHERE age > 18 SELECT { name, age }`
    Combined {
        predicate: Predicate,
        projection: Projection,
    },
}

// ──────────────────────────────────── Projection ────

/// A projection expression enclosed in `{ … }`.
#[derive(Debug, Clone, PartialEq)]
pub struct Projection {
    pub kind: ProjectionKind,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ProjectionKind {
    /// `{ field1, field2 { … }, ..field3, ... }`
    ///
    /// Inclusion mode: an explicit list of fields to include.
    /// If `preserve_unknowns` is true, unmatched fields are copied (the `...` trailer).
    /// If false, unmatched fields are dropped.
    Inclusion {
        items: Vec<ProjectionItem>,
        preserve_unknowns: bool,
    },

    /// `{ .. }` or `{ .. -field1 -field2 }`
    ///
    /// Deep copy mode: recursively copy all fields, optionally excluding some.
    DeepCopy {
        exclusions: Vec<FieldRef>,
    },
}

/// A single item in an inclusion-mode projection.
#[derive(Debug, Clone, PartialEq)]
pub enum ProjectionItem {
    /// `name` or `#1` — flat field inclusion.
    Field(FieldRef),

    /// `address { city }` — enter sub-message, apply nested projection.
    Nested {
        field: FieldRef,
        projection: Box<Projection>,
    },

    /// `..name` or `..#1` — find and copy field at any nesting depth.
    DeepSearch(FieldRef),
}

// ──────────────────────────────────── Field references ────

/// A reference to a field — by name (schema-bound) or by number (schema-free).
#[derive(Debug, Clone, PartialEq)]
pub enum FieldRef {
    /// A named field: `name`, `city`. Resolved to a field number by the schema binder.
    Name(String, Span),
    /// A field number literal: `#1`, `#42`. Usable without a schema.
    Number(u32, Span),
}

impl FieldRef {
    pub fn span(&self) -> Span {
        match self {
            FieldRef::Name(_, s) | FieldRef::Number(_, s) => *s,
        }
    }
}

/// A dotted field path: `address.city`, `#3.#1`.
#[derive(Debug, Clone, PartialEq)]
pub struct FieldPath {
    pub segments: Vec<FieldRef>,
    pub span: Span,
}

// ──────────────────────────────────── Predicate ────

/// A predicate expression tree.
#[derive(Debug, Clone, PartialEq)]
pub struct Predicate {
    pub kind: PredicateKind,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PredicateKind {
    /// `a && b` / `a AND b`
    And(Box<Predicate>, Box<Predicate>),
    /// `a || b` / `a OR b`
    Or(Box<Predicate>, Box<Predicate>),
    /// `!a` / `NOT a`
    Not(Box<Predicate>),

    /// `field == 42`, `field != "x"`, `field > 0`, etc.
    Comparison {
        field: FieldPath,
        op: CompareOp,
        value: Literal,
    },

    /// `exists(field)` / `has(field)`.
    Presence(FieldPath),

    /// `field in [1, 2, 3]`.
    InSet {
        field: FieldPath,
        values: Vec<Literal>,
    },

    /// `field starts_with "pre"`, `field contains "mid"`, etc.
    StringPredicate {
        field: FieldPath,
        op: StringOp,
        value: Literal,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompareOp {
    Eq,     // ==
    Neq,    // !=
    Lt,     // <
    Lte,    // <=
    Gt,     // >
    Gte,    // >=
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StringOp {
    StartsWith,
    EndsWith,
    Contains,
    Matches,
}

// ──────────────────────────────────── Literals ────

#[derive(Debug, Clone, PartialEq)]
pub enum Literal {
    Int(i64, Span),
    String(String, Span),
    Bool(bool, Span),
}

impl Literal {
    pub fn span(&self) -> Span {
        match self {
            Literal::Int(_, s) | Literal::String(_, s) | Literal::Bool(_, s) => *s,
        }
    }
}
```

## `Parser` — `parser.rs`

```rust
use crate::ast::*;
use crate::error::ParseError;
use crate::lexer::{Lexer, Token, TokenKind, Span};

pub struct Parser<'a> {
    lexer: Lexer<'a>,
}

impl<'a> Parser<'a> {
    pub fn new(source: &'a str) -> Self {
        Self { lexer: Lexer::new(source) }
    }

    pub fn parse_query(&mut self) -> Result<Query, ParseError> {
        // Implemented in chunk 4c. Placeholder that delegates
        // to parse_projection for now.
        todo!()
    }

    // ── Projection parsing (this chunk) ──

    fn parse_projection(&mut self) -> Result<Projection, ParseError> {
        todo!()
    }

    fn parse_projection_body(&mut self) -> Result<ProjectionKind, ParseError> {
        todo!()
    }

    fn parse_inclusion_item(&mut self) -> Result<ProjectionItem, ParseError> {
        todo!()
    }

    fn parse_field_ref(&mut self) -> Result<FieldRef, ParseError> {
        todo!()
    }

    // ── Helpers ──

    /// Consume the next token if it matches `kind`. Return error otherwise.
    fn expect(&mut self, expected: &'static str) -> Result<Token, ParseError> {
        todo!()
    }

    /// Peek at the next token's kind without consuming it.
    fn peek_kind(&mut self) -> Result<&TokenKind, ParseError> {
        todo!()
    }
}
```

### Projection parsing algorithm

`parse_projection`:
1. Expect `LBrace`. Record `start` span.
2. Call `parse_projection_body`.
3. Expect `RBrace`. Record `end` span.
4. Return `Projection { kind, span: start.merge(end) }`.

`parse_projection_body`:
1. Peek the next token:
   - `RBrace` → return `Inclusion { items: vec![], preserve_unknowns: false }` (empty = drop all).
   - `DotDot` → enter deep-copy path:
     - Consume `DotDot`.
     - Peek next: if `Minus`, parse exclusions (`-field_ref`)*. Return `DeepCopy { exclusions }`.
     - If `RBrace`, return `DeepCopy { exclusions: vec![] }`.
   - `Ellipsis` → consume, return `Inclusion { items: vec![], preserve_unknowns: true }`.
   - Otherwise → enter inclusion path.
2. Inclusion path:
   - Parse `inclusion_item` into a list.
   - After each item, peek:
     - `Comma` → consume. Peek again:
       - `Ellipsis` → consume, set `preserve_unknowns = true`, break.
       - `RBrace` → trailing comma, break.
       - Otherwise → parse next item.
     - `Ellipsis` → consume, set `preserve_unknowns = true`, break.
     - `RBrace` → break.
   - Return `Inclusion { items, preserve_unknowns }`.

`parse_inclusion_item`:
1. Peek:
   - `DotDot` → consume, parse `field_ref`, return `DeepSearch(field_ref)`.
   - Otherwise → parse `field_ref`. Peek:
     - `LBrace` → parse nested projection, return `Nested { field, projection }`.
     - Otherwise → return `Field(field_ref)`.

`parse_field_ref`:
1. Peek:
   - `Hash` → consume, expect `IntLit`. Return `FieldRef::Number(n, span)`.
   - `Ident(s)` or contextual keyword → consume, return `FieldRef::Name(s, span)`.
   - Otherwise → error.

**Contextual keyword as field name:** when `parse_field_ref` encounters a contextual keyword token (`And`, `Or`, `Not`, `In`, `Exists`, `Has`, `StartsWith`, `EndsWith`, `Contains`, `Matches`, `True`, `False`), it converts the keyword back to its string form and returns `FieldRef::Name`. This allows proto fields named `exists`, `status`, etc. to be referenced in projections.

## Tests — `parser.rs` `#[cfg(test)]`

| Test | Description |
|------|-------------|
| `proj_flat_strict` | `"{ name, age }"` → `Inclusion([Field(name), Field(age)], preserve=false)`. |
| `proj_flat_preserve` | `"{ name, age, ... }"` → `Inclusion([Field(name), Field(age)], preserve=true)`. |
| `proj_trailing_comma` | `"{ name, age, }"` → `Inclusion([Field(name), Field(age)], preserve=false)`. |
| `proj_nested` | `"{ address { city } }"` → `Inclusion([Nested(address, Inclusion([Field(city)], false))], false)`. |
| `proj_nested_preserve` | `"{ address { city, ... }, ... }"` → both inner and outer `preserve=true`. |
| `proj_deep_copy` | `"{ .. }"` → `DeepCopy { exclusions: [] }`. |
| `proj_deep_exclusion` | `"{ .. -payload -thumbnail }"` → `DeepCopy { exclusions: [payload, thumbnail] }`. |
| `proj_deep_search` | `"{ ..name }"` → `Inclusion([DeepSearch(name)], false)`. |
| `proj_scoped_deep` | `"{ departments { ..name } }"` → `Inclusion([Nested(departments, Inclusion([DeepSearch(name)], false))], false)`. |
| `proj_field_number` | `"{ #1, #3 { #1 } }"` → field refs use `Number` variant with correct values. |
| `proj_empty` | `"{ }"` → `Inclusion([], false)`. |
| `proj_preserve_all` | `"{ ... }"` → `Inclusion([], true)`. |
| `proj_mixed_items` | `"{ name, ..tags, address { city }, ... }"` → all four item types. |
| `proj_deep_exclusion_by_number` | `"{ .. -#7 }"` → `DeepCopy { exclusions: [Number(7)] }`. |
| `proj_err_unclosed` | `"{ name"` → `Expected { expected: "}", … }`. |
| `proj_err_ellipsis_not_last` | `"{ ..., name }"` → `EllipsisNotLast`. |

## Verification

```sh
cargo test -p wql-compiler
cargo clippy -p wql-compiler
```

---

# Chunk 4c — Predicate Parser, Combined Form & Public API

## Goal

Implement predicate expression parsing with correct operator precedence, the `WHERE … SELECT …` combined form, and the public `parse()` entry point. After this chunk, the full WQL surface syntax is parseable.

## Deliverables

- `parser.rs`: predicate parsing methods, `parse_query` implementation.
- `lib.rs`: public `parse()` function.
- 25+ predicate and combined-form tests pass.
- All prior chunk tests still pass.

## Predicate parsing

### Operator precedence (lowest to highest)

| Level | Operators | Associativity |
|-------|-----------|---------------|
| 1 | `\|\|` / `OR` | Left |
| 2 | `&&` / `AND` | Left |
| 3 | `!` / `NOT` | Prefix (right) |
| 4 | Atoms | — |

### Parsing methods

```rust
impl<'a> Parser<'a> {
    // ── Predicate parsing (this chunk) ──

    fn parse_predicate(&mut self) -> Result<Predicate, ParseError> {
        self.parse_or_expr()
    }

    fn parse_or_expr(&mut self) -> Result<Predicate, ParseError> {
        // left = parse_and_expr()
        // while peek is || or OR: consume, right = parse_and_expr(), left = Or(left, right)
        todo!()
    }

    fn parse_and_expr(&mut self) -> Result<Predicate, ParseError> {
        // left = parse_unary_expr()
        // while peek is && or AND: consume, right = parse_unary_expr(), left = And(left, right)
        todo!()
    }

    fn parse_unary_expr(&mut self) -> Result<Predicate, ParseError> {
        // if peek is ! or NOT: consume, inner = parse_unary_expr(), return Not(inner)
        // else: parse_primary_expr()
        todo!()
    }

    fn parse_primary_expr(&mut self) -> Result<Predicate, ParseError> {
        // if peek is LParen: consume, inner = parse_predicate(), expect RParen, return inner
        // if peek is Exists or Has: consume, expect LParen, path = parse_field_path(), expect RParen, return Presence
        // else: path = parse_field_path(), then dispatch on next token:
        //   cmp_op → parse_literal → Comparison
        //   In → expect LBracket, parse literal_list, expect RBracket → InSet
        //   StartsWith/EndsWith/Contains/Matches → parse_literal → StringPredicate
        todo!()
    }

    fn parse_field_path(&mut self) -> Result<FieldPath, ParseError> {
        // first = parse_field_ref()
        // while peek is Dot: consume, next = parse_field_ref(), push
        // return FieldPath { segments, span: first.span.merge(last.span) }
        todo!()
    }

    fn parse_literal(&mut self) -> Result<Literal, ParseError> {
        // IntLit → Literal::Int
        // Minus IntLit → Literal::Int (negative)
        // StringLit → Literal::String
        // True → Literal::Bool(true)
        // False → Literal::Bool(false)
        todo!()
    }

    fn parse_literal_list(&mut self) -> Result<Vec<Literal>, ParseError> {
        // parse comma-separated literals until RBracket, allow trailing comma
        todo!()
    }
}
```

### `parse_query` implementation

```rust
fn parse_query(&mut self) -> Result<Query, ParseError> {
    match self.peek_kind()? {
        TokenKind::LBrace => {
            let proj = self.parse_projection()?;
            self.expect_eof()?;
            Ok(Query::Projection(proj))
        }
        TokenKind::Where => {
            self.lexer.next_token()?; // consume WHERE
            let predicate = self.parse_predicate()?;
            self.expect_select()?;    // consume SELECT
            let projection = self.parse_projection()?;
            self.expect_eof()?;
            Ok(Query::Combined { predicate, projection })
        }
        _ => {
            let predicate = self.parse_predicate()?;
            self.expect_eof()?;
            Ok(Query::Predicate(predicate))
        }
    }
}
```

## Public API — `lib.rs`

```rust
pub mod ast;
pub mod error;
pub mod lexer;
pub mod parser;

use ast::Query;
use error::ParseError;

/// Parse a WQL source string into a `Query` AST.
///
/// The returned AST has no IR knowledge and no schema binding.
/// Pass it to the schema binder (Block 5) to resolve field names
/// and validate literal types.
pub fn parse(source: &str) -> Result<Query, ParseError> {
    let mut parser = parser::Parser::new(source);
    parser.parse_query()
}
```

## Tests — `parser.rs` `#[cfg(test)]`

### Predicate parsing

| Test | Description |
|------|-------------|
| `pred_cmp_eq` | `"age == 42"` → `Comparison(path(age), Eq, Int(42))`. |
| `pred_cmp_neq` | `"status != 0"` → `Comparison(path(status), Neq, Int(0))`. |
| `pred_cmp_lt` | `"age < 18"` → `Comparison(path(age), Lt, Int(18))`. |
| `pred_cmp_lte` | `"age <= 18"` → `Comparison(path(age), Lte, Int(18))`. |
| `pred_cmp_gt` | `"age > 18"` → `Comparison(path(age), Gt, Int(18))`. |
| `pred_cmp_gte` | `"age >= 18"` → `Comparison(path(age), Gte, Int(18))`. |
| `pred_cmp_negative` | `"temp > -10"` → `Comparison(path(temp), Gt, Int(-10))`. |
| `pred_cmp_string` | `r#"name == "Alice""#` → `Comparison(path(name), Eq, String("Alice"))`. |
| `pred_cmp_bool` | `"active == true"` → `Comparison(path(active), Eq, Bool(true))`. |
| `pred_nested_field` | `"address.city == \"NYC\""` → `Comparison(path(address, city), Eq, …)`. |
| `pred_field_number_path` | `"#3.#1 == \"NYC\""` → `Comparison(path(#3, #1), Eq, …)`. |
| `pred_and_symbolic` | `"a > 1 && b > 2"` → `And(Cmp(a,Gt,1), Cmp(b,Gt,2))`. |
| `pred_and_keyword` | `"a > 1 AND b > 2"` → same AST as above. |
| `pred_or` | `"a > 1 \|\| b > 2"` → `Or(…)`. |
| `pred_or_keyword` | `"a > 1 OR b > 2"` → same AST. |
| `pred_not_bang` | `"!active == true"` → `Not(Comparison(…))`. |
| `pred_not_keyword` | `"NOT active == true"` → same AST. |
| `pred_precedence` | `"a > 1 \|\| b > 2 && c > 3"` → `Or(Cmp(a), And(Cmp(b), Cmp(c)))`. `&&` binds tighter than `\|\|`. |
| `pred_parens` | `"(a > 1 \|\| b > 2) && c > 3"` → `And(Or(…), Cmp(c))`. |
| `pred_double_not` | `"!!x == 1"` → `Not(Not(Comparison(…)))`. |
| `pred_exists` | `"exists(name)"` → `Presence(path(name))`. |
| `pred_has` | `"has(address.city)"` → `Presence(path(address, city))`. |
| `pred_in_set` | `"status in [1, 2, 3]"` → `InSet(path(status), [Int(1), Int(2), Int(3)])`. |
| `pred_in_set_empty` | `"status in []"` → `InSet(path(status), [])`. |
| `pred_in_set_trailing_comma` | `"status in [1, 2,]"` → `InSet(path(status), [Int(1), Int(2)])`. |
| `pred_starts_with` | `r#"name starts_with "Al""#` → `StringPredicate(path(name), StartsWith, String("Al"))`. |
| `pred_ends_with` | `r#"name ends_with "ce""#` → `StringPredicate(…, EndsWith, …)`. |
| `pred_contains` | `r#"name contains "lic""#` → `StringPredicate(…, Contains, …)`. |
| `pred_matches` | `r#"name matches "^A.*""#` → `StringPredicate(…, Matches, …)`. |
| `pred_complex` | `"age > 18 AND (name starts_with \"A\" OR exists(vip)) AND status in [1, 2]"` → correct nested AST. |

### Combined form

| Test | Description |
|------|-------------|
| `combined_simple` | `r#"WHERE age > 18 SELECT { name }"#` → `Combined { pred: Cmp(age,Gt,18), proj: Inclusion([name], false) }`. |
| `combined_complex` | `r#"WHERE age > 18 AND address.city == "NYC" SELECT { name, address { city }, ... }"#` → `Combined` with correct predicate tree and nested projection. |

### Contextual keyword as field name

| Test | Description |
|------|-------------|
| `field_named_exists` | `"exists == 1"` → `Comparison(path(exists), Eq, Int(1))`. Not a presence check. |
| `field_named_status_in_proj` | `"{ in, has, exists }"` → `Inclusion([Field(in), Field(has), Field(exists)], false)`. |

### Error cases

| Test | Description |
|------|-------------|
| `err_empty_input` | `""` → `UnexpectedEof`. |
| `err_bare_operator` | `"=="` → error (expected field path). |
| `err_missing_rhs` | `"age >"` → `UnexpectedEof`. |
| `err_missing_select` | `"WHERE age > 18"` → `Expected { expected: "SELECT", … }`. |
| `err_missing_projection` | `"WHERE age > 18 SELECT"` → `Expected { expected: "{", … }`. |
| `err_unclosed_paren` | `"(age > 18"` → `Expected { expected: ")", … }`. |
| `err_in_set_unclosed` | `"age in [1, 2"` → `Expected { expected: "]", … }`. |

## Verification

```sh
cargo test -p wql-compiler
cargo clippy -p wql-compiler
cargo fmt --check
```

---

## Constraints & Notes (all chunks)

- **No `wql-ir` dependency for parsing.** The parser and AST modules must not import anything from `wql-ir`. The AST is a pure syntax tree. Schema binding and IR emission happen in Block 5.
- **`std` allowed.** The compiler runs at startup/build time. `String`, `Vec`, `Box`, `std::error::Error` are all available.
- **Spans on all nodes.** Every AST node carries a `Span` referring to byte offsets in the source string. Error messages must include span information.
- **No type checking.** The parser does not validate that comparison operands are type-compatible (e.g., `name > 42` is syntactically valid). Type checking is Block 5's responsibility.
- **No field resolution.** `FieldRef::Name("city")` is left as a name. The schema binder resolves it to a field number.
- **`WHERE` / `SELECT` are reserved.** They cannot be used as field names. All other keywords are contextual.
- **Operator synonyms.** `&&` and `AND` are interchangeable. `||` and `OR` are interchangeable. `!` and `NOT` are interchangeable. The AST does not distinguish which form was used.
- **Trailing commas.** Allowed in projection item lists and `in […]` set literals for ergonomics.
- **Case sensitivity.** Keywords are case-sensitive: `WHERE` is a keyword, `where` is an identifier. `AND` is a keyword, `and` is an identifier. This matches the convention from the IR doc examples.
- **No float literals.** Floats are not supported in v1. The lexer does not parse `3.14` as a float — `3` becomes `IntLit`, `.` becomes `Dot`, `14` becomes `IntLit`. The parser rejects this sequence in context.
- **`in` as field name.** In projection context, `in` is a valid field name. In predicate context, after a field path, `in` triggers set-membership parsing. The grammar is unambiguous because `in` in predicate context always follows a field path and precedes `[`.
