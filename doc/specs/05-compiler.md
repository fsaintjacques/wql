# Block 5 — `wql-compiler` Part 2: Schema Binder & IR Emitter

| | |
|---|---|
| **Status** | Draft |
| **Date** | 2026-03-30 |
| **Depends on** | Block 2 (`wql-ir`), Block 4 (parser & AST) |

---

## Goal

Implement the two remaining compiler stages that transform a parsed WQL AST into executable WVM bytecode:

1. **Schema binder** — resolves named field references to field numbers using a `FileDescriptorSet`; validates literal types; produces a `BoundAst`. Schema-free programs skip this stage.
2. **IR emitter** — walks the `BoundAst` (or raw AST in schema-free mode), allocates registers, emits `Vec<Instruction>` with correct DISPATCH/FRAME nesting.

The final bytecode encoding (header + label resolution + serialization) is delegated to `wql_ir::encode`, which already implements the two-pass linker from Block 2. There is no separate linker stage in this block.

The public entry point is `compile(source, options) -> Result<Vec<u8>, CompileError>`.

---

## Implementation Chunks

Block 5 is split into four sequential chunks. Each chunk compiles and passes its own tests before the next begins.

| Chunk | Scope | Key files |
|-------|-------|-----------|
| 5a | `CompileError`, `CompileOptions`, `compile()` shell, schema-free binding pass-through | `compile.rs`, `error.rs` (extended) |
| 5b | Schema binder — field resolution and type validation | `bind.rs` |
| 5c | IR emitter — projection lowering | `emit.rs` |
| 5d | IR emitter — predicate lowering, combined form, end-to-end tests | `emit.rs` (continued) |

---

## File Tree

```
crates/wql-compiler/
└── src/
    ├── lib.rs       # public parse() + compile() entry points
    ├── ast.rs       # (Block 4, unchanged)
    ├── lexer.rs     # (Block 4, unchanged)
    ├── parser.rs    # (Block 4, unchanged)
    ├── error.rs     # extended with CompileError
    ├── compile.rs   # compile() orchestration, CompileOptions
    ├── bind.rs      # schema binder: resolve names → field numbers
    └── emit.rs      # IR emitter: BoundAst → Vec<Instruction>
```

---

## Shared Types

### `CompileOptions` — `compile.rs`

```rust
/// Options controlling WQL compilation.
pub struct CompileOptions<'a> {
    /// Serialized `FileDescriptorSet` (full transitive closure).
    /// `None` = schema-free mode; field references must use `#N` syntax.
    pub schema: Option<&'a [u8]>,

    /// Fully-qualified root message type, e.g. `"acme.events.OrderPlaced"`.
    /// Required when `schema` is `Some`; ignored when `schema` is `None`.
    pub root_message: Option<&'a str>,
}

impl Default for CompileOptions<'_> {
    fn default() -> Self {
        Self {
            schema: None,
            root_message: None,
        }
    }
}
```

### `CompileError` — `error.rs` (extended)

```rust
/// Errors produced during WQL compilation.
#[derive(Debug)]
pub enum CompileError {
    /// Source failed to parse (wraps Block 4 ParseError).
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

    /// Schema-bound mode requires `root_message` in CompileOptions.
    MissingRootMessage,

    /// Schema-free mode encountered a named field reference.
    NamedFieldWithoutSchema { field: String, span: Span },

    /// Program requires more than 16 registers.
    TooManyRegisters,

    /// Failed to decode the FileDescriptorSet bytes.
    InvalidSchema(String),
}

impl From<ParseError> for CompileError {
    fn from(e: ParseError) -> Self { Self::Parse(e) }
}

impl std::fmt::Display for CompileError { /* ... */ }
impl std::error::Error for CompileError {}
```

### `BoundQuery` / `BoundProjection` / `BoundPredicate` — `bind.rs`

The bound AST mirrors the parsed AST but with all field references resolved to `(field_number, encoding_hint)` pairs. The binder produces these types; the emitter consumes them.

```rust
/// A field reference resolved to a proto field number.
#[derive(Debug, Clone)]
pub struct BoundField {
    pub field_num: u32,
    pub span: Span,
}

