# Block 3 — `wql-runtime`: WVM Interpreter

| | |
|---|---|
| **Status** | Draft |
| **Date** | 2026-03-29 |
| **Depends on** | Block 2 |

---

## Goal

Implement the WVM interpreter in the `wql-runtime` crate. This is the hot path — every message flows through it. The interpreter decodes the program once at load time, then executes it against each `(input, output)` pair. The three public functions (`filter`, `project`, `project_and_filter`) are thin wrappers over a unified `execute() -> (bool, usize)`.

---

## Implementation Chunks

Block 3 is split into four sequential chunks. Each chunk compiles and passes its own tests before the next begins.

| Chunk | Scope | Key files |
|-------|-------|-----------|
| 3a | Wire scanner + test helpers | `wire.rs`, `error.rs`, test utils |
| 3b | Flat projection (DISPATCH + COPY/SKIP) | `vm.rs`, `lib.rs` (`LoadedProgram`, `project`) |
| 3c | Nested projection (FRAME / RECURSE) | `vm.rs` additions |
| 3d | DECODE + predicates + filter | `vm.rs` additions, `lib.rs` (`filter`, `project_and_filter`) |

---

## File Tree

```
crates/wql-runtime/
└── src/
    ├── lib.rs       # public API: LoadedProgram, filter, project, project_and_filter
    ├── error.rs     # RuntimeError
    ├── vm.rs        # Vm struct, execute(), instruction dispatch
    ├── wire.rs      # wire scanner, WireField, protobuf tag/value parsing
    └── test_utils.rs # #[cfg(test)] protobuf wire encoding helpers
```

---

## Shared Types (all chunks)

### `RuntimeError` — `error.rs`

```rust
#[derive(Debug, PartialEq, Eq)]
pub enum RuntimeError {
    /// Input protobuf bytes are malformed (truncated field, bad varint, unknown wire type).
    MalformedInput,
    /// Output buffer is too small (must be >= input.len()).
    OutputBufferTooSmall,
    /// Bool stack underflow (AND/OR/NOT with insufficient operands).
    StackUnderflow,
    /// Program decoding failed.
    Decode(wql_ir::DecodeError),
    /// FRAME nesting exceeded the program's declared max_frame_depth.
    FrameDepthExceeded,
}
```

### `RegisterValue` — `vm.rs`

```rust
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum RegisterValue {
    Int(i64),
    Bytes(Vec<u8>),
}
```

### `LoadedProgram` — `lib.rs`

```rust
pub struct LoadedProgram {
    header: ProgramHeader,
    instructions: Vec<Instruction>,
    /// label_index → instruction_index in `instructions`.
    label_table: Vec<usize>,
}

impl LoadedProgram {
    pub fn from_bytes(buf: &[u8]) -> Result<Self, wql_ir::DecodeError> { todo!() }
}
```

### Test Helpers — `test_utils.rs`

Available to all chunks. `#[cfg(test)]` only.

```rust
/// Encode a protobuf tag as varint bytes.
pub fn encode_tag(field_num: u32, wire_type: u8) -> Vec<u8>;

/// Encode an unsigned varint.
pub fn encode_varint(v: u64) -> Vec<u8>;

/// Encode a varint field: tag + varint value.
pub fn encode_varint_field(field_num: u32, value: u64) -> Vec<u8>;

/// Encode a sint (zigzag) field: tag + zigzag varint.
pub fn encode_sint_field(field_num: u32, value: i64) -> Vec<u8>;

/// Encode a length-delimited field: tag + length varint + payload.
pub fn encode_len_field(field_num: u32, payload: &[u8]) -> Vec<u8>;

/// Encode a fixed32 field: tag + 4 LE bytes.
pub fn encode_fixed32_field(field_num: u32, value: u32) -> Vec<u8>;

/// Encode a fixed64 field: tag + 8 LE bytes.
pub fn encode_fixed64_field(field_num: u32, value: u64) -> Vec<u8>;
```

---

# Chunk 3a — Wire Scanner + Test Helpers

