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
    /// `{ field1, field2 { … } }` — strict inclusion, drop unmatched fields.
    Strict { items: Vec<ProjectionItem> },

    /// `{ field1, .. }` or `{ .. }` or `{ .. -field1 }` — copy mode.
    ///
    /// Copies all unmatched fields. Explicit items are included as usual.
    /// Exclusions (via `-field`) strip specific fields at the current level.
    /// `{ .. }` alone is identity copy. `{ .. -secret }` copies all except `secret`.
    Copy {
        items: Vec<ProjectionItem>,
        exclusions: Vec<FieldRef>,
    },
}

/// A single item in a projection.
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
    #[must_use]
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
    Eq,
    Neq,
    Lt,
    Lte,
    Gt,
    Gte,
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
    #[must_use]
    pub fn span(&self) -> Span {
        match self {
            Literal::Int(_, s) | Literal::String(_, s) | Literal::Bool(_, s) => *s,
        }
    }
}