/// A resolved field path with encoding information for the leaf field.
#[derive(Debug, Clone)]
pub struct BoundFieldPath {
    /// Each segment is a field number. Length 1 = top-level field.
    /// Length 2+ = nested path (e.g., address.city → [3, 1]).
    pub segments: Vec<u32>,
    /// Encoding of the leaf field (for DECODE). `None` if unknown
    /// (schema-free mode with no type context, or field is a sub-message
    /// that is only projected, not decoded).
    pub encoding: Option<Encoding>,
    pub span: Span,
}

#[derive(Debug)]
pub enum BoundQuery {
    Projection(BoundProjection),
    Predicate(BoundPredicate),
    Combined {
        predicate: BoundPredicate,
        projection: BoundProjection,
    },
}

#[derive(Debug)]
pub struct BoundProjection {
    pub kind: BoundProjectionKind,
    pub span: Span,
}

#[derive(Debug)]
pub enum BoundProjectionKind {
    Inclusion {
        items: Vec<BoundProjectionItem>,
        preserve_unknowns: bool,
    },
    DeepCopy {
        exclusions: Vec<u32>,
    },
}

#[derive(Debug)]
pub enum BoundProjectionItem {
    /// Flat field copy.
    Field(BoundField),
    /// Enter sub-message, apply nested projection.
    Nested {
        field: BoundField,
        projection: Box<BoundProjection>,
    },
    /// Deep field search at any nesting depth.
    DeepSearch(BoundField),
}

#[derive(Debug)]
pub struct BoundPredicate {
    pub kind: BoundPredicateKind,
    pub span: Span,
}

#[derive(Debug)]
pub enum BoundPredicateKind {
    And(Box<BoundPredicate>, Box<BoundPredicate>),
    Or(Box<BoundPredicate>, Box<BoundPredicate>),
    Not(Box<BoundPredicate>),
    Comparison {
        field: BoundFieldPath,
        op: CompareOp,
        value: Literal,
    },
    Presence(BoundFieldPath),
    InSet {
        field: BoundFieldPath,
        values: Vec<Literal>,
    },
    StringPredicate {
        field: BoundFieldPath,
        op: StringOp,
        value: Literal,
    },
}
```

---

# Chunk 5a — Compile Shell & Schema-Free Binding

## Goal

Set up the `compile()` entry point, `CompileOptions`, `CompileError`, and the schema-free binding pass-through (which converts `FieldRef::Number` values directly to `BoundField` without a schema).

## Deliverables

- `compile.rs`: `CompileOptions`, `compile()` function shell.
- `error.rs`: `CompileError` enum (extended).
- `bind.rs`: `BoundQuery` types, `bind_schema_free()` function.
- `lib.rs`: public `compile()` re-export.
- 8+ tests pass.

## `compile()` — `compile.rs`

```rust
pub fn compile(source: &str, options: &CompileOptions) -> Result<Vec<u8>, CompileError> {
    let query = crate::parse(source)?;

    let bound = if options.schema.is_some() {
        crate::bind::bind_with_schema(&query, options)?
    } else {
        crate::bind::bind_schema_free(&query)?
    };

    let instructions = crate::emit::emit(&bound)?;
    Ok(wql_ir::encode(&instructions))
}
```

## Schema-free binding — `bind.rs`

In schema-free mode, all field references must be `FieldRef::Number`. The binder walks the AST and converts each `FieldRef::Number(n)` to `BoundField { field_num: n }`. If a `FieldRef::Name` is encountered, return `CompileError::NamedFieldWithoutSchema`.

Encoding inference in schema-free mode is based on the literal type in the predicate context:

| Literal type | Inferred encoding |
|-------------|-------------------|
| `Literal::Int` | `Encoding::Varint` |
| `Literal::Bool` | `Encoding::Varint` |
| `Literal::String` | `Encoding::Len` |

For projection-only fields (no predicate context), `encoding` is `None` — the field is only copied, never decoded.

For `Presence` predicates, `encoding` is `None` — `IS_SET` checks whether the register was written, which is determined by a `DECODE` in the DISPATCH arm. The emitter uses `Encoding::Varint` as a default for presence-only decodes.

## Tests — `compile.rs` `#[cfg(test)]`