## Goal

Implement the protobuf wire format scanner and the test encoding helpers. This chunk has no dependency on `wql-ir` instruction types — it operates purely on raw bytes.

## Deliverables

- `wire.rs`: `WireField` struct, `scan_fields()` iterator/function.
- `error.rs`: `RuntimeError` enum.
- `test_utils.rs`: proto wire encoding helpers.
- 7+ wire scanner tests pass.

## Wire Scanner — `wire.rs`

```rust
use crate::error::RuntimeError;
use wql_ir::WireType;

/// A single protobuf field as read from the wire.
pub(crate) struct WireField<'a> {
    /// Raw tag varint bytes (for verbatim COPY).
    pub tag_bytes: &'a [u8],
    /// Decoded field number.
    pub field_num: u32,
    /// Decoded wire type.
    pub wire_type: WireType,
    /// Raw value bytes (everything after the tag, up to end of this field).
    /// For LEN: includes the length prefix varint + payload.
    pub value_bytes: &'a [u8],
    /// For LEN fields: payload bytes only (excludes length prefix).
    /// For non-LEN fields: empty slice.
    pub len_payload: &'a [u8],
}
```

### `scan_fields`

```rust
/// Iterator over wire fields in a protobuf message byte slice.
pub(crate) struct WireScanner<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> WireScanner<'a> {
    pub fn new(buf: &'a [u8]) -> Self;
}

impl<'a> Iterator for WireScanner<'a> {
    type Item = Result<WireField<'a>, RuntimeError>;
}
```

The scanner must:
- Read the tag varint. Extract `field_num = tag >> 3`, `wire_type = tag & 0x07`.
- Validate `wire_type` via `WireType::from_u8`. Unknown → `RuntimeError::MalformedInput`.
- Based on wire type, determine value extent:
  - `Varint`: scan bytes until high bit clear.
  - `I64`: 8 fixed bytes.
  - `LEN`: read length varint `N`, then `N` payload bytes. `value_bytes` = length varint + payload. `len_payload` = payload only.
  - `I32`: 4 fixed bytes.
- If insufficient bytes at any point → `RuntimeError::MalformedInput`.

### Internal helpers

```rust
/// Read a varint from buf starting at pos. Returns (value, bytes_consumed).
pub(crate) fn read_varint(buf: &[u8], pos: usize) -> Result<(u64, usize), RuntimeError>;
```

This is also used by chunk 3d for DECODE.

## Tests — `wire.rs` `#[cfg(test)]`

| Test | Description |
|------|-------------|
| `wire_scan_varint` | Single varint field (field 1, value 150). Verify `field_num=1`, `wire_type=Varint`, correct `value_bytes`. |
| `wire_scan_i32` | Single fixed32 field (field 2, value 0x12345678). |
| `wire_scan_i64` | Single fixed64 field (field 3, value 0x123456789ABCDEF0). |
| `wire_scan_len` | Single LEN field (field 4, payload b"hello"). Verify `len_payload == b"hello"` and `value_bytes` includes length prefix. |
| `wire_scan_multi` | 3 fields: varint + LEN + fixed32. Verify all parse correctly in order. |
| `wire_scan_empty` | Empty input → iterator yields nothing. |
| `wire_scan_truncated_tag` | Input `[0x80]` (incomplete tag varint) → `MalformedInput`. |
| `wire_scan_truncated_value` | Tag for a LEN field with length=10 but only 3 payload bytes → `MalformedInput`. |
| `wire_scan_unknown_wire_type` | Tag with wire type 6 → `MalformedInput`. |

## Verification

```sh
cargo test -p wql-runtime
cargo clippy -p wql-runtime
cargo build -p wql-runtime --target wasm32-unknown-unknown
```

---

# Chunk 3b — Flat Projection (DISPATCH + COPY/SKIP)

## Goal

Implement the core execution loop: `LoadedProgram`, `Vm`, `DISPATCH` with `COPY`/`SKIP` arm actions and default actions (excluding `RECURSE`). Public `project()` function. This chunk does not handle nesting (FRAME/RECURSE) or predicates (DECODE/CMP).