| Test | Description |
|------|-------------|
| `compile_empty_projection` | `"{ }"` with default options → valid bytecode (round-trip via `wql_ir::decode`). |
| `compile_flat_projection_schema_free` | `"{ #1, #2 }"` → bytecode with `DISPATCH(SKIP, [1→COPY, 2→COPY]), RETURN`. |
| `compile_preserve_unknowns` | `"{ #1, ... }"` → `DISPATCH(COPY, [1→COPY]), RETURN`. |
| `compile_named_field_without_schema` | `"{ name }"` with no schema → `CompileError::NamedFieldWithoutSchema`. |
| `compile_parse_error_propagates` | `"{ unclosed"` → `CompileError::Parse(...)`. |
| `compile_options_default` | `CompileOptions::default()` has `schema: None`. |
| `compile_roundtrip_decode` | Compile a schema-free program, decode with `wql_ir::decode`, verify instruction structure. |
| `compile_predicate_schema_free` | `"#1 > 42"` → bytecode with DISPATCH+DECODE+CMP_GT+RETURN. |

## Verification

```sh
cargo test -p wql-compiler
cargo clippy -p wql-compiler
```

---

# Chunk 5b — Schema Binder

## Goal

Implement `bind_with_schema()` — resolve named field references using a `FileDescriptorSet`, validate literal types against proto field types, and produce a `BoundQuery`.

## Deliverables

- `bind.rs`: `bind_with_schema()`, schema walking helpers.
- 12+ schema binder tests pass.

## Schema Resolution — `bind.rs`

```rust
use prost::Message as _;
use prost_types::{
    DescriptorProto, FieldDescriptorProto, FileDescriptorSet,
    field_descriptor_proto::Type as ProtoType,
};

pub fn bind_with_schema(
    query: &Query,
    options: &CompileOptions,
) -> Result<BoundQuery, CompileError> {
    let schema_bytes = options.schema.unwrap();
    let fds = FileDescriptorSet::decode(schema_bytes)
        .map_err(|e| CompileError::InvalidSchema(e.to_string()))?;

    let root_msg_name = options.root_message.ok_or(CompileError::MissingRootMessage)?;
    let root_msg = find_message(&fds, root_msg_name)
        .ok_or_else(|| CompileError::InvalidMessageType {
            type_name: root_msg_name.to_string(),
        })?;

    // Walk AST, resolve field names against root_msg.
    bind_query(query, root_msg, &fds)
}
```

### Field name resolution

For each `FieldRef::Name(name)`:
1. Search the current `DescriptorProto.field` for a `FieldDescriptorProto` with matching `name`.
2. Extract `field.number` as the resolved field number.
3. Extract `field.type_` to determine the wire type and encoding.
4. If the field is a sub-message (`type_ == TYPE_MESSAGE`), resolve `field.type_name` to the nested `DescriptorProto` for recursive binding.

`FieldRef::Number(n)` in schema-bound mode: accepted as-is (allows mixing named and numbered references). The binder looks up the field by number to infer the encoding.

### Proto type to encoding mapping

```rust
fn proto_type_to_encoding(ty: ProtoType) -> Encoding {
    match ty {
        // Varint-encoded types
        ProtoType::Int32 | ProtoType::Int64
        | ProtoType::Uint32 | ProtoType::Uint64
        | ProtoType::Bool | ProtoType::Enum => Encoding::Varint,

        // Zigzag-encoded types
        ProtoType::Sint32 | ProtoType::Sint64 => Encoding::Sint,

        // Fixed 32-bit types
        ProtoType::Fixed32 | ProtoType::Sfixed32
        | ProtoType::Float => Encoding::I32,

        // Fixed 64-bit types
        ProtoType::Fixed64 | ProtoType::Sfixed64
        | ProtoType::Double => Encoding::I64,

        // Length-delimited types
        ProtoType::String | ProtoType::Bytes
        | ProtoType::Message => Encoding::Len,

        // Group (unsupported in v1)
        ProtoType::Group => Encoding::Len,
    }
}
```

### Literal type validation

When a predicate compares a field to a literal, the binder checks compatibility:

| Field proto type | Allowed literal types |
|-----------------|-----------------------|
| Integer types (`int32`, `int64`, `uint32`, `uint64`, `sint32`, `sint64`, `fixed*`, `sfixed*`) | `Literal::Int` |
| `bool` | `Literal::Bool` |
| `string`, `bytes` | `Literal::String` |
| `enum` | `Literal::Int` (enum ordinal) |
| `float`, `double` | (rejected in v1 — `CompileError::TypeError`) |
| `message` | (rejected — cannot compare sub-messages) |

For `InSet`, all values must be `Literal::Int` and the field must be an integer or enum type.

For string predicates (`starts_with`, `ends_with`, `contains`, `matches`), the field must be `string` or `bytes` and the value must be `Literal::String`.

## Tests — `bind.rs` `#[cfg(test)]`

Tests use a hand-built `FileDescriptorSet` constructed via prost-types struct literals (no `.proto` files or `protoc` needed).

Helper to build a test schema:

```rust
fn test_schema() -> Vec<u8> {
    // Person { name: string = 1, age: int64 = 2, address: Address = 3, status: Status = 4 }
    // Address { city: string = 1, country: string = 2 }
    // enum Status { ACTIVE = 0, INACTIVE = 1 }
    let fds = FileDescriptorSet { file: vec![...] };
    fds.encode_to_vec()
}
```

| Test | Description |
|------|-------------|
| `bind_resolve_name` | `"{ name, age }"` with schema → `BoundProjection` with field numbers 1, 2. |
| `bind_resolve_nested` | `"{ address { city } }"` → nested `BoundProjection` with field 3 containing field 1. |
| `bind_field_number_in_schema` | `"{ #1, #2 }"` with schema → accepted, field numbers pass through. |
| `bind_mixed_name_and_number` | `"{ name, #2 }"` with schema → field numbers 1, 2. |
| `bind_unresolved_field` | `"{ unknown }"` → `CompileError::UnresolvedField`. |
| `bind_invalid_message_type` | Schema bound with non-existent root message → `CompileError::InvalidMessageType`. |
| `bind_missing_root_message` | Schema provided but `root_message` is `None` → `CompileError::MissingRootMessage`. |
| `bind_type_check_int_ok` | `"age > 18"` with `age: int64` → passes. |
| `bind_type_check_string_ok` | `r#"name == "Alice""#` with `name: string` → passes. |
| `bind_type_check_mismatch` | `r#"age == "old""#` with `age: int64` → `CompileError::TypeError`. |
| `bind_predicate_nested` | `r#"address.city == "NYC""#` → path resolves to [3, 1] with `Encoding::Len`. |
| `bind_in_set_type_check` | `"status in [0, 1]"` with `status: enum` → passes. |

## Verification

```sh
cargo test -p wql-compiler
cargo clippy -p wql-compiler
```

---

# Chunk 5c — IR Emitter: Projection Lowering

## Goal

Implement projection lowering: convert a `BoundProjection` into `Vec<Instruction>` with correct DISPATCH arms, FRAME nesting, and RECURSE for deep copy/search.

## Deliverables

- `emit.rs`: `emit()` function, projection lowering.
- 10+ projection emission tests pass.

## Emitter State — `emit.rs`

```rust
struct Emitter {
    instructions: Vec<Instruction>,
    next_label: u32,
    next_register: u8,
    max_frame_depth: u8,
    current_frame_depth: u8,
}

impl Emitter {
    fn new() -> Self { ... }

    fn alloc_label(&mut self) -> u32 {
        let idx = self.next_label;
        self.next_label += 1;
        idx
    }

    fn alloc_register(&mut self) -> Result<u8, CompileError> {
        if self.next_register >= 16 {
            return Err(CompileError::TooManyRegisters);
        }
        let reg = self.next_register;
        self.next_register += 1;
        Ok(reg)
    }

    fn emit(&mut self, instr: Instruction) {
        self.instructions.push(instr);
    }
}
```

## Projection Lowering Rules

### `Inclusion { items, preserve_unknowns }`