## Deliverables

- `lib.rs`: `LoadedProgram::from_bytes`, public `project()`.
- `vm.rs`: `Vm` struct, `execute()`, DISPATCH loop, COPY/SKIP actions, RETURN, LABEL (no-op).
- 6+ projection tests pass.

## `Vm` — `vm.rs`

```rust
pub(crate) struct Vm<'a> {
    instructions: &'a [Instruction],
    label_table: &'a [usize],

    /// R0–R15. None = not set.
    registers: [Option<RegisterValue>; 16],

    /// Predicate bool stack.
    bool_stack: Vec<bool>,
}
```

The `Vm` does **not** own the input/output buffers. Instead, `execute` takes them as arguments:

```rust
impl<'a> Vm<'a> {
    pub fn new(instructions: &'a [Instruction], label_table: &'a [usize]) -> Self;

    /// Execute starting at instruction index `start_pc` over the given
    /// input window, writing to `output` at `output_cursor`.
    /// Returns `(bool, output_bytes_written)`.
    pub fn execute(
        &mut self,
        start_pc: usize,
        input: &[u8],
        output: &mut [u8],
        output_cursor: usize,
    ) -> Result<(bool, usize), RuntimeError>;
}
```

### Execution loop

Starting at `start_pc`, walk instructions sequentially:

1. **`Dispatch { default, arms }`** — For each `WireField` from `WireScanner::new(input)`:
   - Find the first arm where `arm.match_` matches `(field_num, wire_type)`.
   - If matched: execute arm actions. If not: execute default action.
   - `ArmAction::Copy`: append `tag_bytes` + `value_bytes` to output.
   - `ArmAction::Skip`: do nothing.
   - `DefaultAction::Copy`: same as `ArmAction::Copy`.
   - `DefaultAction::Skip`: do nothing.
   - `DefaultAction::Recurse(_)`: **skip in this chunk** (treated as Skip, panic in debug).
   - `ArmAction::Frame(_)`: **skip in this chunk**.
   - `ArmAction::Decode { .. }`: **skip in this chunk**.
   - Advance PC past the DISPATCH.

2. **`Label`** — no-op, advance PC.

3. **`Return`** — stop. Return `(bool_stack.last().copied().unwrap_or(true), bytes_written)`.

4. **All other instructions** — skip in this chunk (advance PC). Predicate instructions become no-ops that will be implemented in chunk 3d.

### `project()` public function

```rust
pub fn project(
    program: &LoadedProgram,
    input: &[u8],
    output: &mut [u8],
) -> Result<usize, RuntimeError> {
    if output.len() < input.len() {
        return Err(RuntimeError::OutputBufferTooSmall);
    }
    let mut vm = Vm::new(&program.instructions, &program.label_table);
    let (_, written) = vm.execute(0, input, output, 0)?;
    Ok(written)
}
```

## Tests — `vm.rs` `#[cfg(test)]`

All tests build a program with `wql_ir::encode`, create protobuf input with test helpers, call `project()`, and verify output bytes.

| Test | Description |
|------|-------------|
| `project_flat_strict` | `DISPATCH(SKIP, [1→COPY, 2→COPY])` on message `{1:varint, 2:LEN, 3:varint}`. Output has fields 1,2 only. |
| `project_flat_preserve` | `DISPATCH(COPY, [3→SKIP])` on same message. Output has fields 1,2 only. |
| `project_identity` | `DISPATCH(COPY, [])` — output equals input. |
| `project_drop_all` | `DISPATCH(SKIP, [])` — output is empty (0 bytes). |
| `project_repeated_field` | Field 1 appears 3 times. `DISPATCH(SKIP, [1→COPY])`. All 3 copied. |
| `project_empty_input` | Empty input → 0 bytes output. |
| `project_output_too_small` | Output buffer smaller than input → `OutputBufferTooSmall`. |

## Verification

```sh
cargo test -p wql-runtime
cargo clippy -p wql-runtime
cargo build -p wql-runtime --target wasm32-unknown-unknown
```