```
default = if preserve_unknowns { DefaultAction::Copy } else { DefaultAction::Skip }

DISPATCH(default, arms)
  for each item:
    Field(n)        → arm: Field(n) → [Copy]
    Nested(n, proj) → arm: Field(n) → [Frame(label)]
                      (emit sub-program at label after current DISPATCH)
    DeepSearch(n)   → arm: Field(n) → [Copy]
                      (deep search handled via RECURSE, see below)
RETURN
```

For `DeepSearch(n)` within an inclusion list: the DISPATCH default must be `Recurse(self_label)` to search nested messages. But if `preserve_unknowns` is also set, we need `Copy` for non-LEN fields and `Recurse` for LEN fields — `Recurse` already handles this (non-LEN fields are skipped by RECURSE). However, `Recurse` and `Copy` cannot both be the default.

Resolution: when the inclusion list contains a `DeepSearch` item, the default is `Recurse(self_label)`, and a `Label` is emitted before the DISPATCH. If `preserve_unknowns` is also true, the behavior degrades gracefully — non-LEN unknown fields are skipped (RECURSE skips non-LEN), which differs from `Copy` semantics. This is an acceptable trade-off for v1; the spec notes this limitation.

### `DeepCopy { exclusions }`

```
LABEL(self_label)
DISPATCH(RECURSE(self_label), arms)
  for each exclusion field_num:
    arm: Field(field_num) → [Skip]
RETURN
```

### Nested sub-programs

Each `Nested { field, projection }` emits:
1. In the parent DISPATCH arm: `Frame(label_idx)`
2. After all top-level instructions: `Label`, then recursively emit the nested projection, then `Return`.

Track `current_frame_depth` — increment on entering a nested emission, decrement on exit. Update `max_frame_depth`.

## Tests — `emit.rs` `#[cfg(test)]`

All tests compile a schema-free WQL program via `compile()`, then decode with `wql_ir::decode()` and inspect the instruction list.

| Test | Description |
|------|-------------|
| `emit_flat_strict` | `"{ #1, #2 }"` → `[Dispatch(Skip, [1→Copy, 2→Copy]), Return]`. |
| `emit_flat_preserve` | `"{ #1, ... }"` → `[Dispatch(Copy, [1→Copy]), Return]`. |
| `emit_empty` | `"{ }"` → `[Dispatch(Skip, []), Return]`. |
| `emit_identity` | `"{ ... }"` → `[Dispatch(Copy, []), Return]`. |
| `emit_nested` | `"{ #1, #3 { #1 } }"` → Dispatch with Frame arm, Label, nested Dispatch, Return. |
| `emit_nested_preserve` | `"{ #1, #3 { #1, ... }, ... }"` → outer default Copy, inner default Copy. |
| `emit_deep_copy` | `"{ .. }"` → `[Label, Dispatch(Recurse(0), []), Return]`. |
| `emit_deep_exclusion` | `"{ .. -#7 }"` → `[Label, Dispatch(Recurse(0), [7→Skip]), Return]`. |
| `emit_deep_search` | `"{ ..#1 }"` → Label, Dispatch(Recurse(self), [1→Copy]), Return. |
| `emit_nested_two_levels` | `"{ #1 { #2 { #3 } } }"` → two FRAME levels, max_frame_depth=2. |

## Verification

```sh
cargo test -p wql-compiler
cargo clippy -p wql-compiler
```

---

# Chunk 5d — IR Emitter: Predicates & Combined Form

## Goal

Implement predicate lowering, combined filter+projection, and end-to-end integration tests that verify compiled bytecode executes correctly via `wql-runtime`.

## Deliverables

- `emit.rs`: predicate lowering, combined DISPATCH merging.
- 20+ predicate and combined tests pass.
- End-to-end tests (compile → execute → verify) pass.

## Predicate Lowering

A predicate program consists of:
1. A `DISPATCH` that decodes fields referenced in the predicate into registers.
2. Comparison / logic instructions that evaluate the predicate on the bool stack.
3. `RETURN`.

### Field collection

Before emitting the DISPATCH, walk the predicate tree and collect all unique `(field_path, encoding)` pairs. Assign a register to each.

```rust
struct FieldDecodeInfo {
    /// Full path segments (e.g., [3, 1] for address.city).
    path: Vec<u32>,
    /// Allocated register index.
    reg: u8,
    /// Encoding for the DECODE instruction.
    encoding: Encoding,
}
```

For multi-segment paths (e.g., `address.city`), the first segment gets a FRAME in the DISPATCH, and the leaf segment gets a DECODE in the nested DISPATCH.

### Predicate instruction emission

After the DISPATCH(es), walk the predicate tree in post-order and emit:

| AST node | Instructions emitted |
|----------|---------------------|
| `Comparison { field, Eq, Int(n) }` | `CmpEq { reg, imm: n }` |
| `Comparison { field, Neq, Int(n) }` | `CmpNeq { reg, imm: n }` |
| `Comparison { field, Lt, Int(n) }` | `CmpLt { reg, imm: n }` |
| `Comparison { field, Lte, Int(n) }` | `CmpLte { reg, imm: n }` |
| `Comparison { field, Gt, Int(n) }` | `CmpGt { reg, imm: n }` |
| `Comparison { field, Gte, Int(n) }` | `CmpGte { reg, imm: n }` |
| `Comparison { field, Eq, String(s) }` | `CmpLenEq { reg, bytes: s.into_bytes() }` |
| `Comparison { field, Eq, Bool(b) }` | `CmpEq { reg, imm: if b { 1 } else { 0 } }` |
| `Presence(field)` | `IsSet { reg }` |
| `InSet { field, values }` | `InSet { reg, values: [ints] }` |
| `StringPredicate { field, StartsWith, String(s) }` | `BytesStarts { reg, bytes }` |
| `StringPredicate { field, EndsWith, String(s) }` | `BytesEnds { reg, bytes }` |
| `StringPredicate { field, Contains, String(s) }` | `BytesContains { reg, bytes }` |
| `StringPredicate { field, Matches, String(s) }` | `BytesMatches { reg, pattern }` (requires `regex` feature) |
| `And(left, right)` | emit left, emit right, `And` |
| `Or(left, right)` | emit left, emit right, `Or` |
| `Not(inner)` | emit inner, `Not` |

### Combined form (predicate + projection)

The combined form merges predicate decode arms and projection copy arms into a single DISPATCH:

- Field only in projection → arm action: `[Copy]`
- Field only in predicate → arm action: `[Decode { reg, encoding }]`
- Field in both → arm action: `[Decode { reg, encoding }, Copy]`
- Nested field in predicate + projection → arm action: `[Frame(label)]` (the nested DISPATCH handles both decode and copy)

After the merged DISPATCH, emit predicate instructions, then RETURN.

### Presence predicates

`Presence(field)` needs the field decoded into a register so `IsSet` can check it. The emitter adds a DECODE arm for the field with a default encoding (`Encoding::Varint` for scalar context, `Encoding::Len` if the field is a sub-message). The value is ignored — only the register-was-written flag matters.

## Tests — `emit.rs` `#[cfg(test)]`

### Predicate tests

| Test | Description |
|------|-------------|
| `emit_pred_cmp_eq` | `"#1 == 42"` → Dispatch+Decode(R0,Varint), CmpEq(R0,42), Return. |
| `emit_pred_cmp_gt` | `"#1 > 18"` → CmpGt. |
| `emit_pred_cmp_neq` | `"#1 != 0"` → CmpNeq. |
| `emit_pred_string_eq` | `r#"#1 == "hello""#` → Dispatch+Decode(R0,Len), CmpLenEq(R0, b"hello"), Return. |
| `emit_pred_bool` | `"#1 == true"` → CmpEq(R0, 1). |
| `emit_pred_and` | `"#1 > 0 && #2 > 0"` → two decodes, CmpGt, CmpGt, And. |
| `emit_pred_or` | `"#1 > 0 \|\| #2 > 0"` → CmpGt, CmpGt, Or. |
| `emit_pred_not` | `"!#1 == 0"` → CmpEq, Not. |
| `emit_pred_nested` | `"#3.#1 > 0"` → Dispatch with Frame, nested Dispatch+Decode, CmpGt. |
| `emit_pred_exists` | `"exists(#1)"` → Dispatch+Decode(R0,Varint), IsSet(R0). |
| `emit_pred_in_set` | `"#1 in [1, 2, 3]"` → InSet(R0, [1,2,3]). |
| `emit_pred_starts_with` | `r#"#1 starts_with "pre""#` → BytesStarts. |