---

# Chunk 3c — Nested Projection (FRAME / RECURSE)

## Goal

Implement `ArmAction::Frame` and `DefaultAction::Recurse` — entering sub-message scope, recursively executing sub-programs, and correctly rewriting length prefixes in the output.

## Deliverables

- `vm.rs`: FRAME execution with length-prefix gap-and-shift.
- `vm.rs`: RECURSE execution (FRAME for unmatched LEN fields).
- 5+ nesting tests pass.

## FRAME Execution

When `ArmAction::Frame(label_idx)` executes for a matched `WireField`:

1. If `wire_type != LEN` → treat as skip (defensive; well-formed programs never do this).
2. Write `tag_bytes` to output at `output_cursor`. Advance cursor by `tag_bytes.len()`.
3. Record `tag_end = output_cursor`.
4. Reserve 5 bytes for the length varint (max LEB128 for u32). Set `sub_start = tag_end + 5`.
5. Recursively call `self.execute(label_table[label_idx], field.len_payload, output, sub_start)`.
6. The sub-call returns `(_, sub_written)`.
7. Encode `sub_written` as a varint → `len_varint` bytes (1–5 bytes, call it `varint_len`).
8. If `varint_len < 5`: shift `output[sub_start..sub_start+sub_written]` left by `5 - varint_len` using `output.copy_within(sub_start..sub_start+sub_written, tag_end + varint_len)`.
9. Write the length varint at `output[tag_end..tag_end+varint_len]`.
10. Return with `output_cursor = tag_end + varint_len + sub_written`.

### RECURSE

When `DefaultAction::Recurse(label_idx)` triggers for an unmatched field:
- If `wire_type == LEN`: execute FRAME logic with `label_idx`.
- If `wire_type != LEN`: skip the field (do nothing).

### Frame depth guard

Track current nesting depth. Before entering a FRAME, check `depth < header.max_frame_depth + 1` (the +1 accounts for the root level not being a frame). If exceeded → `RuntimeError::FrameDepthExceeded`. Use a reasonable hard cap (e.g., 64) as a safety net against malformed programs.

### Varint encoding helper

```rust
/// Encode a u32 as a varint into buf[pos..]. Returns bytes written (1–5).
fn write_varint(buf: &mut [u8], pos: usize, value: u32) -> usize;
```

This is also used by the wire scanner internally. Can go in `wire.rs` or a shared location.

## Tests — `vm.rs` `#[cfg(test)]`

Proto structure for tests:
```
message Outer {
  uint32 id = 1;
  Inner  inner = 2;
}
message Inner {
  string name = 1;
  uint32 value = 2;
}
```

Build input: `encode_varint_field(1, 42) + encode_len_field(2, inner_bytes)` where `inner_bytes = encode_len_field(1, b"Alice") + encode_varint_field(2, 99)`.

| Test | Description |
|------|-------------|
| `frame_simple` | `DISPATCH(SKIP, [1→COPY, 2→FRAME(L0)])`, sub: `DISPATCH(SKIP, [1→COPY])`. Output has `{id, inner{name}}`. Parse output to verify valid proto with correct length prefix. |
| `frame_nested_two` | Three levels deep. Outer → middle → inner. Verify all length prefixes correct. |
| `frame_empty_sub` | FRAME into a sub-message where nothing is copied → sub-output is 0 bytes. Tag + length(0) still emitted. |
| `recurse_deep_search` | `LABEL(L), DISPATCH(RECURSE(L), [1→COPY])`. Field 1 at depth 3. Verify it's found and copied. |
| `recurse_no_match` | RECURSE over nested messages with no field 1 anywhere → output empty. |
| `frame_depth_exceeded` | Deeply nested input exceeding max_frame_depth → `FrameDepthExceeded`. |

## Verification

```sh
cargo test -p wql-runtime
cargo clippy -p wql-runtime
cargo build -p wql-runtime --target wasm32-unknown-unknown
```

---