### Combined form tests

| Test | Description |
|------|-------------|
| `emit_combined_simple` | `"WHERE #2 > 18 SELECT { #1 }"` → Dispatch(Skip, [1→Copy, 2→Decode]), CmpGt, Return. |
| `emit_combined_shared_field` | `"WHERE #1 > 0 SELECT { #1 }"` → arm [Decode, Copy] for field 1. |
| `emit_combined_nested` | `"WHERE #3.#1 > 0 SELECT { #1, #3 { #1 }, ... }"` → merged DISPATCH with Frame for field 3. |
| `emit_combined_preserve` | `"WHERE #1 > 0 SELECT { #1, ... }"` → default Copy. |

### End-to-end tests (compile → execute)

These tests compile WQL source, load the bytecode into `wql-runtime`, execute against hand-built protobuf input, and verify the output.

| Test | Description |
|------|-------------|
| `e2e_project_flat` | Compile `"{ #1, #2 }"`, run `project()` on `{1:varint, 2:LEN, 3:varint}` → output has fields 1, 2 only. |
| `e2e_project_nested` | Compile `"{ #1, #3 { #1 } }"`, run project on nested message → correct output. |
| `e2e_filter_true` | Compile `"#2 > 18"`, run `filter()` on `{2: 25}` → true. |
| `e2e_filter_false` | Same filter on `{2: 10}` → false. |
| `e2e_combined` | Compile `"WHERE #2 > 18 SELECT { #1 }"`, run `project_and_filter()` → Some(n) with field 1. |
| `e2e_deep_copy_exclusion` | Compile `"{ .. -#3 }"`, verify field 3 is stripped. |

## Verification

```sh
cargo test -p wql-compiler
cargo clippy -p wql-compiler
cargo fmt --check
```

---

## Constraints & Notes (all chunks)

- **`wql_ir::encode` is the linker.** The emitter produces `Vec<Instruction>` with label indices (not byte offsets). `wql_ir::encode` handles the two-pass label-to-offset resolution and bytecode serialization. No separate linker stage is needed.
- **Label index convention.** Labels are numbered 0, 1, 2, ... in emission order. `Instruction::Label` at position N in the instruction list is label index N (0-based count of Label instructions seen so far). `DefaultAction::Recurse(idx)` and `ArmAction::Frame(idx)` reference these indices.
- **Register allocation is greedy.** Registers are assigned in AST traversal order. Each unique `(field_path, encoding)` pair gets one register. Registers persist across FRAME scopes (IR invariant I-04).
- **Schema-free encoding inference.** Without a schema, the emitter infers encoding from the literal type: `Int`/`Bool` → `Varint`, `String` → `Len`. For `Presence` without a literal, default to `Varint`.
- **`std` allowed.** The compiler runs at build/startup time. `HashMap`, `String`, `Vec`, `prost::Message` are all available.
- **No float support in v1.** Float comparisons are rejected by the binder (`CompileError::TypeError`). The parser doesn't produce float literals, and the IR has no float comparison instructions.
- **`BYTES_MATCHES` requires `regex` feature.** The emitter checks `cfg!(feature = "regex")` before emitting this instruction and returns `CompileError::UnsupportedConstruct` if the feature is off.
- **Combined DISPATCH merging.** When a field appears in both the predicate and projection, its DISPATCH arm contains both `Decode` and `Copy` actions (`Decode` first, then `Copy`). This ensures the register is loaded and the bytes are forwarded in a single pass.
- **Deep search + preserve_unknowns limitation.** When an inclusion projection contains both `DeepSearch` items and `preserve_unknowns: true`, the RECURSE default causes non-LEN unknown fields to be skipped rather than copied. This is documented and accepted for v1.