# Chunk 3d — DECODE + Predicates + Filter

## Goal

Implement `ArmAction::Decode`, all predicate instructions, the bool stack, and the `filter()` / `project_and_filter()` public functions.

## Deliverables

- `vm.rs`: `RegisterValue`, `ArmAction::Decode` handler, all predicate instruction handlers, bool stack operations.
- `lib.rs`: public `filter()` and `project_and_filter()`.
- 25+ predicate/filter tests pass.
- All prior chunk tests still pass.

## DECODE

When `ArmAction::Decode { reg, encoding }` executes for a `WireField`:

| Encoding | Wire source | Decoding | Register |
|----------|-------------|----------|----------|
| `Varint` | `value_bytes` | Read unsigned varint, cast to i64 | `Int(v)` |
| `Sint` | `value_bytes` | Read unsigned varint, zigzag decode | `Int(v)` |
| `I32` | `value_bytes` | Read 4 LE bytes, sign-extend to i64 | `Int(v)` |
| `I64` | `value_bytes` | Read 8 LE bytes, reinterpret as i64 | `Int(v)` |
| `Len` | `len_payload` | Copy bytes | `Bytes(v.to_vec())` |

Uses `wire::read_varint` for the varint-based encodings.

For zigzag decode: `(n >> 1) ^ -(n & 1)` (same formula as `wql-ir`'s codec, but applied here to the field value).

## Predicate Instructions

All operate on `self.registers` and `self.bool_stack`:

```rust
fn eval_predicate(&mut self, instr: &Instruction) -> Result<(), RuntimeError> {
    match instr {
        Instruction::CmpEq { reg, imm } => {
            let result = matches!(self.registers[*reg as usize], Some(RegisterValue::Int(v)) if v == *imm);
            self.bool_stack.push(result);
        }
        // ... etc
        Instruction::And => {
            let b = self.bool_stack.pop().ok_or(RuntimeError::StackUnderflow)?;
            let a = self.bool_stack.pop().ok_or(RuntimeError::StackUnderflow)?;
            self.bool_stack.push(a && b);
        }
        // ... etc
    }
}
```

### Type mismatch rules

- Integer comparison (`CmpEq`, `CmpLt`, etc.) on `Bytes` register → false.
- Integer comparison on `None` register → false (except `CmpNeq` → true).
- Bytes comparison (`CmpLenEq`, `BytesStarts`, etc.) on `Int` register → false.
- Bytes comparison on `None` register → false.
- `InSet` on `None` or `Bytes` → false.
- `IsSet` on `None` → false. `IsSet` on `Some(_)` → true.

### `filter()` public function

```rust
pub fn filter(program: &LoadedProgram, input: &[u8]) -> Result<bool, RuntimeError> {
    // No output buffer needed — pure filter programs never COPY.
    // Allocate a zero-length output to satisfy execute's signature,
    // or use a small stack buffer as a safety net.
    let mut output = [];
    let mut vm = Vm::new(&program.instructions, &program.label_table);
    let (predicate, _) = vm.execute(0, input, &mut output, 0)?;
    Ok(predicate)
}
```

Note: a pure filter program has no COPY instructions, so output is never written. If a program has both COPY and predicates, `filter()` would fail when trying to write — this is fine because `filter()` is only called with filter-only programs. The combined path uses `project_and_filter()`.

### `project_and_filter()` public function

```rust
pub fn project_and_filter(
    program: &LoadedProgram,
    input: &[u8],
    output: &mut [u8],
) -> Result<Option<usize>, RuntimeError> {
    if output.len() < input.len() {
        return Err(RuntimeError::OutputBufferTooSmall);
    }
    let mut vm = Vm::new(&program.instructions, &program.label_table);
    let (predicate, written) = vm.execute(0, input, output, 0)?;
    Ok(if predicate { Some(written) } else { None })
}
```

## Tests

### Filter tests — `vm.rs` `#[cfg(test)]`

Proto structure: message with `age: uint32 = 1`, `name: string = 2`, `status: uint32 = 3`, nested `address { city: string = 1 }` at field 4.

| Test | Description |
|------|-------------|
| `filter_cmp_eq_true` | `age == 25` on age=25 → true. |
| `filter_cmp_eq_false` | `age == 25` on age=30 → false. |
| `filter_cmp_gt` | `age > 18` on age=25 → true, age=10 → false. |
| `filter_cmp_lt` | `age < 18` on age=10 → true. |
| `filter_cmp_lte` | `age <= 18` on age=18 → true, age=19 → false. |
| `filter_cmp_gte` | `age >= 18` on age=18 → true. |
| `filter_cmp_neq` | `age != 0` on age=5 → true, age=0 → false. |
| `filter_string_eq` | `name == "Alice"` using `CMP_LEN_EQ(R0, b"Alice")`. |
| `filter_bytes_starts` | `name starts_with "Al"`. |
| `filter_bytes_ends` | `name ends_with "ce"`. |
| `filter_bytes_contains` | `name contains "lic"`. |
| `filter_in_set_hit` | `status in [1, 2, 3]` with status=2 → true. |
| `filter_in_set_miss` | `status in [1, 2, 3]` with status=5 → false. |
| `filter_in_set_empty` | `status in []` → false. |
| `filter_is_set_true` | `has(age)` with age present → true. |
| `filter_is_set_false` | `has(age)` with age absent → false. |
| `filter_and` | `age > 18 AND name == "Alice"`. Both true → true. Second false → false. |
| `filter_or` | `age > 65 OR status == 1`. First false, second true → true. |
| `filter_not` | `NOT age == 0` on age=5 → true. |
| `filter_nested_predicate` | Predicate on `address.city`: DISPATCH + FRAME + sub-DISPATCH with DECODE. |
| `filter_unset_register` | CmpEq on unset reg → false. CmpNeq on unset → true. IsSet on unset → false. |
| `filter_type_mismatch` | CmpEq (int comparison) on a Bytes register → false. CmpLenEq on an Int register → false. |

### Combined tests

| Test | Description |
|------|-------------|
| `project_and_filter_pass` | Predicate true → `Some(n)` with valid projected output. |
| `project_and_filter_fail` | Predicate false → `None`. |

### Integration / edge cases

| Test | Description |
|------|-------------|
| `frame_preserves_registers` | DECODE inside FRAME, CMP outside FRAME uses the register → works. |
| `empty_input_filter` | Empty input → filter returns true. |
| `empty_program` | `[RETURN]` → true, 0 bytes. |
| `bool_stack_empty` | Pure projection (no predicates) → predicate is true. |
| `stack_underflow` | `AND` with empty stack → `StackUnderflow`. |

## Verification

```sh
cargo test -p wql-runtime
cargo clippy -p wql-runtime
cargo clippy -p wql-runtime --target wasm32-unknown-unknown
cargo fmt --check
```

---

## Constraints & Notes (all chunks)

- **`no_std` + `extern crate alloc`.** No `std` usage. `Vec`, allocations from `alloc`.
- **No I/O, no syscalls.** Pure function: bytes in, bytes out.
- **Program decoded once.** `LoadedProgram::from_bytes` decodes at load time. Per-message execution never re-decodes.
- **Registers persist across FRAME.** Flat register file, not scoped. Register loaded inside FRAME is visible after exit.
- **Output buffer sizing.** Caller provides `output.len() >= input.len()`. Runtime checks and returns `OutputBufferTooSmall`.
- **FRAME length prefix.** 5-byte-gap-then-shift strategy. Costs one `copy_within` per FRAME exit.
- **Bool stack underflow.** `AND`/`OR`/`NOT` on insufficient stack → `StackUnderflow`. Safety net for malformed bytecode.
- **`BYTES_MATCHES` (regex feature).** Not implemented. If encountered, panic (compiler bug — runtime shouldn't receive it without the feature).
- **`IN_SET` empty.** Evaluates to false. No panic.
- **Unknown wire types.** Types 3, 4, 6, 7 in input → `MalformedInput`.
